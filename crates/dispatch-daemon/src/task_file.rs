//! A task, written where a one-shot run reads it from its standard input.

use std::io::Write;
use std::path::{Path, PathBuf};

use dispatch_core::RequestId;

/// A task's file, removed when this is dropped.
///
/// Held by the pane running the task: the file lives exactly as long as
/// something might still read it, and a daemon that stops leaves none
/// behind.
#[derive(Debug)]
pub struct TaskFile {
    path: PathBuf,
}

impl TaskFile {
    /// Writes `task` to a new file in `dir` that only this user can read.
    pub fn write(dir: &Path, request: RequestId, task: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;

        let path = dir.join(format!("dispatch-task-{request}.txt"));
        let mut handle = dispatch_os::paths::create_private(&path)?;
        // Held only once the file is this call's own: a write that fails
        // part-way removes what it created, and a name that was already
        // taken is left to whoever took it.
        let file = Self { path };
        handle.write_all(task.as_bytes())?;
        handle.flush()?;

        Ok(file)
    }

    /// The value the child's environment carries in
    /// [`dispatch_config::TASK_FILE_ENV`].
    #[must_use]
    pub fn for_redirect(&self) -> String {
        dispatch_os::paths::redirect_operand(&self.path)
    }
}

impl Drop for TaskFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(%error, path = %self.path.display(), "failed to remove a task's file");
        }
    }
}
