//! Where Dispatch keeps its configuration, data, and logs on each platform.

use std::path::{Path, PathBuf};

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

/// Path to the file holding the running daemon's process id.
///
/// Written by the daemon while it is listening. The endpoint says whether a
/// daemon is answering; this says which process to stop.
pub fn daemon_pid_file() -> Result<PathBuf, PathError> {
    Ok(config_dir()?.join("dispatchd.pid"))
}

/// Resolves `path` into a form a child process can be started in.
///
/// `Path::canonicalize` on Windows returns an extended-length path — `\\?\C:\…`
/// — and plenty of programs will not accept one as a working directory.
/// `cmd.exe` prints "UNC paths are not supported. Defaulting to Windows
/// directory." and starts somewhere else entirely, so an agent launched in a
/// project would run outside it. Every project root goes through here.
pub fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    let resolved = path.canonicalize()?;

    match resolved.to_str().and_then(shorten_windows_path) {
        Some(shortened) => Ok(PathBuf::from(shortened)),
        None => Ok(resolved),
    }
}

/// Expands a leading `~` against the home directory of the user running this
/// process.
///
/// A path typed by hand for another machine almost always starts with one,
/// and no shell stands between the client and the daemon to expand it:
/// without this the daemon looks for a directory literally named `~` in
/// whatever directory it was started in. Only `~` and `~/…` are expanded;
/// `~user` is left as typed, because another user's home is not this
/// process's to guess.
pub fn expand_home(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };

    match directories::BaseDirs::new() {
        Some(dirs) => dirs.home_dir().join(rest),
        None => path.to_path_buf(),
    }
}

/// Longest path a Windows program can be expected to handle unprefixed.
const MAX_PATH: usize = 260;

/// Removes the extended-length prefix from a Windows path, where it can go.
///
/// Returns `None` for a path that has to keep its prefix, and for anything that
/// never had one — including every Unix path. Not behind a `#[cfg]`, because
/// the rules are fiddly enough to want testing on the platform the tests
/// actually run on.
fn shorten_windows_path(text: &str) -> Option<String> {
    let shortened = if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        // `\\?\UNC\server\share` is the verbatim spelling of `\\server\share`.
        format!(r"\\{share}")
    } else {
        let rest = text.strip_prefix(r"\\?\")?;

        // Only a drive path can simply lose the prefix. Anything else — a
        // device path, say — means something different without it.
        let mut characters = rest.chars();
        if !characters.next()?.is_ascii_alphabetic() || characters.next() != Some(':') {
            return None;
        }

        rest.to_string()
    };

    // Past this length the prefix is the only thing making the path usable at
    // all, so a program that dislikes the spelling is the lesser problem.
    (shortened.len() < MAX_PATH).then_some(shortened)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_nest_under_the_config_directory() {
        let _guard = crate::env_lock();
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
        let _guard = crate::env_lock();
        for path in [
            config_dir().expect("config_dir resolves"),
            config_file().expect("config_file resolves"),
            harnesses_dir().expect("harnesses_dir resolves"),
            log_file().expect("log_file resolves"),
            daemon_log_file().expect("daemon_log_file resolves"),
            daemon_pid_file().expect("daemon_pid_file resolves"),
        ] {
            assert!(path.is_absolute(), "{} is not absolute", path.display());
        }
    }

    #[test]
    fn a_windows_drive_path_loses_its_extended_length_prefix() {
        // The reason this exists: cmd.exe refuses one as a working directory,
        // and silently starts in the Windows directory instead.
        assert_eq!(
            shorten_windows_path(r"\\?\C:\Users\runneradmin\project").as_deref(),
            Some(r"C:\Users\runneradmin\project")
        );
    }

    #[test]
    fn a_windows_share_is_written_the_way_programs_expect() {
        assert_eq!(
            shorten_windows_path(r"\\?\UNC\build\share\project").as_deref(),
            Some(r"\\build\share\project")
        );
    }

    #[test]
    fn a_path_too_long_to_work_unprefixed_keeps_its_prefix() {
        // Removing it would turn a usable path into an unusable one, which is
        // worse than a program that dislikes the spelling.
        let long = format!(r"\\?\C:\{}", "d".repeat(MAX_PATH));
        assert_eq!(shorten_windows_path(&long), None);
    }

    #[test]
    fn a_verbatim_path_that_is_not_a_drive_keeps_its_prefix() {
        // `\\?\pipe\name` is not `pipe\name`.
        assert_eq!(shorten_windows_path(r"\\?\pipe\dispatchd"), None);
    }

    #[test]
    fn a_path_with_no_prefix_is_left_alone() {
        assert_eq!(shorten_windows_path("/Users/someone/project"), None);
        assert_eq!(shorten_windows_path(r"C:\Users\someone\project"), None);
    }

    #[test]
    fn the_override_variable_is_named_for_dispatch() {
        // XDG_CONFIG_HOME would not do: macOS and Windows ignore it, so a
        // redirected configuration has to have its own variable.
        assert_eq!(CONFIG_DIR_ENV, "DISPATCH_CONFIG_DIR");
    }

    #[test]
    fn the_client_and_the_daemon_log_to_different_files() {
        let _guard = crate::env_lock();
        // Two processes appending to one file interleave their lines.
        let client = log_file().expect("log_file resolves");
        let daemon = daemon_log_file().expect("daemon_log_file resolves");
        assert_ne!(client, daemon);
    }

    #[test]
    fn config_and_log_are_distinct_locations() {
        let _guard = crate::env_lock();
        let config = config_file().expect("config_file resolves");
        let log = log_file().expect("log_file resolves");
        assert_ne!(config, log);
    }

    #[test]
    fn a_leading_tilde_is_the_home_directory() {
        // A path typed by hand for another machine starts with `~` more
        // often than not, and no shell stands between the client and the
        // daemon to expand it.
        let home = directories::BaseDirs::new()
            .expect("a home directory exists in the test environment")
            .home_dir()
            .to_path_buf();

        assert_eq!(expand_home(Path::new("~/code/app")), home.join("code/app"));
        assert_eq!(expand_home(Path::new("~")), home.join(""));
        assert_eq!(
            expand_home(Path::new("/srv/app")),
            PathBuf::from("/srv/app"),
            "an absolute path is left alone"
        );
        assert_eq!(
            expand_home(Path::new("~someone/app")),
            PathBuf::from("~someone/app"),
            "another user's home is not ours to guess"
        );
    }
}
