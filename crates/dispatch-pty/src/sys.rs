//! Raw FFI declarations for the vendored `libghostty-vt`.
//!
//! Hand-written and deliberately narrow: only what Dispatch calls. The full
//! generated surface is produced by `cargo xtask regen-bindings`.
//!
//! Every item mirrors a declaration in
//! `vendor/libghostty-vt/include/ghostty/vt/`. Nothing here is safe to call
//! directly; [`crate::vt`] is the supported interface.

use std::ffi::c_void;

/// Opaque terminal handle. `GhosttyTerminal` in `types.h` is a pointer to an
/// incomplete `struct GhosttyTerminalImpl`.
pub type Terminal = *mut c_void;

/// Opaque formatter handle.
pub type Formatter = *mut c_void;

/// Mirrors `GhosttyResult` in `types.h`.
pub type GhosttyResult = i32;

/// `GHOSTTY_SUCCESS`
pub const SUCCESS: GhosttyResult = 0;

/// Selectors for [`ghostty_terminal_get`], from `GhosttyTerminalData`.
///
/// Only the fields Dispatch reads are declared. The discriminants are fixed
/// by the header and must not be renumbered.
pub mod data {
    /// Terminal width in cells. Output type `uint16_t *`.
    pub const COLS: i32 = 1;
    /// Terminal height in cells. Output type `uint16_t *`.
    pub const ROWS: i32 = 2;
    /// Cursor column, zero-indexed. Output type `uint16_t *`.
    pub const CURSOR_X: i32 = 3;
    /// Cursor row within the active area, zero-indexed. Output type `uint16_t *`.
    pub const CURSOR_Y: i32 = 4;
}

/// `GhosttyFormatterFormat::GHOSTTY_FORMATTER_FORMAT_PLAIN`
pub const FORMAT_PLAIN: i32 = 0;

/// Mirrors `GhosttyFormatterScreenExtra`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FormatterScreenExtra {
    /// `sizeof` this struct, for forward compatibility.
    pub size: usize,
    /// Emit the cursor position.
    pub cursor: bool,
    /// Emit character set designations and invocations.
    pub charsets: bool,
}

/// Mirrors `GhosttyFormatterTerminalExtra`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FormatterTerminalExtra {
    /// `sizeof` this struct, for forward compatibility.
    pub size: usize,
    /// Emit the palette using OSC 4.
    pub palette: bool,
    /// Emit modes differing from their defaults using CSI h/l.
    pub modes: bool,
    /// Emit scrolling region state.
    pub scrolling_region: bool,
    /// Emit tabstop positions.
    pub tabstops: bool,
    /// Emit the working directory using OSC 7.
    pub pwd: bool,
    /// Emit keyboard modes.
    pub keyboard: bool,
    /// Screen-level extras.
    pub screen: FormatterScreenExtra,
}

/// Mirrors `GhosttyFormatterTerminalOptions`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FormatterTerminalOptions {
    /// `sizeof` this struct, for forward compatibility.
    pub size: usize,
    /// Output format to emit.
    pub emit: i32,
    /// Whether to unwrap soft-wrapped lines.
    pub unwrap: bool,
    /// Whether to trim trailing whitespace on non-blank lines.
    pub trim: bool,
    /// Extra terminal state to include.
    pub extra: FormatterTerminalExtra,
    /// Restricts output to a range. Null formats the whole screen.
    pub selection: *const c_void,
}

impl FormatterTerminalOptions {
    /// Options producing plain text for the whole screen.
    ///
    /// The `size` fields are what let the library accept a struct compiled
    /// against an older header, so they must be set from `size_of`.
    #[must_use]
    pub fn plain_text() -> Self {
        Self {
            size: size_of::<Self>(),
            emit: FORMAT_PLAIN,
            unwrap: false,
            trim: true,
            extra: FormatterTerminalExtra {
                size: size_of::<FormatterTerminalExtra>(),
                palette: false,
                modes: false,
                scrolling_region: false,
                tabstops: false,
                pwd: false,
                keyboard: false,
                screen: FormatterScreenExtra {
                    size: size_of::<FormatterScreenExtra>(),
                    cursor: false,
                    charsets: false,
                },
            },
            selection: std::ptr::null(),
        }
    }
}

unsafe extern "C" {
    /// `ghostty_terminal_new(const GhosttyAllocator*, GhosttyTerminal*, uint16_t, uint16_t)`
    ///
    /// A null allocator selects the default allocator.
    pub fn ghostty_terminal_new(
        allocator: *const c_void,
        terminal: *mut Terminal,
        cols: u16,
        rows: u16,
    ) -> GhosttyResult;

    /// `ghostty_terminal_free(GhosttyTerminal)`
    pub fn ghostty_terminal_free(terminal: Terminal);

    /// `ghostty_terminal_vt_write(GhosttyTerminal, const uint8_t*, size_t)`
    pub fn ghostty_terminal_vt_write(terminal: Terminal, data: *const u8, len: usize);

    /// `ghostty_terminal_resize(GhosttyTerminal, uint16_t, uint16_t, uint32_t, uint32_t)`
    pub fn ghostty_terminal_resize(
        terminal: Terminal,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> GhosttyResult;

    /// `ghostty_terminal_get(GhosttyTerminal, GhosttyTerminalData, void*)`
    ///
    /// `out` must point at storage of the type the selector documents.
    pub fn ghostty_terminal_get(terminal: Terminal, data: i32, out: *mut c_void) -> GhosttyResult;

    /// `ghostty_formatter_terminal_new(const GhosttyAllocator*, GhosttyFormatter*, GhosttyTerminal, GhosttyFormatterTerminalOptions)`
    ///
    /// The terminal must outlive the formatter.
    pub fn ghostty_formatter_terminal_new(
        allocator: *const c_void,
        formatter: *mut Formatter,
        terminal: Terminal,
        options: FormatterTerminalOptions,
    ) -> GhosttyResult;

    /// `ghostty_formatter_format_alloc(GhosttyFormatter, const GhosttyAllocator*, uint8_t**, size_t*)`
    ///
    /// The buffer must be released with [`ghostty_free`] using the same
    /// allocator.
    pub fn ghostty_formatter_format_alloc(
        formatter: Formatter,
        allocator: *const c_void,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> GhosttyResult;

    /// `ghostty_formatter_free(GhosttyFormatter)`
    pub fn ghostty_formatter_free(formatter: Formatter);

    /// `ghostty_free(const GhosttyAllocator*, uint8_t*, size_t)`
    pub fn ghostty_free(allocator: *const c_void, ptr: *mut u8, len: usize);
}
