//! Tests for the title scanner.

use super::*;

/// The title a scanner reports for one write.
fn scan(bytes: &[u8]) -> Option<String> {
    TitleScanner::new().scan(bytes)
}

#[test]
fn a_bel_terminated_title_is_read() {
    assert_eq!(
        scan(b"\x1b]2;building\x07"),
        Some("building".to_string()),
        "OSC 2 is what a child uses to name its window"
    );
}

#[test]
fn a_string_terminated_title_is_read() {
    // Both terminators are in use: xterm's BEL and the standard ESC backslash.
    assert_eq!(scan(b"\x1b]2;building\x1b\\"), Some("building".to_string()));
}

#[test]
fn every_title_sequence_is_recognised() {
    // 0 sets both icon name and title, 1 the icon name, 2 the title. A child
    // using any of them is saying what to call it.
    for command in ["0", "1", "2"] {
        let bytes = format!("\x1b]{command};named\x07");
        assert_eq!(
            scan(bytes.as_bytes()),
            Some("named".to_string()),
            "OSC {command} should set a title"
        );
    }
}

#[test]
fn other_operating_system_commands_are_not_titles() {
    // OSC 8 carries a hyperlink and OSC 52 the clipboard. Neither is a name,
    // and putting either in the sidebar would be nonsense.
    assert_eq!(scan(b"\x1b]8;;https://example.com\x07"), None);
    assert_eq!(scan(b"\x1b]52;c;aGVsbG8=\x07"), None);
    assert_eq!(scan(b"\x1b]10;rgb:ff/ff/ff\x07"), None);
}

#[test]
fn a_title_split_across_writes_is_still_one_title() {
    // Output arrives in whatever chunks the pseudoterminal hands over, which
    // has nothing to do with where a sequence begins or ends.
    let mut scanner = TitleScanner::new();

    assert_eq!(scanner.scan(b"\x1b]2;bui"), None);
    assert_eq!(scanner.scan(b"lding the"), None);
    assert_eq!(
        scanner.scan(b" world\x07"),
        Some("building the world".to_string())
    );
}

#[test]
fn a_character_split_across_writes_survives() {
    let mut scanner = TitleScanner::new();

    // The two halves of a single é.
    assert_eq!(scanner.scan(b"\x1b]2;caf\xc3"), None);
    assert_eq!(scanner.scan(b"\xa9\x07"), Some("café".to_string()));
}

#[test]
fn the_newest_title_in_one_write_wins() {
    assert_eq!(
        scan(b"\x1b]2;first\x07between\x1b]2;second\x07"),
        Some("second".to_string()),
        "a caller wants what to display, not a history"
    );
}

#[test]
fn ordinary_output_is_not_mistaken_for_a_title() {
    assert_eq!(scan(b"just text\r\n"), None);
    assert_eq!(scan(b"\x1b[31mred\x1b[0m"), None, "a colour is not a title");
    assert_eq!(scan(b"\x1b]2;\x07"), None, "an empty title says nothing");
    assert_eq!(scan(b"\x1b]2;   \x07"), None, "nor does whitespace");
}

#[test]
fn a_title_that_never_ends_is_not_reported() {
    // A child that opens a sequence and keeps writing would otherwise grow the
    // buffer without limit; once too long, the name is abandoned rather than
    // truncated into something misleading.
    let mut scanner = TitleScanner::new();
    let long = vec![b'a'; MAX_TITLE + 10];

    assert_eq!(scanner.scan(b"\x1b]2;"), None);
    assert_eq!(scanner.scan(&long), None);
    assert_eq!(scanner.scan(b"\x07"), None);

    // And the scanner still works afterwards.
    assert_eq!(scanner.scan(b"\x1b]2;after\x07"), Some("after".to_string()));
}

#[test]
fn control_bytes_are_kept_out_of_a_name() {
    assert_eq!(
        scan(b"\x1b]2;two\tlines\x07"),
        Some("twolines".to_string()),
        "a sidebar row is one line of plain text"
    );
}

#[test]
fn an_interrupted_sequence_does_not_leak_into_the_next() {
    let mut scanner = TitleScanner::new();

    // ESC followed by something other than a backslash abandons the sequence.
    assert_eq!(scanner.scan(b"\x1b]2;abandoned\x1bA"), None);
    assert_eq!(
        scanner.scan(b"\x1b]2;proper\x07"),
        Some("proper".to_string())
    );
}
