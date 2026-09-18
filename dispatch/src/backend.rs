//! Where a pane's process lives: in this process, or in the daemon.
//!
//! Both kinds of pane are drawn the same way, because both have an emulator
//! here. A local pane's emulator is fed by its pseudoterminal; a remote pane's
//! is fed by the bytes the daemon forwards. Everything above this — layout,
//! key encoding, rendering — sees one shape and does not care which it has.
//!
//! Keeping the emulator on the client is what makes a remote pane cheap: the
//! daemon ships the bytes it already has instead of rendering a screen per
//! client per frame.

use anyhow::{Context, Result};
use dispatch_client::Handle;
use dispatch_core::PaneId;
use dispatch_proto::ClientMessage;
use dispatch_pty::{PtySession, RunState, Size, VtTerminal};

/// A pane the daemon owns.
pub struct RemotePane {
    id: PaneId,
    daemon: Handle,
    /// This client's emulator for the pane, fed by the daemon's output.
    terminal: VtTerminal,
    size: Size,
    state: RunState,
}

impl RemotePane {
    /// Prepares a local view of a pane the daemon has started.
    pub fn new(id: PaneId, daemon: Handle, size: Size) -> Result<Self> {
        let terminal = VtTerminal::new(size).context("failed to create a terminal")?;

        Ok(Self {
            id,
            daemon,
            terminal,
            size,
            state: RunState::Running,
        })
    }

    /// Feeds output from the daemon into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.feed(bytes);
    }

    /// Records that the pane's process has exited.
    pub fn set_state(&mut self, state: RunState) {
        self.state = state;
    }
}

/// A pane's process, wherever it is running.
///
/// Dispatch works on its own, with the agents as its children, and against a
/// daemon that owns them instead. The second is what lets the work survive the
/// interface exiting; the first is what makes the program usable with nothing
/// else running.
pub enum Backend {
    /// A process this Dispatch started and owns.
    Local(PtySession),
    /// A process the daemon owns.
    Remote(RemotePane),
}

impl Backend {
    /// The emulator holding this pane's screen.
    #[must_use]
    pub fn terminal(&self) -> &VtTerminal {
        match self {
            Self::Local(session) => session.terminal(),
            Self::Remote(remote) => &remote.terminal,
        }
    }

    /// The emulator, mutably, for scrolling.
    pub fn terminal_mut(&mut self) -> &mut VtTerminal {
        match self {
            Self::Local(session) => session.terminal_mut(),
            Self::Remote(remote) => &mut remote.terminal,
        }
    }

    /// The size the pane was last told about.
    #[must_use]
    pub fn size(&self) -> Size {
        match self {
            Self::Local(session) => session.size(),
            Self::Remote(remote) => remote.size,
        }
    }

    /// Whether the pane's process is still running.
    #[must_use]
    pub fn state(&self) -> RunState {
        match self {
            Self::Local(session) => session.state(),
            Self::Remote(remote) => remote.state,
        }
    }

    /// Sends bytes to the pane's process.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Local(session) => session.write(bytes).context("failed to write to a pane"),
            Self::Remote(remote) => {
                let sent = remote.daemon.send(ClientMessage::WritePane {
                    pane: remote.id,
                    bytes: bytes.to_vec(),
                });

                if sent {
                    Ok(())
                } else {
                    anyhow::bail!("the daemon connection has ended")
                }
            }
        }
    }

    /// Tells the pane its new size.
    pub fn resize(&mut self, size: Size) -> Result<()> {
        match self {
            Self::Local(session) => session.resize(size).context("failed to resize a pane"),
            Self::Remote(remote) => {
                // The local emulator is resized too, so the screen reflows this
                // frame rather than when the daemon's next output arrives.
                remote
                    .terminal
                    .resize(size)
                    .context("failed to resize a pane")?;
                remote.size = size;
                remote.daemon.send(ClientMessage::ResizePane {
                    pane: remote.id,
                    size: (size.cols, size.rows),
                });
                Ok(())
            }
        }
    }

    /// Moves whatever the process has produced into the emulator.
    ///
    /// Returns whether anything arrived. A remote pane is fed by the daemon's
    /// messages instead, so there is nothing to poll.
    pub fn drain(&mut self) -> bool {
        match self {
            Self::Local(session) => session.drain(),
            Self::Remote(_) => false,
        }
    }

    /// Ends the pane's process.
    pub fn terminate(&mut self) {
        match self {
            Self::Local(session) => session.terminate(),
            Self::Remote(remote) => {
                remote
                    .daemon
                    .send(ClientMessage::ClosePane { pane: remote.id });
            }
        }
    }
}
