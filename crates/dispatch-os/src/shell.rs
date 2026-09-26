//! The user's own shell, for a pane that runs one.
//!
//! Asked of the machine that runs the pane, so a project on another machine
//! gets that machine's shell and dotfiles rather than the client's.

/// The shell to run when the user has not named one.
///
/// `$SHELL` first, as a terminal emulator does. Then the login record, for a
/// daemon started by something that never set it. Then `/bin/sh`. On
/// Windows, PowerShell: `pwsh` if it is installed, the `powershell` every
/// Windows has if not.
#[must_use]
pub fn user_shell() -> String {
    imp::user_shell()
}

/// Whether a shell starts as a login shell when the user has not said.
///
/// macOS terminals start login shells and Linux ones do not, and each
/// platform's dotfiles are written for its own habit: that is where the rc
/// file that sets up a prompt like Starship gets read.
#[must_use]
pub fn login_by_default() -> bool {
    cfg!(target_os = "macos")
}

/// Whether this platform's shells take `-l`. PowerShell does not.
#[must_use]
pub fn takes_login_flag() -> bool {
    cfg!(unix)
}

/// The first of the environment's shell and the login record's that names
/// an executable file, or `/bin/sh`.
#[cfg(any(unix, test))]
fn pick(
    env_shell: Option<&str>,
    login_record: Option<&str>,
    executable: impl Fn(&std::path::Path) -> bool,
) -> String {
    [env_shell, login_record]
        .into_iter()
        .flatten()
        .find(|candidate| !candidate.is_empty() && executable(std::path::Path::new(candidate)))
        .map_or_else(|| "/bin/sh".to_string(), str::to_string)
}

/// PowerShell 7 when it is installed, else the Windows PowerShell every
/// Windows has.
#[cfg(any(windows, test))]
fn powershell(pwsh_installed: bool) -> String {
    if pwsh_installed { "pwsh" } else { "powershell" }.to_string()
}

#[cfg(unix)]
mod imp {
    use std::path::Path;

    pub(super) fn user_shell() -> String {
        let env_shell = std::env::var("SHELL").ok();
        let record = login_record();
        super::pick(env_shell.as_deref(), record.as_deref(), is_executable)
    }

    fn is_executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;

        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }

    /// The shell named in this user's password entry.
    fn login_record() -> Option<String> {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };

        let mut buf = vec![0 as libc::c_char; 4096];
        // SAFETY: an all-zero passwd is a valid value for getpwuid_r to
        // overwrite: every field is an integer or a pointer.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();

        // SAFETY: entry, buf and result are valid for the whole call, and
        // buf.len() is the buffer's true length. getpwuid_r writes the
        // entry's strings into buf and points result at entry on success.
        let status =
            unsafe { libc::getpwuid_r(uid, &mut entry, buf.as_mut_ptr(), buf.len(), &mut result) };
        if status != 0 || result.is_null() || entry.pw_shell.is_null() {
            return None;
        }

        // SAFETY: on success pw_shell points at a NUL-terminated string
        // inside buf, which is still alive here.
        let shell = unsafe { std::ffi::CStr::from_ptr(entry.pw_shell) };
        shell.to_str().ok().map(str::to_string)
    }
}

#[cfg(windows)]
mod imp {
    pub(super) fn user_shell() -> String {
        super::powershell(on_path("pwsh.exe"))
    }

    fn on_path(exe: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(exe).is_file()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_environments_shell_comes_first() {
        assert_eq!(
            pick(Some("/usr/bin/zsh"), Some("/bin/bash"), |_| true),
            "/usr/bin/zsh"
        );
    }

    #[test]
    fn a_shell_that_is_not_there_falls_to_the_login_record() {
        let there = |path: &Path| path == Path::new("/bin/bash");

        assert_eq!(
            pick(Some("/nope/zsh"), Some("/bin/bash"), there),
            "/bin/bash"
        );
        assert_eq!(pick(Some(""), Some("/bin/bash"), there), "/bin/bash");
    }

    #[test]
    fn with_nothing_usable_it_is_the_posix_shell() {
        assert_eq!(pick(None, None, |_| true), "/bin/sh");
        assert_eq!(pick(Some("/nope"), Some("/nope"), |_| false), "/bin/sh");
    }

    #[test]
    fn windows_prefers_powershell_seven_when_it_is_installed() {
        assert_eq!(powershell(true), "pwsh");
        assert_eq!(powershell(false), "powershell");
    }

    #[test]
    fn the_shell_found_here_can_be_started() {
        let shell = user_shell();

        assert!(!shell.is_empty());
        if cfg!(unix) {
            assert!(
                Path::new(&shell).is_file(),
                "{shell} is a file this machine has"
            );
        }
    }
}
