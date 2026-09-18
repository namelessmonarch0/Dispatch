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

    /// `ghostty_terminal_scroll_viewport(GhosttyTerminal, GhosttyTerminalScrollViewport)`
    pub fn ghostty_terminal_scroll_viewport(terminal: Terminal, behavior: ScrollViewport);

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

/// Opaque mouse-encoder handle.
pub type MouseEncoder = *mut c_void;

/// Opaque mouse-event handle.
pub type MouseEvent = *mut c_void;

/// `GHOSTTY_MOUSE_ENCODER_OPT_SIZE`
pub const MOUSE_ENCODER_OPT_SIZE: i32 = 2;

/// `GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED`
pub const MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED: i32 = 3;

/// Mouse button values, from `GhosttyMouseButton`.
pub mod mouse_button {
    /// No known button.
    pub const UNKNOWN: i32 = 0;
    /// Left.
    pub const LEFT: i32 = 1;
    /// Right.
    pub const RIGHT: i32 = 2;
    /// Middle.
    pub const MIDDLE: i32 = 3;
    /// Wheel up.
    pub const FOUR: i32 = 4;
    /// Wheel down.
    pub const FIVE: i32 = 5;
    /// Wheel left.
    pub const SIX: i32 = 6;
    /// Wheel right.
    pub const SEVEN: i32 = 7;
}

/// Mouse actions, from `GhosttyMouseAction`.
pub mod mouse_action {
    /// Button pressed.
    pub const PRESS: i32 = 0;
    /// Button released.
    pub const RELEASE: i32 = 1;
    /// Pointer moved.
    pub const MOTION: i32 = 2;
}

/// Mirrors `GhosttyMousePosition`, in surface pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MousePosition {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
}

/// Mirrors `GhosttyMouseEncoderSize`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MouseEncoderSize {
    /// `sizeof` this struct, for forward compatibility.
    pub size: usize,
    /// Screen width in pixels.
    pub screen_width: u32,
    /// Screen height in pixels.
    pub screen_height: u32,
    /// Cell width in pixels. Must not be zero.
    pub cell_width: u32,
    /// Cell height in pixels. Must not be zero.
    pub cell_height: u32,
    /// Top padding.
    pub padding_top: u32,
    /// Bottom padding.
    pub padding_bottom: u32,
    /// Right padding.
    pub padding_right: u32,
    /// Left padding.
    pub padding_left: u32,
}

impl MouseEncoderSize {
    /// A size where one pixel is one cell.
    ///
    /// The encoder works in surface pixels because it was written for a
    /// renderer that draws glyphs. Dispatch has no pixels, so declaring a
    /// one-by-one cell makes pixel space and cell space the same thing and
    /// lets cell coordinates be passed through unchanged.
    #[must_use]
    pub fn in_cells(cols: u16, rows: u16) -> Self {
        Self {
            size: size_of::<Self>(),
            screen_width: u32::from(cols),
            screen_height: u32::from(rows),
            cell_width: 1,
            cell_height: 1,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        }
    }
}

/// Tags for `GhosttyTerminalScrollViewport`.
pub mod scroll {
    /// Jump to the oldest scrollback.
    pub const TOP: i32 = 0;
    /// Return to the active area.
    pub const BOTTOM: i32 = 1;
    /// Move by a signed number of rows.
    pub const DELTA: i32 = 2;
    /// Jump to an absolute row.
    pub const ROW: i32 = 3;
}

/// Mirrors `GhosttyTerminalScrollViewportValue`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union ScrollValue {
    /// Rows to move by; negative is towards older output.
    pub delta: isize,
    /// Absolute row.
    pub row: usize,
    /// Forces the union's size and alignment, as the header does.
    pub _padding: [u64; 2],
}

impl std::fmt::Debug for ScrollValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScrollValue")
    }
}

/// Mirrors `GhosttyTerminalScrollViewport`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ScrollViewport {
    /// Which member of `value` is active.
    pub tag: i32,
    /// The amount or position.
    pub value: ScrollValue,
}

/// Opaque key-encoder handle.
pub type KeyEncoder = *mut c_void;

/// Opaque key-event handle.
pub type KeyEvent = *mut c_void;

/// `GhosttyKeyAction::GHOSTTY_KEY_ACTION_PRESS`
pub const KEY_ACTION_PRESS: i32 = 1;

/// `GHOSTTY_KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT`
pub const KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT: i32 = 6;

/// `GhosttyOptionAsAlt::GHOSTTY_OPTION_AS_ALT_TRUE`
pub const OPTION_AS_ALT_TRUE: i32 = 1;

/// Modifier bits, from the `GHOSTTY_MODS_*` macros.
pub mod mods {
    /// Shift.
    pub const SHIFT: u16 = 1 << 0;
    /// Control.
    pub const CTRL: u16 = 1 << 1;
    /// Alt / Option.
    pub const ALT: u16 = 1 << 2;
    /// Super / Command / Windows.
    pub const SUPER: u16 = 1 << 3;
}

/// Physical keys, from `GhosttyKey`.
///
/// Only the keys crossterm can report are declared. The values come from the
/// enum's declaration order in `key/event.h` and must not be renumbered.
pub mod key {
    /// Unknown key.
    pub const UNIDENTIFIED: i32 = 0;
    /// Backtick.
    pub const BACKQUOTE: i32 = 1;
    /// Backslash.
    pub const BACKSLASH: i32 = 2;
    /// Left bracket.
    pub const BRACKET_LEFT: i32 = 3;
    /// Right bracket.
    pub const BRACKET_RIGHT: i32 = 4;
    /// Comma.
    pub const COMMA: i32 = 5;
    /// Digit zero; digits one to nine follow consecutively.
    pub const DIGIT_0: i32 = 6;
    /// Equals.
    pub const EQUAL: i32 = 16;
    /// Letter A; B to Z follow consecutively.
    pub const A: i32 = 20;
    /// Minus.
    pub const MINUS: i32 = 46;
    /// Period.
    pub const PERIOD: i32 = 47;
    /// Apostrophe.
    pub const QUOTE: i32 = 48;
    /// Semicolon.
    pub const SEMICOLON: i32 = 49;
    /// Forward slash.
    pub const SLASH: i32 = 50;
    /// Backspace.
    pub const BACKSPACE: i32 = 53;
    /// Return.
    pub const ENTER: i32 = 58;
    /// Space.
    pub const SPACE: i32 = 63;
    /// Tab.
    pub const TAB: i32 = 64;
    /// Delete.
    pub const DELETE: i32 = 68;
    /// End.
    pub const END: i32 = 69;
    /// Home.
    pub const HOME: i32 = 71;
    /// Insert.
    pub const INSERT: i32 = 72;
    /// Page down.
    pub const PAGE_DOWN: i32 = 73;
    /// Page up.
    pub const PAGE_UP: i32 = 74;
    /// Down arrow.
    pub const ARROW_DOWN: i32 = 75;
    /// Left arrow.
    pub const ARROW_LEFT: i32 = 76;
    /// Right arrow.
    pub const ARROW_RIGHT: i32 = 77;
    /// Up arrow.
    pub const ARROW_UP: i32 = 78;
    /// Escape.
    pub const ESCAPE: i32 = 120;
    /// F1; F2 to F24 follow consecutively.
    pub const F1: i32 = 121;
}

unsafe extern "C" {
    /// `ghostty_key_encoder_new(const GhosttyAllocator*, GhosttyKeyEncoder*)`
    pub fn ghostty_key_encoder_new(
        allocator: *const c_void,
        encoder: *mut KeyEncoder,
    ) -> GhosttyResult;

    /// `ghostty_key_encoder_free(GhosttyKeyEncoder)`
    pub fn ghostty_key_encoder_free(encoder: KeyEncoder);

    /// `ghostty_key_encoder_setopt(GhosttyKeyEncoder, GhosttyKeyEncoderOption, const void*)`
    ///
    /// `value` points at storage of the type the option documents.
    pub fn ghostty_key_encoder_setopt(encoder: KeyEncoder, option: i32, value: *const c_void);

    /// `ghostty_key_encoder_setopt_from_terminal(GhosttyKeyEncoder, GhosttyTerminal)`
    ///
    /// Copies the terminal's active keyboard modes into the encoder, which is
    /// what makes the encoding match what the child asked for.
    pub fn ghostty_key_encoder_setopt_from_terminal(encoder: KeyEncoder, terminal: Terminal);

    /// `ghostty_key_encoder_encode(GhosttyKeyEncoder, GhosttyKeyEvent, char*, size_t, size_t*)`
    pub fn ghostty_key_encoder_encode(
        encoder: KeyEncoder,
        event: KeyEvent,
        out_buf: *mut u8,
        out_buf_size: usize,
        out_len: *mut usize,
    ) -> GhosttyResult;

    /// `ghostty_key_event_new(const GhosttyAllocator*, GhosttyKeyEvent*)`
    pub fn ghostty_key_event_new(allocator: *const c_void, event: *mut KeyEvent) -> GhosttyResult;

    /// `ghostty_key_event_free(GhosttyKeyEvent)`
    pub fn ghostty_key_event_free(event: KeyEvent);

    /// `ghostty_key_event_set_action(GhosttyKeyEvent, GhosttyKeyAction)`
    pub fn ghostty_key_event_set_action(event: KeyEvent, action: i32);

    /// `ghostty_key_event_set_key(GhosttyKeyEvent, GhosttyKey)`
    pub fn ghostty_key_event_set_key(event: KeyEvent, key: i32);

    /// `ghostty_key_event_set_mods(GhosttyKeyEvent, GhosttyMods)`
    pub fn ghostty_key_event_set_mods(event: KeyEvent, mods: u16);

    /// `ghostty_key_event_set_utf8(GhosttyKeyEvent, const char*, size_t)`
    pub fn ghostty_key_event_set_utf8(event: KeyEvent, utf8: *const u8, len: usize);

    /// `ghostty_key_event_set_unshifted_codepoint(GhosttyKeyEvent, uint32_t)`
    pub fn ghostty_key_event_set_unshifted_codepoint(event: KeyEvent, codepoint: u32);

    /// `ghostty_mouse_encoder_new(const GhosttyAllocator*, GhosttyMouseEncoder*)`
    pub fn ghostty_mouse_encoder_new(
        allocator: *const c_void,
        encoder: *mut MouseEncoder,
    ) -> GhosttyResult;

    /// `ghostty_mouse_encoder_free(GhosttyMouseEncoder)`
    pub fn ghostty_mouse_encoder_free(encoder: MouseEncoder);

    /// `ghostty_mouse_encoder_setopt(GhosttyMouseEncoder, GhosttyMouseEncoderOption, const void*)`
    pub fn ghostty_mouse_encoder_setopt(encoder: MouseEncoder, option: i32, value: *const c_void);

    /// `ghostty_mouse_encoder_setopt_from_terminal(GhosttyMouseEncoder, GhosttyTerminal)`
    ///
    /// Copies the terminal's active mouse tracking modes into the encoder,
    /// which is what decides whether an event is reported at all.
    pub fn ghostty_mouse_encoder_setopt_from_terminal(encoder: MouseEncoder, terminal: Terminal);

    /// `ghostty_mouse_encoder_encode(GhosttyMouseEncoder, GhosttyMouseEvent, char*, size_t, size_t*)`
    pub fn ghostty_mouse_encoder_encode(
        encoder: MouseEncoder,
        event: MouseEvent,
        out_buf: *mut u8,
        out_buf_size: usize,
        out_len: *mut usize,
    ) -> GhosttyResult;

    /// `ghostty_mouse_event_new(const GhosttyAllocator*, GhosttyMouseEvent*)`
    pub fn ghostty_mouse_event_new(
        allocator: *const c_void,
        event: *mut MouseEvent,
    ) -> GhosttyResult;

    /// `ghostty_mouse_event_free(GhosttyMouseEvent)`
    pub fn ghostty_mouse_event_free(event: MouseEvent);

    /// `ghostty_mouse_event_set_action(GhosttyMouseEvent, GhosttyMouseAction)`
    pub fn ghostty_mouse_event_set_action(event: MouseEvent, action: i32);

    /// `ghostty_mouse_event_set_button(GhosttyMouseEvent, GhosttyMouseButton)`
    pub fn ghostty_mouse_event_set_button(event: MouseEvent, button: i32);

    /// `ghostty_mouse_event_set_mods(GhosttyMouseEvent, GhosttyMods)`
    pub fn ghostty_mouse_event_set_mods(event: MouseEvent, mods: u16);

    /// `ghostty_mouse_event_set_position(GhosttyMouseEvent, GhosttyMousePosition)`
    pub fn ghostty_mouse_event_set_position(event: MouseEvent, position: MousePosition);
}
