//! Tests for key encoding.
//!
//! Assertions are against the bytes a child would receive, because that is the
//! contract. The expected sequences are the standard ones a terminal sends in
//! its default mode.

use super::*;

use crate::vt::Size;

/// A terminal in its default keyboard mode.
fn terminal() -> VtTerminal {
    VtTerminal::new(Size::new(80, 24)).expect("a terminal can be created")
}

fn encode(terminal: &VtTerminal, key: Key, mods: Modifiers) -> Vec<u8> {
    KeyEncoder::new()
        .expect("an encoder can be created")
        .encode(terminal, key, mods)
        .expect("encoding succeeds")
}

fn encode_plain(key: Key) -> Vec<u8> {
    encode(&terminal(), key, Modifiers::NONE)
}

#[test]
fn a_letter_encodes_as_itself() {
    assert_eq!(encode_plain(Key::Char('a')), b"a");
    assert_eq!(encode_plain(Key::Char('Z')), b"Z");
}

#[test]
fn a_digit_encodes_as_itself() {
    assert_eq!(encode_plain(Key::Char('7')), b"7");
}

#[test]
fn punctuation_encodes_as_itself() {
    assert_eq!(encode_plain(Key::Char('/')), b"/");
    assert_eq!(encode_plain(Key::Char('-')), b"-");
    assert_eq!(encode_plain(Key::Char(' ')), b" ");
}

#[test]
fn control_letters_encode_as_control_codes() {
    // Ctrl-C must be 0x03 or agents cannot be interrupted.
    let mods = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };

    assert_eq!(encode(&terminal(), Key::Char('c'), mods), vec![0x03]);
    assert_eq!(encode(&terminal(), Key::Char('d'), mods), vec![0x04]);
    assert_eq!(encode(&terminal(), Key::Char('a'), mods), vec![0x01]);
}

#[test]
fn enter_encodes_as_carriage_return() {
    // A newline instead would make shells and agents see a half-submitted line.
    assert_eq!(encode_plain(Key::Enter), b"\r");
}

#[test]
fn tab_and_backspace_encode_as_their_control_codes() {
    assert_eq!(encode_plain(Key::Tab), vec![0x09]);
    assert_eq!(encode_plain(Key::Backspace), vec![0x7f]);
}

#[test]
fn escape_encodes_as_escape() {
    assert_eq!(encode_plain(Key::Escape), vec![0x1b]);
}

#[test]
fn arrows_encode_as_cursor_sequences() {
    assert_eq!(encode_plain(Key::Up), b"\x1b[A");
    assert_eq!(encode_plain(Key::Down), b"\x1b[B");
    assert_eq!(encode_plain(Key::Right), b"\x1b[C");
    assert_eq!(encode_plain(Key::Left), b"\x1b[D");
}

#[test]
fn navigation_keys_encode_as_their_sequences() {
    assert_eq!(encode_plain(Key::Home), b"\x1b[H");
    assert_eq!(encode_plain(Key::End), b"\x1b[F");
    assert_eq!(encode_plain(Key::Insert), b"\x1b[2~");
    assert_eq!(encode_plain(Key::Delete), b"\x1b[3~");
    assert_eq!(encode_plain(Key::PageUp), b"\x1b[5~");
    assert_eq!(encode_plain(Key::PageDown), b"\x1b[6~");
}

#[test]
fn function_keys_encode_as_their_sequences() {
    assert_eq!(encode_plain(Key::Function(1)), b"\x1bOP");
    assert_eq!(encode_plain(Key::Function(5)), b"\x1b[15~");
}

#[test]
fn alt_prefixes_a_letter_with_escape() {
    let mods = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };

    assert_eq!(encode(&terminal(), Key::Char('b'), mods), b"\x1bb");
}

#[test]
fn application_cursor_mode_changes_the_arrows() {
    // The child sets DECCKM, and the very next arrow must be encoded its way.
    // Getting this wrong breaks arrow keys inside anything full-screen.
    let mut terminal = terminal();
    assert_eq!(
        encode(&terminal, Key::Up, Modifiers::NONE),
        b"\x1b[A",
        "normal cursor mode"
    );

    terminal.feed(b"\x1b[?1h");

    assert_eq!(
        encode(&terminal, Key::Up, Modifiers::NONE),
        b"\x1bOA",
        "application cursor mode"
    );
}

#[test]
fn leaving_application_cursor_mode_restores_the_arrows() {
    let mut terminal = terminal();
    terminal.feed(b"\x1b[?1h");
    terminal.feed(b"\x1b[?1l");

    assert_eq!(encode(&terminal, Key::Up, Modifiers::NONE), b"\x1b[A");
}

#[test]
fn the_kitty_protocol_changes_the_encoding() {
    // Claude Code and friends negotiate this. The encoder follows the terminal
    // rather than assuming, which is the whole reason it is configured from it.
    let mut terminal = terminal();
    let plain = encode(&terminal, Key::Char('a'), Modifiers::NONE);

    // Flag 8 reports all keys as escape codes. Flag 1 is disambiguation
    // only and leaves a plain letter alone, so it would not show the
    // encoder following the terminal.
    terminal.feed(b"\x1b[>8u");
    let kitty = encode(&terminal, Key::Char('a'), Modifiers::NONE);

    assert_ne!(
        plain, kitty,
        "enabling the kitty keyboard protocol should change the encoding"
    );
}

#[test]
fn one_encoder_serves_many_keystrokes() {
    // Encoders are per pane and live as long as it does.
    let terminal = terminal();
    let mut encoder = KeyEncoder::new().expect("creatable");

    for key in [Key::Char('h'), Key::Char('i'), Key::Enter] {
        let bytes = encoder
            .encode(&terminal, key, Modifiers::NONE)
            .expect("encoding succeeds");
        assert!(!bytes.is_empty(), "{key:?} produced nothing");
    }
}

#[test]
fn an_unmapped_character_still_sends_its_text() {
    // Not a key the enum names, but the child should still receive it.
    assert_eq!(encode_plain(Key::Char('é')), "é".as_bytes());
}

#[test]
fn an_out_of_range_function_key_does_not_panic() {
    let _ = encode_plain(Key::Function(99));
}
