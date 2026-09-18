//! Temporary Windows diagnostics for ConPTY.
//!
//! Not part of the library surface. Remove once the Windows pseudoterminal
//! path is understood.

#![cfg(all(test, windows))]

use std::io::Read;
use std::sync::mpsc::channel;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Reports the full path of any loadable `conpty.dll`, or why it did not load.
fn probe_conpty() -> String {
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, LoadLibraryW};

    let name: Vec<u16> = "conpty.dll\0".encode_utf16().collect();

    // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
    let handle = unsafe { LoadLibraryW(name.as_ptr()) };
    if handle.is_null() {
        return format!("not loadable ({})", std::io::Error::last_os_error());
    }

    let mut buf = [0u16; 512];
    // SAFETY: `handle` is a live module handle and `buf` has the stated length.
    let len = unsafe { GetModuleFileNameW(handle, buf.as_mut_ptr(), buf.len() as u32) };
    let path = String::from_utf16_lossy(&buf[..len as usize]);

    // SAFETY: `handle` came from LoadLibraryW and is not used afterwards.
    unsafe { FreeLibrary(handle) };

    format!("loaded from {path:?}")
}

/// Drives ConPTY directly and reports everything observable, so one CI run
/// says which stage is failing rather than only that output never arrived.
#[test]
fn conpty_diagnostics() {
    let mut report = String::new();
    report.push_str("\n--- ConPTY diagnostics ---\n");

    // Which conpty.dll, if any, the bare search path would have found.
    report.push_str(&format!(
        "conpty.dll on PATH before hardening: {}\n",
        probe_conpty()
    ));
    dispatch_os::dll::restrict_search_path();
    report.push_str(&format!(
        "conpty.dll on PATH after hardening:  {}\n",
        probe_conpty()
    ));

    let pty = native_pty_system();
    let pair = match pty.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(pair) => pair,
        Err(e) => panic!("{report}openpty failed: {e:?}"),
    };
    report.push_str("openpty: ok\n");

    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/c", "echo hello"]);
    report.push_str(&format!("cwd set to: {:?}\n", std::env::temp_dir()));
    cmd.cwd(std::env::temp_dir());

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(child) => child,
        Err(e) => panic!("{report}spawn failed: {e:?}"),
    };
    report.push_str(&format!("spawn: ok, pid={:?}\n", child.process_id()));

    let reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => panic!("{report}try_clone_reader failed: {e:?}"),
    };
    report.push_str("try_clone_reader: ok\n");

    drop(pair.slave);
    report.push_str("slave dropped\n");

    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        let mut total = Vec::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Ok((total.clone(), "eof".to_string())));
                    return;
                }
                Ok(n) => {
                    total.extend_from_slice(&buf[..n]);
                    if total.len() > 2048 {
                        let _ = tx.send(Ok((total.clone(), "enough".to_string())));
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("{e:?}")));
                    return;
                }
            }
        }
    });

    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok((bytes, why))) => report.push_str(&format!(
            "reader finished ({why}): {} bytes: {:?}\n",
            bytes.len(),
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(200)
                .collect::<String>()
        )),
        Ok(Err(e)) => report.push_str(&format!("reader error: {e}\n")),
        Err(e) => report.push_str(&format!("reader timed out after 15s: {e:?}\n")),
    }

    // try_wait avoids blocking forever if the child is stuck.
    for i in 0..30 {
        match child.try_wait() {
            Ok(Some(status)) => {
                report.push_str(&format!("child exited after ~{}00ms: {status:?}\n", i));
                panic!("{report}");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                report.push_str(&format!("try_wait error: {e:?}\n"));
                panic!("{report}");
            }
        }
    }

    report.push_str("child still running after 3s\n");
    panic!("{report}");
}
