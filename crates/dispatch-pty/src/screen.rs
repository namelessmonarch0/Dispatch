//! Reading a pane's screen out of libghostty-vt as plain data.
//!
//! A [`Screen`] is a snapshot: owned text, colours and attributes, with no
//! borrow of the emulator. That boundary is deliberate. Rendering happens in
//! `dispatch-tui`, which must never touch a raw pointer or hold a handle
//! across a frame.

use std::ffi::c_void;

use crate::sys;
use crate::vt::{Size, VtError, VtTerminal};

/// A 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl From<sys::ColorRgb> for Rgb {
    fn from(c: sys::ColorRgb) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

/// Text decoration for one cell.
///
/// Colours are separate because the library resolves them for us, flattening
/// palette indices and the several places a colour can come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Faint.
    pub faint: bool,
    /// Blinking.
    pub blink: bool,
    /// Foreground and background swapped.
    pub inverse: bool,
    /// Hidden.
    pub invisible: bool,
    /// Struck through.
    pub strikethrough: bool,
    /// Underlined, in any of the underline styles.
    pub underline: bool,
}

impl From<&sys::Style> for Attrs {
    fn from(s: &sys::Style) -> Self {
        Self {
            bold: s.bold,
            italic: s.italic,
            faint: s.faint,
            blink: s.blink,
            inverse: s.inverse,
            invisible: s.invisible,
            strikethrough: s.strikethrough,
            // The distinction between single, double, curly, dotted and
            // dashed is not representable in a terminal cell downstream, so
            // it collapses to "underlined".
            underline: s.underline != 0,
        }
    }
}

/// One cell of the screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// The grapheme cluster displayed here. Empty means blank.
    ///
    /// A cluster, not a character: an emoji with a modifier or a base letter
    /// with combining marks occupies one cell and several codepoints.
    pub text: String,
    /// Foreground, when the cell sets one.
    pub fg: Option<Rgb>,
    /// Background, when the cell sets one.
    pub bg: Option<Rgb>,
    /// Decoration.
    pub attrs: Attrs,
}

impl Cell {
    /// Whether the cell has nothing to draw.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.text.is_empty() || self.text == " "
    }
}

/// A snapshot of one pane's visible screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// Rows, top to bottom. Each holds exactly [`Screen::size`] columns.
    pub rows: Vec<Vec<Cell>>,
    /// Where the cursor is, when it is visible and positioned.
    pub cursor: Option<(u16, u16)>,
    /// Size in cells.
    pub size: Size,
}

impl Screen {
    /// The cell at `(x, y)`, if the screen has one there.
    #[must_use]
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        self.rows.get(y as usize)?.get(x as usize)
    }

    /// Each row's text, with trailing blanks trimmed.
    ///
    /// For assertions and diagnostics.
    #[must_use]
    pub fn text_lines(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let line: String = row
                    .iter()
                    .map(|c| if c.text.is_empty() { " " } else { &c.text })
                    .collect();
                line.trim_end().to_string()
            })
            .collect()
    }
}

/// Reads screens out of a terminal.
///
/// Holds the library's render state, which is reused across frames rather
/// than rebuilt, since it exists to be diffed against the previous one.
#[derive(Debug)]
pub struct ScreenReader {
    state: sys::RenderState,
}

// SAFETY: the handle is owned exclusively by this value, and every method
// takes &mut self, so the library never sees concurrent use of it.
unsafe impl Send for ScreenReader {}

impl ScreenReader {
    /// Creates a reader.
    pub fn new() -> Result<Self, VtError> {
        let mut state: sys::RenderState = std::ptr::null_mut();

        // SAFETY: `state` is a valid out-pointer and a null allocator selects
        // the library's default allocator.
        let code = unsafe { sys::ghostty_render_state_new(std::ptr::null(), &raw mut state) };
        check("ghostty_render_state_new", code)?;

        Ok(Self { state })
    }

    /// Reads the terminal's current screen.
    pub fn read(&mut self, terminal: &VtTerminal) -> Result<Screen, VtError> {
        // SAFETY: both handles are live; update borrows the terminal only for
        // the duration of the call.
        let code = unsafe { sys::ghostty_render_state_update(self.state, terminal.handle()) };
        check("ghostty_render_state_update", code)?;

        let size = Size {
            cols: self.get_u16(sys::render_data::COLS, "COLS")?,
            rows: self.get_u16(sys::render_data::ROWS, "ROWS")?,
        };

        let cursor = self.read_cursor()?;
        let rows = self.read_rows(size)?;

        Ok(Screen { rows, cursor, size })
    }

    fn get_u16(&mut self, selector: i32, name: &'static str) -> Result<u16, VtError> {
        let mut value: u16 = 0;

        // SAFETY: each selector passed here documents output type `uint16_t *`.
        let code = unsafe {
            sys::ghostty_render_state_get(self.state, selector, (&raw mut value).cast::<c_void>())
        };
        check(name, code)?;
        Ok(value)
    }

    fn get_bool(&mut self, selector: i32, name: &'static str) -> Result<bool, VtError> {
        let mut value: bool = false;

        // SAFETY: each selector passed here documents output type `bool *`.
        let code = unsafe {
            sys::ghostty_render_state_get(self.state, selector, (&raw mut value).cast::<c_void>())
        };
        check(name, code)?;
        Ok(value)
    }

    /// Where the cursor should be drawn, if anywhere.
    fn read_cursor(&mut self) -> Result<Option<(u16, u16)>, VtError> {
        if !self.get_bool(sys::render_data::CURSOR_VISIBLE, "CURSOR_VISIBLE")? {
            return Ok(None);
        }

        // A visible cursor can still be scrolled out of the viewport.
        if !self.get_bool(
            sys::render_data::CURSOR_VIEWPORT_HAS_VALUE,
            "CURSOR_VIEWPORT_HAS_VALUE",
        )? {
            return Ok(None);
        }

        Ok(Some((
            self.get_u16(sys::render_data::CURSOR_VIEWPORT_X, "CURSOR_VIEWPORT_X")?,
            self.get_u16(sys::render_data::CURSOR_VIEWPORT_Y, "CURSOR_VIEWPORT_Y")?,
        )))
    }

    /// Walks every row and reads its cells.
    fn read_rows(&mut self, size: Size) -> Result<Vec<Vec<Cell>>, VtError> {
        let mut iterator: sys::RowIterator = std::ptr::null_mut();

        // SAFETY: valid out-pointer; null allocator selects the default.
        let code = unsafe {
            sys::ghostty_render_state_row_iterator_new(std::ptr::null(), &raw mut iterator)
        };
        check("ghostty_render_state_row_iterator_new", code)?;

        // Bind the iterator to this render state.
        //
        // SAFETY: both handles are live and ROW_ITERATOR documents output type
        // `GhosttyRenderStateRowIterator *`.
        let code = unsafe {
            sys::ghostty_render_state_get(
                self.state,
                sys::render_data::ROW_ITERATOR,
                (&raw mut iterator).cast::<c_void>(),
            )
        };
        if let Err(error) = check("ROW_ITERATOR", code) {
            // SAFETY: the iterator was created above and is not used again.
            unsafe { sys::ghostty_render_state_row_iterator_free(iterator) };
            return Err(error);
        }

        let result = self.collect_rows(iterator, size);

        // SAFETY: freed exactly once, on every path, and not used afterwards.
        unsafe { sys::ghostty_render_state_row_iterator_free(iterator) };

        result
    }

    fn collect_rows(
        &mut self,
        iterator: sys::RowIterator,
        size: Size,
    ) -> Result<Vec<Vec<Cell>>, VtError> {
        let mut rows = Vec::with_capacity(size.rows as usize);

        // SAFETY: the iterator is live for this whole loop.
        while unsafe { sys::ghostty_render_state_row_iterator_next(iterator) } {
            rows.push(read_row(iterator, size.cols)?);

            // The viewport should not produce more rows than it has, but a
            // runaway iterator would otherwise allocate without bound.
            if rows.len() > size.rows as usize {
                break;
            }
        }

        // A screen shorter than its own height would index out of bounds in a
        // renderer that trusts `size`.
        rows.resize_with(size.rows as usize, || blank_row(size.cols));

        Ok(rows)
    }
}

/// Reads one row's cells.
fn read_row(iterator: sys::RowIterator, cols: u16) -> Result<Vec<Cell>, VtError> {
    let mut cells: sys::RowCells = std::ptr::null_mut();

    // SAFETY: valid out-pointer; null allocator selects the default.
    let code = unsafe { sys::ghostty_render_state_row_cells_new(std::ptr::null(), &raw mut cells) };
    check("ghostty_render_state_row_cells_new", code)?;

    // Bind the cells iterator to the current row.
    //
    // SAFETY: both handles are live and CELLS documents output type
    // `GhosttyRenderStateRowCells *`.
    let code = unsafe {
        sys::ghostty_render_state_row_get(
            iterator,
            sys::row_data::CELLS,
            (&raw mut cells).cast::<c_void>(),
        )
    };
    if let Err(error) = check("ROW_DATA_CELLS", code) {
        // SAFETY: created above, not used again.
        unsafe { sys::ghostty_render_state_row_cells_free(cells) };
        return Err(error);
    }

    let mut row = Vec::with_capacity(cols as usize);

    // SAFETY: the cells iterator is live for this whole loop.
    while unsafe { sys::ghostty_render_state_row_cells_next(cells) } {
        row.push(read_cell(cells));
        if row.len() >= cols as usize {
            break;
        }
    }

    // SAFETY: freed exactly once and not used afterwards.
    unsafe { sys::ghostty_render_state_row_cells_free(cells) };

    // Rows can report fewer cells than the screen is wide.
    row.resize_with(cols as usize, Cell::default);
    Ok(row)
}

/// Reads the cell the iterator is currently on.
///
/// Infallible by design: a cell that cannot be read renders blank rather than
/// failing the frame. One unreadable cell must not blank a whole pane.
fn read_cell(cells: sys::RowCells) -> Cell {
    Cell {
        text: read_cell_text(cells),
        fg: read_cell_color(cells, sys::cell_data::FG_COLOR),
        bg: read_cell_color(cells, sys::cell_data::BG_COLOR),
        attrs: read_cell_attrs(cells),
    }
}

/// Reads a cell's grapheme cluster as UTF-8.
fn read_cell_text(cells: sys::RowCells) -> String {
    // Most cells are one ASCII byte; this covers a base codepoint plus
    // combining marks without a second call.
    let mut bytes = [0u8; 32];
    let mut buffer = sys::Buffer {
        ptr: bytes.as_mut_ptr(),
        cap: bytes.len(),
        len: 0,
    };

    // SAFETY: `cells` is live and `buffer` describes `bytes` accurately.
    let code = unsafe {
        sys::ghostty_render_state_row_cells_get(
            cells,
            sys::cell_data::GRAPHEMES_UTF8,
            (&raw mut buffer).cast::<c_void>(),
        )
    };

    if code != sys::SUCCESS {
        // A cluster longer than the buffer reports OUT_OF_SPACE; such a cell
        // is pathological, and a blank is better than a failed frame.
        return String::new();
    }

    if buffer.len == 0 || buffer.len > bytes.len() {
        return String::new();
    }

    String::from_utf8_lossy(&bytes[..buffer.len]).into_owned()
}

/// Reads one of a cell's resolved colours.
fn read_cell_color(cells: sys::RowCells, selector: i32) -> Option<Rgb> {
    let mut color = sys::ColorRgb::default();

    // SAFETY: `cells` is live and both colour selectors document output type
    // `GhosttyColorRgb *`.
    let code = unsafe {
        sys::ghostty_render_state_row_cells_get(cells, selector, (&raw mut color).cast::<c_void>())
    };

    // INVALID_VALUE means the cell sets no colour here, and the caller should
    // fall back to the terminal default. That is not an error.
    if code == sys::SUCCESS {
        Some(color.into())
    } else {
        None
    }
}

/// Reads a cell's decoration.
fn read_cell_attrs(cells: sys::RowCells) -> Attrs {
    let mut style = sys::Style::empty();

    // SAFETY: `cells` is live, STYLE documents output type `GhosttyStyle *`,
    // and `style` has its `size` field set as the library requires.
    let code = unsafe {
        sys::ghostty_render_state_row_cells_get(
            cells,
            sys::cell_data::STYLE,
            (&raw mut style).cast::<c_void>(),
        )
    };

    if code == sys::SUCCESS {
        Attrs::from(&style)
    } else {
        Attrs::default()
    }
}

fn blank_row(cols: u16) -> Vec<Cell> {
    vec![Cell::default(); cols as usize]
}

fn check(operation: &'static str, code: sys::GhosttyResult) -> Result<(), VtError> {
    if code == sys::SUCCESS {
        Ok(())
    } else {
        Err(VtError { operation, code })
    }
}

impl Drop for ScreenReader {
    fn drop(&mut self) {
        // SAFETY: the handle came from ghostty_render_state_new, is freed
        // exactly once here, and is not used afterwards.
        unsafe { sys::ghostty_render_state_free(self.state) };
    }
}

#[cfg(test)]
mod tests;
