//! The daemon's event loop.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use dispatch_config::{DelegationLimits, HarnessRegistry, TaskInput, TaskRun};
use dispatch_core::{PaneId, PaneStatus, Project, ProjectId, ProjectSource, RequestId};
use dispatch_os::ipc::{Closer, Connection, Listener};
use dispatch_proto::{
    ClientMessage, DelegateOutcome, Frame, FrameError, PaneUpdate, ProtocolError, Role,
    ServerMessage,
};
use dispatch_pty::{Pty, RunState, Size};

use crate::budgets::Budgets;
use crate::delegation::Pending;
use crate::outbox::{Inbox, Outbox, Refused};
use crate::pane::DaemonPane;
use crate::task_file::{Leftovers, TaskFile};

/// How much of a subagent's output its caller is given.
///
/// Enough for an agent to act on, far short of a session: the pane keeps the
/// rest, and a person can open it.
const TAIL_BYTES: usize = 8 * 1024;

/// How long the loop waits for an event before checking panes again.
///
/// Panes are polled rather than waited on, so this is the floor on how quickly
/// output reaches a client.
const TICK: Duration = Duration::from_millis(8);

/// How long a subagent's output may go on arriving after its process exited.
///
/// A pane's exit and the end of its output are separate events, so a caller
/// waiting on the output is answered once the pseudoterminal is finished rather
/// than once the process is gone. Finished may never come, and not only in the
/// awkward case of a grandchild holding the pseudoterminal open: on Windows it
/// never comes at all, because `Pty` keeps the pseudoconsole alive on purpose
/// and the master therefore never reaches end-of-file. So this is the ordinary
/// path there, not a fallback, and it has to be long enough for ConPTY's pipe to
/// catch up with the process object.
const TAIL_GRACE: Duration = Duration::from_millis(250);

/// How often task files that could not be removed are tried again.
///
/// Often enough that one outlives the process holding it by about this
/// long, and seldom enough that a file that keeps refusing costs nothing.
const RETRY_LEFTOVERS: Duration = Duration::from_secs(1);

/// How long a stopping daemon keeps trying to remove task files its panes'
/// processes, just ended, still held.
const LEFTOVERS_AT_SHUTDOWN: Duration = Duration::from_secs(2);

/// How many events may wait for the loop.
///
/// Full, a client's reader thread waits to hand its next request over, and
/// the socket behind it fills: a client sending faster than the daemon acts
/// is slowed down rather than queued for.
const EVENT_BACKLOG: usize = 1024;

/// When the frame a client is part-way through began, while it is
/// part-way through one.
///
/// Set by the client's reader thread, read by the loop, so a client that
/// starts a frame and stops can be told from one that is merely idle.
#[derive(Clone, Default)]
struct FrameClock(Arc<std::sync::Mutex<Option<Instant>>>);

impl FrameClock {
    fn start(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    fn finish(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn since(&self) -> Option<Instant> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Where task files go unless a test says otherwise.
///
/// Dispatch's own directory for them, the user's alone. With no home
/// directory to put that under, a directory of Dispatch's own inside the
/// temporary directory: `create_private_dir` refuses it if another user
/// made it first, which it could not do for the temporary directory itself.
fn default_task_dir() -> PathBuf {
    match dispatch_os::paths::task_dir() {
        Ok(dir) => dir,
        Err(error) => {
            let fallback = std::env::temp_dir().join("dispatch-tasks");
            tracing::warn!(
                %error,
                dir = %fallback.display(),
                "no directory of Dispatch's own for task files; using one in the temporary directory"
            );
            fallback
        }
    }
}

/// Releases a client's place against `Budgets::max_clients` once nothing is
/// left running on its behalf.
///
/// A seat is taken once, when a client attaches, but two threads act for it
/// afterwards -- its reader and its writer -- and either can outlive the
/// other: a writer can go on delivering to a client whose reader has
/// already ended, so freeing the seat the moment either thread exits would
/// let the other go on costing a slot nothing accounts for. Wrapped in an
/// `Arc` and cloned once per thread, so `Drop` runs exactly once, whichever
/// thread's clone happens to be the last to go -- which also covers a
/// thread that panics rather than returning.
struct Seat(Arc<AtomicUsize>);

impl Drop for Seat {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Tells the loop a client's reader thread has ended, however it ended.
///
/// Held by that thread and sent from its `Drop`, not written inline after
/// its read loop: a frame a peer sent that fails to decode ends the thread
/// through a panic, which unwinds past any code placed after the loop, so
/// only a guard's `Drop` -- run during that unwind the same as at an
/// ordinary return -- reaches the loop either way. Without it, a client
/// whose reader crashed would stay in `self.clients` forever: nothing would
/// ever close its connection or free its seat, because both wait on the
/// `Event::Detached` this sends.
struct DetachOnDrop {
    id: ClientId,
    events: SyncSender<Event>,
}

impl Drop for DetachOnDrop {
    fn drop(&mut self) {
        let _ = self.events.send(Event::Detached(self.id));
    }
}

/// Failures starting or running the daemon.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// The transport failed.
    #[error(transparent)]
    Ipc(#[from] dispatch_os::ipc::IpcError),

    /// Configuration could not be read.
    #[error(transparent)]
    Config(#[from] dispatch_config::ConfigError),
}

/// Identifies one attached client.
type ClientId = u64;

/// Asks a running daemon to stop.
///
/// Taken before [`Daemon::serve`] is called, because that consumes the daemon.
/// A signal handler runs on another thread, so this is the flag it sets rather
/// than a method on the daemon itself.
#[derive(Debug, Clone)]
pub struct Shutdown(Arc<AtomicBool>);

impl Shutdown {
    /// Asks the daemon to stop after its current pass.
    pub fn request(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether a stop has been asked for.
    #[must_use]
    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// What connects the loop to one client's threads.
struct Wiring {
    /// Its queue.
    outbox: Outbox,
    /// Ends its connection, both halves, whoever holds them.
    closer: Closer,
    /// When it is part-way through a frame, set by its reader thread.
    frame: FrameClock,
}

/// Something the loop reacts to.
enum Event {
    /// A client attached.
    Attached(ClientId, Wiring),
    /// A client said something.
    Request(ClientId, ClientMessage),
    /// A client went away.
    Detached(ClientId),
}

/// An attached client.
struct Client {
    outbox: Outbox,
    /// Ends its connection, both halves, whoever holds them.
    closer: Closer,
    /// Whether it has asked for pane events. A client that has not subscribed
    /// is still connected but silent, which is what a one-shot command wants.
    subscribed: bool,
    /// What the connection is for, from its `Hello`. Output and delegation
    /// prompts are broadcast to interface clients only; a delegate caller
    /// wants the fate of its own request and nothing else.
    role: Role,
    /// Whether its `Hello` has been accepted.
    ///
    /// Nothing but a `Hello` is acted on before then: a peer that has not
    /// said which protocol it speaks may mean something else by every byte
    /// that follows, and one that was refused must not get to act anyway.
    ready: bool,
    /// When it is part-way through a frame, so [`Daemon::enforce_deadlines`]
    /// can tell a stalled client from an idle one.
    frame: FrameClock,
    /// When it attached, so [`Daemon::enforce_deadlines`] can tell a client
    /// that is taking too long to say `Hello`.
    attached: Instant,
}

/// The daemon.
pub struct Daemon {
    panes: HashMap<PaneId, DaemonPane>,
    clients: HashMap<ClientId, Client>,
    harnesses: HarnessRegistry,
    projects: HashMap<ProjectId, Project>,
    events: Receiver<Event>,
    sender: SyncSender<Event>,
    device: String,
    stop: Arc<AtomicBool>,
    limits: DelegationLimits,
    /// Limits on what one client may cost the daemon.
    budgets: Budgets,
    /// Requests asked about and not yet answered.
    pending: HashMap<RequestId, Pending>,
    /// Panes the user has approved for every future request, for as long as
    /// this daemon runs.
    blanket: HashSet<PaneId>,
    /// Where a task delivered in a file is written.
    task_dir: PathBuf,
    /// Task files whose removal failed, to be tried again.
    leftovers: Leftovers,
    /// When `leftovers` was last tried.
    leftovers_tried: Instant,
}

impl Daemon {
    /// Creates a daemon serving `harnesses`, with the default delegation
    /// limits.
    #[must_use]
    pub fn new(harnesses: HarnessRegistry, device: impl Into<String>) -> Self {
        Self::with_limits(harnesses, device, DelegationLimits::default())
    }

    /// Creates a daemon serving `harnesses`, with explicit delegation limits.
    #[must_use]
    pub fn with_limits(
        harnesses: HarnessRegistry,
        device: impl Into<String>,
        limits: DelegationLimits,
    ) -> Self {
        let (sender, events) = sync_channel(EVENT_BACKLOG);

        Self {
            panes: HashMap::new(),
            clients: HashMap::new(),
            harnesses,
            projects: HashMap::new(),
            events,
            sender,
            device: device.into(),
            stop: Arc::new(AtomicBool::new(false)),
            limits,
            budgets: Budgets::default(),
            pending: HashMap::new(),
            blanket: HashSet::new(),
            task_dir: default_task_dir(),
            leftovers: Leftovers::default(),
            leftovers_tried: Instant::now(),
        }
    }

    /// Replaces the limits on what one client may cost.
    ///
    /// Before `serve`, which consumes the daemon.
    pub fn set_budgets(&mut self, budgets: Budgets) {
        self.budgets = budgets;
    }

    /// Where task files are written: [`dispatch_os::paths::task_dir`] unless
    /// told otherwise.
    #[doc(hidden)]
    pub fn set_task_dir(&mut self, dir: PathBuf) {
        self.task_dir = dir;
    }

    /// Returns the handle that stops this daemon.
    ///
    /// Must be taken before [`Daemon::serve`], which consumes the daemon.
    #[must_use]
    pub fn shutdown_handle(&self) -> Shutdown {
        Shutdown(Arc::clone(&self.stop))
    }

    /// Registers a project the daemon will spawn panes in.
    ///
    /// Reopening a root already registered returns the id it already has, so a
    /// client that opens the same directory twice does not end up with two
    /// entries for one checkout.
    ///
    /// `root` is expected to be absolute and to exist; the daemon resolves it
    /// for a client in [`ClientMessage::OpenProject`].
    pub fn open_project(&mut self, root: PathBuf) -> ProjectId {
        if let Some(existing) = self.projects.values().find(|p| p.root == root) {
            return existing.id;
        }

        // The daemon is on the machine the directory is on, so it is the only
        // side that can tell a repository from a plain directory.
        let source = if root.join(".git").exists() {
            ProjectSource::GitRepo { remote: None }
        } else {
            ProjectSource::LocalDir
        };

        let project = Project::new(root, source);
        let id = project.id;
        self.projects.insert(id, project);
        id
    }

    /// The projects registered, in no particular order.
    #[must_use]
    pub fn projects(&self) -> Vec<Project> {
        self.projects.values().cloned().collect()
    }

    /// How many panes are running.
    #[must_use]
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Accepts connections until the listener fails, serving them all.
    ///
    /// First sweeps up the task files a daemon that did not stop cleanly
    /// left behind: `listener` is bound, so this is the one daemon serving
    /// this configuration, and nothing still means to hand those files over.
    pub fn serve(mut self, listener: Listener) -> Result<(), DaemonError> {
        crate::task_file::sweep(&self.task_dir);

        let sender = self.sender.clone();
        let max_clients = self.budgets.max_clients;
        let live = Arc::new(AtomicUsize::new(0));
        let mut next_id = 0;

        // Accepting blocks, so it runs on its own thread and hands each
        // connection to the loop.
        std::thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok(connection) => {
                        // Counted by seats, held until both of a client's
                        // threads have ended; closed at once rather than
                        // served badly.
                        if live.load(Ordering::Relaxed) >= max_clients {
                            tracing::warn!(
                                max_clients,
                                "turning a client away: too many are connected"
                            );
                            connection.closer().close();
                            continue;
                        }
                        next_id += 1;
                        if spawn_client(next_id, connection, &sender, &live).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to accept a connection");
                        break;
                    }
                }
            }
        });

        self.run();
        Ok(())
    }

    /// The loop. Public so tests can drive it without a listener.
    ///
    /// Returns once [`Shutdown::request`] has been called, after killing the
    /// panes: they are the daemon's children, and an orphan agent no client can
    /// ever reattach to is worse than a stopped one.
    pub fn run(&mut self) {
        while !self.stop.load(Ordering::Relaxed) {
            match self.events.recv_timeout(TICK) {
                Ok(event) => self.handle(event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                // Every sender is gone, which cannot happen while the daemon
                // holds one, so this means shutdown.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }

            self.pump_panes();
            self.enforce_deadlines();
        }

        self.close_all_panes();
        self.clear_leftovers_on_the_way_out();
    }

    /// Tries once more to remove task files a pane's process still held,
    /// for as long as the processes just ended may take to let go.
    fn clear_leftovers_on_the_way_out(&mut self) {
        let deadline = Instant::now() + LEFTOVERS_AT_SHUTDOWN;
        loop {
            self.leftovers.retry();
            if self.leftovers.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.leftovers.report();
    }

    /// Terminates every pane, so nothing outlives the daemon.
    fn close_all_panes(&mut self) {
        for (id, mut pane) in self.panes.drain() {
            tracing::info!(pane = %id, "terminating a pane on shutdown");
            pane.session.terminate();
        }
    }

    /// Runs one pass, for tests.
    pub fn tick(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.handle(event);
        }
        self.pump_panes();
        self.enforce_deadlines();
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Attached(id, wiring) => {
                self.clients.insert(
                    id,
                    Client {
                        outbox: wiring.outbox,
                        closer: wiring.closer,
                        subscribed: false,
                        role: Role::default(),
                        ready: false,
                        frame: wiring.frame,
                        attached: Instant::now(),
                    },
                );
                tracing::info!(client = id, "client attached");
            }
            Event::Detached(id) => {
                // A client `refuse`d or already `hang_up`'d on is forgotten
                // already: its writer thread is on its own from here,
                // flushing a refusal or already being closed.
                if self.clients.contains_key(&id) {
                    // Its own sending half has ended, so the client itself
                    // is gone -- not merely idle, which is between frames,
                    // not the end of the stream -- and closing the
                    // connection now is what stops its writer from
                    // lingering, stuck delivering to a peer that stopped
                    // reading or has already left, holding a thread, a
                    // socket and its seat open for nothing.
                    self.hang_up(id);
                } else {
                    self.abandon(id);
                }
                tracing::info!(client = id, "client detached");
            }
            Event::Request(id, message) => self.handle_request(id, message),
        }
    }

    fn handle_request(&mut self, id: ClientId, message: ClientMessage) {
        let Some(client) = self.clients.get(&id) else {
            // Refused, hung up on, or detached. Its reader may still be
            // forwarding frames it had already read -- a peer can send a
            // request right behind a Hello it is about to be refused for --
            // and none of them is anyone's to act on.
            tracing::debug!(client = id, "ignoring a request from a client that is gone");
            return;
        };

        if !client.ready && !matches!(message, ClientMessage::Hello { .. }) {
            tracing::info!(client = id, "a client spoke before its Hello");
            self.send(
                id,
                ServerMessage::Error {
                    error: ProtocolError::Other("the connection must begin with a Hello".into()),
                },
            );
            self.refuse(id);
            return;
        }

        match message {
            ClientMessage::Hello {
                version,
                client,
                role,
            } => {
                if !dispatch_proto::VERSION.is_compatible_with(version) {
                    // Refuse rather than proceed: a major mismatch means the
                    // peer may misread anything sent after this.
                    self.send(
                        id,
                        ServerMessage::Error {
                            error: ProtocolError::IncompatibleVersion {
                                peer: version,
                                ours: dispatch_proto::VERSION,
                            },
                        },
                    );
                    self.refuse(id);
                    return;
                }

                if let Some(existing) = self.clients.get_mut(&id) {
                    existing.role = role;
                    existing.ready = true;
                }

                tracing::info!(client = id, %version, name = %client, "handshake accepted");
                self.send(
                    id,
                    ServerMessage::Welcome {
                        version: dispatch_proto::VERSION,
                        device: self.device.clone(),
                    },
                );
            }

            ClientMessage::Subscribe => {
                let role = self.clients.get(&id).map(|c| c.role).unwrap_or_default();
                if let Some(client) = self.clients.get_mut(&id) {
                    client.subscribed = true;
                }

                // A delegate caller draws nothing: it waits on the fate of one
                // request, and the fleet's projects, panes, history, statuses
                // and prompts would be a firehose it never reads. `broadcast`
                // already keeps all of that from reaching it as things happen;
                // without this, `Subscribe`'s catch-up would hand it the same
                // firehose in one burst the instant it asked.
                if role != Role::Interface {
                    return;
                }

                // Describe what already exists, so a client attaching to a
                // running daemon sees the panes rather than waiting for one to
                // change. Projects come first: a pane names the project it
                // belongs to, and a client cannot place one it has not heard
                // of.
                let mut existing: Vec<ServerMessage> = self
                    .projects
                    .values()
                    .map(|project| ServerMessage::ProjectOpened {
                        project: project.clone(),
                    })
                    .collect();

                for pane in self.panes.values() {
                    existing.push(ServerMessage::PaneSpawned {
                        pane: pane.id,
                        project: pane.project,
                        harness: pane.harness.clone(),
                        parent: pane.parent,
                        durable: pane.durable,
                    });

                    // What the pane has printed, so a client that reattaches
                    // sees the work rather than a blank rectangle.
                    if !pane.history.is_empty() {
                        existing.push(ServerMessage::PaneOutput {
                            pane: pane.id,
                            bytes: pane.history.clone(),
                        });
                    }

                    // A pane whose process has already exited says so, since
                    // the change happened before this client was listening.
                    if !pane.status.is_live() {
                        existing.push(ServerMessage::PaneChanged {
                            pane: pane.id,
                            update: PaneUpdate::Status {
                                status: pane.status,
                            },
                        });
                    }
                }

                // A request already put to the user is put to this client too,
                // rather than only to whoever was subscribed at the time.
                //
                // Oldest first: that is the order the client documents its own
                // queue in and the order it shows them, so `HashMap` order would
                // otherwise decide which prompt a reattaching user is asked
                // about first — and it would not be the one that has been
                // waiting longest.
                let mut waiting: Vec<&Pending> = self.pending.values().collect();
                waiting.sort_by_key(|pending| pending.asked);
                for pending in waiting {
                    existing.push(pending.announcement.clone());
                }

                self.send_batch(id, existing);
            }

            ClientMessage::OpenProject { root } => self.open_project_for(id, root),

            ClientMessage::CloseProject { project } => self.close_project_for(id, project),

            ClientMessage::SpawnPane {
                project,
                harness,
                size,
            } => self.spawn_pane(id, project, &harness, Size::new(size.0, size.1)),

            ClientMessage::WritePane { pane, bytes } => {
                let Some(target) = self.panes.get_mut(&pane) else {
                    self.send(
                        id,
                        ServerMessage::Error {
                            error: ProtocolError::NoSuchPane(pane),
                        },
                    );
                    return;
                };

                match target.session.write(&bytes) {
                    Ok(()) => {}
                    // Said to the one client that sent it, which is the one
                    // whose paste just went nowhere; the rest of the fleet
                    // has no use for it.
                    Err(dispatch_pty::PtyError::InputFull { waiting }) => {
                        let dropped = bytes.len();
                        self.send(
                            id,
                            ServerMessage::Error {
                                error: ProtocolError::Other(format!(
                                    "pane {pane} is not reading its input: {waiting} bytes are \
                                     still waiting for it, so these {dropped} were dropped"
                                )),
                            },
                        );
                    }
                    Err(error) => tracing::warn!(%error, "failed to write to a pane"),
                }
            }

            ClientMessage::ResizePane { pane, size } => {
                let Some(target) = self.panes.get_mut(&pane) else {
                    self.send(
                        id,
                        ServerMessage::Error {
                            error: ProtocolError::NoSuchPane(pane),
                        },
                    );
                    return;
                };

                if let Err(error) = target.session.resize(Size::new(size.0, size.1)) {
                    tracing::warn!(%error, "failed to resize a pane");
                }
            }

            ClientMessage::ClosePane { pane } => {
                if let Some(mut target) = self.panes.remove(&pane) {
                    // The pane is being killed, not allowed to finish; its
                    // caller, if it has one, is answered here or not at all.
                    self.answer_for_a_closed_subagent(&mut target);
                    target.session.terminate();
                    // A pane that is gone can be asked for nothing more, so its
                    // blanket approval goes with it.
                    self.blanket.remove(&pane);
                    self.broadcast(ServerMessage::PaneClosed { pane });
                    self.refuse_requests_from(pane);
                    self.drop_children_of(pane);
                } else {
                    self.send(
                        id,
                        ServerMessage::Error {
                            error: ProtocolError::NoSuchPane(pane),
                        },
                    );
                }
            }

            ClientMessage::Ping { token } => self.send(id, ServerMessage::Pong { token }),

            ClientMessage::DelegateRequest {
                parent,
                harness,
                task,
                size,
            } => self.delegate_request(id, parent, harness, task, size),

            ClientMessage::DelegateDecision {
                request,
                approve,
                blanket,
            } => {
                let Some(waiting) = self.pending.remove(&request) else {
                    // Already answered, by another client or by the deadline.
                    return;
                };

                if !approve {
                    self.resolve(waiting.id, waiting.caller, DelegateOutcome::Denied);
                    return;
                }

                if blanket {
                    self.blanket.insert(waiting.parent);
                }

                self.approve(
                    waiting.id,
                    waiting.parent,
                    &waiting.harness,
                    &waiting.task,
                    waiting.size,
                    waiting.caller,
                    blanket,
                );
            }

            ClientMessage::Unknown => {
                tracing::debug!(
                    client = id,
                    "received an unknown message from a newer peer; ignoring"
                );
            }
        }
    }

    /// Resolves a client's path and registers it, telling everyone.
    fn open_project_for(&mut self, client: ClientId, root: PathBuf) {
        // Resolved here rather than on the client: the client may be on another
        // machine, and a relative or symlinked path has to mean the same thing
        // to every client looking at this project. `resolve` rather than
        // `canonicalize` because this becomes a pane's working directory, and
        // Windows' extended-length spelling is one some programs refuse.

        // `~` first: a root typed for this machine from another one arrives
        // with no shell having expanded it.
        let expanded = dispatch_os::paths::expand_home(&root);

        let reason = match dispatch_os::paths::resolve(&expanded) {
            Ok(resolved) if resolved.is_dir() => {
                self.opened_for(client, root, resolved);
                return;
            }
            Ok(resolved) => format!("not a directory: {}", resolved.display()),
            Err(error) => error.to_string(),
        };

        // Named by the root as it was sent, not as it resolved: the client
        // keeps what it typed, and has to be able to find it to forget it.
        self.send(client, ServerMessage::ProjectRefused { root, reason });
    }

    /// Registers a root that has been resolved and checked, and tells every
    /// subscriber.
    ///
    /// `root` is the root as `client` sent it, which only that client keeps.
    fn opened_for(&mut self, client: ClientId, root: PathBuf, resolved: PathBuf) {
        let id = self.open_project(resolved);
        let project = self.projects[&id].clone();
        tracing::info!(project = %id, root = %project.root.display(), "project opened");

        // The asker keeps what it typed, and the row it is about to get names
        // what that became. Told first, so by the time the row arrives the
        // asker's records already match it — and even when the two are equal,
        // since checking would only move the comparison here from the client.
        self.send(
            client,
            ServerMessage::ProjectResolved {
                root,
                resolved: project.root.clone(),
            },
        );

        // Every client hears about it: they are looking at the same fleet, and
        // a project one of them opened is one they can all spawn into.
        self.broadcast(ServerMessage::ProjectOpened { project });
    }

    /// Forgets a project, once nothing is running in it.
    ///
    /// The daemon outlives its clients, so a project it keeps is one the next
    /// `Subscribe` announces again: a client that dropped it from its own list
    /// would be handed it straight back. Refused while it has panes — they are
    /// the daemon's, and a project it had forgotten would leave them running
    /// with no row to reach them by.
    fn close_project_for(&mut self, client: ClientId, project: ProjectId) {
        if !self.projects.contains_key(&project) {
            self.send(
                client,
                ServerMessage::Error {
                    error: ProtocolError::NoSuchProject(project),
                },
            );
            return;
        }

        if self.panes.values().any(|pane| pane.project == project) {
            self.send(
                client,
                ServerMessage::Error {
                    error: ProtocolError::Other(format!(
                        "project {project} still has panes; close them first"
                    )),
                },
            );
            return;
        }

        self.projects.remove(&project);
        tracing::info!(project = %project, "project closed");

        // Every client hears about it, as they do when one is opened: they are
        // looking at the same fleet.
        self.broadcast(ServerMessage::ProjectClosed { project });
    }

    fn spawn_pane(&mut self, client: ClientId, project: ProjectId, harness: &str, size: Size) {
        let Some(root) = self.projects.get(&project).map(|p| p.root.clone()) else {
            self.send(
                client,
                ServerMessage::Error {
                    error: ProtocolError::NoSuchProject(project),
                },
            );
            return;
        };

        let Some(def) = self.harnesses.get(harness) else {
            self.send(
                client,
                ServerMessage::Error {
                    error: ProtocolError::Other(format!("unknown harness {harness:?}")),
                },
            );
            return;
        };

        let mut launch = def.launch_for_current_platform();
        let id = PaneId::new();
        for (key, value) in self.pane_env(id) {
            launch.env.entry(key).or_insert(value);
        }

        let session = match Pty::spawn(&launch, &root, size) {
            Ok(session) => session,
            Err(error) => {
                self.send(
                    client,
                    ServerMessage::Error {
                        error: ProtocolError::Other(format!("failed to start {harness}: {error}")),
                    },
                );
                return;
            }
        };

        let pane = DaemonPane {
            id,
            session,
            harness: harness.to_string(),
            project,
            history: Vec::new(),
            status: PaneStatus::Starting,
            parent: None,
            durable: true,
            request: None,
            caller: None,
            exited_at: None,
            task_file: None,
        };
        // Announced from the pane's own field rather than repeated here: what a
        // client draws has to be what the daemon is holding.
        let durable = pane.durable;
        self.panes.insert(id, pane);

        // At INFO because a pane appearing and a pane surviving a client are the
        // two things a report about the daemon is usually about, and without
        // this the log says a project opened and then nothing.
        tracing::info!(pane = %id, %harness, project = %project, "pane spawned");

        // Every client hears about it, not just the one that asked, because
        // they are all looking at the same fleet.
        self.broadcast(ServerMessage::PaneSpawned {
            pane: id,
            project,
            harness: harness.to_string(),
            parent: None,
            durable,
        });
    }

    /// The environment a pane needs to talk back to this daemon.
    ///
    /// `DISPATCH_PANE` is attribution, not a permission: the socket is
    /// owner-only, and anything that can connect can already spawn panes. It
    /// decides which pane a request is attributed to, and protects nothing.
    ///
    /// `PATH` gains the directory holding the `dispatch` binary — a sibling of
    /// this executable — so `dispatch delegate` is runnable from inside a pane.
    /// Where there is no sibling, `PATH` is left alone: "command not found" is
    /// honest, and a daemon pretending otherwise is not.
    fn pane_env(&self, pane: PaneId) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("DISPATCH_PANE".to_string(), pane.to_string());

        if let Ok(dir) = dispatch_os::paths::config_dir() {
            env.insert(
                dispatch_os::paths::CONFIG_DIR_ENV.to_string(),
                dir.display().to_string(),
            );
        }

        if let Some(bin) = client_binary_dir() {
            let existing = std::env::var("PATH").unwrap_or_default();
            let separator = if cfg!(windows) { ";" } else { ":" };
            env.insert(
                "PATH".to_string(),
                format!("{}{separator}{existing}", bin.display()),
            );
        }

        env
    }

    /// The one-shot run of `task` under `harness`, with the environment pane
    /// `pane` would start with, or `None` when the harness has no one-shot
    /// form for this platform.
    ///
    /// One place builds it, so what is judged before a run starts is what
    /// starts.
    fn task_run(&self, harness: &str, task: &str, pane: PaneId) -> Option<TaskRun> {
        let mut run = self.harnesses.get(harness)?.task_launch(task)?;
        for (key, value) in self.pane_env(pane) {
            run.launch.env.entry(key).or_insert(value);
        }
        if run.input == TaskInput::Argument {
            // Its task is in its arguments, so a task file named in its
            // environment could only be stale -- inherited, or set in the
            // harness file -- and a redirect against it would read another
            // file than the task.
            run.launch.env.remove(dispatch_config::TASK_FILE_ENV);
            run.launch
                .unset
                .insert(dispatch_config::TASK_FILE_ENV.to_string());
        }
        Some(run)
    }

    /// Why `run` must not start, if the form it came from puts the task where
    /// a shell parses it.
    fn unsafe_task_form(&self, harness: &str, run: &TaskRun) -> Option<String> {
        self.harnesses
            .get(harness)?
            .task_refusal_as(std::env::consts::OS, &run.launch)
    }

    /// Refuses, approves, or asks about a request to delegate.
    fn delegate_request(
        &mut self,
        caller: ClientId,
        parent: PaneId,
        harness: String,
        task: String,
        size: (u16, u16),
    ) {
        // One id for the whole call: a refusal answers with the same id a
        // pending entry would have carried, rather than one the caller never
        // saw.
        let request = RequestId::new();

        let Some(asking) = self.panes.get(&parent) else {
            self.send(
                caller,
                ServerMessage::Error {
                    error: ProtocolError::NoSuchPane(parent),
                },
            );
            return;
        };

        let project = asking.project;
        // An empty harness means "whatever the asking pane is running": an agent
        // delegating to another of itself is the common case.
        let harness = if harness.is_empty() {
            asking.harness.clone()
        } else {
            harness
        };

        // Built as `approve` will build it, environment and all: which file a
        // bare command names depends on `PATH`. The pane id is a stand-in,
        // since nothing is judged by it.
        let run = self.task_run(&harness, &task, PaneId::new());

        // A form that would put the task on cmd.exe's command line is refused
        // whatever the caps say: approving it would not make it safe.
        if let Some(reason) = run
            .as_ref()
            .and_then(|run| self.unsafe_task_form(&harness, run))
        {
            tracing::info!(%parent, %harness, %reason, "refused an unsafe task form");
            self.resolve(request, caller, DelegateOutcome::Refused { reason });
            return;
        }

        let depth = self.depth_of(parent);
        let live = self.live_children(parent);
        // The same predicate `approve` will use to actually launch it: a
        // harness with `[task]` but an empty argument list has no form either,
        // and asking the user about it only to refuse it after they approve is
        // worse than refusing up front.
        let has_task_form = run.is_some();

        if let Some(reason) =
            crate::delegation::refusal(depth, live, self.limits, has_task_form, &harness)
        {
            tracing::info!(%parent, %harness, %reason, "refused a delegation");
            self.resolve(request, caller, DelegateOutcome::Refused { reason });
            return;
        }

        // A pane the user has already approved for everything does not ask
        // again, for as long as this daemon runs.
        if self.blanket.contains(&parent) {
            self.approve(request, parent, &harness, &task, size, caller, true);
            return;
        }

        let announcement = ServerMessage::DelegatePending {
            request,
            parent,
            project,
            harness: harness.clone(),
            task: task.clone(),
            depth,
        };

        self.pending.insert(
            request,
            Pending {
                id: request,
                parent,
                harness,
                task,
                size,
                caller,
                asked: Instant::now(),
                announcement: announcement.clone(),
            },
        );

        self.broadcast(announcement);
    }

    /// How many parents the pane already has above it.
    fn depth_of(&self, pane: PaneId) -> u8 {
        let mut depth: u8 = 0;
        let mut current = self.panes.get(&pane).and_then(|p| p.parent);

        while let Some(id) = current {
            depth = depth.saturating_add(1);
            current = self.panes.get(&id).and_then(|p| p.parent);
        }

        depth
    }

    /// How many of a pane's subagents are still running.
    fn live_children(&self, parent: PaneId) -> usize {
        self.panes
            .values()
            .filter(|p| p.parent == Some(parent))
            .filter(|p| matches!(p.session.state(), RunState::Running))
            .count()
    }

    /// Starts an approved subagent and tells everyone.
    #[allow(clippy::too_many_arguments)]
    fn approve(
        &mut self,
        request: RequestId,
        parent: PaneId,
        harness: &str,
        task: &str,
        size: (u16, u16),
        caller: ClientId,
        durable: bool,
    ) {
        // The pane or its project can be gone by the time an approval reaches
        // here — a decision that crossed with the pane closing, or a blanket
        // approval racing a close within the same call. Either way the caller
        // must be answered rather than left hanging: `DelegateDecision` has
        // already dropped this request's `Pending` entry, so nothing else will
        // ever get to it. No prompt needs withdrawing from anyone else here:
        // a request whose pane closed while pending was already resolved by
        // `refuse_requests_from`, and a blanket approval never had one.
        let Some(asking) = self.panes.get(&parent) else {
            self.send(
                caller,
                ServerMessage::DelegateResolved {
                    request,
                    outcome: DelegateOutcome::Refused {
                        reason: "the pane that asked has been closed".into(),
                    },
                },
            );
            return;
        };
        let project = asking.project;

        let Some(root) = self.projects.get(&project).map(|p| p.root.clone()) else {
            self.send(
                caller,
                ServerMessage::DelegateResolved {
                    request,
                    outcome: DelegateOutcome::Refused {
                        reason: format!("project {project} no longer exists"),
                    },
                },
            );
            return;
        };

        let id = PaneId::new();
        let run = self.task_run(harness, task, id);

        // Judged again on the run that is about to start, as the request
        // was when it arrived: which file a bare command names is decided
        // by the filesystem now, not then.
        if let Some(reason) = run
            .as_ref()
            .and_then(|run| self.unsafe_task_form(harness, run))
        {
            tracing::info!(%parent, %harness, %reason, "refused an unsafe task form on approval");
            self.resolve(request, caller, DelegateOutcome::Refused { reason });
            return;
        }

        // Asked again here, and not only when the request arrived: several
        // requests can each see a free slot while they wait, and every one
        // of them would start on approval. The cap is on what runs, so it is
        // enforced where things start running -- with the same predicate the
        // request was first judged by, so the two can never disagree.
        let depth = self.depth_of(parent);
        let live = self.live_children(parent);
        if let Some(reason) =
            crate::delegation::refusal(depth, live, self.limits, run.is_some(), harness)
        {
            tracing::info!(%parent, %harness, %reason, "refused an approved delegation");
            self.resolve(request, caller, DelegateOutcome::Refused { reason });
            return;
        }
        let Some(run) = run else {
            // `refusal` refuses a missing form first, so this cannot be
            // reached; kept as a refusal rather than a panic all the same.
            self.resolve(
                request,
                caller,
                DelegateOutcome::Refused {
                    reason: format!("harness {harness:?} has no [task] form"),
                },
            );
            return;
        };

        let mut launch = run.launch;
        let task_file = match run.input {
            TaskInput::Argument => None,
            TaskInput::File => {
                match TaskFile::write(&self.task_dir, request, task, &self.leftovers) {
                    Ok(file) => {
                        launch.env.insert(
                            dispatch_config::TASK_FILE_ENV.to_string(),
                            file.for_redirect(),
                        );
                        Some(file)
                    }
                    Err(error) => {
                        self.resolve(
                            request,
                            caller,
                            DelegateOutcome::Refused {
                                reason: format!(
                                    "could not write the task down for {harness}: {error}"
                                ),
                            },
                        );
                        return;
                    }
                }
            }
        };

        let session = match Pty::spawn(&launch, &root, Size::new(size.0, size.1)) {
            Ok(session) => session,
            Err(error) => {
                self.resolve(
                    request,
                    caller,
                    DelegateOutcome::Refused {
                        reason: format!("failed to start {harness}: {error}"),
                    },
                );
                return;
            }
        };

        self.panes.insert(
            id,
            DaemonPane {
                id,
                session,
                harness: harness.to_string(),
                project,
                history: Vec::new(),
                status: PaneStatus::Starting,
                parent: Some(parent),
                durable,
                request: Some(request),
                caller: Some(caller),
                exited_at: None,
                task_file,
            },
        );

        self.resolve(request, caller, DelegateOutcome::Approved { pane: id });

        self.broadcast(ServerMessage::PaneSpawned {
            pane: id,
            project,
            harness: harness.to_string(),
            parent: Some(parent),
            durable,
        });
    }

    /// Resolves a request: answers its caller, and tells every other interface
    /// client so a prompt already on screen does not linger past its answer.
    ///
    /// The caller is excluded from the broadcast half: it already has its
    /// answer from the direct send, and a caller that is also a subscribed
    /// interface client — an agent delegating from a pane someone is watching —
    /// would otherwise be told twice.
    fn resolve(&mut self, request: RequestId, caller: ClientId, outcome: DelegateOutcome) {
        let message = ServerMessage::DelegateResolved { request, outcome };
        self.send(caller, message.clone());
        self.broadcast_except(Some(caller), message);
    }

    /// Answers the caller of a subagent whose pane is being closed under it.
    ///
    /// The pane leaves the map right after this, so `pump_panes` will never see
    /// its exit, and the request left `pending` when it was approved: without
    /// this the caller waits for a result that nothing will ever send. The exit
    /// is reported as -1 because the subagent did not choose it — the work was
    /// cut short, and a fabricated success or failure code would both be lies.
    fn answer_for_a_closed_subagent(&mut self, pane: &mut DaemonPane) {
        let (Some(request), Some(caller)) = (pane.request.take(), pane.caller.take()) else {
            return;
        };

        let start = pane.history.len().saturating_sub(TAIL_BYTES);
        let tail = pane.history[start..].to_vec();

        self.send(
            caller,
            ServerMessage::DelegateFinished {
                request,
                exit: -1,
                tail,
            },
        );
    }

    /// Refuses the requests a closing pane was waiting on.
    ///
    /// Its agent is going away with it, so there is nobody left to hand a
    /// subagent's output to, and a prompt for a pane that no longer exists must
    /// not be answerable.
    fn refuse_requests_from(&mut self, parent: PaneId) {
        let orphaned: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, waiting)| waiting.parent == parent)
            .map(|(id, _)| *id)
            .collect();

        for request in orphaned {
            let Some(waiting) = self.pending.remove(&request) else {
                continue;
            };
            self.resolve(
                request,
                waiting.caller,
                DelegateOutcome::Refused {
                    reason: "the pane that asked has been closed".into(),
                },
            );
        }
    }

    /// Drops what a departed client was waiting on.
    ///
    /// A one-off subagent still running exists to answer a caller. No caller,
    /// no reason to keep spending, so it goes. A blanket-approved one keeps
    /// running: that is what the user said when they approved the pane rather
    /// than the request.
    ///
    /// A one-off subagent that has already exited is a different case: in
    /// practice `dispatch delegate` exits the instant it has its
    /// `DelegateFinished`, so this runs on almost every successful delegation.
    /// The pane's output is exactly what [`TAIL_BYTES`] exists so a person can
    /// still read past the caller's own slice of it; reaping it here would
    /// throw that away for no reason. It is orphaned rather than terminated: its
    /// `caller` and `request` are cleared so nothing later tries to answer a
    /// caller that is gone, and it is left for a person to close.
    fn abandon(&mut self, caller: ClientId) {
        // Withdrawn rather than silently dropped. Ctrl-C on `dispatch delegate`
        // is the case the design names, and it is the one that used to leave a
        // prompt on screen for a request that no longer exists: the user presses
        // `a`, `DelegateDecision` finds no pending entry, and nothing at all
        // happens. Every other resolution path goes through `resolve`, which
        // broadcasts; this one cannot, because the client `resolve` would answer
        // is the one that just went away. So only the interface clients hear it,
        // and they hear exactly what closes a prompt.
        let dropped: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, waiting)| waiting.caller == caller)
            .map(|(id, _)| *id)
            .collect();

        for request in dropped {
            self.pending.remove(&request);
            tracing::info!(%request, "the caller of a pending delegation has gone");
            self.broadcast(ServerMessage::DelegateResolved {
                request,
                outcome: DelegateOutcome::Refused {
                    reason: "the call that asked for it has gone".into(),
                },
            });
        }

        let mut orphaned = Vec::new();
        for pane in self.panes.values_mut() {
            if pane.caller != Some(caller) || pane.durable {
                continue;
            }

            if matches!(pane.session.state(), RunState::Running) {
                orphaned.push(pane.id);
            } else {
                pane.caller = None;
                pane.request = None;
            }
        }

        self.terminate_panes(orphaned);
    }

    /// Terminates a closed pane's one-off children, leaving durable ones running.
    ///
    /// A closed pane can ask for nothing more, so anything it started to answer
    /// a caller that no longer exists goes with it; a blanket-approved child is
    /// the user's own approval of that pane's work, not of this one, and outlives
    /// it.
    ///
    /// Recurses into each dropped pane's own children, or a grandchild would be
    /// left with a `parent` pointing at nothing: `depth_of` would under-report
    /// its depth and `live_children` would undercount its parent's live
    /// subagents. Guarded on the ids already collected in this call, not the
    /// whole pane table, so a parent link that somehow formed a cycle cannot
    /// loop forever — it can still revisit a pane through two different
    /// branches, and the guard is what keeps that from being infinite rather
    /// than merely redundant.
    fn drop_children_of(&mut self, parent: PaneId) {
        let mut ids = Vec::new();
        self.collect_children(parent, &mut ids);
        self.terminate_panes(ids);
    }

    /// Collects the one-off descendants of `parent`, depth-first, appending
    /// their ids to `into` without duplicates.
    fn collect_children(&self, parent: PaneId, into: &mut Vec<PaneId>) {
        for pane in self.panes.values() {
            if pane.parent != Some(parent) || pane.durable || into.contains(&pane.id) {
                continue;
            }
            into.push(pane.id);
            self.collect_children(pane.id, into);
        }
    }

    /// Terminates and announces each of the given panes.
    fn terminate_panes(&mut self, ids: Vec<PaneId>) {
        for id in ids {
            if let Some(mut pane) = self.panes.remove(&id) {
                tracing::info!(pane = %id, "a subagent's reason to run is gone");
                // A cascaded child can itself be a running subagent with its own
                // caller waiting on it, and it is being killed here exactly as a
                // directly closed pane is: the same answer is owed.
                self.answer_for_a_closed_subagent(&mut pane);
                pane.session.terminate();
                self.blanket.remove(&id);
                self.broadcast(ServerMessage::PaneClosed { pane: id });
                self.refuse_requests_from(id);
            }
        }
    }

    /// Refuses requests whose time is up.
    ///
    /// The daemon owns this deadline. Without it an agent on an unattended daemon
    /// waits for a person who is not there, and a late approval would start a
    /// subagent nobody is waiting for.
    fn expire_requests(&mut self) {
        let limit = Duration::from_secs(self.limits.request_timeout_secs);

        let expired: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, waiting)| waiting.asked.elapsed() >= limit)
            .map(|(id, _)| *id)
            .collect();

        for request in expired {
            let Some(waiting) = self.pending.remove(&request) else {
                continue;
            };

            tracing::info!(%request, "a delegation request went unanswered");
            self.resolve(
                request,
                waiting.caller,
                DelegateOutcome::Expired {
                    after_secs: self.limits.request_timeout_secs,
                },
            );
        }
    }

    /// Moves pane output out to clients and notices processes that exited.
    fn pump_panes(&mut self) {
        let mut messages = Vec::new();
        let mut exited = Vec::new();

        for (id, pane) in &mut self.panes {
            let output = pane.session.drain();
            if !output.is_empty() {
                pane.remember(&output);
                messages.push(ServerMessage::PaneOutput {
                    pane: *id,
                    bytes: output,
                });
            }

            // Reported once: a status resent every pass would be a message per
            // tick per exited pane, forever.
            if let RunState::Exited(code) = pane.session.state()
                && pane.status.is_live()
            {
                pane.status = PaneStatus::Exited(code);
                pane.exited_at = Some(Instant::now());
                // Read by now, and nothing will read it again.
                pane.task_file = None;
                exited.push((*id, code));
            }
        }

        for (id, code) in &exited {
            // The pane stays until a client closes it, so its final output can
            // still be read.
            messages.push(ServerMessage::PaneChanged {
                pane: *id,
                update: PaneUpdate::Status {
                    status: PaneStatus::Exited(*code),
                },
            });
        }

        for message in messages {
            self.broadcast(message);
        }

        // A subagent's caller is waiting on exactly this. Not answered on the
        // tick the exit is noticed: the output is still arriving then, and a
        // tail taken at the exit is the subagent's answer with the answer
        // missing. Whether that shows depends on the platform, which is the
        // worst kind of depends -- ConPTY's pipe lags the process object, so on
        // Windows it is the ordinary case rather than a race seen once.
        let mut answers = Vec::new();
        for pane in self.panes.values_mut() {
            let RunState::Exited(code) = pane.session.state() else {
                continue;
            };

            let waited_long_enough = pane.session.is_finished()
                || pane
                    .exited_at
                    .is_some_and(|exited| exited.elapsed() >= TAIL_GRACE);

            if !waited_long_enough {
                continue;
            }

            if let (Some(request), Some(caller)) = (pane.request.take(), pane.caller) {
                let start = pane.history.len().saturating_sub(TAIL_BYTES);
                answers.push((
                    caller,
                    ServerMessage::DelegateFinished {
                        request,
                        exit: code,
                        tail: pane.history[start..].to_vec(),
                    },
                ));
            }
        }

        for (caller, answer) in answers {
            self.send(caller, answer);
        }

        self.expire_requests();

        if !self.leftovers.is_empty() && self.leftovers_tried.elapsed() >= RETRY_LEFTOVERS {
            self.leftovers_tried = Instant::now();
            self.leftovers.retry();
        }
    }

    /// Hangs up on clients that ran out of time: one that never said
    /// `Hello`, and one that began a frame and never finished it.
    fn enforce_deadlines(&mut self) {
        let now = Instant::now();

        let late: Vec<(ClientId, &'static str)> = self
            .clients
            .iter()
            .filter_map(|(id, client)| {
                if !client.ready && now.duration_since(client.attached) >= self.budgets.handshake {
                    return Some((*id, "it never said hello"));
                }
                if client
                    .frame
                    .since()
                    .is_some_and(|began| now.duration_since(began) >= self.budgets.frame)
                {
                    return Some((*id, "it stopped part-way through a message"));
                }
                None
            })
            .collect();

        for (id, why) in late {
            tracing::info!(
                client = id,
                why,
                "hanging up on a client that ran out of time"
            );
            self.hang_up(id);
        }
    }

    /// Forgets a client, ends its connection at once, and drops what it was
    /// waiting on.
    ///
    /// For a client whose writer thread is stuck: past its budget, live or
    /// asked-for, there is nothing more it can be told and nothing to wait
    /// for, so the connection ends now rather than however long the write
    /// the writer thread is blocked on would otherwise take to fail on its
    /// own.
    ///
    /// `Closer::close` can block for up to about two seconds -- on Windows it
    /// keeps cancelling until nothing is left in flight on either pipe -- and
    /// the loop calls this from the same thread that ticks every pane and
    /// drains every event; blocking here would stall the whole daemon behind
    /// one client's connection. So the client is forgotten first, then closed
    /// on a thread of its own that outlives this call, and only then is what
    /// it was waiting on dropped.
    ///
    /// Not for a refusal or a protocol violation, which has just queued the
    /// message explaining why: see [`Self::refuse`], which lets that reach
    /// the peer first.
    ///
    /// Logs nothing itself: every caller already has, in its own words --
    /// out of time, behind on what it asked for, or simply gone -- and
    /// "hung up" would be the wrong word for the last of those, which is
    /// the client leaving on its own rather than the daemon choosing to end
    /// it.
    fn hang_up(&mut self, id: ClientId) {
        if let Some(client) = self.clients.remove(&id) {
            let closer = client.closer;
            std::thread::spawn(move || closer.close());
        }
        self.abandon(id);
    }

    /// Forgets a client and drops what it was waiting on, without touching
    /// its connection.
    ///
    /// For a refusal or a protocol violation, sent as the `ServerMessage`
    /// just queued ahead of this call: closing here -- immediately, as
    /// [`Self::hang_up`] does for a client past its budget -- races that
    /// write, and `Closer::close` can win it, so the peer would see a bare
    /// disconnect instead of the reason. Forgetting the client only drops
    /// this end's `Outbox`; the writer thread's own clone of the connection's
    /// `Closer` is what ends it, once `Inbox::recv` returns `None` -- the
    /// refusal delivered and nothing left queued -- or a write itself fails.
    fn refuse(&mut self, id: ClientId) {
        self.clients.remove(&id);
        tracing::info!(client = id, "refused a client");
        self.abandon(id);
    }

    /// Sends to one client.
    ///
    /// Not judged against the client's live-traffic budget: this is what it
    /// asked for. See [`Self::send_batch`], of which this is the one-message
    /// case.
    fn send(&mut self, id: ClientId, message: ServerMessage) {
        self.send_batch(id, vec![message]);
    }

    /// Sends every message in `messages` to one client, as a single reply.
    ///
    /// `ClientMessage::Subscribe`'s whole catch-up goes through here in one
    /// call: checked once, against the asked-for backlog already waiting,
    /// rather than once per message, so a reply that clears the check is
    /// delivered whole -- never split or refused partway through by its own
    /// bulk. A client that keeps asking for things without ever reading the
    /// answers is hung up all the same, just like one that falls behind on
    /// live traffic.
    fn send_batch(&mut self, id: ClientId, messages: Vec<ServerMessage>) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };

        match client.outbox.send_all(messages, self.budgets.outbox_bytes) {
            Ok(()) => {}
            // A failed send means the writer thread is gone, so the client
            // has disconnected and should be forgotten rather than retried.
            Err(Refused::Gone) => {
                self.clients.remove(&id);
            }
            Err(Refused::Behind { queued }) => {
                tracing::warn!(
                    client = id,
                    queued,
                    "hanging up on a client that is behind on what it asked for"
                );
                self.hang_up(id);
            }
        }
    }

    /// Sends to every subscribed client.
    fn broadcast(&mut self, message: ServerMessage) {
        self.broadcast_except(None, message);
    }

    /// Sends to every subscribed interface client except `exclude`, when given.
    ///
    /// `resolve` uses the exclusion: it already sends the caller its answer
    /// directly, and a caller that is also a subscribed interface client (an
    /// agent delegating from its own pane, watched by the same client) would
    /// otherwise be told twice.
    fn broadcast_except(&mut self, exclude: Option<ClientId>, message: ServerMessage) {
        let mut gone = Vec::new();
        let mut behind = Vec::new();

        for (id, client) in &self.clients {
            // A delegate caller wants the fate of its own request; the fleet's
            // output and every other pane's prompts are a firehose it never
            // reads.
            if Some(*id) == exclude || !client.subscribed || client.role != Role::Interface {
                continue;
            }
            match client
                .outbox
                .send_within(message.clone(), self.budgets.outbox_bytes)
            {
                Ok(()) => {}
                Err(Refused::Gone) => gone.push(*id),
                Err(Refused::Behind { queued }) => behind.push((*id, queued)),
            }
        }

        for id in gone {
            self.clients.remove(&id);
        }

        // Hung up on rather than skipped: a client that misses output it is
        // never told it missed draws a screen that is quietly wrong. One that
        // reconnects is replayed the lot.
        for (id, queued) in behind {
            tracing::warn!(
                client = id,
                queued,
                "hanging up on a client that stopped reading"
            );
            self.hang_up(id);
        }
    }
}

/// The directory holding the `dispatch` client binary, when it sits beside this
/// one.
fn client_binary_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "dispatch.exe"
    } else {
        "dispatch"
    };

    let candidate = exe.with_file_name(name);
    if candidate.is_file() {
        exe.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

/// Runs one client connection on its own threads.
///
/// Reading and writing are separate so a client that stops reading cannot
/// block the daemon, and the daemon's own loop never blocks on a socket.
fn spawn_client(
    id: ClientId,
    connection: Connection,
    events: &SyncSender<Event>,
    live: &Arc<AtomicUsize>,
) -> Result<(), dispatch_os::ipc::IpcError> {
    let closer = connection.closer();
    // The writer thread gets its own clone: `Daemon::hang_up` closes the one
    // in `Wiring` at once, for a client stuck mid-write, while this one ends
    // the connection only once the writer has nothing left to deliver (or a
    // write itself fails) -- the ordinary way a refusal reaches its peer
    // before the connection does.
    let writer_closer = closer.clone();
    let (mut reader, mut writer) = connection.split();
    let (outbox, inbox) = crate::outbox::pair();
    let frame = FrameClock::default();

    if events
        .send(Event::Attached(
            id,
            Wiring {
                outbox,
                closer,
                frame: frame.clone(),
            },
        ))
        .is_err()
    {
        return Ok(());
    }
    live.fetch_add(1, Ordering::Relaxed);
    // Held by both threads below; the seat is freed once whichever of them
    // ends last drops its clone, not when the first of the two does.
    let seat = Arc::new(Seat(Arc::clone(live)));

    let incoming = events.clone();
    let reader_seat = Arc::clone(&seat);
    std::thread::spawn(move || {
        let _detached = DetachOnDrop {
            id,
            events: incoming.clone(),
        };

        loop {
            match Frame::read_watched::<_, ClientMessage>(&mut reader, || frame.start()) {
                Ok(message) => {
                    frame.finish();
                    if incoming.send(Event::Request(id, message)).is_err() {
                        break;
                    }
                }
                Err(FrameError::Disconnected) => break,
                Err(error) => {
                    tracing::warn!(client = id, %error, "dropping a client");
                    break;
                }
            }
        }

        drop(reader_seat);
    });

    std::thread::spawn(move || {
        while let Some(message) = inbox.recv() {
            // Logged rather than swallowed: a write that fails here is how a
            // client ends up waiting for an answer the daemon believes it sent,
            // and a silent `break` leaves nothing to read afterwards. Debug
            // rather than warn, because an ordinary disconnect arrives this way
            // too — the point is that it can be seen at all.
            if let Err(error) = Frame::write(&mut writer, &message) {
                tracing::debug!(client = id, %error, "failed to write to a client");
                break;
            }
        }

        // Reached once there is nothing left to deliver -- `Daemon::refuse`
        // dropped the `Outbox` after queueing the reason, and this is what
        // was queued -- or once a write above failed. Either way the
        // connection is done with; `Daemon::hang_up` closes the other clone
        // itself, immediately, for a client this thread is instead stuck
        // mid-write to.
        writer_closer.close();
        drop(seat);
    });

    Ok(())
}

/// Lets a test drive the loop without a socket.
impl Daemon {
    /// Attaches a fake client and returns its inbox.
    ///
    /// Taking from the inbox is what reading is: a test that never takes is
    /// a client that has stopped reading.
    #[doc(hidden)]
    pub fn attach_for_test(&mut self, id: u64) -> Inbox {
        let (outbox, inbox) = crate::outbox::pair();
        self.handle(Event::Attached(
            id,
            Wiring {
                outbox,
                closer: Closer::default(),
                frame: FrameClock::default(),
            },
        ));
        inbox
    }

    /// Delivers a message as if a client had sent it.
    #[doc(hidden)]
    pub fn request_for_test(&mut self, id: u64, message: ClientMessage) {
        self.handle(Event::Request(id, message));
    }

    /// Detaches a fake client.
    #[doc(hidden)]
    pub fn detach_for_test(&mut self, id: u64) {
        self.handle(Event::Detached(id));
    }

    /// The pid of every pane's process, for tests that watch a tree end.
    #[doc(hidden)]
    #[must_use]
    pub fn pane_pids_for_test(&self) -> Vec<u32> {
        self.panes
            .values()
            .filter_map(|pane| pane.session.pid())
            .collect()
    }
}

#[cfg(test)]
mod tests;
