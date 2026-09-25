//! Owning the host terminal, and giving it back.
//!
//! Dispatch puts the terminal into raw mode, switches to the alternate screen
//! and captures the mouse. All three must be undone before the process exits
//! or the user is left with a shell that does not echo, has no scrollback and
//! reports mouse movement as garbage.

use std::io::{Stdout, stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use dispatch_tui::theme::{self, Depth, Replies, Theme};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// How long the terminal is given to say what its colours are.
///
/// An upper bound rather than a wait: the device-attributes reply ends it as
/// soon as the terminal has answered, which locally takes a millisecond or
/// two. Long enough for an SSH round trip, because an answer that arrives
/// after the event loop has started is read as keystrokes.
const ASK_TIMEOUT: Duration = Duration::from_secs(1);

/// A terminal that restores itself.
///
/// Restoration happens in `Drop`, so it also runs when the process unwinds
/// from a panic. A panic that left the terminal in raw mode would hide its own
/// backtrace.
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    theme: Theme,
}

impl TerminalGuard {
    /// Takes over the terminal.
    pub fn acquire() -> Result<Self> {
        enable_raw_mode().context("failed to put the terminal into raw mode")?;

        // Raw, so the answers are neither echoed nor held back for a newline;
        // before the alternate screen and the event loop, so nothing else is
        // reading yet.
        let theme = ask_for_theme();

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

        Ok(Self { terminal, theme })
    }

    /// The ratatui terminal, for drawing.
    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    /// The colours Dispatch draws its chrome in, mixed from what the terminal
    /// said its own are.
    pub fn theme(&self) -> Theme {
        self.theme
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

/// Asks the terminal for its colours and mixes Dispatch's from them.
fn ask_for_theme() -> Theme {
    let answer = dispatch_os::tty::ask(theme::QUERY, ASK_TIMEOUT, |bytes| {
        Replies::parse(bytes).done
    });
    let depth = Depth::from_colorterm(std::env::var("COLORTERM").ok().as_deref());

    Theme::new(Replies::parse(&answer).palette(), depth)
}
