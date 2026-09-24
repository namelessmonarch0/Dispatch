//! The projects a user keeps.
//!
//! Dispatch is started against a directory, but the fleet is not one
//! directory: the sidebar is the list of projects the user has chosen to keep,
//! and it outlives the process until they remove one. Nothing here scans the
//! disk, and nothing is kept that was not opened.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// The file the list lives in, inside the configuration directory.
const FILE: &str = "projects.toml";

/// The file's shape.
///
/// This machine's roots stay at the top level, where every file written
/// before machines existed put them. Each remote machine gets a table of its
/// own; an older build reading this file ignores those tables.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    /// This machine's project roots, in the order they were first opened.
    #[serde(default)]
    roots: Vec<PathBuf>,
    /// Each remote machine's, keyed by its registry name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    machines: BTreeMap<String, Kept>,
}

/// One remote machine's kept roots.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    /// Project roots on this remote machine, in the order they were first opened.
    #[serde(default)]
    roots: Vec<PathBuf>,
}

impl Saved {
    /// The list for `machine`, or this machine's for `None`.
    fn list_mut(&mut self, machine: Option<&str>) -> &mut Vec<PathBuf> {
        match machine {
            None => &mut self.roots,
            Some(name) => &mut self.machines.entry(name.to_string()).or_default().roots,
        }
    }
}

/// Reads the whole file. No file is an empty one.
fn read(dir: &Path) -> Result<Saved, ConfigError> {
    crate::store::read(dir, FILE)
}

/// This machine's remembered project roots, oldest first.
///
/// No file yet means nothing has been kept, which is what a first run looks
/// like rather than a failure.
pub fn load(dir: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    Ok(read(dir)?.roots)
}

/// A remote machine's remembered project roots, oldest first, exactly as they
/// were typed.
///
/// Unresolved on purpose: they are paths on another machine, and only its
/// daemon can resolve them.
pub fn load_on(dir: &Path, machine: &str) -> Result<Vec<PathBuf>, ConfigError> {
    Ok(read(dir)?
        .machines
        .get(machine)
        .map(|kept| kept.roots.clone())
        .unwrap_or_default())
}

/// Writes this machine's list, replacing whatever was there — and leaving
/// every remote machine's alone.
pub fn save(dir: &Path, roots: &[PathBuf]) -> Result<(), ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        saved.roots = roots.to_vec();
        Ok(((), true))
    })
}

/// Adds `root` to this machine's list, if it is not already on it.
///
/// Answers whether the file was written: every start opens what is kept, and
/// rewriting the file each time would churn it for nothing.
pub fn remember(dir: &Path, root: &Path) -> Result<bool, ConfigError> {
    remember_in(dir, None, root)
}

/// Adds `root` to a remote machine's list, if it is not already on it.
pub fn remember_on(dir: &Path, machine: &str, root: &Path) -> Result<bool, ConfigError> {
    remember_in(dir, Some(machine), root)
}

/// Takes `root` off this machine's list.
///
/// Answers whether it was there to take off.
pub fn forget(dir: &Path, root: &Path) -> Result<bool, ConfigError> {
    forget_in(dir, None, root)
}

/// Takes `root` off a remote machine's list.
pub fn forget_on(dir: &Path, machine: &str, root: &Path) -> Result<bool, ConfigError> {
    forget_in(dir, Some(machine), root)
}

/// Drops a remote machine's whole list, for a machine that has been removed.
///
/// Answers whether it had one.
pub fn forget_machine(dir: &Path, machine: &str) -> Result<bool, ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        let had = saved.machines.remove(machine).is_some();
        Ok((had, had))
    })
}

/// Shared implementation for adding `root` to either this machine's or a remote machine's list.
///
/// `machine: None` means this machine's top-level list; `machine: Some(name)` means that remote
/// machine's table in the machines map. This one body serves both `remember` and `remember_on`.
fn remember_in(dir: &Path, machine: Option<&str>, root: &Path) -> Result<bool, ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        let list = saved.list_mut(machine);
        if list.iter().any(|kept| kept == root) {
            return Ok((false, false));
        }

        list.push(root.to_path_buf());
        Ok((true, true))
    })
}

/// Shared implementation for removing `root` from either this machine's or a remote machine's list.
///
/// `machine: None` means this machine's top-level list; `machine: Some(name)` means that remote
/// machine's table in the machines map. This one body serves both `forget` and `forget_on`.
fn forget_in(dir: &Path, machine: Option<&str>, root: &Path) -> Result<bool, ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        let list = saved.list_mut(machine);
        let before = list.len();
        list.retain(|kept| kept != root);

        let forgot = list.len() != before;
        Ok((forgot, forgot))
    })
}

#[cfg(test)]
mod tests;
