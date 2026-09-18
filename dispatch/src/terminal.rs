//! Owning the host terminal, and giving it back.
//!
//! Dispatch puts the terminal into raw mode, switches to the alternate screen
//! and captures the mouse. All three must be undone before the process exits
//! or the user is left with a shell that does not echo, has no scrollback and
//! reports mouse movement as garbage.

use std::io::{Stdout, stdout};

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// A terminal that restores itself.
///
/// Restoration happens in `Drop`, so it also runs when the process unwinds
/// from a panic. A panic that left the terminal in raw mode would hide its own
/// backtrace.
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Takes over the terminal.
    pub fn acquire() -> Result<Self> {
        enable_raw_mode().context("failed to put the terminal into raw mode")?;

        let mut out = stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            // Without this a paste arrives as individual keystrokes, and an
            // agent acts on each line as it lands.
            EnableBracketedPaste,
        )
        .context("failed to configure the terminal")?;

        let terminal = Terminal::new(CrosstermBackend::new(out))
            .context("failed to create the terminal backend")?;

        Ok(Self { terminal })
    }

    /// The ratatui terminal, for drawing.
    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    /// Undoes everything `acquire` did.
    ///
    /// Every step is attempted even if an earlier one fails: a failure to
    /// leave the alternate screen must not also leave raw mode on.
    fn restore() {
        let mut out = stdout();
        let _ = execute!(
            out,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::restore();
    }
}

/// Restores the terminal before the default panic handler prints anything.
///
/// Without this a panic message is written into the alternate screen, which is
/// then torn down, so the user sees a process that vanished with no
/// explanation.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        default(info);
    }));
}
