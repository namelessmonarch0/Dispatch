//! Raw FFI declarations for the vendored `libghostty-vt`.
//!
//! Hand-written for now and deliberately minimal: enough to prove the library
//! links and round-trips bytes on every target. The full surface is generated
//! by `cargo xtask regen-bindings` and will replace this module.
//!
//! Every item here mirrors `vendor/libghostty-vt/include/ghostty/vt/`.

use std::ffi::c_void;

/// Opaque terminal handle. Mirrors `GhosttyTerminal` in `types.h`, which is a
/// pointer to an incomplete `struct GhosttyTerminalImpl`.
pub type Terminal = *mut c_void;

/// Mirrors `GhosttyResult` in `types.h`.
pub type Result = i32;

/// `GHOSTTY_SUCCESS`
pub const SUCCESS: Result = 0;

unsafe extern "C" {
    /// `ghostty_terminal_new(const GhosttyAllocator*, GhosttyTerminal*, uint16_t, uint16_t)`
    ///
    /// Passing a null allocator selects the default allocator.
    pub fn ghostty_terminal_new(
        allocator: *const c_void,
        terminal: *mut Terminal,
        cols: u16,
        rows: u16,
    ) -> Result;

    /// `ghostty_terminal_free(GhosttyTerminal)`
    pub fn ghostty_terminal_free(terminal: Terminal);

    /// `ghostty_terminal_vt_write(GhosttyTerminal, const uint8_t*, size_t)`
    pub fn ghostty_terminal_vt_write(terminal: Terminal, data: *const u8, len: usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a terminal, writes a VT stream through it, and frees it.
    ///
    /// This is the canary for the whole engine choice. `ghostty_terminal_vt_write`
    /// is the exact call that faults with `STATUS_ACCESS_VIOLATION` on the
    /// unsupported `x86_64-windows-msvc` target, so exercising it here means a
    /// bad target configuration fails in CI rather than at runtime in a pane.
    #[test]
    fn terminal_round_trips_a_vt_stream() {
        let mut terminal: Terminal = std::ptr::null_mut();

        // SAFETY: `terminal` is a valid out-pointer and a null allocator
        // selects libghostty-vt's default allocator, per allocator.h.
        let result = unsafe { ghostty_terminal_new(std::ptr::null(), &raw mut terminal, 80, 24) };
        assert_eq!(result, SUCCESS, "ghostty_terminal_new failed");
        assert!(!terminal.is_null(), "terminal handle is null after success");

        // Plain text, an SGR colour sequence, a cursor move, and an erase --
        // enough to drive the parser through several states rather than only
        // the ground state.
        let stream: &[u8] = b"hello\x1b[31mred\x1b[0m\x1b[2;5Hmoved\x1b[2J";

        // SAFETY: `terminal` was created above and is not freed until below.
        // `stream` outlives the call and its length is its true byte length.
        unsafe { ghostty_terminal_vt_write(terminal, stream.as_ptr(), stream.len()) };

        // SAFETY: `terminal` is a live handle from `ghostty_terminal_new` and
        // is not used after this call.
        unsafe { ghostty_terminal_free(terminal) };
    }

    #[test]
    fn rejects_a_zero_sized_terminal() {
        let mut terminal: Terminal = std::ptr::null_mut();

        // SAFETY: same contract as above; cols/rows are documented as needing
        // to be greater than zero, so this exercises the error path.
        let result = unsafe { ghostty_terminal_new(std::ptr::null(), &raw mut terminal, 0, 0) };
        assert_ne!(result, SUCCESS, "a zero-sized terminal should be rejected");
    }
}
