//! The daemon's event loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use dispatch_config::HarnessRegistry;
use dispatch_core::{PaneId, PaneStatus, ProjectId};
use dispatch_os::ipc::{Connection, Listener};
use dispatch_proto::{ClientMessage, Frame, FrameError, PaneUpdate, ProtocolError, ServerMessage};
use dispatch_pty::{PtySession, RunState, Size};

use crate::pane::DaemonPane;

/// How long the loop waits for an event before checking panes again.
///
/// Panes are polled rather than waited on, so this is the floor on how quickly
/// output reaches a client.
const TICK: Duration = Duration::from_millis(8);

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

/// Something the loop reacts to.
enum Event {
    /// A client attached.
    Attached(ClientId, Sender<ServerMessage>),
    /// A client said something.
    Request(ClientId, ClientMessage),
    /// A client went away.
    Detached(ClientId),
}

/// An attached client.
struct Client {
    outbox: Sender<ServerMessage>,
    /// Whether it has asked for pane events. A client that has not subscribed
    /// is still connected but silent, which is what a one-shot command wants.
    subscribed: bool,
}

/// The daemon.
pub struct Daemon {
    panes: HashMap<PaneId, DaemonPane>,
    clients: HashMap<ClientId, Client>,
    harnesses: HarnessRegistry,
    projects: HashMap<ProjectId, PathBuf>,
    events: Receiver<Event>,
    sender: Sender<Event>,
    device: String,
}

impl Daemon {
    /// Creates a daemon serving `harnesses`.
    #[must_use]
    pub fn new(harnesses: HarnessRegistry, device: impl Into<String>) -> Self {
        let (sender, events) = channel();

        Self {
            panes: HashMap::new(),
            clients: HashMap::new(),
            harnesses,
            projects: HashMap::new(),
            events,
            sender,
            device: device.into(),
        }
    }

    /// Registers a project the daemon will spawn panes in.
    pub fn add_project(&mut self, id: ProjectId, root: PathBuf) {
        self.projects.insert(id, root);
    }

    /// How many panes are running.
    #[must_use]
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Accepts connections until the listener fails, serving them all.
    pub fn serve(mut self, listener: Listener) -> Result<(), DaemonError> {
        let sender = self.sender.clone();
        let mut next_id = 0;

        // Accepting blocks, so it runs on its own thread and hands each
        // connection to the loop.
        std::thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok(connection) => {
                        next_id += 1;
                        if spawn_client(next_id, connection, &sender).is_err() {
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
    pub fn run(&mut self) {
        loop {
            match self.events.recv_timeout(TICK) {
                Ok(event) => self.handle(event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                // Every sender is gone, which cannot happen while the daemon
                // holds one, so this means shutdown.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }

            self.pump_panes();
        }
    }

    /// Runs one pass, for tests.
    pub fn tick(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.handle(event);
        }
        self.pump_panes();
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Attached(id, outbox) => {
                self.clients.insert(
                    id,
                    Client {
                        outbox,
                        subscribed: false,
                    },
                );
                tracing::info!(client = id, "client attached");
            }
            Event::Detached(id) => {
                self.clients.remove(&id);
                tracing::info!(client = id, "client detached");
            }
            Event::Request(id, message) => self.handle_request(id, message),
        }
    }

    fn handle_request(&mut self, id: ClientId, message: ClientMessage) {
        match message {
            ClientMessage::Hello { version, client } => {
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
                    self.clients.remove(&id);
                    return;
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
                if let Some(client) = self.clients.get_mut(&id) {
                    client.subscribed = true;
                }

                // Describe what already exists, so a client attaching to a
                // running daemon sees the panes rather than waiting for one to
                // change.
                let existing: Vec<ServerMessage> = self
                    .panes
                    .values()
                    .map(|pane| ServerMessage::PaneSpawned {
                        pane: pane.id,
                        project: pane.project,
                        harness: pane.harness.clone(),
                    })
                    .collect();

                for message in existing {
                    self.send(id, message);
                }
            }

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

                if let Err(error) = target.session.write(&bytes) {
                    tracing::warn!(%error, "failed to write to a pane");
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
                    target.session.terminate();
                    self.broadcast(ServerMessage::PaneClosed { pane });
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
        }
    }

    fn spawn_pane(&mut self, client: ClientId, project: ProjectId, harness: &str, size: Size) {
        let Some(root) = self.projects.get(&project).cloned() else {
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

        let launch = def.launch_for_current_platform();

        let session = match PtySession::spawn(&launch, &root, size) {
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

        let id = PaneId::new();
        self.panes.insert(
            id,
            DaemonPane {
                id,
                session,
                harness: harness.to_string(),
                project,
            },
        );

        // Every client hears about it, not just the one that asked, because
        // they are all looking at the same fleet.
        self.broadcast(ServerMessage::PaneSpawned {
            pane: id,
            project,
            harness: harness.to_string(),
        });
    }

    /// Moves pane output out to clients and notices processes that exited.
    fn pump_panes(&mut self) {
        let mut messages = Vec::new();
        let mut exited = Vec::new();

        for (id, pane) in &mut self.panes {
            let output = pane.session.drain_output();
            if !output.is_empty() {
                messages.push(ServerMessage::PaneOutput {
                    pane: *id,
                    bytes: output,
                });
            }

            if let RunState::Exited(code) = pane.session.state() {
                exited.push((*id, code));
            }
        }

        for (id, code) in exited {
            // The pane stays until a client closes it, so its final output can
            // still be read.
            messages.push(ServerMessage::PaneChanged {
                pane: id,
                update: PaneUpdate::Status {
                    status: PaneStatus::Exited(code),
                },
            });
        }

        for message in messages {
            self.broadcast(message);
        }
    }

    /// Sends to one client.
    fn send(&mut self, id: ClientId, message: ServerMessage) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };

        // A failed send means the writer thread is gone, so the client has
        // disconnected and should be forgotten rather than retried.
        if client.outbox.send(message).is_err() {
            self.clients.remove(&id);
        }
    }

    /// Sends to every subscribed client.
    fn broadcast(&mut self, message: ServerMessage) {
        let mut gone = Vec::new();

        for (id, client) in &self.clients {
            if !client.subscribed {
                continue;
            }
            if client.outbox.send(message.clone()).is_err() {
                gone.push(*id);
            }
        }

        for id in gone {
            self.clients.remove(&id);
        }
    }
}

/// Runs one client connection on its own threads.
///
/// Reading and writing are separate so a client that stops reading cannot
/// block the daemon, and the daemon's own loop never blocks on a socket.
fn spawn_client(
    id: ClientId,
    connection: Connection,
    events: &Sender<Event>,
) -> Result<(), dispatch_os::ipc::IpcError> {
    let (mut reader, mut writer) = connection.split()?;
    let (outbox, outgoing) = channel::<ServerMessage>();

    if events.send(Event::Attached(id, outbox)).is_err() {
        return Ok(());
    }

    let incoming = events.clone();
    std::thread::spawn(move || {
        loop {
            match Frame::read::<_, ClientMessage>(&mut reader) {
                Ok(message) => {
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

        let _ = incoming.send(Event::Detached(id));
    });

    std::thread::spawn(move || {
        while let Ok(message) = outgoing.recv() {
            if Frame::write(&mut writer, &message).is_err() {
                break;
            }
        }
    });

    Ok(())
}

/// Lets a test drive the loop without a socket.
impl Daemon {
    /// Attaches a fake client and returns its inbox.
    #[doc(hidden)]
    pub fn attach_for_test(&mut self, id: u64) -> Receiver<ServerMessage> {
        let (outbox, inbox) = channel();
        self.handle(Event::Attached(id, outbox));
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
}

#[cfg(test)]
mod tests;
