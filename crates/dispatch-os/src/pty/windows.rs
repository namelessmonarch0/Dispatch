//! Dispatch's own ConPTY spawn.
//!
//! What `portable-pty` 0.9 does, with one difference that is the reason
//! this exists: the process is created suspended and put in a Job Object
//! before it runs a single instruction, so nothing it starts can escape the
//! job that `process::terminate_tree` ends.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

/// The flags `portable-pty` 0.9 created every pseudoconsole with, so panes
/// behave as they did: ask the host where its cursor is (answered in
/// `spawn`), redraw correctly on resize, and pass keys in win32-input-mode.
const PSEUDOCONSOLE_INHERIT_CURSOR: u32 = 0x1;
const PSEUDOCONSOLE_RESIZE_QUIRK: u32 = 0x2;
const PSEUDOCONSOLE_WIN32_INPUT_MODE: u32 = 0x4;

/// A pseudoconsole, closed when dropped.
pub(super) struct Terminal(HPCON);

impl Terminal {
    pub(super) fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
        // SAFETY: a live pseudoconsole; the size is passed by value.
        let result = unsafe { ResizePseudoConsole(self.0, coord(rows, cols)) };
        if result != 0 {
            return Err(std::io::Error::other(format!(
                "resizing the pseudoconsole failed: HRESULT {result:#x}"
            )));
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: created by CreatePseudoConsole and closed exactly once,
        // here. Before Windows 11 24H2 this waits until what the console
        // still has to say is read; the reader keeps reading until
        // end-of-file (see `dispatch-pty`), which is what lets it return.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

/// A process to wait on.
pub(super) struct Child(OwnedHandle);

impl Child {
    pub(super) fn wait(self) -> i32 {
        let handle = self.0.as_raw_handle() as HANDLE;
        // SAFETY: a live process handle; waiting on one is always defined.
        unsafe { WaitForSingleObject(handle, INFINITE) };

        let mut code = 0u32;
        // SAFETY: a live process handle and a place for its code.
        if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
            return 1;
        }
        if code == 0 {
            0
        } else {
            i32::try_from(code).unwrap_or(1)
        }
    }
}

pub(super) fn spawn(
    command: &super::PtyCommand<'_>,
    rows: u16,
    cols: u16,
) -> std::io::Result<super::PtyProcess> {
    // ConPTY reads the child's input from one pipe and writes its output to
    // another. This side keeps the writing end of the first and the reading
    // end of the second; ConPTY's ends are closed here once it holds its
    // own, so the pipes end when it does.
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;

    let mut console: HPCON = 0;
    // SAFETY: both handles are live, and `console` receives the new
    // pseudoconsole, owned by `Terminal` from here on.
    let created = unsafe {
        CreatePseudoConsole(
            coord(rows, cols),
            input_read.as_raw_handle() as HANDLE,
            output_write.as_raw_handle() as HANDLE,
            PSEUDOCONSOLE_INHERIT_CURSOR
                | PSEUDOCONSOLE_RESIZE_QUIRK
                | PSEUDOCONSOLE_WIN32_INPUT_MODE,
            &mut console,
        )
    };
    if created != 0 {
        return Err(std::io::Error::other(format!(
            "failed to create a pseudoconsole: HRESULT {created:#x}"
        )));
    }
    let terminal = Terminal(console);
    drop(input_read);
    drop(output_write);

    let (process, pid) = match start(command, console) {
        Ok(started) => started,
        Err(error) => {
            // The output's reading end first. Before Windows 11 24H2,
            // closing a pseudoconsole waits until what it still has to say
            // is read or its pipe is broken, and nothing is reading it yet.
            drop(output_read);
            drop(terminal);
            return Err(error);
        }
    };

    let mut writer = std::fs::File::from(input_write);
    answer_inherit_cursor(&mut writer);

    Ok(super::PtyProcess {
        reader: Box::new(std::fs::File::from(output_read)),
        writer: Box::new(writer),
        terminal: super::Terminal(terminal),
        child: super::Child(Child(process)),
        pid: Some(pid),
    })
}

/// Starts `command` attached to `console`, inside a job of its own, and
/// returns the process and its pid.
fn start(command: &super::PtyCommand<'_>, console: HPCON) -> std::io::Result<(OwnedHandle, u32)> {
    let mut attributes = AttributeList::with_pseudoconsole(console)?;

    // SAFETY: an all-zero STARTUPINFOEXW is valid; the fields that matter are
    // set below.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).expect("small");
    // Explicitly invalid, as `portable-pty` does: otherwise a daemon whose
    // own standard handles point at a log file would hand them to the child,
    // which would write there instead of to its terminal.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attributes.as_mut_ptr();

    let environment = environment(command.env);
    let program = resolve(command.program, &environment);
    let application = wide(program.as_os_str());
    let mut line = command_line(&program, command.args);
    let block = environment_block(&environment);
    let cwd = start_in(command.cwd, &environment);

    // SAFETY: an all-zero PROCESS_INFORMATION is filled by the call.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every string is NUL-terminated and outlives the call; the
    // command line is mutable, as CreateProcessW requires; `startup` -- all
    // of it, since EXTENDED_STARTUPINFO_PRESENT has the call read past its
    // first field -- and its attribute list live across the call.
    let started = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            block.as_ptr().cast(),
            cwd.as_ref().map_or(std::ptr::null(), |cwd| cwd.as_ptr()),
            (&raw const startup).cast::<STARTUPINFOW>(),
            &mut info,
        )
    };
    if started == 0 {
        let error = std::io::Error::last_os_error();
        return Err(std::io::Error::new(
            error.kind(),
            format!("cannot run {}: {error}", program.display()),
        ));
    }

    // SAFETY: both handles were just returned to this process, which owns
    // them from here on.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess as RawHandle) };
    // SAFETY: as above.
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread as RawHandle) };

    crate::process::contain(process.as_raw_handle() as HANDLE, info.dwProcessId);

    // SAFETY: the process's one thread, created suspended.
    if unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) } == u32::MAX {
        let error = std::io::Error::last_os_error();
        // Never resumed, it would never run: ended rather than handed back
        // to hang whoever waits on it. By its job when it has one; failing
        // that, by the handle held here, which may always end it.
        if crate::process::terminate_tree(info.dwProcessId, std::time::Duration::ZERO).is_err() {
            // SAFETY: a live process handle, opened with every access.
            unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, 1) };
        }
        return Err(error);
    }

    Ok((process, info.dwProcessId))
}

/// Answers the pseudoconsole's inherit-cursor question.
///
/// `PSEUDOCONSOLE_INHERIT_CURSOR` makes ConPTY ask its host where the cursor
/// is and wait for the answer before it pumps anything. A terminal emulator
/// answers because it is one; Dispatch hosts the pseudoconsole instead, so
/// without this the child starts, prints nothing, and never exits. Row 1,
/// column 1. A failure is not fatal on its own, so it is logged.
fn answer_inherit_cursor(writer: &mut std::fs::File) {
    use std::io::Write;

    if let Err(error) = writer.write_all(b"\x1b[1;1R").and_then(|()| writer.flush()) {
        tracing::warn!(%error, "failed to answer the ConPTY inherit-cursor handshake");
    }
}

fn coord(rows: u16, cols: u16) -> COORD {
    COORD {
        X: i16::try_from(cols).unwrap_or(i16::MAX),
        Y: i16::try_from(rows).unwrap_or(i16::MAX),
    }
}

fn pipe() -> std::io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: both out-pointers are valid; default security makes the ends
    // uninheritable, which is what a pseudoconsole's pipes should be.
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both handles were just created and are owned from here on.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read as RawHandle),
            OwnedHandle::from_raw_handle(write as RawHandle),
        )
    })
}

/// A thread attribute list carrying one pseudoconsole.
struct AttributeList(Vec<usize>);

impl AttributeList {
    fn with_pseudoconsole(console: HPCON) -> std::io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a null list only asks how large one must be.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };

        // usizes, not bytes: the list holds pointers and must be aligned.
        let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list: LPPROC_THREAD_ATTRIBUTE_LIST = buffer.as_mut_ptr().cast();
        // SAFETY: `buffer` holds at least `size` bytes.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut attributes = Self(buffer);

        // The pseudoconsole attribute's value is the HPCON itself, passed in
        // the pointer's place, as Microsoft's own example does.
        // SAFETY: an initialised list with room for one attribute.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attributes.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const core::ffi::c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(attributes)
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: initialised in `with_pseudoconsole`, deleted exactly once.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

/// This process's environment with `overrides` on top, keyed as Windows
/// keys it: case-insensitively.
fn environment(overrides: &BTreeMap<String, String>) -> BTreeMap<String, (OsString, OsString)> {
    let mut environment: BTreeMap<String, (OsString, OsString)> = std::env::vars_os()
        .map(|(key, value)| (key.to_string_lossy().to_uppercase(), (key, value)))
        .collect();
    for (key, value) in overrides {
        environment.insert(key.to_uppercase(), (key.into(), value.into()));
    }
    environment
}

/// The environment as `CreateProcessW` takes it: `KEY=VALUE` strings, each
/// NUL-terminated, sorted, ending in one more NUL.
fn environment_block(environment: &BTreeMap<String, (OsString, OsString)>) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in environment.values() {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// The file `program` names, found as [`super::resolve_program`] finds it
/// with the child's own `PATH` and `PATHEXT`.
fn resolve(program: &str, environment: &BTreeMap<String, (OsString, OsString)>) -> PathBuf {
    let value = |key: &str| environment.get(key).map(|(_, value)| value.as_os_str());
    super::resolve_program(program, value("PATH"), value("PATHEXT"))
}

/// Where the process starts: `cwd` when it is a directory, and otherwise the
/// user's profile, as `portable-pty` did -- and, with the home directory,
/// still does on Unix -- so a pane whose directory has gone still starts.
/// `None` leaves it in this process's own directory.
fn start_in(cwd: &Path, environment: &BTreeMap<String, (OsString, OsString)>) -> Option<Vec<u16>> {
    let profile = environment
        .get("USERPROFILE")
        .map(|(_, value)| Path::new(value));
    let dir = Some(cwd)
        .filter(|dir| dir.is_dir())
        .or(profile.filter(|dir| dir.is_dir()))?;
    // The call wants a full path; a relative one means this process's
    // directory, as it did to `portable-pty`.
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    Some(wide(dir.as_os_str()))
}

/// The command line: the program, then each argument, quoted for the C
/// runtime, NUL-terminated.
fn command_line(program: &Path, args: &[String]) -> Vec<u16> {
    let mut line = super::quote_for_crt(&program.to_string_lossy());
    for arg in args {
        line.push(' ');
        line.push_str(&super::quote_for_crt(arg));
    }
    line.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}
