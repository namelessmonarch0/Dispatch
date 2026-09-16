//! Waiting for the operating system to ask Dispatch to exit.
//!
//! The TUI puts the terminal into raw mode, switches to the alternate screen,
//! and enables mouse capture. All three must be undone before the process
//! exits or the user's shell is left unusable, so shutdown requests have to be
//! observed rather than allowed to kill the process outright.

/// Resolves when the operating system asks the process to shut down.
///
/// On Unix that is `SIGINT` or `SIGTERM`. On Windows it is Ctrl-C, Ctrl-Break,
/// the console being closed, the user logging off, or the system shutting
/// down.
///
/// Resolves once. Callers that need to keep serving after the first request
/// should call it again.
pub async fn shutdown_requested() -> std::io::Result<()> {
    imp::shutdown_requested().await
}

#[cfg(unix)]
mod imp {
    use tokio::signal::unix::{SignalKind, signal};

    pub(super) async fn shutdown_requested() -> std::io::Result<()> {
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;

        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
            // The controlling terminal went away, which for an SSH session
            // means the connection dropped.
            _ = hangup.recv() => {}
        }

        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};

    pub(super) async fn shutdown_requested() -> std::io::Result<()> {
        let mut c = ctrl_c()?;
        let mut break_ = ctrl_break()?;
        let mut close = ctrl_close()?;
        let mut logoff = ctrl_logoff()?;
        let mut shutdown = ctrl_shutdown()?;

        tokio::select! {
            _ = c.recv() => {}
            _ = break_.recv() => {}
            _ = close.recv() => {}
            _ = logoff.recv() => {}
            _ = shutdown.recv() => {}
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    /// Registering the handlers must succeed, and the future must not resolve
    /// on its own. A future that resolved immediately would tear the TUI down
    /// the moment it started.
    #[tokio::test]
    async fn does_not_resolve_without_a_signal() {
        let result = tokio::time::timeout(Duration::from_millis(150), shutdown_requested()).await;
        assert!(
            result.is_err(),
            "shutdown_requested resolved without a signal being sent"
        );
    }
}
