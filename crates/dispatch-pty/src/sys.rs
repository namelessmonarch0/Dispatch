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

/// Opaque render-state handle.
pub type RenderState = *mut c_void;

/// Opaque row-iterator handle.
pub type RowIterator = *mut c_void;

/// Opaque row-cells iterator handle.
pub type RowCells = *mut c_void;

/// Selectors for [`ghostty_render_state_get`], from `GhosttyRenderStateData`.
pub mod render_data {
    /// Width in cells. Output type `uint16_t *`.
    pub const COLS: i32 = 1;
    /// Height in cells. Output type `uint16_t *`.
    pub const ROWS: i32 = 2;
    /// Row iterator. Output type `GhosttyRenderStateRowIterator *`.
    pub const ROW_ITERATOR: i32 = 4;
    /// Whether the cursor is visible. Output type `bool *`.
    pub const CURSOR_VISIBLE: i32 = 11;
    /// Whether the cursor has a viewport position. Output type `bool *`.
    pub const CURSOR_VIEWPORT_HAS_VALUE: i32 = 14;
    /// Cursor column in the viewport. Output type `uint16_t *`.
    pub const CURSOR_VIEWPORT_X: i32 = 15;
    /// Cursor row in the viewport. Output type `uint16_t *`.
    pub const CURSOR_VIEWPORT_Y: i32 = 16;
}

/// Selectors for [`ghostty_render_state_row_get`].
pub mod row_data {
    /// Cells iterator for this row. Output type `GhosttyRenderStateRowCells *`.
    pub const CELLS: i32 = 3;
}

/// Selectors for [`ghostty_render_state_row_cells_get`].
pub mod cell_data {
    /// Full style. Output type `GhosttyStyle *`.
    pub const STYLE: i32 = 2;
    /// Resolved background colour. Output type `GhosttyColorRgb *`.
    pub const BG_COLOR: i32 = 5;
    /// Resolved foreground colour. Output type `GhosttyColorRgb *`.
    pub const FG_COLOR: i32 = 6;
    /// Grapheme cluster as UTF-8. Output type `GhosttyBuffer *`.
    pub const GRAPHEMES_UTF8: i32 = 9;
}

/// `GHOSTTY_INVALID_VALUE`, returned when a cell has no explicit colour.
pub const INVALID_VALUE: GhosttyResult = -2;

/// `GHOSTTY_OUT_OF_SPACE`, returned when a buffer was too small.
pub const OUT_OF_SPACE: GhosttyResult = -3;

/// Mirrors `GhosttyColorRgb`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ColorRgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

/// Mirrors `GhosttyBuffer`.
#[repr(C)]
#[derive(Debug)]
pub struct Buffer {
    /// Destination. May be null when `cap` is zero, to query the size needed.
    pub ptr: *mut u8,
    /// Capacity of `ptr` in bytes.
    pub cap: usize,
    /// Bytes written, or the capacity required when the call ran out of space.
    pub len: usize,
}

/// Mirrors `GhosttyStyleColorValue`, a union whose largest member is 8 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub union StyleColorValue {
    /// Palette index.
    pub palette: u8,
    /// Direct colour.
    pub rgb: ColorRgb,
    /// Forces the union's size and alignment, as the header does.
    pub _padding: u64,
}

impl std::fmt::Debug for StyleColorValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StyleColorValue")
    }
}

/// Mirrors `GhosttyStyleColor`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StyleColor {
    /// Which member of `value` is active.
    pub tag: i32,
    /// The colour.
    pub value: StyleColorValue,
}

/// Mirrors `GhosttyStyle`.
///
/// Colours are read through the resolved `FG_COLOR` and `BG_COLOR` selectors
/// instead of these fields, which are left here so the layout matches.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// `sizeof` this struct, for forward compatibility.
    pub size: usize,
    /// Foreground.
    pub fg_color: StyleColor,
    /// Background.
    pub bg_color: StyleColor,
    /// Underline colour.
    pub underline_color: StyleColor,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Faint.
    pub faint: bool,
    /// Blinking.
    pub blink: bool,
    /// Reversed foreground and background.
    pub inverse: bool,
    /// Hidden.
    pub invisible: bool,
    /// Struck through.
    pub strikethrough: bool,
    /// Overlined.
    pub overline: bool,
    /// One of the `GHOSTTY_SGR_UNDERLINE_*` values; zero means none.
    pub underline: i32,
}

impl Style {
    /// A zeroed style with `size` set, ready to be filled by the library.
    #[must_use]
    pub fn empty() -> Self {
        // SAFETY: every field is a plain integer, bool or #[repr(C)] struct of
        // those, so an all-zero bit pattern is a valid value for each. `size`
        // is set immediately afterwards, which is the only field the library
        // requires the caller to initialise.
        let mut style: Self = unsafe { std::mem::zeroed() };
        style.size = size_of::<Self>();
        style
    }
}

unsafe extern "C" {
    /// `ghostty_render_state_new(const GhosttyAllocator*, GhosttyRenderState*)`
    pub fn ghostty_render_state_new(
        allocator: *const c_void,
        state: *mut RenderState,
    ) -> GhosttyResult;

    /// `ghostty_render_state_free(GhosttyRenderState)`
    pub fn ghostty_render_state_free(state: RenderState);

    /// `ghostty_render_state_update(GhosttyRenderState, GhosttyTerminal)`
    pub fn ghostty_render_state_update(state: RenderState, terminal: Terminal) -> GhosttyResult;

    /// `ghostty_render_state_get(GhosttyRenderState, GhosttyRenderStateData, void*)`
    pub fn ghostty_render_state_get(
        state: RenderState,
        data: i32,
        out: *mut c_void,
    ) -> GhosttyResult;

    /// `ghostty_render_state_row_iterator_new(const GhosttyAllocator*, GhosttyRenderStateRowIterator*)`
    pub fn ghostty_render_state_row_iterator_new(
        allocator: *const c_void,
        out_iterator: *mut RowIterator,
    ) -> GhosttyResult;

    /// `ghostty_render_state_row_iterator_free(GhosttyRenderStateRowIterator)`
    pub fn ghostty_render_state_row_iterator_free(iterator: RowIterator);

    /// `ghostty_render_state_row_iterator_next(GhosttyRenderStateRowIterator)`
    pub fn ghostty_render_state_row_iterator_next(iterator: RowIterator) -> bool;

    /// `ghostty_render_state_row_get(GhosttyRenderStateRowIterator, GhosttyRenderStateRowData, void*)`
    pub fn ghostty_render_state_row_get(
        iterator: RowIterator,
        data: i32,
        out: *mut c_void,
    ) -> GhosttyResult;

    /// `ghostty_render_state_row_cells_new(const GhosttyAllocator*, GhosttyRenderStateRowCells*)`
    pub fn ghostty_render_state_row_cells_new(
        allocator: *const c_void,
        out_cells: *mut RowCells,
    ) -> GhosttyResult;

    /// `ghostty_render_state_row_cells_free(GhosttyRenderStateRowCells)`
    pub fn ghostty_render_state_row_cells_free(cells: RowCells);

    /// `ghostty_render_state_row_cells_next(GhosttyRenderStateRowCells)`
    pub fn ghostty_render_state_row_cells_next(cells: RowCells) -> bool;

    /// `ghostty_render_state_row_cells_get(GhosttyRenderStateRowCells, GhosttyRenderStateRowCellsData, void*)`
    pub fn ghostty_render_state_row_cells_get(
        cells: RowCells,
        data: i32,
        out: *mut c_void,
    ) -> GhosttyResult;
}
