//! Tests for pane rendering.
//!
//! Each drives a real terminal emulator and asserts on the ratatui buffer, so
//! the whole path from escape sequence to painted cell is covered.

use super::*;

use dispatch_pty::{ScreenReader, Size, VtTerminal};

/// Feeds `bytes` to a terminal of `size` and reads its screen.
fn screen(bytes: &[u8], size: Size) -> Screen {
    let mut terminal = VtTerminal::new(size).expect("a terminal can be created");
    terminal.feed(bytes);
    ScreenReader::new()
        .expect("a reader can be created")
        .read(&terminal)
        .expect("the screen can be read")
}

/// Renders `bytes` into a buffer of the same size.
fn render(bytes: &[u8], cols: u16, rows: u16) -> (Buffer, Screen) {
    let screen = screen(bytes, Size::new(cols, rows));
    let area = Rect::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(area);
    PaneWidget::new(&screen).render(area, &mut buf);
    (buf, screen)
}

/// The symbols of one row, joined and trimmed.
fn row_text(buf: &Buffer, y: u16) -> String {
    let width = buf.area.width;
    let line: String = (0..width)
        .filter_map(|x| buf.cell((x, y)))
        .map(|c| c.symbol())
        .collect();
    line.trim_end().to_string()
}

fn cell_at(buf: &Buffer, x: u16, y: u16) -> &ratatui::buffer::Cell {
    buf.cell((x, y)).expect("cell is inside the buffer")
}

#[test]
fn text_is_painted_where_it_belongs() {
    let (buf, _) = render(b"hello", 20, 3);
    assert_eq!(row_text(&buf, 0), "hello");
}

#[test]
fn several_lines_are_painted_on_their_own_rows() {
    let (buf, _) = render(b"one\r\ntwo\r\nthree", 20, 3);

    assert_eq!(row_text(&buf, 0), "one");
    assert_eq!(row_text(&buf, 1), "two");
    assert_eq!(row_text(&buf, 2), "three");
}

#[test]
fn a_truecolour_foreground_reaches_the_buffer() {
    let (buf, _) = render(b"\x1b[38;2;10;20;30mX", 10, 2);

    assert_eq!(cell_at(&buf, 0, 0).fg, Color::Rgb(10, 20, 30));
}

#[test]
fn a_truecolour_background_reaches_the_buffer() {
    let (buf, _) = render(b"\x1b[48;2;1;2;3mX", 10, 2);

    assert_eq!(cell_at(&buf, 0, 0).bg, Color::Rgb(1, 2, 3));
}

#[test]
fn an_unstyled_cell_inherits_the_host_terminals_colours() {
    // Reset rather than a concrete colour: that is what makes a pane look
    // native inside whatever theme the user runs.
    let (buf, _) = render(b"plain", 10, 2);

    let cell = cell_at(&buf, 0, 0);
    assert_eq!(cell.fg, Color::Reset);
    assert_eq!(cell.bg, Color::Reset);
}

#[test]
fn attributes_reach_the_buffer_as_modifiers() {
    let (buf, _) = render(b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[4mU", 10, 2);

    assert!(cell_at(&buf, 0, 0).modifier.contains(Modifier::BOLD));
    assert!(cell_at(&buf, 1, 0).modifier.contains(Modifier::ITALIC));
    assert!(cell_at(&buf, 2, 0).modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn blinking_is_dropped() {
    // A grid of blinking panes is unreadable, and several agents blink a
    // spinner or cursor of their own.
    let (buf, _) = render(b"\x1b[5mB", 10, 2);

    assert!(!cell_at(&buf, 0, 0).modifier.contains(Modifier::SLOW_BLINK));
    assert!(!cell_at(&buf, 0, 0).modifier.contains(Modifier::RAPID_BLINK));
}

#[test]
fn a_blank_cell_still_carries_its_background() {
    // Erase-in-line after setting a background paints the rest of the row.
    let (buf, _) = render(b"\x1b[48;2;9;9;9m\x1b[K", 10, 2);

    assert_eq!(
        cell_at(&buf, 5, 0).bg,
        Color::Rgb(9, 9, 9),
        "a blank cell with a background must be painted, not skipped"
    );
}

#[test]
fn a_wide_character_claims_both_of_its_cells() {
    let (buf, _) = render("日x".as_bytes(), 10, 2);

    assert_eq!(cell_at(&buf, 0, 0).symbol(), "日");
    assert_eq!(
        cell_at(&buf, 1, 0).symbol(),
        "",
        "the second half of a wide character must be cleared"
    );
    assert_eq!(cell_at(&buf, 2, 0).symbol(), "x");
}

#[test]
fn rendering_into_a_smaller_area_clips_rather_than_overflows() {
    // The emulator and the layout learn about a resize at different moments,
    // so they disagree for at least one frame.
    let screen = screen(b"abcdefghij\r\nklmnopqrst", Size::new(10, 4));
    let area = Rect::new(0, 0, 4, 2);
    let mut buf = Buffer::empty(area);

    PaneWidget::new(&screen).render(area, &mut buf);

    assert_eq!(row_text(&buf, 0), "abcd");
    assert_eq!(row_text(&buf, 1), "klmn");
}

#[test]
fn rendering_into_a_larger_area_leaves_the_rest_untouched() {
    let screen = screen(b"hi", Size::new(4, 1));
    let area = Rect::new(0, 0, 10, 3);
    let mut buf = Buffer::empty(area);

    PaneWidget::new(&screen).render(area, &mut buf);

    assert_eq!(row_text(&buf, 0), "hi");
}

#[test]
fn a_pane_is_painted_at_its_offset() {
    // Panes sit in a grid, so nothing may assume it starts at the origin.
    let screen = screen(b"xy", Size::new(4, 2));
    let area = Rect::new(3, 2, 4, 2);
    let mut buf = Buffer::empty(Rect::new(0, 0, 10, 6));

    PaneWidget::new(&screen).render(area, &mut buf);

    assert_eq!(cell_at(&buf, 3, 2).symbol(), "x");
    assert_eq!(cell_at(&buf, 4, 2).symbol(), "y");
    assert_eq!(cell_at(&buf, 0, 0).symbol(), " ", "outside the pane");
}

#[test]
fn a_zero_sized_area_paints_nothing() {
    let screen = screen(b"hello", Size::new(10, 2));
    let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));

    PaneWidget::new(&screen).render(Rect::new(0, 0, 0, 2), &mut buf);
    PaneWidget::new(&screen).render(Rect::new(0, 0, 10, 0), &mut buf);

    assert_eq!(row_text(&buf, 0), "", "nothing should have been painted");
}

#[test]
fn only_a_focused_pane_reports_a_cursor() {
    let screen = screen(b"abc", Size::new(10, 2));
    let area = Rect::new(0, 0, 10, 2);

    assert_eq!(
        PaneWidget::new(&screen).focused(true).cursor_position(area),
        Some((3, 0))
    );
    assert_eq!(
        PaneWidget::new(&screen)
            .focused(false)
            .cursor_position(area),
        None,
        "an unfocused pane must not claim the cursor"
    );
}

#[test]
fn the_reported_cursor_is_offset_with_the_pane() {
    let screen = screen(b"abc", Size::new(10, 2));
    let area = Rect::new(5, 4, 10, 2);

    assert_eq!(
        PaneWidget::new(&screen).focused(true).cursor_position(area),
        Some((8, 4))
    );
}

#[test]
fn a_hidden_cursor_is_not_reported() {
    let screen = screen(b"\x1b[?25labc", Size::new(10, 2));
    let area = Rect::new(0, 0, 10, 2);

    assert_eq!(
        PaneWidget::new(&screen).focused(true).cursor_position(area),
        None
    );
}

#[test]
fn a_cursor_outside_the_area_is_not_reported() {
    // Mid-resize the cursor can sit beyond the pane's current rectangle, and
    // placing the terminal cursor there would put it in a neighbouring pane.
    let screen = screen(b"\x1b[1;9H", Size::new(10, 2));
    let area = Rect::new(0, 0, 4, 2);

    assert_eq!(
        PaneWidget::new(&screen).focused(true).cursor_position(area),
        None
    );
}
