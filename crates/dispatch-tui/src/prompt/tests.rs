//! Tests for the one-line prompt.

use super::*;

fn render(prompt: &Prompt, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    prompt.render(area, &mut buf);

    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .filter_map(|x| buf.cell((x, y)))
                .map(|c| c.symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn what_is_typed_is_what_is_shown() {
    let mut prompt = Prompt::new("Open on tower", "a directory on that machine");
    for c in "~/code".chars() {
        prompt.push(c);
    }

    let screen = render(&prompt, 80, 20);
    assert!(screen.contains("Open on tower"), "{screen}");
    assert!(screen.contains("~/code"), "{screen}");
    assert!(screen.contains("a directory on that machine"), "{screen}");
}

#[test]
fn an_answer_is_what_was_typed_without_its_edges() {
    let mut prompt = Prompt::new("t", "h");
    assert_eq!(prompt.answer(), None, "nothing typed is no answer");

    for c in "   ".chars() {
        prompt.push(c);
    }
    assert_eq!(prompt.answer(), None, "spaces alone are no answer");

    for c in "~/app ".chars() {
        prompt.push(c);
    }
    assert_eq!(prompt.answer(), Some("~/app"));
}

#[test]
fn typing_clears_an_error_but_not_work_in_progress() {
    // An error is about what was typed before; a check in progress is not.
    let mut prompt = Prompt::new("t", "h");
    prompt.set_note(Some(Note::Error("no such host".into())));
    prompt.push('a');
    assert_eq!(prompt.note(), None);

    prompt.set_note(Some(Note::Busy("checking…".into())));
    prompt.push('b');
    assert_eq!(prompt.note(), Some(&Note::Busy("checking…".into())));
}

#[test]
fn a_note_is_drawn_under_the_input() {
    let mut prompt = Prompt::new("t", "h").with_input("tower");
    prompt.set_note(Some(Note::Error("tower is already registered".into())));

    let screen = render(&prompt, 80, 20);
    assert!(screen.contains("tower is already registered"), "{screen}");
}

#[test]
fn backspace_takes_the_last_character_and_stops_at_nothing() {
    let mut prompt = Prompt::new("t", "h").with_input("ab");
    prompt.backspace();
    assert_eq!(prompt.input(), "a");
    prompt.backspace();
    prompt.backspace();
    assert_eq!(prompt.input(), "");
}

#[test]
fn a_long_input_shows_its_end() {
    // The end is where the cursor is, and where the user is typing.
    let long = format!("{}-the-end", "x".repeat(200));
    let prompt = Prompt::new("t", "h").with_input(long);

    let screen = render(&prompt, 60, 10);
    assert!(screen.contains("-the-end"), "{screen}");
}

#[test]
fn a_note_wider_than_the_hint_is_shown_whole() {
    // The box is sized to its widest line. A reason left out of that is cut
    // at the box's edge, and the part cut is usually the part that says why.
    let reason = "ssh: connect to host tower port 22: Connection refused, twice";
    let mut prompt = Prompt::new("Add a machine", "an ssh target").with_input("tower");
    prompt.set_note(Some(Note::Error(reason.into())));

    let screen = render(&prompt, 100, 30);
    assert!(screen.contains(reason), "{screen}");
}

#[test]
fn a_note_too_wide_for_the_area_wraps_rather_than_being_cut() {
    let reason = "ssh: connect to host tower.example.org port 22: Connection refused";
    let mut prompt = Prompt::new("t", "h").with_input("tower");
    prompt.set_note(Some(Note::Error(reason.into())));

    let screen = render(&prompt, 50, 20);
    for word in reason.split(' ') {
        assert!(screen.contains(word), "{word:?} is missing from\n{screen}");
    }
}

#[test]
fn a_tiny_area_draws_nothing_rather_than_panicking() {
    let prompt = Prompt::new("t", "h").with_input("abc");
    let _ = render(&prompt, 3, 2);
}

#[test]
fn a_note_wraps_between_words_and_never_inside_a_character() {
    assert_eq!(wrap("short", 10), ("short", None));
    assert_eq!(wrap("one two three", 7), ("one two", Some("three")));
    assert_eq!(wrap("one two three", 8), ("one two", Some("three")));
    assert_eq!(wrap("abcdefgh", 3), ("abc", Some("defgh")), "one long word");
    // `…` is three bytes; a cut at it must land on its edge.
    assert_eq!(wrap("ab…cd", 2), ("ab", Some("…cd")));
}
