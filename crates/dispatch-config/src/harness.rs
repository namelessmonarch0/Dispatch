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

/// How to run a harness once, on one task, without a person at the keyboard.
///
/// Delegation needs a form that finishes: an interactive agent waits for input
/// forever, so a caller blocking on one would never be answered. A harness
/// without this cannot be delegated to, and Dispatch says so rather than
/// guessing at flags that may mean something else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLaunch {
    /// Arguments for the one-shot form. Exactly one `{task}` placeholder is
    /// expected; it is replaced as a single argument.
    #[serde(default)]
    pub args: Vec<String>,
}

/// A registered coding agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessDef {
    /// Stable identifier, matching the file stem by convention.
    pub id: String,
    /// Name shown in pickers.
    pub display_name: String,

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
}

impl HarnessDef {
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

    /// The launch for running `task` once, or `None` when the harness has no
    /// non-interactive form.
    ///
    /// The task replaces `{task}` inside each argument, which keeps it one
    /// argv element however the harness spells the flag — `"{task}"` or
    /// `"--prompt={task}"`. It is never passed through a shell, so quotes,
    /// newlines and `$(…)` in a task are inert.
    #[must_use]
    pub fn task_launch(&self, task: &str) -> Option<Launch> {
        let form = self.task.as_ref()?;
        let mut launch = self.launch_for_current_platform();

        launch.args = form
            .args
            .iter()
            .map(|arg| arg.replace("{task}", task))
            .collect();

        Some(launch)
    }
}
