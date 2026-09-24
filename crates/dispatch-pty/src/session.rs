//! Running one agent in one pseudoterminal.
//!
//! A reader thread pulls bytes off the pseudoterminal and hands them to the
//! application loop over a channel, so a chatty agent can never block
//! rendering. The loop drains the channel and feeds the emulator.
//!
//! Two shapes, because there are two kinds of caller. [`Pty`] is the process
//! and its bytes. [`PtySession`] adds an emulator, and is what something drawing
//! a pane wants. The daemon does not draw: it forwards bytes to clients that
//! each run their own emulator, so an emulator on its side would parse every
//! byte a second time and hold a screen nothing ever reads.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel};
use std::time::Duration;

use dispatch_config::Launch;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::vt::{Size, VtError, VtTerminal};

/// How much input may wait for a pane that is not reading it.
///
/// Past any paste a person makes, and far short of what an unread queue
/// would otherwise grow to. A write that would take the waiting input past
/// this is refused whole -- half a paste arriving later would be worse than
/// none -- so a pane that has stopped reading costs this much memory and no
/// more. A write into an empty queue is always taken, whatever its size: a
/// paste bigger than the budget still reaches a pane that reads it.
pub const INPUT_BUDGET: usize = 8 * 1024 * 1024;

/// How many reads of output may wait between the reader thread and `drain`.
///
/// Full, the reader stops reading, the pseudoterminal's own buffer fills,
/// and the child blocks on its next write: a pane that prints faster than it
/// is drawn is slowed down, not held in memory.
const OUTPUT_CHUNKS: usize = 32;

/// The most output one [`Pty::drain`] hands over, give or take the read that
/// crosses it.
///
/// Without a limit, draining a pane that prints without pause never
/// finishes: the reader refills the channel as fast as it is emptied, and
/// the daemon's loop never reaches its other panes.
pub const DRAIN_BUDGET: usize = 128 * 1024;

/// Failures while running a pseudoterminal.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    /// The pseudoterminal could not be opened.
    #[error("failed to open a pseudoterminal: {0}")]
    Open(#[source] anyhow::Error),

    /// The harness process could not be started.
    #[error("failed to start {command:?}: {source}")]
    Spawn {
        /// The command that could not be started.
        command: String,
        /// Underlying error.
        #[source]
        source: anyhow::Error,
    },

    /// Writing to the pseudoterminal failed.
    #[error("failed to write to the pseudoterminal: {0}")]
    Write(#[source] std::io::Error),

    /// The pane has not read the input already sent to it.
    #[error("the pane is not reading its input; {waiting} bytes are still waiting for it")]
    InputFull {
        /// How much was already queued.
        waiting: usize,
    },

    /// The terminal emulator reported a failure.
    #[error(transparent)]
    Vt(#[from] VtError),
}

/// Something that happened on a pseudoterminal.
#[derive(Debug)]
enum PtyEvent {
    /// Bytes read from the child.
    Output(Vec<u8>),
    /// The child exited with this status code.
    Exited(i32),
}

/// Whether a pane's process is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// The process is alive.
    Running,
    /// The process exited with this status code.
    Exited(i32),
}

/// One agent and its pseudoterminal, with no emulator.
///
/// Carries bytes rather than a screen. Use [`PtySession`] to draw a pane;
/// use this to move a pane's output somewhere else.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    /// Held only on Windows, where dropping the slave closes the ConPTY
    /// pseudoconsole and leaves the child writing into a dead console.
    _slave: Option<Box<dyn portable_pty::SlavePty + Send>>,
    /// Input waiting for the pane, written by a thread of its own.
    ///
    /// A write to a pseudoterminal blocks once its buffer is full, and it
    /// stays full for as long as the program behind it is not reading. Done
    /// on the caller's thread, that wait is the daemon's whole loop.
    input: Sender<Vec<u8>>,
    /// How many bytes are queued and not yet written.
    waiting: Arc<AtomicUsize>,
    /// Bounded so a pane printing faster than it is drained is slowed rather
    /// than stored: see [`OUTPUT_CHUNKS`].
    events: Receiver<PtyEvent>,
    size: Size,
    /// Process id of the child, used to terminate its whole tree.
    pid: Option<u32>,
    state: RunState,
    /// Whether everything the child printed has been delivered.
    finished: bool,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty")
            .field("size", &self.size)
            .field("pid", &self.pid)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Pty {
    /// Starts `launch` in a new pseudoterminal rooted at `cwd`.
    pub fn spawn(launch: &Launch, cwd: &Path, size: Size) -> Result<Self, PtyError> {
        // Must happen before the pseudoterminal layer loads anything. On
        // Windows it decides whether ConPTY comes from the kernel or from
        // whatever conpty.dll happens to sit on PATH.
        dispatch_os::dll::restrict_search_path();

        let pty_size = PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = native_pty_system()
            .openpty(pty_size)
            .map_err(PtyError::Open)?;

        let mut command = CommandBuilder::new(&launch.command);
        command.args(&launch.args);
        command.cwd(cwd);
        for (key, value) in &launch.env {
            command.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|source| PtyError::Spawn {
                command: launch.command.clone(),
                source,
            })?;

        // On Unix the slave is held open by the child, and dropping our copy
        // is what lets the reader see end-of-file when the child exits.
        //
        // On Windows the same drop closes the ConPTY pseudoconsole out from
        // under the child, which then blocks writing into a dead console: no
        // output ever arrives and it never exits on its own. Hold it instead.
        // Exit is detected by waiting on the child either way, so nothing here
        // depends on end-of-file.
        let slave = if cfg!(windows) {
            Some(pair.slave)
        } else {
            None
        };

        let pid = child.process_id();

        let reader = pair.master.try_clone_reader().map_err(PtyError::Open)?;
        let writer = pair.master.take_writer().map_err(PtyError::Open)?;

        let mut writer = writer;
        answer_inherit_cursor_handshake(&mut writer);

        let (input, queued) = channel();
        let waiting = Arc::new(AtomicUsize::new(0));
        spawn_writer(writer, queued, Arc::clone(&waiting));

        let (tx, events) = sync_channel(OUTPUT_CHUNKS);
        spawn_reader(reader, tx.clone());
        spawn_waiter(child, tx);

        Ok(Self {
            master: pair.master,
            _slave: slave,
            input,
            waiting,
            events,
            size,
            pid,
            state: RunState::Running,
            finished: false,
        })
    }

    /// Takes what the child has produced, up to about [`DRAIN_BUDGET`].
    ///
    /// Never blocks: a pane with nothing to say costs one failed receive.
    /// What is left waits for the next call.
    pub fn drain(&mut self) -> Vec<u8> {
        let drained = drain_from(&self.events, DRAIN_BUDGET);
        if let Some(code) = drained.exited {
            self.state = RunState::Exited(code);
        }
        if drained.finished {
            self.finished = true;
        }
        drained.output
    }

    /// Whether the child exited *and* everything it printed has been delivered.
    ///
    /// [`Pty::state`] answers a different question. The reader and the waiter
    /// are separate threads, so an exit can be reported while output is still in
    /// flight — on Windows that is the ordinary case, because ConPTY's pipe lags
    /// the process object. Anything that reads a child's whole output has to
    /// wait for this, not for the exit.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Waits for the child to exit, collecting what it prints.
    ///
    /// Intended for tests and for short-lived commands; an application loop uses
    /// [`Pty::drain`].
    pub fn drain_until_exit(&mut self, timeout: Duration) -> (RunState, Vec<u8>) {
        let deadline = std::time::Instant::now() + timeout;
        let mut output = Vec::new();

        while std::time::Instant::now() < deadline {
            match self.events.recv_timeout(Duration::from_millis(20)) {
                Ok(PtyEvent::Output(bytes)) => output.extend_from_slice(&bytes),
                Ok(PtyEvent::Exited(code)) => {
                    self.state = RunState::Exited(code);
                    // Keep draining briefly: output already in flight should
                    // land before the caller looks at what was printed.
                    while let Ok(PtyEvent::Output(bytes)) =
                        self.events.recv_timeout(Duration::from_millis(50))
                    {
                        output.extend_from_slice(&bytes);
                    }
                    return (self.state, output);
                }
                Err(_) => continue,
            }
        }

        (self.state, output)
    }

    /// Queues bytes for the child, as if typed.
    ///
    /// Never blocks. Refused with [`PtyError::InputFull`] when input is
    /// already waiting and this would take it past [`INPUT_BUDGET`].
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        if bytes.is_empty() {
            return Ok(());
        }

        let waiting = self.waiting.load(Ordering::Acquire);
        if waiting > 0 && waiting.saturating_add(bytes.len()) > INPUT_BUDGET {
            return Err(PtyError::InputFull { waiting });
        }

        self.waiting.fetch_add(bytes.len(), Ordering::AcqRel);
        if self.input.send(bytes.to_vec()).is_err() {
            self.waiting.fetch_sub(bytes.len(), Ordering::AcqRel);
            return Err(PtyError::Write(std::io::Error::from(
                std::io::ErrorKind::BrokenPipe,
            )));
        }

        Ok(())
    }

    /// Resizes the pseudoterminal.
    ///
    /// The child learns about this through SIGWINCH, so programs that redraw on
    /// resize do so against the new size.
    pub fn resize(&mut self, size: Size) -> Result<(), PtyError> {
        if size == self.size {
            return Ok(());
        }

        self.master
            .resize(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(PtyError::Open)?;

        self.size = size;
        Ok(())
    }

    /// Whether the child is still running.
    #[must_use]
    pub fn state(&self) -> RunState {
        self.state
    }

    /// The child's process id, while it is alive.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// The current size.
    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    /// Terminates the child and everything it spawned.
    ///
    /// Agents start subprocesses, so killing only the direct child would leave
    /// them holding this pane's file descriptors.
    pub fn terminate(&mut self) {
        let Some(pid) = self.pid else {
            return;
        };

        if let Err(error) =
            dispatch_os::process::terminate_tree(pid, dispatch_os::process::DEFAULT_GRACE)
        {
            tracing::warn!(%pid, %error, "failed to terminate the pane's process tree");
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if matches!(self.state, RunState::Running) {
            self.terminate();
        }
    }
}

/// One agent, its pseudoterminal, and its screen.
///
/// What something drawing a pane wants: the bytes are fed to an emulator here,
/// and the screen read off it.
#[derive(Debug)]
pub struct PtySession {
    pty: Pty,
    terminal: VtTerminal,
}

impl PtySession {
    /// Starts `launch` in a new pseudoterminal rooted at `cwd`.
    pub fn spawn(launch: &Launch, cwd: &Path, size: Size) -> Result<Self, PtyError> {
        Ok(Self {
            pty: Pty::spawn(launch, cwd, size)?,
            terminal: VtTerminal::new(size)?,
        })
    }

    /// Feeds everything the child has produced into the emulator.
    ///
    /// Returns whether anything changed, so a caller can skip a redraw.
    pub fn drain(&mut self) -> bool {
        let before = self.pty.state();
        let output = self.drain_output();

        // The state as well as the output: a pane that has just exited needs a
        // redraw for its status, and one that exited a while ago must not ask
        // for one every frame.
        !output.is_empty() || self.pty.state() != before
    }

    /// Feeds pending output into the emulator and returns the raw bytes.
    ///
    /// The caller reads the child's title out of them: the emulator models the
    /// screen, and a title is not on the screen.
    pub fn drain_output(&mut self) -> Vec<u8> {
        let output = self.pty.drain();
        self.terminal.feed(&output);
        output
    }

    /// Feeds output for up to `timeout`, returning once the child exits.
    ///
    /// Intended for tests and for short-lived commands; the application loop
    /// uses [`PtySession::drain`].
    pub fn drain_until_exit(&mut self, timeout: Duration) -> RunState {
        let (state, output) = self.pty.drain_until_exit(timeout);
        self.terminal.feed(&output);
        state
    }

    /// Sends bytes to the child, as if typed.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        self.pty.write(bytes)
    }

    /// Resizes both the pseudoterminal and the emulator.
    ///
    /// The child redraws against the size the emulator now has, so the two must
    /// move together.
    pub fn resize(&mut self, size: Size) -> Result<(), PtyError> {
        self.pty.resize(size)?;
        self.terminal.resize(size)?;
        Ok(())
    }

    /// The pane's screen.
    #[must_use]
    pub fn terminal(&self) -> &VtTerminal {
        &self.terminal
    }

    /// The pane's screen, mutably, for scrolling its viewport.
    pub fn terminal_mut(&mut self) -> &mut VtTerminal {
        &mut self.terminal
    }

    /// Whether the child is still running.
    #[must_use]
    pub fn state(&self) -> RunState {
        self.pty.state()
    }

    /// The child's process id, while it is alive.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pty.pid()
    }

    /// The current size.
    #[must_use]
    pub fn size(&self) -> Size {
        self.pty.size()
    }

    /// Terminates the child and everything it spawned.
    pub fn terminate(&mut self) {
        self.pty.terminate();
    }
}

/// Unblocks ConPTY's inherit-cursor handshake on Windows.
///
/// `portable-pty` creates the pseudoconsole with `PSEUDOCONSOLE_INHERIT_CURSOR`,
/// which makes ConPTY ask the containing terminal where its cursor is and wait
/// for the answer before it starts pumping. A terminal emulator answers because
/// it is one; Dispatch embeds the pseudoconsole instead, so without this the
/// child starts, produces no output, and never exits.
///
/// Sends a cursor position report for row 1, column 1. A failure here is not
/// fatal on its own, so it is logged rather than returned.
fn answer_inherit_cursor_handshake(writer: &mut Box<dyn Write + Send>) {
    if !cfg!(windows) {
        return;
    }

    if let Err(error) = writer.write_all(b"\x1b[1;1R").and_then(|()| writer.flush()) {
        tracing::warn!(%error, "failed to answer the ConPTY inherit-cursor handshake");
    }
}

/// What one pass over a pane's events found.
struct Drained {
    /// The output taken.
    output: Vec<u8>,
    /// The exit status, when the exit was among the events taken.
    exited: Option<i32>,
    /// Whether the channel was found closed: nothing more will ever arrive.
    finished: bool,
}

/// Takes events until `budget` bytes of output are in hand, or none are
/// waiting.
///
/// Apart from [`Pty`] so a test can fill the channel itself: a real pane
/// cannot be made to have a known amount waiting at the moment it is
/// drained.
fn drain_from(events: &Receiver<PtyEvent>, budget: usize) -> Drained {
    let mut drained = Drained {
        output: Vec::new(),
        exited: None,
        finished: false,
    };

    while drained.output.len() < budget {
        match events.try_recv() {
            Ok(PtyEvent::Output(bytes)) => drained.output.extend_from_slice(&bytes),
            Ok(PtyEvent::Exited(code)) => drained.exited = Some(code),
            Err(TryRecvError::Empty) => break,
            // Both senders are gone, which happens only once the reader has
            // reached end-of-file and the waiter has reported the exit.
            Err(TryRecvError::Disconnected) => {
                drained.finished = true;
                break;
            }
        }
    }

    drained
}

/// Reads the pseudoterminal until end-of-file, forwarding bytes.
fn spawn_reader(mut reader: Box<dyn Read + Send>, tx: SyncSender<PtyEvent>) {
    std::thread::spawn(move || {
        // Large enough that a burst of output is a few reads rather than
        // hundreds, small enough not to sit idle holding memory per pane.
        let mut buf = [0u8; 8192];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                        // The session is gone; nothing left to deliver to.
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

/// Writes queued input to the pseudoterminal, in order, until the pane goes.
///
/// Keeps taking from the queue after a write fails -- a child that has
/// exited stops accepting input -- so the count of what is waiting stays
/// true and nothing sent later blocks on a thread that has stopped.
fn spawn_writer(
    mut writer: Box<dyn Write + Send>,
    queued: Receiver<Vec<u8>>,
    waiting: Arc<AtomicUsize>,
) {
    std::thread::spawn(move || {
        let mut broken = false;

        while let Ok(bytes) = queued.recv() {
            if !broken && let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                tracing::debug!(%error, "a pane stopped accepting input");
                broken = true;
            }
            waiting.fetch_sub(bytes.len(), Ordering::AcqRel);
        }
    });
}

/// Waits for the child and reports its exit status.
fn spawn_waiter(mut child: Box<dyn portable_pty::Child + Send + Sync>, tx: SyncSender<PtyEvent>) {
    std::thread::spawn(move || {
        let code = match child.wait() {
            Ok(status) => {
                // ExitStatus reports success plus a platform code; a failed
                // exit with no code still has to be distinguishable from a
                // clean one.
                if status.success() {
                    0
                } else {
                    i32::try_from(status.exit_code()).unwrap_or(1)
                }
            }
            Err(_) => 1,
        };

        let _ = tx.send(PtyEvent::Exited(code));
    });
}

#[cfg(test)]
mod tests;
