//! What a pane is doing, worked out from what it prints and what is on its
//! screen.
//!
//! The harness's rules read the screen and the title; output arriving says
//! the program is busy even where no rule knows its interface; and time
//! damps the change from working to idle, so a pause between two chunks of a
//! reply does not flicker the sidebar.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dispatch_config::status::{RuleState, StatusInput, StatusRules};
use dispatch_pty::Signals;

/// Output this soon after our own input is its echo, not the program at work.
pub const ECHO: Duration = Duration::from_millis(150);

/// Output this soon after a pane is resized is it redrawing to its new size.
///
/// Every agent, and every shell, repaints on `SIGWINCH`, and a resize is
/// Dispatch's doing rather than the program's: without this, opening or
/// closing a pane would set every sibling it squeezed spinning and then
/// "finished". Longer than [`ECHO`] because a remote pane's redraw has the
/// daemon to cross twice, once for the resize going out and once for the
/// repaint coming back.
pub const REDRAW: Duration = Duration::from_millis(500);

/// How long after output a pane still counts as working.
pub const ACTIVE_FOR: Duration = Duration::from_secs(1);

/// How long an idle verdict must hold before a working pane is called idle.
pub const SETTLE: Duration = Duration::from_millis(700);

/// What a pane is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Busy.
    Working,
    /// Waiting for the next thing to do.
    Idle,
    /// Waiting on a decision only the user can make.
    Blocked,
}

/// One pane's evidence, and the verdict last reported from it.
#[derive(Debug)]
pub struct Tracker {
    rules: Arc<StatusRules>,
    title: String,
    progress: String,
    last_input: Option<Instant>,
    last_resize: Option<Instant>,
    last_output: Option<Instant>,
    reported: Option<Verdict>,
    /// When the raw verdict turned idle under a reported working one.
    idle_since: Option<Instant>,
}

impl Tracker {
    /// A tracker reading a pane with `rules`.
    #[must_use]
    pub fn new(rules: Arc<StatusRules>) -> Self {
        Self {
            rules,
            title: String::new(),
            progress: String::new(),
            last_input: None,
            last_resize: None,
            last_output: None,
            reported: None,
            idle_since: None,
        }
    }

    /// We sent the pane a keystroke, a paste or a pointer event.
    pub fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// We told the pane its new size.
    pub fn resized(&mut self, now: Instant) {
        self.last_resize = Some(now);
    }

    /// The pane printed something.
    ///
    /// Within [`ECHO`] of our own input it is the terminal echoing what was
    /// typed, and within [`REDRAW`] of a resize it is the program repainting
    /// to fit; neither counts. Typing into a pane, or making room beside it,
    /// is not the agent working.
    pub fn output(&mut self, now: Instant) {
        let within = |at: Option<Instant>, window: Duration| {
            at.is_some_and(|at| now.saturating_duration_since(at) < window)
        };
        let ours = within(self.last_input, ECHO) || within(self.last_resize, REDRAW);

        if !ours {
            self.last_output = Some(now);
        }
    }

    /// The pane's title or progress changed.
    pub fn signals(&mut self, signals: &Signals) {
        if let Some(title) = &signals.title {
            self.title.clone_from(title);
        }
        if let Some(progress) = &signals.progress {
            self.progress.clone_from(progress);
        }
    }

    /// Works out the pane's state against its live `screen`, returning it
    /// when it differs from the last one reported.
    pub fn evaluate(&mut self, now: Instant, screen: &[String]) -> Option<Verdict> {
        let rule = self.rules.evaluate(&StatusInput {
            title: &self.title,
            progress: &self.progress,
            screen,
        });
        let active = self
            .last_output
            .is_some_and(|at| now.saturating_duration_since(at) < ACTIVE_FOR);

        let raw = match rule {
            Some(RuleState::Blocked) => Verdict::Blocked,
            Some(RuleState::Working) => Verdict::Working,
            _ if active => Verdict::Working,
            _ => Verdict::Idle,
        };

        if raw != Verdict::Idle {
            self.idle_since = None;
        }

        // Only working to idle is damped: a prompt appearing, a turn
        // starting, or leaving a prompt are all worth showing the moment they
        // happen, and none of them flickers.
        let next = if self.reported == Some(Verdict::Working) && raw == Verdict::Idle {
            let since = *self.idle_since.get_or_insert(now);
            if now.saturating_duration_since(since) < SETTLE {
                return None;
            }
            Verdict::Idle
        } else {
            raw
        };

        if self.reported == Some(next) {
            return None;
        }

        self.reported = Some(next);
        self.idle_since = None;
        Some(next)
    }
}

#[cfg(test)]
mod tests;
