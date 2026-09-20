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

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

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

/// Everything `config.toml` can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Limits on delegation.
    pub delegation: DelegationLimits,
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
            _ => unknown.push(section.clone()),
        }
    }

    unknown.sort();
    unknown
}

#[cfg(test)]
mod tests;
