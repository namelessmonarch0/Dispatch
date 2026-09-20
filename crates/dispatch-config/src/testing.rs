//! A temporary directory, shared by this crate's test modules.
//!
//! One type with one counter, deliberately: two modules each keeping a
//! `static NEXT` of their own started both of their counters at zero, so a
//! label used in both — `missing` was used in both — handed out the same path
//! twice and one test's `remove_dir_all` raced the other's `create_dir_all`.
//! The counter is what keeps parallel tests apart, and there can only be one of
//! it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A temporary directory that cleans itself up.
pub struct TempDir(PathBuf);

impl TempDir {
    /// Creates a directory nothing else in this process will be handed.
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let path = std::env::temp_dir().join(format!(
            "dispatch-config-{}-{label}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir is writable");
        Self(path)
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `name` into the directory and returns its path.
    pub fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, contents).expect("temp dir is writable");
        path
    }

    /// Writes a `config.toml` into the directory and returns its path.
    pub fn config(&self, contents: &str) -> PathBuf {
        self.write("config.toml", contents)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
