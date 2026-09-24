//! Loading Dispatch's configuration and harness definitions.

pub mod config;
pub mod defaults;
pub mod harness;
pub mod machines;
pub mod projects;
mod store;

/// Shared by this crate's test modules, so there is one temporary-directory
/// counter rather than one per module.
#[cfg(test)]
mod testing;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use config::{Config, DelegationLimits, LoadedConfig};
pub use harness::{
    HarnessDef, Launch, SettingDef, SettingKind, TASK_FILE_ENV, TaskArgs, TaskInput, TaskLaunch,
    TaskRun,
};

/// Failures while loading configuration.
///
/// Every variant names the file at fault. A bad TOML must never take the TUI
/// down, and the user has to be told which file to fix.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A platform directory could not be determined.
    #[error(transparent)]
    Path(#[from] dispatch_os::paths::PathError),

    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// File at fault.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },

    /// A harness file was not valid TOML, or was missing a required field.
    #[error("{path}: {source}")]
    Toml {
        /// File at fault.
        path: PathBuf,
        /// Underlying parse error.
        #[source]
        source: toml::de::Error,
    },

    /// A harness file declares an id that does not match its file name.
    #[error("{path}: declares id {declared:?} but the file name says {expected:?}")]
    IdMismatch {
        /// File at fault.
        path: PathBuf,
        /// The `id` field inside the file.
        declared: String,
        /// The file stem.
        expected: String,
    },

    /// A machine could not be registered under the name it was given.
    #[error("{path}: {reason}")]
    Machine {
        /// The registry file.
        path: PathBuf,
        /// What is wrong with the name.
        reason: String,
    },
}

/// Every harness Dispatch knows about, keyed by id.
#[derive(Debug, Clone, Default)]
pub struct HarnessRegistry {
    harnesses: BTreeMap<String, HarnessDef>,
}

impl FromIterator<HarnessDef> for HarnessRegistry {
    /// Collects definitions held in memory rather than read from a directory.
    ///
    /// A later id wins, as it does when two files declare one: the registry is
    /// keyed by id and cannot hold both.
    fn from_iter<I: IntoIterator<Item = HarnessDef>>(defs: I) -> Self {
        Self {
            harnesses: defs.into_iter().map(|def| (def.id.clone(), def)).collect(),
        }
    }
}

impl HarnessRegistry {
    /// Loads every `*.toml` in `dir`.
    ///
    /// A file that fails to parse is reported rather than skipped: silently
    /// dropping a harness would look like it was never registered.
    pub fn load_from_dir(dir: &Path) -> Result<Self, ConfigError> {
        let mut harnesses = BTreeMap::new();

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            // No directory yet means no harnesses yet, which is not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { harnesses });
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: dir.to_path_buf(),
                    source,
                });
            }
        };

        for entry in entries {
            let path = entry
                .map_err(|source| ConfigError::Io {
                    path: dir.to_path_buf(),
                    source,
                })?
                .path();

            if path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }

            let def = Self::load_file(&path)?;
            harnesses.insert(def.id.clone(), def);
        }

        Ok(Self { harnesses })
    }

    /// Loads and validates one harness file.
    pub fn load_file(path: &Path) -> Result<HarnessDef, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let def: HarnessDef = toml::from_str(&text).map_err(|source| ConfigError::Toml {
            path: path.to_path_buf(),
            source,
        })?;

        // The file stem is what the picker and any saved reference use, so a
        // mismatch would make the harness unreachable under its own name.
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if def.id != stem {
            return Err(ConfigError::IdMismatch {
                path: path.to_path_buf(),
                declared: def.id,
                expected: stem,
            });
        }

        Ok(def)
    }

    /// Looks up a harness by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&HarnessDef> {
        self.harnesses.get(id)
    }

    /// Every harness, ordered by id.
    pub fn all(&self) -> impl Iterator<Item = &HarnessDef> {
        self.harnesses.values()
    }

    /// How many harnesses are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.harnesses.len()
    }

    /// Whether no harnesses are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.harnesses.is_empty()
    }
}

/// Writes the built-in harness files into `dir`: any that are missing, and
/// any still exactly as an earlier Dispatch wrote them.
///
/// Returns the ids written. A file the user has edited is left alone, so an
/// upgrade never discards local changes; one nobody touched is brought up to
/// date, so a fix to a built-in reaches installations made before it.
///
/// An upgrade that fails is logged and skipped rather than returned: the
/// file it would have replaced is still a valid harness, and a Windows form
/// it leaves in place is refused when used, so nothing is gained by stopping
/// Dispatch from starting over it. Failing to write a missing file is still
/// an error, as it always was.
pub fn write_missing_built_ins(dir: &Path) -> Result<Vec<&'static str>, ConfigError> {
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut written = Vec::new();

    for built_in in defaults::BUILT_INS {
        let path = dir.join(format!("{}.toml", built_in.id));

        if !path.exists() {
            std::fs::write(&path, built_in.toml).map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
            written.push(built_in.id);
            continue;
        }

        match upgrade(&path, built_in) {
            Ok(true) => written.push(built_in.id),
            Ok(false) => {}
            Err(error) => tracing::warn!(
                harness = built_in.id,
                %error,
                "could not upgrade a built-in harness nobody edited; it is left as it was"
            ),
        }
    }

    Ok(written)
}

/// Replaces `path` with `built_in`'s current body if it is still exactly a
/// body an earlier Dispatch wrote, and says whether it did.
///
/// Replaced, never rewritten in place, so another Dispatch starting at the
/// same moment reads the old body or the new and never part of either. A
/// link is followed first: the file it names is what gets replaced, and the
/// link -- the user's arrangement -- stays.
fn upgrade(path: &Path, built_in: &defaults::BuiltIn) -> Result<bool, ConfigError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| ConfigError::Io { path, source }
    };

    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        // Not text, so not anything Dispatch wrote.
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => return Ok(false),
        Err(error) => return Err(io(path)(error)),
    };

    // Compared without carriage returns: a file written from a checkout
    // with CRLF line endings is still the same file.
    let unix = |text: &str| text.replace("\r\n", "\n");
    let untouched = built_in
        .superseded
        .iter()
        .any(|old| unix(old) == unix(&existing));
    if !untouched {
        return Ok(false);
    }

    let is_link = std::fs::symlink_metadata(path)
        .map_err(io(path))?
        .file_type()
        .is_symlink();
    let target = if is_link {
        std::fs::canonicalize(path).map_err(io(path))?
    } else {
        path.to_path_buf()
    };

    // Judged again at the last moment: a user saving their own edit while
    // this ran keeps it.
    let replaced = store::replace_unless_changed(&target, built_in.toml, |current| {
        unix(current) == unix(&existing)
    })?;
    if replaced {
        tracing::info!(
            harness = built_in.id,
            "upgraded a built-in harness nobody edited"
        );
    } else {
        tracing::info!(
            harness = built_in.id,
            "a built-in harness changed while it was being upgraded; left as it now is"
        );
    }
    Ok(replaced)
}

#[cfg(test)]
mod tests;

/// Finds harnesses that are installed but not yet registered.
///
/// Looks for each candidate on `PATH` and returns those that exist and have no
/// definition yet, so the harness manager can offer exactly what would work.
///
/// Candidates are the built-ins plus anything named in `extra`, so a user can
/// look for a tool Dispatch does not ship a definition for.
#[must_use]
pub fn discover_unregistered(registry: &HarnessRegistry, extra: &[String]) -> Vec<String> {
    let mut found = Vec::new();

    let candidates = defaults::BUILT_INS
        .iter()
        .map(|b| b.id.to_string())
        .chain(extra.iter().cloned());

    for candidate in candidates {
        if registry.get(&candidate).is_some() || found.contains(&candidate) {
            continue;
        }
        if which(&candidate).is_some() {
            found.push(candidate);
        }
    }

    found
}

/// Whether `name` resolves to an executable on `PATH`.
///
/// Windows needs the extension list: `claude` there is typically `claude.cmd`,
/// which a bare name would never find.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(str::to_lowercase)
            .collect()
    } else {
        vec![String::new()]
    };

    for dir in std::env::split_paths(&path) {
        for extension in &extensions {
            let candidate = dir.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Writes a definition for `id`, using the built-in when one exists.
///
/// Returns the path written. Refuses to overwrite, so registering something
/// already present cannot discard a user's edits.
pub fn register_harness(dir: &Path, id: &str) -> Result<PathBuf, ConfigError> {
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let path = dir.join(format!("{id}.toml"));
    if path.exists() {
        return Ok(path);
    }

    let body = defaults::BUILT_INS
        .iter()
        .find(|b| b.id == id)
        .map_or_else(|| generic_harness(id), |b| b.toml.to_string());

    std::fs::write(&path, body).map_err(|source| ConfigError::Io {
        path: path.clone(),
        source,
    })?;

    Ok(path)
}

/// A definition for a harness Dispatch ships no template for.
///
/// Launches the command with no arguments, which is what a terminal agent
/// does by default, and adds the Windows shim wrapper since that is needed far
/// more often than not.
fn generic_harness(id: &str) -> String {
    format!(
        "id = {id:?}\n\
         display_name = {id:?}\n\
         command = {id:?}\n\
         args = []\n\
         \n\
         # Installed on Windows as a .cmd shim more often than not, which\n\
         # CreateProcess cannot execute directly.\n\
         [platform.windows]\n\
         command = \"cmd.exe\"\n\
         args = [\"/c\", {id:?}]\n"
    )
}
