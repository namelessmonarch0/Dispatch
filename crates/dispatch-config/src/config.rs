//! Dispatch's own configuration, as opposed to a harness's.
//!
//! Absent means default: a fresh install has no `config.toml`, and must behave
//! exactly like a configured one that changed nothing. Every field therefore
//! has a default, and a file may mention only what it changes.
//!
//! An unknown key is kept rather than rejected. A newer Dispatch's key must not
//! stop an older one starting — two versions share a configuration directory
//! whenever a machine is mid-upgrade — but a typo must not be silent either, so
//! unknown keys are reported by name for the caller to log.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::harness::Launch;

/// Limits on delegation, which starts processes on this machine.
///
/// These refuse rather than prompt. A prompt for the five-hundredth request is
/// not a safeguard; it is a way to make someone hold down `d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationLimits {
    /// How deep delegation may nest. At `1`, a subagent cannot delegate.
    pub max_depth: u8,
    /// How many subagents one pane may have running at once.
    pub max_live_per_parent: usize,
    /// How long a request waits for an answer before it is refused.
    ///
    /// There is no value that means "no deadline", and `0` is not it: a request
    /// is then already out of time when the daemon's next pass comes round, so
    /// `0` refuses everything nobody was quick enough to approve. Deferral has a
    /// floor by design — an agent on an unattended daemon must not wait for a
    /// person who is not there — so the way to wait longer is a larger number.
    pub request_timeout_secs: u64,
}

impl Default for DelegationLimits {
    fn default() -> Self {
        Self {
            max_depth: 1,
            max_live_per_parent: 4,
            request_timeout_secs: 600,
        }
    }
}

/// How the interface draws itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceConfig {
    /// Whether things move: spinners, pulses, easing, transitions. Off, every
    /// change is shown at once and nothing animates.
    pub motion: bool,
}

impl Default for InterfaceConfig {
    fn default() -> Self {
        Self { motion: true }
    }
}

/// Whether the user's shell starts as a login shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginShell {
    /// As this platform's terminals do: a login shell on macOS, not
    /// elsewhere.
    #[default]
    Auto,
    /// Always pass `-l`.
    Always,
    /// Never pass `-l`.
    Never,
}

/// The shell a `shell` pane runs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// The program. `None` means the machine's own: `$SHELL`, then the
    /// login record, then `/bin/sh`.
    pub command: Option<String>,
    /// Arguments, after any `-l`.
    pub args: Vec<String>,
    /// Whether it starts as a login shell.
    pub login: LoginShell,
}

impl ShellConfig {
    /// How to start the shell on this machine.
    #[must_use]
    pub fn launch(&self) -> Launch {
        self.launch_with(
            dispatch_os::shell::user_shell,
            dispatch_os::shell::login_by_default(),
            dispatch_os::shell::takes_login_flag(),
        )
    }

    /// [`Self::launch`], with what it would ask the machine handed in, so
    /// every platform's rules can be tested on any one of them.
    pub(crate) fn launch_with(
        &self,
        user_shell: impl FnOnce() -> String,
        login_by_default: bool,
        takes_login_flag: bool,
    ) -> Launch {
        let command = self
            .command
            .clone()
            .filter(|command| !command.trim().is_empty())
            .unwrap_or_else(user_shell);

        let login = match self.login {
            LoginShell::Auto => login_by_default,
            LoginShell::Always => true,
            LoginShell::Never => false,
        };

        let mut args = Vec::new();
        if login && takes_login_flag {
            args.push("-l".to_string());
        }
        args.extend(self.args.iter().cloned());

        Launch {
            command,
            args,
            env: BTreeMap::new(),
        }
    }
}

/// Everything `config.toml` can say.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Limits on delegation.
    pub delegation: DelegationLimits,
    /// How the interface draws itself. The daemon ignores it.
    pub interface: InterfaceConfig,
    /// The shell a `shell` pane runs. Read by whichever side starts panes:
    /// the daemon, or a standalone client.
    pub shell: ShellConfig,
}

/// A loaded configuration, plus the keys this build did not understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    /// The configuration, with defaults for anything unmentioned.
    pub config: Config,
    /// Dotted paths of keys this build ignored, for the caller to log.
    pub unknown: Vec<String>,
}

impl Config {
    /// Loads `path`, or the defaults when it does not exist.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Ok(Self::load_reporting(path)?.config)
    }

    /// Loads `path`, also reporting keys this build ignored.
    pub fn load_reporting(path: &Path) -> Result<LoadedConfig, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadedConfig {
                    config: Self::default(),
                    unknown: Vec::new(),
                });
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Toml {
            path: path.to_path_buf(),
            source,
        })?;

        // Parsed a second time as plain tables to find keys the typed parse
        // silently dropped. Cheap: this file is a handful of lines, read once
        // per process.
        let raw: toml::Table = toml::from_str(&text).map_err(|source| ConfigError::Toml {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(LoadedConfig {
            config,
            unknown: unknown_keys(&raw),
        })
    }
}

/// Dotted paths of keys Dispatch does not know.
fn unknown_keys(raw: &toml::Table) -> Vec<String> {
    const DELEGATION: [&str; 3] = ["max_depth", "max_live_per_parent", "request_timeout_secs"];
    const SHELL: [&str; 3] = ["command", "args", "login"];

    let mut unknown = Vec::new();

    for (section, value) in raw {
        match (section.as_str(), value) {
            ("delegation", toml::Value::Table(table)) => {
                for key in table.keys() {
                    if !DELEGATION.contains(&key.as_str()) {
                        unknown.push(format!("delegation.{key}"));
                    }
                }
            }
            ("interface", toml::Value::Table(table)) => {
                for key in table.keys() {
                    if key != "motion" {
                        unknown.push(format!("interface.{key}"));
                    }
                }
            }
            ("shell", toml::Value::Table(table)) => {
                for key in table.keys() {
                    if !SHELL.contains(&key.as_str()) {
                        unknown.push(format!("shell.{key}"));
                    }
                }
            }
            _ => unknown.push(section.clone()),
        }
    }

    unknown.sort();
    unknown
}

#[cfg(test)]
mod tests;
