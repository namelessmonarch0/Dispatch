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

/// Environment variable that overrides where configuration is read from.
///
/// Useful for a portable install, for running two configurations side by
/// side, and for tests, which must not read or write the developer's own
/// harnesses. It is an explicit variable rather than XDG_CONFIG_HOME because
/// that is ignored on macOS and Windows.
pub const CONFIG_DIR_ENV: &str = "DISPATCH_CONFIG_DIR";

/// Directory holding `config.toml`.
pub fn config_dir() -> Result<PathBuf, PathError> {
    if let Some(path) = std::env::var_os(CONFIG_DIR_ENV)
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }

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
///
/// Follows [`CONFIG_DIR_ENV`] when it is set, so a redirected configuration
/// keeps its logs with it rather than writing into the real data directory.
pub fn log_file() -> Result<PathBuf, PathError> {
    if let Some(path) = std::env::var_os(CONFIG_DIR_ENV)
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path).join("dispatch.log"));
    }

    Ok(project_dirs()?.data_dir().join("dispatch.log"))
}

/// Path to the daemon's log file.
///
/// Separate from [`log_file`] because the client and the daemon are two
/// processes: appending both to one file interleaves their lines, and the point
/// of reading a daemon log is usually to find out what it did while no client
/// was watching.
pub fn daemon_log_file() -> Result<PathBuf, PathError> {
    if let Some(path) = std::env::var_os(CONFIG_DIR_ENV)
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path).join("dispatchd.log"));
    }

    Ok(project_dirs()?.data_dir().join("dispatchd.log"))
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
            daemon_log_file().expect("daemon_log_file resolves"),
        ] {
            assert!(path.is_absolute(), "{} is not absolute", path.display());
        }
    }

    #[test]
    fn the_override_variable_is_named_for_dispatch() {
        // XDG_CONFIG_HOME would not do: macOS and Windows ignore it, so a
        // redirected configuration has to have its own variable.
        assert_eq!(CONFIG_DIR_ENV, "DISPATCH_CONFIG_DIR");
    }

    #[test]
    fn the_client_and_the_daemon_log_to_different_files() {
        // Two processes appending to one file interleave their lines.
        let client = log_file().expect("log_file resolves");
        let daemon = daemon_log_file().expect("daemon_log_file resolves");
        assert_ne!(client, daemon);
    }

    #[test]
    fn config_and_log_are_distinct_locations() {
        let config = config_file().expect("config_file resolves");
        let log = log_file().expect("log_file resolves");
        assert_ne!(config, log);
    }
}
