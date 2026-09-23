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
fn a_tiny_area_draws_nothing_rather_than_panicking() {
    let prompt = Prompt::new("t", "h").with_input("abc");
    let _ = render(&prompt, 3, 2);
}
