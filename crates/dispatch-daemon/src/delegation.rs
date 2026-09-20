//! Deciding whether a delegation request may be asked about at all.
//!
//! These rules refuse without prompting. A prompt for the five-hundredth
//! request is not a safeguard; it is a way to make someone hold down `d`. The
//! user's judgement is for requests that are plausible.

use std::time::Instant;

use dispatch_config::DelegationLimits;
use dispatch_core::{PaneId, RequestId};
use dispatch_proto::ServerMessage;

/// A request that has been asked about and not yet answered.
pub struct Pending {
    /// The request.
    pub id: RequestId,
    /// The pane that asked.
    pub parent: PaneId,
    /// Which harness would run.
    pub harness: String,
    /// What it would be asked to do.
    pub task: String,
    /// Size to start the subagent at.
    pub size: (u16, u16),
    /// Which client is waiting for the answer.
    pub caller: u64,
    /// When it was asked, for the deadline.
    pub asked: Instant,
    /// What the interface clients were told, so a late subscriber can be sent
    /// the same thing without rebuilding it.
    pub announcement: ServerMessage,
}

/// Why a request cannot be asked about, if it cannot.
///
/// `depth` is how many parents the asking pane already has, `live` how many of
/// its children are running, and `has_task_form` whether the harness has a
/// non-interactive shape to run at all.
#[must_use]
pub fn refusal(
    depth: u8,
    live: usize,
    limits: DelegationLimits,
    has_task_form: bool,
    harness: &str,
) -> Option<String> {
    if !has_task_form {
        return Some(format!(
            "harness {harness:?} has no [task] form, so it cannot be run on one task; \
             add one or delegate to a harness that has one"
        ));
    }

    if depth >= limits.max_depth {
        return Some(format!(
            "delegation is capped at depth {}; this pane is already a subagent",
            limits.max_depth
        ));
    }

    if live >= limits.max_live_per_parent {
        return Some(format!(
            "this pane already has {live} subagents running, and the cap is {}",
            limits.max_live_per_parent
        ));
    }

    None
}

#[cfg(test)]
mod tests;
