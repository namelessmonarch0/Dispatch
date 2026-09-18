//! Tests for the list picker.

use super::*;

fn picker() -> Picker {
    Picker::new(
        "Harness",
        vec![
            Item::new("claude", "Claude Code"),
            Item::new("codex", "Codex").with_detail("codex"),
            Item::new("agy", "agy"),
        ],
    )
}

fn render(picker: &Picker, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    picker.render(area, &mut buf);
    buf
}

fn text(buf: &Buffer) -> String {
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
fn the_first_item_starts_selected() {
    let picker = picker();
    assert_eq!(picker.selected_index(), 0);
    assert_eq!(picker.selected().expect("an item").id, "claude");
}

#[test]
fn moving_down_advances_the_selection() {
    let mut picker = picker();
    picker.next();
    assert_eq!(picker.selected().expect("an item").id, "codex");
}

#[test]
fn moving_past_the_end_wraps_to_the_start() {
    // These lists are short; stopping at the bottom is more annoying than
    // useful.
    let mut picker = picker();
    picker.next();
    picker.next();
    picker.next();
    assert_eq!(picker.selected().expect("an item").id, "claude");
}

#[test]
fn moving_up_from_the_start_wraps_to_the_end() {
    let mut picker = picker();
    picker.previous();
    assert_eq!(picker.selected().expect("an item").id, "agy");
}

#[test]
fn an_empty_picker_selects_nothing_and_does_not_panic() {
    let mut picker = Picker::new("Empty", Vec::new());

    assert!(picker.is_empty());
    assert_eq!(picker.selected(), None);

    picker.next();
    picker.previous();

    assert_eq!(picker.selected(), None);
}

#[test]
fn the_title_and_every_label_are_drawn() {
    let buf = render(&picker(), 40, 10);
    let text = text(&buf);

    assert!(text.contains("Harness"), "{text}");
    assert!(text.contains("Claude Code"), "{text}");
    assert!(text.contains("Codex"), "{text}");
    assert!(text.contains("agy"), "{text}");
}

#[test]
fn the_selected_row_is_highlighted_across_its_width() {
    // A highlight only behind the text reads as an artefact rather than a
    // selection.
    let buf = render(&picker(), 40, 10);

    // Find the row holding the first label.
    let row = (0..buf.area.height)
        .find(|y| {
            (0..buf.area.width)
                .filter_map(|x| buf.cell((x, *y)))
                .map(|c| c.symbol())
                .collect::<String>()
                .contains("Claude Code")
        })
        .expect("the first label is drawn");

    let highlighted = (1..buf.area.width - 1)
        .filter(|x| buf.cell((*x, row)).expect("cell exists").bg == Color::Cyan)
        .count();

    assert!(
        highlighted > "Claude Code".len(),
        "the highlight should span the row, covered {highlighted} cells"
    );
}

#[test]
fn an_unselected_row_is_not_highlighted() {
    let buf = render(&picker(), 40, 10);
    let text_rows: Vec<String> = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .filter_map(|x| buf.cell((x, y)))
                .map(|c| c.symbol())
                .collect()
        })
        .collect();

    let row = text_rows
        .iter()
        .position(|r| r.contains("Codex"))
        .expect("the second label is drawn") as u16;

    assert_ne!(
        buf.cell((2, row)).expect("cell exists").bg,
        Color::Cyan,
        "only the selected row should be highlighted"
    );
}

#[test]
fn detail_is_drawn_after_the_label() {
    let buf = render(&picker(), 40, 10);
    let text = text(&buf);
    assert!(text.contains("Codex"), "{text}");
    assert!(text.contains("codex"), "{text}");
}

#[test]
fn the_picker_is_centred() {
    let buf = render(&picker(), 60, 20);

    // The border should not touch the edges of a much larger area.
    let top_row: String = (0..buf.area.width)
        .filter_map(|x| buf.cell((x, 0)))
        .map(|c| c.symbol())
        .collect();

    assert_eq!(
        top_row.trim(),
        "",
        "the first row should be clear of the picker"
    );
}

#[test]
fn an_empty_picker_says_so() {
    let buf = render(&Picker::new("Nothing", Vec::new()), 40, 10);
    assert!(text(&buf).contains("nothing to choose"));
}

#[test]
fn a_tiny_area_paints_nothing_rather_than_panicking() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 3, 2));
    picker().render(Rect::new(0, 0, 3, 2), &mut buf);
    assert_eq!(text(&buf).trim(), "");
}

#[test]
fn a_list_taller_than_the_box_scrolls_to_keep_the_selection_visible() {
    let items: Vec<Item> = (0..20)
        .map(|i| Item::new(format!("id{i}"), format!("item-{i}")))
        .collect();
    let mut picker = Picker::new("Long", items);

    for _ in 0..15 {
        picker.next();
    }

    let buf = render(&picker, 40, 8);
    assert!(
        text(&buf).contains("item-15"),
        "the selected row must stay visible: {}",
        text(&buf)
    );
}
