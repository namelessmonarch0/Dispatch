//! A task, written where a one-shot run reads it from its standard input.

use std::io::Write;
use std::path::{Path, PathBuf};

use dispatch_core::RequestId;

/// How every task file's name begins; the request's id follows.
const PREFIX: &str = "dispatch-task-";

/// How every task file's name ends.
const SUFFIX: &str = ".txt";

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
        dispatch_os::paths::create_private_dir(dir)?;

        let path = dir.join(format!("{PREFIX}{request}{SUFFIX}"));
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

/// Removes every task file left in `dir` by a daemon that stopped without
/// cleaning up after itself -- one that was killed, or crashed -- and
/// nothing else.
///
/// For a daemon that has just bound its endpoint: that is proof no other
/// daemon serves this configuration, so no running one still means to hand
/// these to anybody. Only names exactly as [`TaskFile::write`] makes them
/// are touched, so a file of the user's that happens to be there is not.
pub fn sweep(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(%error, dir = %dir.display(), "could not look for task files left behind");
            return;
        }
    };

    for entry in entries.flatten() {
        let is_task_file = entry.file_type().is_ok_and(|kind| kind.is_file())
            && entry.file_name().to_str().is_some_and(is_task_file_name);
        if !is_task_file {
            continue;
        }

        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::info!(path = %path.display(), "removed a task file left behind"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                %error,
                path = %path.display(),
                "could not remove a task file left behind"
            ),
        }
    }
}

/// Whether `name` is exactly what [`TaskFile::write`] names a file.
fn is_task_file_name(name: &str) -> bool {
    name.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
        .is_some_and(|id| {
            id.parse::<RequestId>()
                .is_ok_and(|request| request.to_string() == id)
        })
}
