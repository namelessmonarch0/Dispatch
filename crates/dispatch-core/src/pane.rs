//! Panes: one running agent each.

use serde::{Deserialize, Serialize};

use crate::id::{PaneId, ProjectId};

/// Identifies a harness definition, such as `claude` or `codex`.
///
/// A plain string rather than an enum: harnesses are declared in TOML and can
/// be added by the user at runtime, so the set is not known at compile time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HarnessId(String);

impl HarnessId {
    /// Creates a harness identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HarnessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a pane is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneStatus {
    /// The process has been spawned but has not produced output yet.
    Starting,
    /// The agent is working.
    Running,
    /// The agent is waiting on the user.
    Idle,
    /// The process exited with this status code.
    Exited(i32),
}

impl PaneStatus {
    /// Whether the underlying process is still alive.
    #[must_use]
    pub fn is_live(&self) -> bool {
        !matches!(self, Self::Exited(_))
    }
}

/// How a pane participates in orchestration.
///
/// A pane starts as a [`PaneRole::Worker`] and becomes an
/// [`PaneRole::Orchestrator`] when its first child is approved — see
/// [`crate::AppState::adopt_pane`]. Nothing asks the user to declare a pane an
/// orchestrator: there is no mode to learn and no way to set it wrongly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PaneRole {
    /// An ordinary agent pane.
    #[default]
    Worker,
    /// A pane whose agent delegates tasks to other panes.
    Orchestrator,
}

/// One agent, running in one pseudoterminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    /// Stable identifier.
    pub id: PaneId,
    /// The project this pane belongs to.
    pub project: ProjectId,
    /// Which harness is running.
    pub harness: HarnessId,
    /// Title, taken from the terminal's own title sequence when it sets one.
    pub title: String,
    /// What the pane is doing.
    pub status: PaneStatus,
    /// How the pane participates in orchestration.
    pub role: PaneRole,
    /// The pane that delegated this one's work, when it was delegated.
    #[serde(default)]
    pub parent: Option<PaneId>,
    /// Whether this pane outlives the caller that asked for it.
    ///
    /// Set when the user approved it for the whole parent pane rather than
    /// once: that is how they say "let this pane's work run".
    #[serde(default)]
    pub durable: bool,
    /// Whether this pane is closed but kept as a row for live children.
    #[serde(default)]
    pub closed: bool,
    /// The git branch the pane's foreground program is working on, while it
    /// is inside a repository.
    ///
    /// Reported by whichever machine runs the pane; `None` from a daemon too
    /// old to say.
    #[serde(default)]
    pub branch: Option<String>,
}

impl Pane {
    /// Creates a pane in [`PaneStatus::Starting`].
    #[must_use]
    pub fn new(project: ProjectId, harness: HarnessId) -> Self {
        let title = harness.as_str().to_string();
        Self {
            id: PaneId::new(),
            project,
            harness,
            title,
            status: PaneStatus::Starting,
            role: PaneRole::Worker,
            parent: None,
            durable: false,
            closed: false,
            branch: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_pane_starts_as_a_starting_worker() {
        let pane = Pane::new(ProjectId::new(), HarnessId::new("claude"));
        assert_eq!(pane.status, PaneStatus::Starting);
        assert_eq!(pane.role, PaneRole::Worker);
    }

    #[test]
    fn the_title_defaults_to_the_harness_name() {
        let pane = Pane::new(ProjectId::new(), HarnessId::new("codex"));
        assert_eq!(pane.title, "codex");
    }

    #[test]
    fn only_an_exited_pane_is_not_live() {
        assert!(PaneStatus::Starting.is_live());
        assert!(PaneStatus::Running.is_live());
        assert!(PaneStatus::Idle.is_live());
        assert!(!PaneStatus::Exited(0).is_live());
        assert!(!PaneStatus::Exited(1).is_live());
    }

    #[test]
    fn a_new_pane_has_no_parent_and_is_not_a_tombstone() {
        let pane = Pane::new(ProjectId::new(), HarnessId::new("claude"));

        assert_eq!(pane.parent, None);
        assert!(!pane.durable);
        assert!(!pane.closed);
    }
}
