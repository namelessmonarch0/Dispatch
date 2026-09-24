//! A task, written where a one-shot run reads it from its standard input.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dispatch_core::RequestId;

/// How every task file's name begins; the request's id follows.
const PREFIX: &str = "dispatch-task-";

/// How every task file's name ends.
const SUFFIX: &str = ".txt";

/// A task's file, removed when this is dropped.
///
/// Held by the pane running the task: the file lives exactly as long as
/// something might still read it, and a daemon that stops leaves none
/// behind. One that cannot be removed then -- on Windows, while a process
/// being killed still holds it open -- goes to its [`Leftovers`] to be tried
/// again.
#[derive(Debug)]
pub struct TaskFile {
    path: PathBuf,
    leftovers: Leftovers,
}

/// Task files whose removal failed, to be tried again.
///
/// Shared by every [`TaskFile`] a daemon writes, which puts itself here when
/// dropping it could not remove it; the daemon tries them again while it
/// runs, and once more on its way out.
#[derive(Debug, Clone, Default)]
pub struct Leftovers(Arc<Mutex<Vec<PathBuf>>>);

impl Leftovers {
    /// Whether nothing is waiting to be removed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Tries to remove each once more, keeping those that still refuse.
    pub fn retry(&self) {
        self.lock().retain(|path| match std::fs::remove_file(path) {
            Ok(()) => {
                tracing::info!(path = %path.display(), "removed a task's file on a later try");
                false
            }
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        });
    }

    /// Says which are still there, for a daemon that is stopping: the next
    /// to serve this configuration sweeps them.
    pub fn report(&self) {
        for path in self.lock().iter() {
            tracing::warn!(
                path = %path.display(),
                "a task's file is left behind; the next daemon to start removes it"
            );
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<PathBuf>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl TaskFile {
    /// Writes `task` to a new file in `dir` that only this user can read.
    ///
    /// Should it outlive its removal, it is handed to `leftovers`.
    pub fn write(
        dir: &Path,
        request: RequestId,
        task: &str,
        leftovers: &Leftovers,
    ) -> std::io::Result<Self> {
        dispatch_os::paths::create_private_dir(dir)?;

        let path = dir.join(format!("{PREFIX}{request}{SUFFIX}"));
        let mut handle = dispatch_os::paths::create_private(&path)?;
        // Held only once the file is this call's own: a write that fails
        // part-way removes what it created, and a name that was already
        // taken is left to whoever took it.
        let file = Self {
            path,
            leftovers: leftovers.clone(),
        };
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
            tracing::warn!(
                %error,
                path = %self.path.display(),
                "failed to remove a task's file; trying again later"
            );
            self.leftovers.lock().push(std::mem::take(&mut self.path));
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
    // What a link names is somebody's choice, not Dispatch's directory.
    if std::fs::symlink_metadata(dir).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        tracing::warn!(dir = %dir.display(), "not sweeping task files through a link");
        return;
    }

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
