//! Constraining where Windows looks for dynamic libraries.
//!
//! Only meaningful on Windows; a no-op elsewhere.

/// Restricts DLL resolution to System32 for the rest of the process.
///
/// Called once, before anything can load a library on our behalf.
///
/// Two reasons. `portable-pty` probes for a sideloaded `conpty.dll` through
/// the bare DLL search path and prefers it over the ConPTY exports in
/// `kernel32.dll`. When an unrelated `conpty.dll` happens to sit on `PATH`,
/// `CreatePseudoConsole` still succeeds but nothing pumps the pseudoconsole:
/// children start, produce no output, and never exit. Restricting the search
/// makes that probe miss, so the kernel's own implementation is used.
///
/// It also means Dispatch never loads another application's DLL from `PATH`,
/// which is the DLL planting problem the same search path creates.
pub fn restrict_search_path() {
    imp::restrict_search_path();
}

#[cfg(windows)]
mod imp {
    use std::sync::Once;

    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
    };

    static ONCE: Once = Once::new();

    pub(super) fn restrict_search_path() {
        ONCE.call_once(|| {
            // SAFETY: takes a flags value by value and touches no memory owned
            // by this process. Failure is not fatal: it leaves the default
            // search order in place, which is what would have happened anyway.
            let ok = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
            if ok == 0 {
                tracing::warn!(
                    error = %std::io::Error::last_os_error(),
                    "could not restrict the DLL search path; a sideloaded conpty.dll on PATH may break panes"
                );
            }
        });
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn restrict_search_path() {}
}
