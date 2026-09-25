//! Tests for the theme.

use super::*;

#[test]
fn mixing_moves_one_colour_toward_another() {
    let black = Rgb(0, 0, 0);
    let white = Rgb(255, 255, 255);

    assert_eq!(black.mix(white, 0.0), black);
    assert_eq!(black.mix(white, 1.0), white);
    assert_eq!(black.mix(white, 0.5), Rgb(128, 128, 128));
}

#[test]
fn every_role_is_mixed_from_the_palette() {
    let palette = Palette::FALLBACK;
    let theme = Theme::new(palette, Depth::TrueColor);
    let rgb = |c: Rgb| Color::Rgb(c.0, c.1, c.2);

    assert_eq!(
        theme.faded,
        rgb(palette.foreground.mix(palette.background, 0.45))
    );
    assert_eq!(
        theme.tint,
        rgb(palette.background.mix(palette.foreground, 0.10))
    );
    assert_eq!(theme.tab, rgb(palette.background.mix(palette.accent, 0.30)));
    assert_eq!(theme.accent, rgb(palette.accent));
    assert_eq!(theme.text, rgb(palette.foreground));
}

#[test]
fn a_terminal_without_24_bit_colour_gets_palette_indices() {
    let theme = Theme::new(Palette::FALLBACK, Depth::Indexed);

    for colour in [theme.faded, theme.tint, theme.tab, theme.text] {
        assert!(matches!(colour, Color::Indexed(_)), "{colour:?}");
    }
}

#[test]
fn the_accent_in_256_colours_is_palette_slot_5_itself() {
    // It was read from slot 5, so slot 5 is exact where the nearest cube
    // entry to its colour is only close.
    let theme = Theme::new(Palette::FALLBACK, Depth::Indexed);

    assert_eq!(theme.accent, Color::Indexed(5));
}

#[test]
fn colorterm_says_whether_24_bit_colour_is_there() {
    assert_eq!(Depth::from_colorterm(Some("truecolor")), Depth::TrueColor);
    assert_eq!(Depth::from_colorterm(Some("24bit")), Depth::TrueColor);
    assert_eq!(Depth::from_colorterm(Some("")), Depth::Indexed);
    assert_eq!(Depth::from_colorterm(None), Depth::Indexed);
}

#[test]
fn the_nearest_palette_entry_is_found_in_the_cube_or_the_grey_ramp() {
    assert_eq!(nearest_indexed(Rgb(0, 0, 0)), 16);
    assert_eq!(nearest_indexed(Rgb(255, 255, 255)), 231);
    assert_eq!(nearest_indexed(Rgb(255, 0, 0)), 196);
    assert_eq!(nearest_indexed(Rgb(128, 128, 128)), 244);
}

#[test]
fn replies_are_read_in_every_width_and_both_terminators() {
    let replies = Replies::parse(
        b"\x1b]10;rgb:c8c8/c8c8/d8d8\x07\
          \x1b]11;rgb:16/16/1e\x1b\\\
          \x1b]4;5;rgb:b/a/f\x07",
    );

    assert_eq!(replies.foreground, Some(Rgb(0xc8, 0xc8, 0xd8)));
    assert_eq!(replies.background, Some(Rgb(0x16, 0x16, 0x1e)));
    assert_eq!(replies.accent, Some(Rgb(0xbb, 0xaa, 0xff)));
    assert!(!replies.done, "no device attributes yet");
}

#[test]
fn an_rgba_reply_is_read_for_its_colour() {
    let replies = Replies::parse(b"\x1b]11;rgba:1616/1616/1e1e/ffff\x07");

    assert_eq!(replies.background, Some(Rgb(0x16, 0x16, 0x1e)));
}

#[test]
fn the_device_attributes_reply_ends_the_wait() {
    assert!(Replies::parse(b"\x1b[?62;22c").done);
    assert!(
        !Replies::parse(b"\x1b[12;40R").done,
        "another report is not the one that ends it"
    );
}

#[test]
fn keystrokes_and_other_sequences_between_replies_are_skipped() {
    let replies = Replies::parse(b"x\x1b[A\x1b]11;rgb:0000/0000/0000\x07q\x1b[?1;2c");

    assert_eq!(replies.background, Some(Rgb(0, 0, 0)));
    assert!(replies.done);
}

#[test]
fn a_reply_split_across_reads_is_read_once_it_is_whole() {
    let whole: &[u8] = b"\x1b]11;rgb:1616/1616/1e1e\x07\x1b[?62c";
    let first = &whole[..12];

    let partial = Replies::parse(first);
    assert_eq!(partial.background, None, "half a reply is not a colour");
    assert!(!partial.done, "and does not end the wait");

    let complete = Replies::parse(whole);
    assert_eq!(complete.background, Some(Rgb(0x16, 0x16, 0x1e)));
    assert!(complete.done);
}

#[test]
fn a_colour_not_given_is_taken_from_the_fallback() {
    let palette = Replies::parse(b"\x1b]11;rgb:ffff/ffff/ffff\x07").palette();

    assert_eq!(palette.background, Rgb(255, 255, 255));
    assert_eq!(palette.foreground, Palette::FALLBACK.foreground);
    assert_eq!(palette.accent, Palette::FALLBACK.accent);
}

#[test]
fn the_query_asks_for_all_three_colours_then_device_attributes() {
    assert_eq!(
        QUERY,
        b"\x1b]10;?\x07\x1b]11;?\x07\x1b]4;5;?\x07\x1b[c".as_slice()
    );
}

#[test]
fn a_blend_runs_between_two_colours_at_the_themes_depth() {
    let theme = Theme::fallback();
    let (from, to) = (theme.rgb(Role::Faded), theme.rgb(Role::Accent));

    assert_eq!(
        theme.blend(from, to, 0.0),
        Color::Rgb(from.0, from.1, from.2)
    );
    assert_eq!(theme.blend(from, to, 1.0), theme.accent);
    assert!(matches!(
        Theme::new(Palette::FALLBACK, Depth::Indexed).blend(from, to, 0.5),
        Color::Indexed(_)
    ));
}

#[test]
fn the_pulse_colour_is_the_background_most_of_the_way_to_the_accent() {
    let theme = Theme::fallback();
    let palette = Palette::FALLBACK;

    assert_eq!(
        theme.rgb(Role::Pulse),
        palette.background.mix(palette.accent, 0.55)
    );
    assert_eq!(theme.rgb(Role::Background), palette.background);
}
