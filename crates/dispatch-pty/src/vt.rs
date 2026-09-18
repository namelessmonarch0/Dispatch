//! Safe terminal emulation.
//!
//! All `unsafe` in Dispatch is confined to this module and [`crate::sys`].
//! Nothing above it touches a raw pointer, and no handle borrowed from
//! libghostty-vt escapes a call.

use std::ffi::c_void;

use crate::sys;

/// A failure reported by libghostty-vt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{operation} failed: {code}")]
pub struct VtError {
    /// The C function that failed.
    pub operation: &'static str,
    /// The `GhosttyResult` it returned.
    pub code: i32,
}

impl VtError {
    fn check(operation: &'static str, code: sys::GhosttyResult) -> Result<(), Self> {
        if code == sys::SUCCESS {
            Ok(())
        } else {
            Err(Self { operation, code })
        }
    }
}

/// Where the cursor sits, in cells, zero-indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Column.
    pub x: u16,
    /// Row within the active area.
    pub y: u16,
}

/// Terminal size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Width in cells.
    pub cols: u16,
    /// Height in cells.
    pub rows: u16,
}

impl Size {
    /// Creates a size, clamping each dimension to at least one cell.
    ///
    /// libghostty-vt rejects a zero dimension, and a terminal briefly sized to
    /// nothing is a normal consequence of a window being dragged small.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }
}

/// The terminal state behind one pane.
///
/// Feed it bytes from a pseudoterminal; read a screen back out.
///
/// Swapping this for another VT engine means implementing this surface and
/// nothing else.
#[derive(Debug)]
pub struct VtTerminal {
    /// Owned handle. Freed exactly once, in `Drop`.
    handle: sys::Terminal,
}

// SAFETY: the handle is owned exclusively by this value and libghostty-vt
// does not use thread-local state for it. `&mut self` on every mutating
// method keeps the library's no-reentrancy requirement for vt_write.
unsafe impl Send for VtTerminal {}

impl VtTerminal {
    /// Creates a terminal of the given size.
    pub fn new(size: Size) -> Result<Self, VtError> {
        let mut handle: sys::Terminal = std::ptr::null_mut();

        // SAFETY: `handle` is a valid out-pointer, and a null allocator
        // selects the library's default allocator per allocator.h.
        let code = unsafe {
            sys::ghostty_terminal_new(std::ptr::null(), &raw mut handle, size.cols, size.rows)
        };
        VtError::check("ghostty_terminal_new", code)?;

        debug_assert!(!handle.is_null(), "success must yield a handle");
        Ok(Self { handle })
    }

    /// Feeds bytes from the pseudoterminal into the emulator.
    ///
    /// Takes `&mut self` because libghostty-vt documents `vt_write` as
    /// non-reentrant.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        // SAFETY: the handle is live for the lifetime of self, and `bytes`
        // outlives the call with its true length.
        unsafe { sys::ghostty_terminal_vt_write(self.handle, bytes.as_ptr(), bytes.len()) };
    }

    /// Resizes the terminal.
    ///
    /// Pixel dimensions are reported to programs that ask for them; zero means
    /// unknown, which is what a terminal that does not render glyphs itself
    /// should say.
    pub fn resize(&mut self, size: Size) -> Result<(), VtError> {
        // SAFETY: the handle is live and the size is non-zero in both
        // dimensions by construction.
        let code = unsafe { sys::ghostty_terminal_resize(self.handle, size.cols, size.rows, 0, 0) };
        VtError::check("ghostty_terminal_resize", code)
    }

    /// Reads one `uint16_t`-valued field.
    fn get_u16(&self, selector: i32, operation: &'static str) -> Result<u16, VtError> {
        let mut value: u16 = 0;

        // SAFETY: every selector passed here is documented with output type
        // `uint16_t *`, which is what `value` is.
        let code = unsafe {
            sys::ghostty_terminal_get(self.handle, selector, (&raw mut value).cast::<c_void>())
        };
        VtError::check(operation, code)?;
        Ok(value)
    }

    /// The terminal's current size in cells.
    pub fn size(&self) -> Result<Size, VtError> {
        Ok(Size {
            cols: self.get_u16(sys::data::COLS, "ghostty_terminal_get(COLS)")?,
            rows: self.get_u16(sys::data::ROWS, "ghostty_terminal_get(ROWS)")?,
        })
    }

    /// Where the cursor sits.
    pub fn cursor(&self) -> Result<Cursor, VtError> {
        Ok(Cursor {
            x: self.get_u16(sys::data::CURSOR_X, "ghostty_terminal_get(CURSOR_X)")?,
            y: self.get_u16(sys::data::CURSOR_Y, "ghostty_terminal_get(CURSOR_Y)")?,
        })
    }

    /// The visible screen as plain text, one line per row.
    ///
    /// Styling is dropped. Used for assertions and diagnostics; rendering
    /// reads cells and their styles instead.
    pub fn plain_text(&self) -> Result<String, VtError> {
        let mut formatter: sys::Formatter = std::ptr::null_mut();
        let options = sys::FormatterTerminalOptions::plain_text();

        // SAFETY: the handle outlives the formatter, which is freed below on
        // every path. A null allocator selects the default allocator.
        let code = unsafe {
            sys::ghostty_formatter_terminal_new(
                std::ptr::null(),
                &raw mut formatter,
                self.handle,
                options,
            )
        };
        VtError::check("ghostty_formatter_terminal_new", code)?;

        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len: usize = 0;

        // SAFETY: `formatter` is live, and both out-pointers are valid.
        let code = unsafe {
            sys::ghostty_formatter_format_alloc(
                formatter,
                std::ptr::null(),
                &raw mut ptr,
                &raw mut len,
            )
        };

        // Free the formatter before returning either way: the error path must
        // not leak it.
        //
        // SAFETY: `formatter` came from ghostty_formatter_terminal_new and is
        // not used afterwards.
        unsafe { sys::ghostty_formatter_free(formatter) };

        VtError::check("ghostty_formatter_format_alloc", code)?;

        if ptr.is_null() || len == 0 {
            return Ok(String::new());
        }

        // SAFETY: the library returned `ptr` with `len` initialised bytes,
        // and the slice is copied before the buffer is released.
        let text = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();

        // SAFETY: released with the same (default) allocator and length the
        // library allocated it with, as ghostty_formatter_format_alloc
        // documents.
        unsafe { sys::ghostty_free(std::ptr::null(), ptr, len) };

        // The formatter emits UTF-8; a replacement character is a better
        // outcome than refusing to render a pane over one bad byte.
        Ok(String::from_utf8_lossy(&text).into_owned())
    }
}

impl Drop for VtTerminal {
    fn drop(&mut self) {
        // SAFETY: the handle came from ghostty_terminal_new, is freed exactly
        // once here, and is not used afterwards.
        unsafe { sys::ghostty_terminal_free(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> VtTerminal {
        VtTerminal::new(Size::new(80, 24)).expect("a terminal can be created")
    }

    /// Trims each line and drops trailing blank lines, so assertions can name
    /// the content without encoding the blank remainder of the screen.
    fn visible_lines(terminal: &VtTerminal) -> Vec<String> {
        let text = terminal.plain_text().expect("formatting succeeds");
        let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }

    #[test]
    fn plain_text_comes_back_out() {
        let mut terminal = terminal();
        terminal.feed(b"hello world");
        assert_eq!(visible_lines(&terminal), vec!["hello world"]);
    }

    #[test]
    fn styling_is_parsed_and_not_shown_as_text() {
        let mut terminal = terminal();
        terminal.feed(b"\x1b[31mred\x1b[0m plain");
        assert_eq!(visible_lines(&terminal), vec!["red plain"]);
    }

    #[test]
    fn the_cursor_moves_with_the_text() {
        let mut terminal = terminal();
        assert_eq!(terminal.cursor().expect("readable"), Cursor { x: 0, y: 0 });

        terminal.feed(b"abc");
        assert_eq!(terminal.cursor().expect("readable"), Cursor { x: 3, y: 0 });
    }

    #[test]
    fn absolute_cursor_positioning_is_honoured() {
        let mut terminal = terminal();
        // CUP is one-indexed; row 3 column 5 is (4, 2) in zero-indexed cells.
        terminal.feed(b"\x1b[3;5H");
        assert_eq!(terminal.cursor().expect("readable"), Cursor { x: 4, y: 2 });
    }

    #[test]
    fn newlines_advance_the_row() {
        let mut terminal = terminal();
        terminal.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(visible_lines(&terminal), vec!["one", "two", "three"]);
    }

    #[test]
    fn the_screen_can_be_erased() {
        let mut terminal = terminal();
        terminal.feed(b"visible");
        assert!(!visible_lines(&terminal).is_empty());

        terminal.feed(b"\x1b[2J");
        assert!(
            visible_lines(&terminal).is_empty(),
            "erase-in-display should clear the screen"
        );
    }

    #[test]
    fn the_reported_size_matches_what_was_asked_for() {
        let terminal = VtTerminal::new(Size::new(120, 40)).expect("creatable");
        assert_eq!(
            terminal.size().expect("readable"),
            Size {
                cols: 120,
                rows: 40
            }
        );
    }

    #[test]
    fn resizing_changes_the_reported_size() {
        let mut terminal = terminal();
        terminal.resize(Size::new(100, 30)).expect("resizable");
        assert_eq!(
            terminal.size().expect("readable"),
            Size {
                cols: 100,
                rows: 30
            }
        );
    }

    #[test]
    fn a_zero_dimension_is_clamped_rather_than_rejected() {
        // A window dragged to nothing should not take a pane down with it.
        let terminal = VtTerminal::new(Size::new(0, 0)).expect("zero is clamped, not rejected");
        assert_eq!(
            terminal.size().expect("readable"),
            Size { cols: 1, rows: 1 }
        );
    }

    #[test]
    fn text_survives_a_resize() {
        let mut terminal = terminal();
        terminal.feed(b"persistent");
        terminal.resize(Size::new(100, 30)).expect("resizable");
        assert_eq!(visible_lines(&terminal), vec!["persistent"]);
    }

    #[test]
    fn feeding_nothing_is_harmless() {
        let mut terminal = terminal();
        terminal.feed(b"");
        assert!(visible_lines(&terminal).is_empty());
    }

    #[test]
    fn a_split_escape_sequence_is_reassembled() {
        // A pseudoterminal read can end mid-sequence, so the emulator has to
        // carry parser state between feeds.
        let mut terminal = terminal();
        terminal.feed(b"\x1b[3");
        terminal.feed(b";5H");
        assert_eq!(terminal.cursor().expect("readable"), Cursor { x: 4, y: 2 });
    }

    #[test]
    fn invalid_utf8_does_not_panic() {
        let mut terminal = terminal();
        terminal.feed(&[0xff, 0xfe, b'o', b'k']);
        let _ = terminal.plain_text().expect("formatting still succeeds");
    }
}
