//! Changing a small TOML file that more than one process changes.
//!
//! Every running Dispatch writes `projects.toml` and `machines.toml` back as
//! the user works. Two of them each reading the file, adding one entry and
//! writing the whole file back would keep only the second entry; a write cut
//! short would leave half a file the next start cannot parse; and a reader
//! could land on the half. Every change goes through [`update`], which does
//! none of those.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::ConfigError;

/// Reads `file` in `dir`. No file is the default value: a first run.
pub(crate) fn read<T: Default + DeserializeOwned>(
    dir: &Path,
    file: &str,
) -> Result<T, ConfigError> {
    let path = dir.join(file);

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(source) => return Err(ConfigError::Io { path, source }),
    };

    toml::from_str(&text).map_err(|source| ConfigError::Toml { path, source })
}

/// Applies `change` to `file`'s current contents and writes the result back,
/// as one step no other `update` of the same file can come between.
///
/// `change` returns what the caller wants back, and whether it changed
/// anything; nothing changed, nothing is written. The file is replaced, never
/// rewritten in place, so a reader sees the old contents or the new, never a
/// part of either, and a write that fails leaves the old contents where they
/// were.
pub(crate) fn update<T, R>(
    dir: &Path,
    file: &str,
    change: impl FnOnce(&mut T) -> Result<(R, bool), ConfigError>,
) -> Result<R, ConfigError>
where
    T: Default + Serialize + DeserializeOwned,
{
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let _held = lock(dir, file)?;

    let mut value: T = read(dir, file)?;
    let (answer, changed) = change(&mut value)?;

    if changed {
        let text = toml::to_string_pretty(&value).expect("a registry serialises");
        replace(&dir.join(file), &text)?;
    }

    Ok(answer)
}

/// Takes `file`'s lock, held until the returned file is dropped.
///
/// A file of its own beside the one it guards: on Windows a lock is
/// mandatory, and one on the registry itself would stop the readers, which
/// take no lock, from reading it.
fn lock(dir: &Path, file: &str) -> Result<std::fs::File, ConfigError> {
    let path = dir.join(format!("{file}.lock"));

    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;

    lock.lock()
        .map_err(|source| ConfigError::Io { path, source })?;
    Ok(lock)
}

/// Writes `text` beside `path`, then moves it into place.
///
/// Staged under a fixed name, which is safe because only the holder of the
/// lock writes it.
fn replace(path: &Path, text: &str) -> Result<(), ConfigError> {
    let staged = path.with_extension("toml.tmp");

    let written = (|| {
        let mut file = std::fs::File::create(&staged)?;
        file.write_all(text.as_bytes())?;
        // On disk before the rename: a rename that reached the disk first
        // would, after a crash, leave the name pointing at nothing.
        file.sync_all()
    })();

    if let Err(source) = written {
        let _ = std::fs::remove_file(&staged);
        return Err(ConfigError::Io {
            path: staged,
            source,
        });
    }

    std::fs::rename(&staged, path).map_err(|source| {
        let _ = std::fs::remove_file(&staged);
        ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}
