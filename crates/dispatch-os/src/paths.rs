//! Where Dispatch keeps its configuration, data, and logs on each platform.

use std::path::PathBuf;

use directories::ProjectDirs;

/// Failure to locate a platform directory.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The platform has no home directory to anchor the paths to.
    #[error(
        "could not determine a home directory for the current user; \
         Dispatch needs one to locate its configuration"
    )]
    NoHomeDirectory,
}

/// Resolves Dispatch's platform directories.
///
/// On Linux this follows the XDG base directory spec (`~/.config/dispatch`),
/// on macOS `~/Library/Application Support`, and on Windows `%APPDATA%`.
fn project_dirs() -> Result<ProjectDirs, PathError> {
    ProjectDirs::from("", "", "dispatch").ok_or(PathError::NoHomeDirectory)
}

/// Directory holding `config.toml`.
pub fn config_dir() -> Result<PathBuf, PathError> {
    Ok(project_dirs()?.config_dir().to_path_buf())
}

/// Path to the top-level configuration file.
pub fn config_file() -> Result<PathBuf, PathError> {
    Ok(config_dir()?.join("config.toml"))
}

/// Directory holding one TOML file per registered harness.
pub fn harnesses_dir() -> Result<PathBuf, PathError> {
    Ok(config_dir()?.join("harnesses"))
}

/// Path to the log file.
///
/// A TUI owns the screen, so diagnostics cannot go to stdout or stderr.
pub fn log_file() -> Result<PathBuf, PathError> {
    Ok(project_dirs()?.data_dir().join("dispatch.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_nest_under_the_config_directory() {
        let dir = config_dir().expect("a home directory exists in the test environment");

        assert!(
            config_file()
                .expect("config_file resolves when config_dir does")
                .starts_with(&dir)
        );
        assert!(
            harnesses_dir()
                .expect("harnesses_dir resolves when config_dir does")
                .starts_with(&dir)
        );
    }

    #[test]
    fn paths_are_absolute() {
        for path in [
            config_dir().expect("config_dir resolves"),
            config_file().expect("config_file resolves"),
            harnesses_dir().expect("harnesses_dir resolves"),
            log_file().expect("log_file resolves"),
        ] {
            assert!(path.is_absolute(), "{} is not absolute", path.display());
        }
    }

    #[test]
    fn config_and_log_are_distinct_locations() {
        let config = config_file().expect("config_file resolves");
        let log = log_file().expect("log_file resolves");
        assert_ne!(config, log);
    }
}
