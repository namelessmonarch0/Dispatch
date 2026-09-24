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
/// On Unix it asks politely first, waits up to `grace`, then kills what is
/// left. On Windows it is ended at once, because no polite request reaches a
/// tree of console programs reliably; there `grace` can only lengthen the
/// wait for a process [`spawn_contained`] did not start to be gone. A tree
/// that has already exited is treated as success: the caller wants it gone,
/// and it is gone.
pub fn terminate_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
    imp::terminate_tree(pid, grace)
}

/// Asks `pid`'s group or job to stop, and kills what is left after `grace`,
/// without waiting afterwards for it to be gone.
///
/// On Linux a killed leader nobody has waited for is still a member of its
/// group, so waiting for the group to vanish before the leader is reaped
/// waits out the whole of [`terminate_tree`]'s kill timeout on a zombie.
/// This is for the callers that reap, or hold what reaping needs. A command
/// transport's closer signals under the lock that keeps its child's pid from
/// being freed, which the reaper needs too; the reaper signals, waits for
/// the leader, and only then waits for the rest with [`wait_for_tree`].
pub(crate) fn signal_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
    imp::signal_tree(pid, grace)
}

/// Waits, for at most the kill timeout, until nothing is left of the group
/// or job `pid` led, once `pid` itself has been signalled and reaped.
///
/// Sends nothing: it only asks. Once the group empties, its id is free for
/// the system to give away, so a signal from here could reach a stranger.
pub(crate) fn wait_for_tree(pid: u32) {
    imp::wait_for_tree(pid);
}

/// Starts `command` so that [`terminate_tree`] reaches everything it starts.
///
/// On Unix the child leads a session of its own, so its pid names a process
/// group every descendant stays in unless it leaves on purpose. On Windows it
/// is created suspended, put in a Job Object of its own, and only then
/// resumed: it cannot start anything outside the job, because it starts
/// nothing before it is in it. Any creation flags already set on `command`
/// are replaced on Windows.
pub fn spawn_contained(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    imp::spawn_contained(command)
}

/// Whether `pid` names a process that has not exited. For tests.
///
/// On Unix a process that has exited but not been reaped still answers, so
/// a test polls rather than asking once.
#[doc(hidden)]
#[must_use]
pub fn is_running(pid: u32) -> bool {
    imp::is_running(pid)
}

/// Every process descended from `pid` now, nearest first. For tests.
#[doc(hidden)]
#[must_use]
pub fn descendants(pid: u32) -> Vec<u32> {
    imp::descendants(pid)
}

/// Puts a process that has been created suspended in a job of its own, and
/// records it for [`terminate_tree`]. The caller resumes it.
///
/// For a process something other than [`spawn_contained`] creates: a pane's,
/// started with `CreateProcessW` to attach a pseudoconsole, which a `Command`
/// cannot do.
#[cfg(windows)]
#[expect(
    unused_imports,
    reason = "made for the pane backend, which creates its processes itself and does not call this yet"
)]
pub(crate) use imp::contain;

/// The pids reachable downward from `root` through `(pid, parent)` pairs,
/// breadth first.
fn below(root: u32, pairs: &[(u32, u32)]) -> Vec<u32> {
    let mut found: Vec<u32> = Vec::new();
    let mut parent = root;
    let mut next = 0;
    loop {
        for &(pid, of) in pairs {
            if of == parent && pid != root && !found.contains(&pid) {
                found.push(pid);
            }
        }
        let Some(&deeper) = found.get(next) else {
            return found;
        };
        parent = deeper;
        next += 1;
    }
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
        signal_tree(pid, grace)?;

        // SIGKILL is delivered asynchronously, so returning here would let the
        // caller observe a process that is dead but not yet torn down. Callers
        // close a pane expecting the tree to be gone, so wait for it. A tree
        // that went within its grace is already gone, and this returns at once.
        wait_for_group_to_exit(pid, KILL_TIMEOUT);

        Ok(())
    }

    pub(super) fn signal_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
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
        Ok(())
    }

    pub(super) fn wait_for_tree(pid: u32) {
        wait_for_group_to_exit(pid, KILL_TIMEOUT);
    }

    pub(super) fn spawn_contained(
        command: &mut std::process::Command,
    ) -> std::io::Result<std::process::Child> {
        use std::os::unix::process::CommandExt;

        // SAFETY: setsid is async-signal-safe and is the documented way to
        // leave the parent's session and become a process group leader,
        // which is what lets `killpg` reach every descendant later. The
        // closure allocates nothing and touches no shared state.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        command.spawn()
    }

    pub(super) fn is_running(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 sends nothing and only asks whether the pid exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    pub(super) fn descendants(pid: u32) -> Vec<u32> {
        // `ps` rather than /proc, so macOS and Linux answer the same way.
        let Ok(output) = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,ppid="])
            .output()
        else {
            return Vec::new();
        };

        let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .collect();

        super::below(pid, &pairs)
    }
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::os::windows::io::AsRawHandle;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    use super::{Duration, ProcessError};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, OpenProcess, OpenThread, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, ResumeThread, THREAD_SUSPEND_RESUME, TerminateProcess,
        WaitForSingleObject,
    };

    /// Starts the child in its own process group, with no console of its own.
    ///
    /// `CREATE_NEW_PROCESS_GROUP` is what keeps a Ctrl-C in the parent's console
    /// from reaching it, and `DETACHED_PROCESS` stops it inheriting that console
    /// at all — a daemon has no business writing to the interface's screen.
    const DETACHED: u32 = 0x0000_0008 | 0x0000_0200;

    /// How long to wait for a tree to be gone once it has been ended.
    ///
    /// Only bounds the wait: a process that outlives `TerminateJobObject` is
    /// stuck in the kernel, and no amount of waiting will change that.
    const KILL_TIMEOUT: Duration = Duration::from_secs(2);

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

    /// Owns a handle so it is closed on every exit path.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle is owned here and closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// A Job Object whose processes are ended when it is closed.
    ///
    /// Closing is how a daemon that dies takes its panes with it -- as a
    /// clean shutdown would -- instead of leaving agents nobody can reach.
    struct Job(Owned);

    // SAFETY: a job handle may be used from any thread; this one is only
    // closed on drop.
    unsafe impl Send for Job {}

    impl Job {
        fn new() -> std::io::Result<Self> {
            // SAFETY: both arguments may be null: default security, no name.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let job = Self(Owned(handle));

            // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION means
            // no limits; one flag is then set.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is the structure this class names, and lives
            // across the call.
            let set = unsafe {
                SetInformationJobObject(
                    job.0.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                        .expect("a small struct"),
                )
            };
            if set == 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(job)
        }

        fn assign(&self, process: HANDLE) -> std::io::Result<()> {
            // SAFETY: both handles are live for the call.
            if unsafe { AssignProcessToJobObject(self.0.0, process) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        fn terminate(&self) -> std::io::Result<()> {
            // SAFETY: a live job handle.
            if unsafe { TerminateJobObject(self.0.0, 1) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        /// How many processes in the job have not exited.
        fn active(&self) -> std::io::Result<u32> {
            // SAFETY: an all-zero accounting structure is valid to fill.
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is the structure this class names.
            let queried = unsafe {
                QueryInformationJobObject(
                    self.0.0,
                    JobObjectBasicAccountingInformation,
                    (&raw mut info).cast(),
                    u32::try_from(std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
                        .expect("a small struct"),
                    std::ptr::null_mut(),
                )
            };
            if queried == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(info.ActiveProcesses)
        }

        /// Polls until every process in the job has exited, or
        /// [`KILL_TIMEOUT`] passes.
        fn wait_until_empty(&self) -> std::io::Result<()> {
            let deadline = Instant::now() + KILL_TIMEOUT;
            loop {
                match self.active()? {
                    0 => return Ok(()),
                    _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    left => {
                        return Err(std::io::Error::other(format!(
                            "{left} processes outlived their job"
                        )));
                    }
                }
            }
        }
    }

    /// The job each contained process was put in, by pid.
    ///
    /// Keyed by pid because that is what every caller of `terminate_tree`
    /// holds. A pid the system reuses for a later contained process replaces
    /// the old entry, and closing the old job ends whatever was left in it.
    fn jobs() -> &'static Mutex<HashMap<u32, Job>> {
        static JOBS: OnceLock<Mutex<HashMap<u32, Job>>> = OnceLock::new();
        JOBS.get_or_init(Mutex::default)
    }

    pub(crate) fn contain(process: HANDLE, pid: u32) {
        match Job::new().and_then(|job| job.assign(process).map(|()| job)) {
            Ok(job) => {
                jobs()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(pid, job);
            }
            // Already in a job that forbids another. It still runs, but
            // ending it ends only it; said here rather than discovered when
            // its children outlive it.
            Err(error) => tracing::warn!(
                %error,
                pid,
                "could not put a process in a job of its own; its children will outlive it"
            ),
        }
    }

    pub(super) fn spawn_contained(
        command: &mut std::process::Command,
    ) -> std::io::Result<std::process::Child> {
        use std::os::windows::process::CommandExt;

        // Suspended, so it runs nothing before it is in its job; in a group
        // of its own, as a command transport always was, so a Ctrl-C meant
        // for this process does not reach it.
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
        let child = command.spawn()?;

        contain(child.as_raw_handle() as HANDLE, child.id());

        if let Err(error) = resume(child.id()) {
            // Never resumed, it would never run: end it rather than hand back
            // a process that hangs whoever waits on it.
            let _ = terminate_tree(child.id(), Duration::ZERO);
            return Err(error);
        }

        Ok(child)
    }

    /// Resumes the one thread of a process created suspended.
    ///
    /// `std::process::Child` keeps no handle to the thread, so it is found
    /// by its owner's pid.
    fn resume(pid: u32) -> std::io::Result<()> {
        // SAFETY: a snapshot of every thread; owned below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let snapshot = Owned(snapshot);

        // SAFETY: an all-zero entry with its size set is what the walk expects.
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(std::mem::size_of::<THREADENTRY32>()).expect("small");

        let mut resumed = false;
        // SAFETY: a live snapshot and a correctly sized entry.
        let mut more = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: a thread id the snapshot just reported.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let thread = Owned(thread);
                // SAFETY: a live thread handle with THREAD_SUSPEND_RESUME.
                if unsafe { ResumeThread(thread.0) } == u32::MAX {
                    return Err(std::io::Error::last_os_error());
                }
                resumed = true;
            }
            // SAFETY: as for Thread32First.
            more = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
        }

        if resumed {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the suspended process has no thread to resume",
            ))
        }
    }

    pub(super) fn terminate_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
        let map = |source| ProcessError::Terminate { pid, source };

        let job = jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
        let Some(job) = job else {
            return terminate_one(pid, grace);
        };

        // Ended at once: Windows has no polite request a tree of console
        // programs reliably honours, so the wait is for the tree to be gone
        // rather than for it to leave on its own.
        job.terminate().map_err(map)?;
        job.wait_until_empty().map_err(map)
    }

    /// Ends a contained pid's job as [`terminate_tree`] does, without waiting
    /// for it to empty; the job stays recorded, so a later `terminate_tree`
    /// or [`wait_for_tree`] can do that waiting.
    ///
    /// Nothing here needs a reaper -- a terminated process signals its
    /// handle whether or not anyone has waited for it -- so a process that
    /// was never contained is ended as `terminate_tree` ends it.
    pub(super) fn signal_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
        let map = |source| ProcessError::Terminate { pid, source };

        let jobs = jobs().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = jobs.get(&pid) {
            return job.terminate().map_err(map);
        }
        drop(jobs);

        terminate_one(pid, grace)
    }

    pub(super) fn wait_for_tree(pid: u32) {
        let job = jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
        // Whatever is still in it when the wait gives up is ended as the job
        // closes. A process that was never contained has no rest to wait
        // for: the leader was all there was, and it has been waited for.
        if let Some(job) = job {
            let _ = job.wait_until_empty();
        }
    }

    /// Ends one process that was never contained -- a daemon started
    /// detached, say.
    fn terminate_one(pid: u32, grace: Duration) -> Result<(), ProcessError> {
        let map = |source| ProcessError::Terminate { pid, source };

        // SAFETY: OpenProcess takes access flags and a pid by value.
        let raw = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        if raw.is_null() {
            let error = std::io::Error::last_os_error();
            // No such process: it has exited and been waited for, which is
            // what the caller wanted. Anything else -- access denied above
            // all -- says nothing about whether it is gone.
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Ok(());
            }
            return Err(map(error));
        }
        let process = Owned(raw);

        // SAFETY: a live handle opened with PROCESS_TERMINATE.
        if unsafe { TerminateProcess(process.0, 1) } == 0 {
            let error = std::io::Error::last_os_error();
            // Ending a process that has already exited fails; it is gone all
            // the same.
            if has_exited(&process) {
                return Ok(());
            }
            return Err(map(error));
        }

        let millis = u32::try_from(grace.max(KILL_TIMEOUT).as_millis()).unwrap_or(u32::MAX);
        // SAFETY: a live handle opened with SYNCHRONIZE -- without it, as
        // before, this wait failed every time.
        match unsafe { WaitForSingleObject(process.0, millis) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(map(std::io::Error::from(std::io::ErrorKind::TimedOut))),
            _ => Err(map(std::io::Error::last_os_error())),
        }
    }

    /// Whether the process behind `process` has exited.
    ///
    /// Its handle is signalled, rather than its exit code read: a process
    /// may exit with `STILL_ACTIVE`'s own value, and would read as running.
    fn has_exited(process: &Owned) -> bool {
        // SAFETY: a live handle opened with SYNCHRONIZE; a zero wait only asks.
        unsafe { WaitForSingleObject(process.0, 0) == WAIT_OBJECT_0 }
    }

    pub(super) fn is_running(pid: u32) -> bool {
        // SAFETY: OpenProcess takes access flags and a pid by value.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if raw.is_null() {
            return false;
        }
        let process = Owned(raw);
        // SAFETY: a live handle opened with SYNCHRONIZE; a zero wait only asks.
        unsafe { WaitForSingleObject(process.0, 0) == WAIT_TIMEOUT }
    }

    pub(super) fn descendants(pid: u32) -> Vec<u32> {
        // SAFETY: a snapshot of every process; owned below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let snapshot = Owned(snapshot);

        // SAFETY: an all-zero entry with its size set is what the walk expects.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).expect("small");

        let mut pairs = Vec::new();
        // SAFETY: a live snapshot and a correctly sized entry.
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        while more {
            pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
            // SAFETY: as for Process32FirstW.
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }

        super::below(pid, &pairs)
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

        // Polled, as every sibling test polls: the group has been signalled,
        // but a killed process answers `kill(pid, 0)` until whoever inherited
        // it reaps it, and on macOS that is launchd, on its own schedule.
        const PATIENCE: Duration = Duration::from_secs(5);
        let deadline = std::time::Instant::now() + PATIENCE;
        while pid_is_alive(grandchild) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
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

    #[test]
    fn a_contained_tree_is_found_and_ended_whole() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_contained(&mut command).expect("sh starts");
        let pid = child.id();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while descendants(pid).len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "sh never started both sleeps"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let everyone: Vec<u32> = std::iter::once(pid).chain(descendants(pid)).collect();

        terminate_tree(pid, DEFAULT_GRACE).expect("the tree is ended");
        child.wait().expect("sh can be reaped");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while everyone.iter().any(|p| is_running(*p)) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(everyone.iter().all(|p| !is_running(*p)), "{everyone:?}");
    }

    #[test]
    fn descendants_are_listed_nearest_first_and_a_cycle_ends_the_walk() {
        // Windows records a parent's pid once and never updates it, so once
        // pids are reused a descendant can appear as its own ancestor's
        // parent.
        let pairs = [(2, 1), (3, 1), (4, 2), (5, 4), (1, 5), (9, 8)];

        assert_eq!(below(1, &pairs), vec![2, 3, 4, 5]);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// Waits for `condition`, returning whether it held in time.
    fn eventually(patience: Duration, condition: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + patience;
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        condition()
    }

    /// `cmd.exe` running `ping` for thirty seconds: a child with a grandchild.
    fn tree() -> std::process::Command {
        let mut command = std::process::Command::new("cmd.exe");
        command
            .args(["/d", "/c", "ping -n 30 127.0.0.1 >nul"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }

    #[test]
    fn a_contained_process_runs() {
        // Created suspended: one never resumed would hang this forever.
        let mut command = std::process::Command::new("cmd.exe");
        command.args(["/d", "/c", "exit 3"]);
        let mut child = spawn_contained(&mut command).expect("cmd.exe starts");

        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(child.wait().map(|status| status.code()));
        });
        let status = finished
            .recv_timeout(Duration::from_secs(10))
            .expect("it ran to the end")
            .expect("waiting succeeds");
        assert_eq!(status, Some(3));
    }

    #[test]
    fn terminating_a_contained_tree_ends_the_grandchild_too() {
        let mut child = spawn_contained(&mut tree()).expect("cmd.exe starts");
        let pid = child.id();
        assert!(
            eventually(Duration::from_secs(10), || !descendants(pid).is_empty()),
            "cmd.exe never started ping"
        );
        let everyone: Vec<u32> = std::iter::once(pid).chain(descendants(pid)).collect();

        terminate_tree(pid, DEFAULT_GRACE).expect("the tree is ended");
        let _ = child.wait();

        assert!(
            eventually(Duration::from_secs(5), || everyone
                .iter()
                .all(|p| !is_running(*p))),
            "a process in the tree outlived it: {everyone:?}"
        );
    }

    #[test]
    fn terminating_a_process_that_has_gone_is_not_an_error() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "exit 0"])
            .spawn()
            .expect("cmd.exe starts");
        let pid = child.id();
        child.wait().expect("it exits");
        // Its last handle closed, the pid names nothing: the answer is the
        // one for no such process, not one about an exited process.
        drop(child);

        terminate_tree(pid, DEFAULT_GRACE).expect("a process that has gone is what was asked for");
    }

    /// Whether `pid` still names a process object, running or exited.
    ///
    /// An exited process keeps its pid while anyone holds a handle to it;
    /// only once the last handle is closed can the system hand the pid out
    /// again.
    fn names_a_process(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

        // SAFETY: OpenProcess takes access flags and a pid by value.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: a handle this call just opened, closed exactly once.
        unsafe { CloseHandle(handle) };
        true
    }

    #[test]
    fn a_contained_process_keeps_its_pid_until_it_is_ended() {
        // A contained process that exits on its own -- every pane does -- is
        // still recorded until it is ended. Were its pid free meanwhile, the
        // system could give it to a stranger, and ending that stranger by pid
        // would end the stale job instead, and report success.
        let mut command = std::process::Command::new("cmd.exe");
        command
            .args(["/d", "/c", "exit 0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = spawn_contained(&mut command).expect("cmd.exe starts");
        let pid = child.id();
        child.wait().expect("it exits");
        drop(child);

        // Watched for a while rather than asked once: something else briefly
        // holding the exited process open would answer for it.
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            assert!(
                names_a_process(pid),
                "the pid of a process still recorded as contained was let go"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        terminate_tree(pid, DEFAULT_GRACE).expect("an exited contained process is ended");
    }

    /// Where the helper below writes its answer.
    const HELPER_ANSWER: &str = "DISPATCH_TEST_HELPER_ANSWER";
    /// Set when the helper is to put itself in a job that forbids breakaway.
    const HELPER_FORBIDS_BREAKAWAY: &str = "DISPATCH_TEST_HELPER_FORBIDS_BREAKAWAY";

    /// Not a test of its own: the body of the contained helper the two tests
    /// below start. Run without its variables, it does nothing.
    ///
    /// Starts a detached `ping` and answers `started <pid>` or
    /// `failed <error>`. Asked to, it first puts itself in a job that forbids
    /// breakaway, as a CI runner's or an OpenSSH session's may.
    #[test]
    fn detach_as_a_contained_helper() {
        let Some(answer) = std::env::var_os(HELPER_ANSWER) else {
            return;
        };
        let answer = std::path::PathBuf::from(answer);

        if std::env::var_os(HELPER_FORBIDS_BREAKAWAY).is_some() {
            forbid_breakaway();
        }

        let said = match spawn_detached(
            std::path::Path::new("ping.exe"),
            &["-n".into(), "30".into(), "127.0.0.1".into()],
        ) {
            Ok(pid) => format!("started {pid}"),
            Err(error) => format!("failed {error}"),
        };

        // Renamed into place, so the test never reads half of it.
        let partial = answer.with_extension("partial");
        std::fs::write(&partial, said).expect("the answer is writable");
        std::fs::rename(&partial, &answer).expect("the answer can be put in place");
    }

    /// Puts this process in a job of its own, one with no limits at all --
    /// breakaway included -- nested inside whatever job it is already in.
    fn forbid_breakaway() {
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        // SAFETY: both arguments may be null: default security, no name.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!job.is_null(), "{}", std::io::Error::last_os_error());
        // SAFETY: a live job handle, and the pseudo-handle for this process.
        // The job handle is left open: this helper exits in a moment.
        let assigned = unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) };
        assert_ne!(assigned, 0, "{}", std::io::Error::last_os_error());
    }

    /// Runs the helper contained, reads its answer, and ends its job.
    fn run_contained_helper(label: &str, forbid_breakaway: bool) -> String {
        let dir = std::env::temp_dir().join(format!("dispatch-os-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        let answer = dir.join("answer");

        let mut command =
            std::process::Command::new(std::env::current_exe().expect("the test binary"));
        command
            .args([
                "--exact",
                "process::windows_tests::detach_as_a_contained_helper",
                "--test-threads=1",
            ])
            .env(HELPER_ANSWER, &answer)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if forbid_breakaway {
            command.env(HELPER_FORBIDS_BREAKAWAY, "1");
        }
        let mut helper = spawn_contained(&mut command).expect("the test binary runs");

        let answered = eventually(Duration::from_secs(10), || answer.exists());
        let said = std::fs::read_to_string(&answer).unwrap_or_default();

        terminate_tree(helper.id(), DEFAULT_GRACE).expect("the helper's job is ended");
        let _ = helper.wait();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(answered, "the helper never answered");
        said
    }

    /// The pid the helper started, or a failure saying what it said instead.
    fn started(said: &str) -> u32 {
        said.strip_prefix("started ")
            .and_then(|pid| pid.parse().ok())
            .unwrap_or_else(|| panic!("the helper did not start a detached process: {said}"))
    }

    /// The job this test process runs in, if any, for a failure that may be
    /// the runner's doing rather than this crate's.
    fn own_job() -> String {
        use windows_sys::Win32::System::JobObjects::{
            IsProcessInJob, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, QueryInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let mut inside = 0;
        // SAFETY: the pseudo-handle for this process; a null job asks about
        // any job; `inside` is a valid place for the answer.
        if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut inside) } == 0 {
            return format!("unknown ({})", std::io::Error::last_os_error());
        }
        if inside == 0 {
            return "none".into();
        }

        // SAFETY: an all-zero structure is valid to fill.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a null job names this process's own; `limits` is the
        // structure this class names.
        let read = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                (&raw mut limits).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .expect("a small struct"),
                std::ptr::null_mut(),
            )
        };
        if read == 0 {
            return format!(
                "one whose limits cannot be read ({})",
                std::io::Error::last_os_error()
            );
        }
        format!(
            "one with limit flags {:#x}",
            limits.BasicLimitInformation.LimitFlags
        )
    }

    #[test]
    fn a_daemon_started_from_a_contained_process_outlives_its_job() {
        // A local `dispatchd --stdio` bridge runs contained, as every command
        // transport does, and starts the daemon it bridges to. That daemon
        // owns agents: ending the transport must not end them.
        let detached = started(&run_contained_helper("breakaway", false));

        let outlived = is_running(detached);
        let _ = terminate_tree(detached, DEFAULT_GRACE);
        assert!(
            outlived,
            "the detached process died with the job of the process that started it; \
             this test process's own job: {}",
            own_job()
        );
    }

    #[test]
    fn a_detached_process_still_starts_inside_a_job_that_forbids_breakaway() {
        // An OpenSSH session on Windows, or a CI runner, may run everything in
        // a job that refuses to let anything leave. The daemon must still
        // start there -- inside that job, since it cannot be anywhere else.
        started(&run_contained_helper("no-breakaway", true));

        // Nothing to clean up: ending the helper's job ended the one nested
        // in it, and what the helper started with it. Ending its pid again
        // could end a stranger the system has since given that pid to.
    }
}
