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
    pub fn launch_for_current_platform(&self) -> &Launch {
        self.launch_for(std::env::consts::OS)
    }

    /// The launch configuration for a named platform.
    ///
    /// Falls back to the default when the platform has no override, so a
    /// harness that behaves the same everywhere needs only one entry.
    #[must_use]
    pub fn launch_for(&self, os: &str) -> &Launch {
        self.platform.get(os).unwrap_or(&self.launch)
    }
}
