//! Starting a process that outlives its parent, and killing one with its
//! children.
//!
//! Agents start subprocesses -- language servers, test runners, build tools.
//! Killing only the process Dispatch spawned leaves those orphaned and still
//! holding the pane's file descriptors, so closing a pane has to terminate the
//! whole tree.

use std::time::Duration;

/// Failure to start or terminate a process.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// The process could not be started.
    #[error("failed to start {program}: {source}")]
    Spawn {
        /// What was being started.
        program: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The operating system refused the request.
    #[error("failed to terminate process tree for pid {pid}: {source}")]
    Terminate {
        /// Process whose tree was targeted.
        pid: u32,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

/// How long a tree is given to exit on its own before it is killed outright.
pub const DEFAULT_GRACE: Duration = Duration::from_millis(250);

/// Starts `program` detached from this process's terminal, and returns its pid.
///
/// Detached means two things, both needed by a daemon a client starts on demand:
/// it keeps running when the client exits, and a Ctrl-C in the terminal the
/// client was started from does not reach it. Without the second, quitting
/// Dispatch with Ctrl-C would take the agents with it, which is the thing the
/// daemon exists to prevent.
///
/// Its output goes nowhere: a daemon logs to a file, and anything it printed
/// would land in the middle of the client's interface.
pub fn spawn_detached(
    program: &std::path::Path,
    args: &[std::ffi::OsString],
) -> Result<u32, ProcessError> {
    imp::spawn_detached(program, args, &[])
}

/// Starts `program` detached, with `env` set on top of what it inherits.
///
/// The variable this exists for is `DISPATCH_CONFIG_DIR`: a daemon started
/// for an endpoint that is not the starter's own resolves that endpoint from
/// its configuration directory, so it has to be told which one. Set on the
/// child rather than on this process, because the parent is still using its
/// own configuration -- and because `set_var` is unsound beside other
/// threads, which every caller here has.
pub fn spawn_detached_with_env(
    program: &std::path::Path,
    args: &[std::ffi::OsString],
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<u32, ProcessError> {
    imp::spawn_detached(program, args, env)
}

/// Terminates `pid` and every process in its group or job.
///
/// Asks politely first, waits up to `grace`, then kills what is left. A tree
/// that has already exited is treated as success: the caller wants it gone,
/// and it is gone.
pub fn terminate_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
    imp::terminate_tree(pid, grace)
}

/// The working directory of whatever is in the foreground of the terminal
/// `pid` runs in.
///
/// For a shell running `cd wt && claude` that is Claude in `wt`, not the
/// shell: the foreground program is the one the user is looking at. Falls
/// back to `pid`'s own directory when the terminal's foreground cannot be
/// read, and is `None` where neither can -- a process that has exited, one
/// owned by another user, or a platform with no way to ask.
#[must_use]
pub fn working_dir(pid: u32) -> Option<std::path::PathBuf> {
    cwd::working_dir(pid)
}

#[cfg(unix)]
mod imp {
    use super::{Duration, ProcessError};

    pub(super) fn spawn_detached(
        program: &std::path::Path,
        args: &[std::ffi::OsString],
        env: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<u32, ProcessError> {
        use std::os::unix::process::CommandExt;

        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // SAFETY: setsid is async-signal-safe and is the documented way to
        // leave the parent's session and process group, which is what stops a
        // Ctrl-C in the parent's terminal from reaching this child. The closure
        // allocates nothing and touches no shared state.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        command
            .spawn()
            .map(|child| child.id())
            .map_err(|source| ProcessError::Spawn {
                program: program.display().to_string(),
                source,
            })
    }

    /// How long to wait for a tree to disappear after it has been killed
    /// outright.
    ///
    /// Only bounds the wait; a process that ignores SIGKILL is stuck in the
    /// kernel and no amount of waiting will change that.
    const KILL_TIMEOUT: Duration = Duration::from_secs(2);

    /// Sends `signal` to the process group led by `pid`.
    ///
    /// Returns `Ok(false)` when no such group exists, which means the tree has
    /// already exited.
    fn signal_group(pid: u32, signal: i32) -> Result<bool, std::io::Error> {
        // A pid that does not fit in pid_t cannot name a real process, and
        // negating it would address an unrelated group.
        let pid: libc::pid_t = pid
            .try_into()
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;

        // SAFETY: killpg takes a process group id and a signal number by
        // value and touches no memory owned by this process.
        let result = unsafe { libc::killpg(pid, signal) };
        if result == 0 {
            return Ok(true);
        }

        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            // No such process group: already gone.
            Some(libc::ESRCH) => Ok(false),
            _ => Err(error),
        }
    }

    /// Reports whether the process group led by `pid` still has members.
    fn group_is_alive(pid: u32) -> bool {
        matches!(signal_group(pid, 0), Ok(true))
    }

    /// Polls until the group has no members, or `timeout` elapses.
    ///
    /// Returns whether the group is gone.
    fn wait_for_group_to_exit(pid: u32, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if !group_is_alive(pid) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(super) fn terminate_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
        let map = |source| ProcessError::Terminate { pid, source };

        if !signal_group(pid, libc::SIGTERM).map_err(map)? {
            return Ok(());
        }

        // Poll rather than sleep the whole grace period: a well-behaved agent
        // exits in a few milliseconds and the caller is closing a pane, which
        // should feel immediate.
        if wait_for_group_to_exit(pid, grace) {
            return Ok(());
        }

        signal_group(pid, libc::SIGKILL).map_err(map)?;

        // SIGKILL is delivered asynchronously, so returning here would let the
        // caller observe a process that is dead but not yet torn down. Callers
        // close a pane expecting the tree to be gone, so wait for it.
        wait_for_group_to_exit(pid, KILL_TIMEOUT);

        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::{Duration, ProcessError};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};

    /// Starts the child in its own process group, with no console of its own.
    ///
    /// `CREATE_NEW_PROCESS_GROUP` is what keeps a Ctrl-C in the parent's console
    /// from reaching it, and `DETACHED_PROCESS` stops it inheriting that console
    /// at all — a daemon has no business writing to the interface's screen.
    const DETACHED: u32 = 0x0000_0008 | 0x0000_0200;

    pub(super) fn spawn_detached(
        program: &std::path::Path,
        args: &[std::ffi::OsString],
        env: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<u32, ProcessError> {
        use std::os::windows::process::CommandExt;

        std::process::Command::new(program)
            .args(args)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(DETACHED)
            .spawn()
            .map(|child| child.id())
            .map_err(|source| ProcessError::Spawn {
                program: program.display().to_string(),
                source,
            })
    }
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
        WaitForSingleObject,
    };

    /// Owns a process handle so it is closed on every exit path.
    struct ProcessHandle(HANDLE);

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            // SAFETY: self.0 came from OpenProcess and is closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn terminate_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
        // SAFETY: OpenProcess takes access flags and a pid by value.
        let raw = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_INFORMATION, 0, pid) };
        if raw.is_null() {
            // The process is already gone, which is what the caller wanted.
            return Ok(());
        }
        let handle = ProcessHandle(raw);

        // The child is assigned to a job object at spawn time, so terminating
        // it tears down everything it started. See `spawn` in this crate.
        //
        // SAFETY: handle.0 is a live process handle opened with
        // PROCESS_TERMINATE.
        let terminated = unsafe { TerminateProcess(handle.0, 1) };
        if terminated == 0 {
            return Err(ProcessError::Terminate {
                pid,
                source: std::io::Error::last_os_error(),
            });
        }

        let millis = u32::try_from(grace.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: handle.0 is a live process handle; waiting on it is always
        // defined and the timeout is passed by value.
        let waited = unsafe { WaitForSingleObject(handle.0, millis) };
        if waited != WAIT_OBJECT_0 {
            // Windows has no second, harder kill to escalate to; report what
            // the wait saw so the caller can log it.
            return Err(ProcessError::Terminate {
                pid,
                source: std::io::Error::last_os_error(),
            });
        }

        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod cwd {
    use std::path::PathBuf;

    pub(super) fn working_dir(pid: u32) -> Option<PathBuf> {
        foreground(pid)
            .and_then(|leader| std::fs::read_link(format!("/proc/{leader}/cwd")).ok())
            .or_else(|| std::fs::read_link(format!("/proc/{pid}/cwd")).ok())
    }

    /// The foreground process group of the terminal `pid` runs in.
    fn foreground(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        tpgid(&stat)
    }

    /// Field 8 of a `/proc/<pid>/stat` line: the foreground process group of
    /// the process's terminal.
    ///
    /// The command name is field 2, in parentheses, and may itself hold
    /// spaces or parentheses, so fields are counted from the last `)` rather
    /// than from the start of the line.
    pub(super) fn tpgid(stat: &str) -> Option<u32> {
        let rest = &stat[stat.rfind(')')? + 1..];

        // After the name: state, ppid, pgrp, session, tty_nr, tpgid.
        let field: i64 = rest.split_whitespace().nth(5)?.parse().ok()?;

        // -1 is "no controlling terminal".
        u32::try_from(field).ok().filter(|group| *group > 0)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_foreground_group_is_read_past_a_name_with_spaces_and_parentheses() {
            // The command name is free text in parentheses, so counting
            // fields from the start of the line breaks on the first name
            // with a space in it.
            let stat = "4242 (a (weird) name) S 1 4242 4242 34817 5151 4194560 0 0";

            assert_eq!(tpgid(stat), Some(5151));
        }

        #[test]
        fn a_process_with_no_terminal_has_no_foreground_group() {
            let stat = "4242 (daemon) S 1 4242 4242 0 -1 4194560 0 0";

            assert_eq!(tpgid(stat), None);
        }

        #[test]
        fn a_process_with_no_terminal_reports_its_own_directory() {
            use std::os::unix::process::CommandExt;

            let dir = std::env::temp_dir().join(format!("dispatch-os-cwd-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir is writable");
            let dir = dir.canonicalize().expect("the temp dir resolves");

            let mut command = std::process::Command::new("sleep");
            command.arg("30").current_dir(&dir);
            // SAFETY: setsid is async-signal-safe and the closure allocates
            // nothing. It detaches the child from the test runner's terminal,
            // whose foreground job would otherwise be the answer.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().expect("sleep starts");

            let found = working_dir(child.id());

            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);

            assert_eq!(found, Some(dir));
        }

        #[test]
        fn a_process_that_does_not_exist_has_no_directory() {
            assert_eq!(working_dir(u32::MAX), None);
        }
    }
}

#[cfg(target_os = "macos")]
mod cwd {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    pub(super) fn working_dir(pid: u32) -> Option<PathBuf> {
        foreground(pid)
            .and_then(directory_of)
            .or_else(|| directory_of(pid))
    }

    /// The foreground process group of the terminal `pid` runs in.
    fn foreground(pid: u32) -> Option<u32> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;

        // SAFETY: `proc_bsdinfo` is plain data, and all zeroes is a valid
        // value of it.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };

        // SAFETY: the buffer is `info`, owned by this frame, and `size` is
        // its exact length.
        let read = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };

        (read == size && info.e_tpgid > 0).then_some(info.e_tpgid)
    }

    /// `pid`'s own working directory.
    fn directory_of(pid: u32) -> Option<PathBuf> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;

        // SAFETY: `proc_vnodepathinfo` is plain data, and all zeroes is a
        // valid value of it.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };

        // SAFETY: the buffer is `info`, owned by this frame, and `size` is
        // its exact length.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        if read != size {
            return None;
        }

        // `vip_path` is a MAXPATHLEN buffer, which libc spells as 32 rows of
        // 32 to stay within what an old compiler could derive traits for.
        let bytes: Vec<u8> = info
            .pvi_cdir
            .vip_path
            .as_flattened()
            .iter()
            .map(|&byte| byte as u8)
            .collect();
        let path = CStr::from_bytes_until_nul(&bytes).ok()?;

        (!path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path.to_bytes())))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod cwd {
    /// Windows has no terminal foreground to ask about, and nothing else
    /// Dispatch runs on is supported.
    pub(super) fn working_dir(_pid: u32) -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A [`Launch`]-free command that sleeps, spelled per platform.
    fn sleeper() -> (std::path::PathBuf, Vec<std::ffi::OsString>) {
        if cfg!(windows) {
            (
                std::path::PathBuf::from("cmd.exe"),
                vec!["/c".into(), "timeout /t 3 /nobreak".into()],
            )
        } else {
            (
                std::path::PathBuf::from("/bin/sh"),
                vec!["-c".into(), "sleep 3".into()],
            )
        }
    }

    #[test]
    fn a_detached_child_starts() {
        let (program, args) = sleeper();
        let pid = spawn_detached(&program, &args).expect("the child starts");

        assert!(pid > 0);
        terminate_tree(pid, DEFAULT_GRACE).expect("the child can be killed");
    }

    #[test]
    fn a_detached_child_is_given_the_environment_it_was_started_with() {
        // How a bridge tells the daemon it starts which configuration
        // directory to listen under. Proven by the child reporting the
        // variable back, because a variable the parent set on itself would
        // read the same either way.
        let dir = std::env::temp_dir().join(format!("dispatch-os-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        let answer = dir.join("answer");

        let pid = spawn_detached_with_env(
            &std::path::PathBuf::from("/bin/sh"),
            &[
                "-c".into(),
                format!("printf '%s' \"$DISPATCH_TEST_DIR\" > {}", answer.display()).into(),
            ],
            &[("DISPATCH_TEST_DIR".into(), "/somewhere/else".into())],
        )
        .expect("the child starts");
        assert!(pid > 0);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(contents) = std::fs::read_to_string(&answer)
                && !contents.is_empty()
            {
                assert_eq!(contents, "/somewhere/else");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the child never reported its environment"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starting_something_that_is_not_there_says_so() {
        let missing = std::path::PathBuf::from("dispatch-no-such-program");
        let error = spawn_detached(&missing, &[]).expect_err("there is no such program");

        assert!(
            matches!(error, ProcessError::Spawn { .. }),
            "expected a spawn failure, got {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_detached_child_leaves_this_process_group() {
        // The point of detaching: a Ctrl-C in the terminal that started the
        // client reaches the client's group, and the daemon must not be in it.
        let (program, args) = sleeper();
        let pid = spawn_detached(&program, &args).expect("the child starts");

        let group = std::process::Command::new("ps")
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .expect("ps runs");
        let group: i32 = String::from_utf8_lossy(&group.stdout)
            .trim()
            .parse()
            .expect("ps reports a process group");

        // SAFETY: getpgrp takes no arguments and only reads this process's own
        // group.
        let ours = unsafe { libc::getpgrp() };
        assert_ne!(group, ours, "the child should lead a group of its own");
        assert_eq!(
            group,
            i32::try_from(pid).expect("a pid fits"),
            "and it should be its own leader"
        );

        terminate_tree(pid, DEFAULT_GRACE).expect("the child can be killed");
    }

    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    /// Reports whether `pid` names a live process.
    fn pid_is_alive(pid: u32) -> bool {
        let pid = pid as libc::pid_t;
        // SAFETY: signal 0 performs error checking without sending a signal
        // and touches no memory owned by this process.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Spawns a shell that is its own session leader and forks a grandchild.
    ///
    /// Returns the shell's [`Child`] plus the grandchild's pid. The grandchild
    /// is what makes this test worth having: killing the shell alone would
    /// leave it running.
    fn spawn_tree_with_grandchild(pid_file: &std::path::Path) -> (std::process::Child, u32) {
        let script = format!("sleep 30 & echo $! > {}; sleep 30", pid_file.display());

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        // SAFETY: setsid is async-signal-safe and the closure allocates
        // nothing, which is the requirement pre_exec imposes.
        unsafe {
            command.pre_exec(|| {
                // Detaches into a new session, making the child a process
                // group leader so its pid names the whole group.
                libc::setsid();
                Ok(())
            });
        }

        let mut child = command.spawn().expect("sh is available");

        // Wait for the shell to record the grandchild's pid.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Ok(contents) = std::fs::read_to_string(pid_file)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                return (child, pid);
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Do not leak the shell into the rest of the suite just because the
        // setup step failed.
        let _ = child.kill();
        let _ = child.wait();
        panic!("the shell never wrote the grandchild pid");
    }

    #[test]
    fn terminating_a_tree_kills_the_grandchild_too() {
        let dir = std::env::temp_dir().join(format!("dispatch-os-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        let pid_file = dir.join("grandchild.pid");

        let (mut child, grandchild) = spawn_tree_with_grandchild(&pid_file);
        let child_pid = child.id();
        assert!(pid_is_alive(child_pid), "the shell should be running");
        assert!(pid_is_alive(grandchild), "the grandchild should be running");

        terminate_tree(child_pid, DEFAULT_GRACE).expect("terminating the tree succeeds");

        // Reap the shell so it does not linger as a zombie and report alive.
        child.wait().expect("the shell can be reaped");

        assert!(
            !pid_is_alive(grandchild),
            "the grandchild outlived its process group"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terminating_an_exited_tree_succeeds() {
        let mut child = Command::new("true").spawn().expect("true is available");
        let pid = child.id();

        // Reaping the child before asking for its group to be killed is what
        // makes this the "already gone" case.
        child.wait().expect("true can be reaped");

        terminate_tree(pid, DEFAULT_GRACE).expect("a tree that has already exited is not an error");
    }
}
