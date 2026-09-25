//! Tests for reading screens out of the emulator.

use super::*;

fn screen_of(bytes: &[u8], size: Size) -> Screen {
    let mut terminal = VtTerminal::new(size).expect("a terminal can be created");
    terminal.feed(bytes);

    let mut reader = ScreenReader::new().expect("a reader can be created");
    reader.read(&terminal).expect("the screen can be read")
}

fn default_screen(bytes: &[u8]) -> Screen {
    screen_of(bytes, Size::new(20, 5))
}

#[test]
fn plain_text_lands_in_the_right_cells() {
    let screen = default_screen(b"hi");

    assert_eq!(screen.cell(0, 0).expect("cell exists").text, "h");
    assert_eq!(screen.cell(1, 0).expect("cell exists").text, "i");
    assert!(screen.cell(2, 0).expect("cell exists").is_blank());
}

#[test]
fn the_screen_is_exactly_the_size_it_reports() {
    let screen = screen_of(b"", Size::new(20, 5));

    assert_eq!(screen.size, Size { cols: 20, rows: 5 });
    assert_eq!(screen.rows.len(), 5, "one entry per row");
    for (i, row) in screen.rows.iter().enumerate() {
        assert_eq!(row.len(), 20, "row {i} should be full width");
    }
}

#[test]
fn every_row_is_full_width_even_when_short() {
    // A renderer indexes by the reported size, so a ragged screen would panic
    // or silently drop cells.
    let screen = screen_of(b"a\r\nbb\r\nccc", Size::new(40, 6));

    for (i, row) in screen.rows.iter().enumerate() {
        assert_eq!(row.len(), 40, "row {i}");
    }
}

#[test]
fn lines_are_separated_by_row() {
    let screen = default_screen(b"one\r\ntwo");
    let lines = screen.text_lines();

    assert_eq!(lines[0], "one");
    assert_eq!(lines[1], "two");
}

#[test]
fn a_foreground_colour_is_resolved_to_rgb() {
    // SGR 31 is palette red; the library resolves it through the palette, so
    // a renderer never has to own a palette of its own.
    let screen = default_screen(b"\x1b[31mR");

    let cell = screen.cell(0, 0).expect("cell exists");
    assert_eq!(cell.text, "R");
    assert!(cell.fg.is_some(), "a coloured cell should report a colour");
}

#[test]
fn a_truecolour_foreground_comes_back_exactly() {
    let screen = default_screen(b"\x1b[38;2;10;20;30mX");

    let cell = screen.cell(0, 0).expect("cell exists");
    assert_eq!(
        cell.fg,
        Some(Rgb {
            r: 10,
            g: 20,
            b: 30
        })
    );
}

#[test]
fn a_truecolour_background_comes_back_exactly() {
    let screen = default_screen(b"\x1b[48;2;1;2;3mX");

    let cell = screen.cell(0, 0).expect("cell exists");
    assert_eq!(cell.bg, Some(Rgb { r: 1, g: 2, b: 3 }));
}

#[test]
fn an_unstyled_cell_reports_no_colour() {
    // No colour means "use the terminal default", which the renderer decides.
    let screen = default_screen(b"plain");

    let cell = screen.cell(0, 0).expect("cell exists");
    assert_eq!(cell.fg, None);
    assert_eq!(cell.bg, None);
}

#[test]
fn text_attributes_are_reported() {
    let screen = default_screen(b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[4mU\x1b[0m\x1b[9mS");

    assert!(screen.cell(0, 0).expect("cell exists").attrs.bold);
    assert!(screen.cell(1, 0).expect("cell exists").attrs.italic);
    assert!(screen.cell(2, 0).expect("cell exists").attrs.underline);
    assert!(screen.cell(3, 0).expect("cell exists").attrs.strikethrough);
}

#[test]
fn attributes_are_reset_by_sgr_zero() {
    let screen = default_screen(b"\x1b[1mB\x1b[0mp");

    assert!(screen.cell(0, 0).expect("cell exists").attrs.bold);
    assert!(
        !screen.cell(1, 0).expect("cell exists").attrs.bold,
        "SGR 0 should clear bold"
    );
}

#[test]
fn every_underline_style_reads_as_underlined() {
    // Curly, dotted and dashed underlines have no distinct representation
    // downstream, so they must all still render as underlined rather than as
    // nothing.
    for sgr in [b"\x1b[4m".as_slice(), b"\x1b[4:3m", b"\x1b[4:4m"] {
        let mut bytes = sgr.to_vec();
        bytes.push(b'U');
        let screen = default_screen(&bytes);
        assert!(
            screen.cell(0, 0).expect("cell exists").attrs.underline,
            "underline variant {sgr:?} should read as underlined"
        );
    }
}

#[test]
fn the_cursor_is_reported_where_it_sits() {
    let screen = default_screen(b"abc");
    assert_eq!(screen.cursor, Some((3, 0)));
}

#[test]
fn absolute_positioning_moves_the_reported_cursor() {
    let screen = default_screen(b"\x1b[3;5H");
    assert_eq!(screen.cursor, Some((4, 2)));
}

#[test]
fn a_hidden_cursor_is_not_reported() {
    // DECTCEM off. Drawing a cursor a program asked to hide is a visible bug.
    let screen = default_screen(b"\x1b[?25labc");
    assert_eq!(screen.cursor, None);
}

#[test]
fn a_wide_character_keeps_its_cluster_together() {
    let screen = default_screen("日x".as_bytes());

    assert_eq!(screen.cell(0, 0).expect("cell exists").text, "日");
    // A wide character occupies two cells; the second carries no text of its
    // own, and the next character starts after it.
    assert_eq!(screen.cell(2, 0).expect("cell exists").text, "x");
}

#[test]
fn a_wide_characters_second_cell_reads_as_a_blank_in_its_line() {
    // Status rules match against these lines, so a rule for wide text has
    // to allow for the blank.
    let screen = default_screen("日本x".as_bytes());

    assert_eq!(screen.text_lines()[0], "日 本 x");
}

#[test]
fn combining_marks_stay_in_one_cell() {
    // "e" followed by a combining acute accent is one grapheme cluster and
    // must not be split across two cells.
    let screen = default_screen("e\u{0301}!".as_bytes());

    let first = screen.cell(0, 0).expect("cell exists");
    assert!(
        first.text.chars().count() >= 2,
        "expected a multi-codepoint cluster, got {:?}",
        first.text
    );
    assert_eq!(screen.cell(1, 0).expect("cell exists").text, "!");
}

#[test]
fn an_erased_screen_reads_blank() {
    let screen = default_screen(b"gone\x1b[2J");

    assert!(
        screen.rows.iter().flatten().all(Cell::is_blank),
        "erase-in-display should leave every cell blank"
    );
}

#[test]
fn a_reader_can_be_used_for_several_reads() {
    // The render state is reused across frames rather than rebuilt, so a
    // second read must reflect what changed rather than the first snapshot.
    let mut terminal = VtTerminal::new(Size::new(20, 5)).expect("creatable");
    let mut reader = ScreenReader::new().expect("creatable");

    terminal.feed(b"first");
    let first = reader.read(&terminal).expect("readable");
    assert_eq!(first.text_lines()[0], "first");

    terminal.feed(b"\r\nsecond");
    let second = reader.read(&terminal).expect("readable");
    assert_eq!(second.text_lines()[0], "first");
    assert_eq!(second.text_lines()[1], "second");
}

#[test]
fn a_resized_screen_reports_its_new_shape() {
    let mut terminal = VtTerminal::new(Size::new(20, 5)).expect("creatable");
    terminal.feed(b"text");
    terminal.resize(Size::new(40, 10)).expect("resizable");

    let mut reader = ScreenReader::new().expect("creatable");
    let screen = reader.read(&terminal).expect("readable");

    assert_eq!(screen.size, Size { cols: 40, rows: 10 });
    assert_eq!(screen.rows.len(), 10);
    assert!(screen.rows.iter().all(|r| r.len() == 40));
}
