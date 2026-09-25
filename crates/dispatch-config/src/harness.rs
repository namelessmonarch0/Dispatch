//! Harness definitions: how to launch each coding agent.
//!
//! Harnesses are data, not code. A user can register a new one by dropping a
//! TOML file into the harnesses directory, or through the harness manager in
//! the TUI, without Dispatch being rebuilt.

use std::collections::BTreeMap;

use dispatch_core::HarnessId;
use serde::{Deserialize, Serialize};

/// What a setting accepts, so the harness manager can render a form for a
/// harness Dispatch has never seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettingKind {
    /// Free text.
    Text {
        /// Value used when the user does not supply one.
        #[serde(default)]
        default: Option<String>,
    },
    /// One of a fixed set of values, such as a model or effort level.
    Choice {
        /// The values on offer.
        options: Vec<String>,
        /// Value used when the user does not supply one.
        #[serde(default)]
        default: Option<String>,
    },
    /// A flag.
    Bool {
        /// Value used when the user does not supply one.
        #[serde(default)]
        default: Option<bool>,
    },
}

/// One configurable setting a harness exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingDef {
    /// Key the setting is referenced by in argument templates.
    pub key: String,
    /// Human-readable label shown in the harness manager.
    pub label: String,
    /// What the setting accepts.
    #[serde(flatten)]
    pub kind: SettingKind,
}

/// How to launch a harness on one platform.
///
/// Windows needs its own entry more often than not: `claude` there is
/// typically a `claude.cmd` shim, which cannot be executed directly and has to
/// be run through `cmd.exe /c`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Launch {
    /// Executable to run. Resolved on `PATH` when not absolute.
    pub command: String,
    /// Arguments, which may contain `{placeholder}` templates.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to set for the child, on top of what it
    /// inherits.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// One platform's arguments for a one-shot run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskArgs {
    /// Arguments, with `{task}` standing for the task.
    #[serde(default)]
    pub args: Vec<String>,
}

/// How to run a harness once, on one task, without a person at the keyboard.
///
/// Delegation needs a form that finishes: an interactive agent waits for input
/// forever, so a caller blocking on one would never be answered. A harness
/// without this cannot be delegated to, and Dispatch says so rather than
/// guessing at flags that may mean something else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLaunch {
    /// Arguments for the one-shot form, with `{task}` standing for the task.
    #[serde(default)]
    pub args: Vec<String>,
    /// Per-platform overrides, keyed by `std::env::consts::OS` exactly as
    /// `HarnessDef::platform` is. A platform whose interactive launch needs a
    /// wrapper needs it here too: the wrapper is how the executable is reached,
    /// and a one-shot run reaches it the same way.
    #[serde(default)]
    pub platform: BTreeMap<String, TaskArgs>,
}

/// The mark drawn beside a harness that names none of its own.
///
/// A terminal, because that is what every harness is until it says otherwise.
/// Nerd Font, like the rest of Dispatch's glyphs.
pub const DEFAULT_ICON: &str = "\u{f120}";

/// The mark a harness Dispatch ships is drawn with, by id.
fn built_in_icon(id: &str) -> Option<&'static str> {
    match id {
        "claude" => Some("\u{ec82}"),
        "codex" => Some("\u{ec81}"),
        "agy" => Some("\u{e7f0}"),
        "opencode" => Some("\u{f121}"),
        _ => None,
    }
}

/// A registered coding agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessDef {
    /// Stable identifier, matching the file stem by convention.
    pub id: String,
    /// Name shown in pickers.
    pub display_name: String,

    /// A single character drawn beside this harness's panes in the sidebar.
    ///
    /// Optional: a harness that names none is drawn with [`DEFAULT_ICON`]. One
    /// column is reserved for it, so a glyph wider than a cell pushes the title
    /// of that row alone out of line.
    #[serde(default)]
    pub icon: Option<String>,

    /// Default launch configuration, used when no platform override applies.
    #[serde(flatten)]
    pub launch: Launch,

    /// Launch overrides per platform, keyed by `std::env::consts::OS`
    /// (`"windows"`, `"macos"`, `"linux"`).
    #[serde(default)]
    pub platform: BTreeMap<String, Launch>,

    /// Environment variables to set for the child process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// The non-interactive form used when another agent delegates to this one.
    #[serde(default)]
    pub task: Option<TaskLaunch>,

    /// Settings the harness manager offers for this harness.
    #[serde(default)]
    pub settings: Vec<SettingDef>,

    /// Rules that read this agent's state off its screen. Absent means the
    /// built-in rules for its id, if Dispatch has some.
    #[serde(default)]
    pub status: Option<crate::status::StatusDef>,
}

impl HarnessDef {
    /// The mark drawn beside this harness's panes.
    ///
    /// A file with no `icon` key falls back on its id before the generic
    /// glyph: Dispatch never rewrites a harness file it already wrote, so
    /// every installation made before icons existed has four of them.
    #[must_use]
    pub fn icon(&self) -> &str {
        self.icon
            .as_deref()
            .or_else(|| built_in_icon(&self.id))
            .unwrap_or(DEFAULT_ICON)
    }

    /// The identifier as a [`HarnessId`].
    #[must_use]
    pub fn harness_id(&self) -> HarnessId {
        HarnessId::new(&self.id)
    }

    /// The launch configuration for the platform Dispatch is running on.
    #[must_use]
    pub fn launch_for_current_platform(&self) -> Launch {
        self.launch_for(std::env::consts::OS)
    }

    /// The launch configuration for a named platform.
    ///
    /// Falls back to the default when the platform has no override, so a
    /// harness that behaves the same everywhere needs only one entry.
    ///
    /// The harness-level environment is merged in, with any platform-specific
    /// entry winning, so `env` can be declared once and still be overridden
    /// where a platform needs something different.
    #[must_use]
    pub fn launch_for(&self, os: &str) -> Launch {
        let base = self.platform.get(os).unwrap_or(&self.launch);

        let mut launch = base.clone();
        for (key, value) in &self.env {
            launch
                .env
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        launch
    }

    /// The launch for running `task` once on the current platform.
    #[must_use]
    pub fn task_launch(&self, task: &str) -> Option<Launch> {
        self.task_launch_for(std::env::consts::OS, task)
    }

    /// The launch for running `task` once on a named platform.
    ///
    /// Returns `None` when the harness has no one-shot form for that platform,
    /// including when its argument list is empty: without arguments there is no
    /// way to tell the agent what the task is, so there is nothing to run.
    #[must_use]
    pub fn task_launch_for(&self, os: &str, task: &str) -> Option<Launch> {
        let form = self.task.as_ref()?;

        let args = match form.platform.get(os) {
            Some(override_) => &override_.args,
            None => &form.args,
        };
        if args.is_empty() {
            return None;
        }

        let mut launch = self.launch_for(os);
        launch.args = args.iter().map(|arg| arg.replace("{task}", task)).collect();
        Some(launch)
    }
}
