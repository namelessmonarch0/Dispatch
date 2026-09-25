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

fn signals(bytes: &[u8]) -> Signals {
    TitleScanner::new().scan_signals(bytes)
}

#[test]
fn a_bare_bell_is_reported() {
    assert!(signals(b"done\x07").bell);
}

#[test]
fn a_bell_that_ends_a_title_is_not_a_bell() {
    let found = signals(b"\x1b]0;Claude Code\x07");

    assert_eq!(found.title.as_deref(), Some("Claude Code"));
    assert!(!found.bell, "BEL terminated the sequence, it did not ring");
}

#[test]
fn a_progress_report_is_read_after_its_nine() {
    assert_eq!(
        signals(b"\x1b]9;4;1;40\x07").progress.as_deref(),
        Some("4;1;40")
    );
    assert_eq!(
        signals(b"\x1b]9;4;0\x1b\\").progress.as_deref(),
        Some("4;0"),
        "either terminator ends it"
    );
}

#[test]
fn a_notification_is_not_progress() {
    // `OSC 9` alone is iTerm2's notification; only `9;4` is progress.
    assert_eq!(signals(b"\x1b]9;build finished\x07").progress, None);
}

#[test]
fn a_progress_report_split_across_writes_is_still_read() {
    let mut scanner = TitleScanner::new();

    assert_eq!(scanner.scan_signals(b"\x1b]9;4;").progress, None);
    assert_eq!(
        scanner.scan_signals(b"1;75\x07").progress.as_deref(),
        Some("4;1;75")
    );
}

#[test]
fn a_title_a_progress_report_and_a_bell_arrive_together() {
    let found = signals(b"\x1b]2;\xe2\xa0\x8b working\x07\x1b]9;4;3\x07ready\x07");

    assert_eq!(found.title.as_deref(), Some("\u{280b} working"));
    assert_eq!(found.progress.as_deref(), Some("4;3"));
    assert!(found.bell);
}

#[test]
fn scan_still_returns_only_the_title() {
    let mut scanner = TitleScanner::new();

    assert_eq!(
        scanner
            .scan(b"\x1b]9;4;1;10\x07\x1b]0;name\x07\x07")
            .as_deref(),
        Some("name")
    );
}
