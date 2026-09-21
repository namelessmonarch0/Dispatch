//! The projects a user keeps.
//!
//! Dispatch is started against a directory, but the fleet is not one
//! directory: the sidebar is the list of projects the user has chosen to keep,
//! and it outlives the process until they remove one. Nothing here scans the
//! disk, and nothing is kept that was not opened.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// The file the list lives in, inside the configuration directory.
const FILE: &str = "projects.toml";

/// The file's shape.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    /// Project roots, in the order they were first opened.
    #[serde(default)]
    roots: Vec<PathBuf>,
}

/// The remembered project roots, oldest first.
///
/// No file yet means nothing has been kept, which is what a first run looks
/// like rather than a failure.
pub fn load(dir: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let path = dir.join(FILE);

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ConfigError::Io { path, source }),
    };

    let saved: Saved = toml::from_str(&text).map_err(|source| ConfigError::Toml {
        path: path.clone(),
        source,
    })?;

    Ok(saved.roots)
}

/// Writes the list, replacing whatever was there.
pub fn save(dir: &Path, roots: &[PathBuf]) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let path = dir.join(FILE);
    let text = toml::to_string_pretty(&Saved {
        roots: roots.to_vec(),
    })
    .expect("a list of paths serialises");

    std::fs::write(&path, text).map_err(|source| ConfigError::Io { path, source })
}

/// Adds `root` to the list, if it is not already on it.
///
/// Answers whether the file was written: every start opens what is kept, and
/// rewriting the file each time would churn it for nothing.
pub fn remember(dir: &Path, root: &Path) -> Result<bool, ConfigError> {
    let mut roots = load(dir)?;

    if roots.iter().any(|kept| kept == root) {
        return Ok(false);
    }

    roots.push(root.to_path_buf());
    save(dir, &roots)?;
    Ok(true)
}

/// Takes `root` off the list.
///
/// Answers whether it was there to take off.
pub fn forget(dir: &Path, root: &Path) -> Result<bool, ConfigError> {
    let mut roots = load(dir)?;
    let before = roots.len();

    roots.retain(|kept| kept != root);
    if roots.len() == before {
        return Ok(false);
    }

    save(dir, &roots)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
