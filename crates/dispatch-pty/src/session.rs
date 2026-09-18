//! Running one agent in one pseudoterminal.
//!
//! A reader thread pulls bytes off the pseudoterminal and hands them to the
//! application loop over a channel, so a chatty agent can never block
//! rendering. The loop drains the channel and feeds the emulator.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Duration;

use dispatch_config::Launch;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::vt::{Size, VtError, VtTerminal};

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

/// One agent, its pseudoterminal, and its screen.
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    /// Held only on Windows, where dropping the slave closes the ConPTY
    /// pseudoconsole and leaves the child writing into a dead console.
    _slave: Option<Box<dyn portable_pty::SlavePty + Send>>,
    writer: Box<dyn Write + Send>,
    events: Receiver<PtyEvent>,
    terminal: VtTerminal,
    size: Size,
    /// Process id of the child, used to terminate its whole tree.
    pid: Option<u32>,
    state: RunState,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("size", &self.size)
            .field("pid", &self.pid)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl PtySession {
    /// Starts `launch` in a new pseudoterminal rooted at `cwd`.
    pub fn spawn(launch: &Launch, cwd: &Path, size: Size) -> Result<Self, PtyError> {
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

        let (tx, events) = channel();
        spawn_reader(reader, tx.clone());
        spawn_waiter(child, tx);

        Ok(Self {
            master: pair.master,
            _slave: slave,
            writer,
            events,
            terminal: VtTerminal::new(size)?,
            size,
            pid,
            state: RunState::Running,
        })
    }

    /// Feeds everything the child has produced into the emulator.
    ///
    /// Returns whether anything changed, so a caller can skip a redraw.
    /// Never blocks: a pane with nothing to say costs one failed receive.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;

        loop {
            match self.events.try_recv() {
                Ok(PtyEvent::Output(bytes)) => {
                    self.terminal.feed(&bytes);
                    changed = true;
                }
                Ok(PtyEvent::Exited(code)) => {
                    self.state = RunState::Exited(code);
                    changed = true;
                }
                // Both senders are gone, which only happens once the child has
                // exited and its output has been delivered.
                Err(TryRecvError::Disconnected) | Err(TryRecvError::Empty) => break,
            }
        }

        changed
    }

    /// Feeds output for up to `timeout`, returning once the child exits.
    ///
    /// Intended for tests and for short-lived commands; the application loop
    /// uses [`PtySession::drain`].
    pub fn drain_until_exit(&mut self, timeout: Duration) -> RunState {
        let deadline = std::time::Instant::now() + timeout;

        while std::time::Instant::now() < deadline {
            match self.events.recv_timeout(Duration::from_millis(20)) {
                Ok(PtyEvent::Output(bytes)) => self.terminal.feed(&bytes),
                Ok(PtyEvent::Exited(code)) => {
                    self.state = RunState::Exited(code);
                    // Keep draining briefly: output already in flight should
                    // land before the caller looks at the screen.
                    while let Ok(PtyEvent::Output(bytes)) =
                        self.events.recv_timeout(Duration::from_millis(50))
                    {
                        self.terminal.feed(&bytes);
                    }
                    return self.state;
                }
                Err(_) => continue,
            }
        }

        self.state
    }

    /// Sends bytes to the child, as if typed.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        self.writer.write_all(bytes).map_err(PtyError::Write)?;
        self.writer.flush().map_err(PtyError::Write)
    }

    /// Resizes both the pseudoterminal and the emulator.
    ///
    /// The child learns about this through SIGWINCH, so programs that redraw
    /// on resize do so against the size the emulator now has.
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

        self.terminal.resize(size)?;
        self.size = size;
        Ok(())
    }

    /// The pane's screen.
    #[must_use]
    pub fn terminal(&self) -> &VtTerminal {
        &self.terminal
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

impl Drop for PtySession {
    fn drop(&mut self) {
        if matches!(self.state, RunState::Running) {
            self.terminate();
        }
    }
}

/// Reads the pseudoterminal until end-of-file, forwarding bytes.
fn spawn_reader(mut reader: Box<dyn Read + Send>, tx: Sender<PtyEvent>) {
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

/// Waits for the child and reports its exit status.
fn spawn_waiter(mut child: Box<dyn portable_pty::Child + Send + Sync>, tx: Sender<PtyEvent>) {
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
