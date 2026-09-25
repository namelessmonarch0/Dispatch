//! Tests for the project sidebar.

use super::*;

use crate::theme::Theme;
use dispatch_core::{Device, DeviceId, HarnessId, Pane, PaneId, Project, ProjectSource};

/// State with two projects; the first is selected.
fn state() -> (AppState, ProjectId, ProjectId) {
    let mut state = AppState::new();
    let alpha = state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir));
    let beta = state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));
    (state, alpha, beta)
}

fn spawn(state: &mut AppState, project: ProjectId, harness: &str) -> PaneId {
    state
        .spawn_pane(project, HarnessId::new(harness))
        .expect("the project exists")
}

fn render(state: &AppState, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    Sidebar::new(state).render(area, &mut buf);
    buf
}

fn row_text(buf: &Buffer, y: u16) -> String {
    let line: String = (0..buf.area.width)
        .filter_map(|x| buf.cell((x, y)))
        .map(|c| c.symbol())
        .collect();
    line.trim_end().to_string()
}

fn all_text(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders `state` and returns each row's text, trailing whitespace trimmed
/// but leading whitespace kept: indentation is what tells a child row apart
/// from its parent.
fn render_lines(state: &AppState, width: u16, height: u16) -> Vec<String> {
    let buf = render(state, width, height);
    (0..buf.area.height).map(|y| row_text(&buf, y)).collect()
}

/// The first row inside the frame, and the first column inside it.
///
/// Every row assertion is relative to these: the frame's own edge is not part
/// of the list.
const TOP: u16 = 1;
const LEFT: u16 = 1;

/// Which column `text` starts in.
///
/// Measured by the text itself rather than by counting leading spaces: a
/// pane's twisty is a blank when it has nothing to fold, so
/// leading-whitespace counting cannot tell "no twisty" apart from "less
/// indented".
fn column_of(line: &str, text: &str) -> usize {
    let byte = line
        .find(text)
        .unwrap_or_else(|| panic!("expected {text:?} in {line:?}"));
    line[..byte].chars().count()
}

/// What a click at `(x, y)` finds, with nothing scrolled.
fn hit(state: &AppState, area: Rect, x: u16, y: u16) -> Option<Hit> {
    hit_test(state, area, &Scroll::new(), x, y)
}

#[test]
fn projects_are_listed_in_the_order_they_were_added() {
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 10);

    assert!(row_text(&buf, TOP).contains("alpha"));
    assert!(row_text(&buf, TOP + 1).contains("beta"));
}

#[test]
fn panes_are_listed_under_their_project() {
    let (mut state, alpha, beta) = state();
    spawn(&mut state, alpha, "claude");
    spawn(&mut state, beta, "codex");

    let buf = render(&state, WIDTH, 10);

    assert!(row_text(&buf, TOP).contains("alpha"));
    assert!(row_text(&buf, TOP + 1).contains("claude"));
    assert!(row_text(&buf, TOP + 2).contains("beta"));
    assert!(row_text(&buf, TOP + 3).contains("codex"));
}

#[test]
fn panes_of_an_unselected_project_are_still_listed() {
    // The sidebar is how you find work running elsewhere, so it shows every
    // project's panes even though only one project's are on screen.
    let (mut state, _, beta) = state();
    spawn(&mut state, beta, "opencode");

    let text = all_text(&render(&state, WIDTH, 10));
    assert!(text.contains("opencode"));
}

#[test]
fn the_selected_project_is_emphasised() {
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 10);

    let selected = buf.cell((LEFT + NAME, TOP)).expect("cell exists");
    let unselected = buf.cell((LEFT + NAME, TOP + 1)).expect("cell exists");

    assert!(selected.modifier.contains(Modifier::BOLD));
    assert!(!unselected.modifier.contains(Modifier::BOLD));
}

#[test]
fn the_focused_pane_is_tinted() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");
    spawn(&mut state, alpha, "codex");

    // Spawning focuses the new pane, so codex is focused.
    let buf = render(&state, WIDTH, 10);
    let tint = Theme::fallback().tint;

    assert_ne!(
        buf.cell((LEFT + 8, TOP + 1)).expect("cell exists").bg,
        tint,
        "claude is not focused"
    );
    for x in LEFT + 4..WIDTH - 1 {
        assert_eq!(
            buf.cell((x, TOP + 2)).expect("cell exists").bg,
            tint,
            "column {x} of the focused row is tinted"
        );
    }
    assert!(
        !row_text(&buf, TOP + 2).contains('▌'),
        "and not marked with a bar"
    );
}

#[test]
fn a_focused_panes_text_is_the_palettes_foreground() {
    // The tint is mixed from the palette, which is the fallback's when the
    // terminal did not say what its own is. Text left in the terminal's
    // colour could then be a light theme's dark text on a dark tint.
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let buf = render(&state, WIDTH, 10);
    let theme = Theme::fallback();

    assert_eq!(
        buf.cell((LEFT + PANE + NAME, TOP + 1))
            .expect("cell exists")
            .fg,
        theme.text,
        "the title"
    );
    assert_eq!(
        buf.cell((WIDTH - 3, TOP + 1)).expect("cell exists").fg,
        Color::Yellow,
        "the state glyph keeps its own colour"
    );
}

#[test]
fn a_selected_projects_text_is_the_palettes_foreground() {
    let (state, _, _) = state();

    let buf = render(&state, WIDTH, 10);

    assert_eq!(
        buf.cell((LEFT + NAME, TOP)).expect("cell exists").fg,
        Theme::fallback().text
    );
}

#[test]
fn a_long_name_is_truncated_rather_than_overflowing() {
    let mut state = AppState::new();
    state.add_project(
        Project::new("/tmp/x", ProjectSource::LocalDir)
            .with_name("a-very-long-project-name-that-will-not-fit"),
    );

    let buf = render(&state, 20, 5);
    let line = row_text(&buf, TOP);

    assert!(line.chars().count() <= 20, "line overflowed: {line:?}");
    assert!(line.contains('…'), "truncation should be visible: {line:?}");
}

#[test]
fn a_wide_name_is_cut_by_the_columns_it_takes() {
    // Four CJK characters take eight columns; counting them as four would
    // let the row run past the frame.
    let mut state = AppState::new();
    state.add_project(
        Project::new("/tmp/x", ProjectSource::LocalDir).with_name("日本語のプロジェクト名前"),
    );

    let buf = render(&state, 20, 5);
    let line = row_text(&buf, TOP);

    assert!(
        line.ends_with('│'),
        "the frame is not overwritten: {line:?}"
    );
    assert!(line.contains('…'), "the cut is visible: {line:?}");
}

#[test]
fn rendering_stops_at_the_bottom_of_the_area() {
    // More projects than rows must not panic or paint outside.
    let mut state = AppState::new();
    for i in 0..20 {
        state.add_project(Project::new(format!("/tmp/p{i}"), ProjectSource::LocalDir));
    }

    let buf = render(&state, WIDTH, 3);

    assert_eq!(buf.area.height, 3);
    assert!(row_text(&buf, TOP).contains("p0"));
}

#[test]
fn an_empty_state_lists_nothing() {
    // The frame is still drawn — it is part of the layout, not of the list.
    let state = AppState::new();
    let buf = render(&state, WIDTH, 5);

    for y in TOP..4 {
        assert_eq!(
            row_text(&buf, y).trim_matches('│').trim(),
            "",
            "row {y} lists nothing"
        );
    }
}

#[test]
fn a_zero_sized_area_paints_nothing() {
    let (state, _, _) = state();
    let mut buf = Buffer::empty(Rect::new(0, 0, WIDTH, 5));

    Sidebar::new(&state).render(Rect::new(0, 0, 0, 5), &mut buf);

    assert_eq!(all_text(&buf).trim(), "");
}

#[test]
fn a_subagent_is_listed_under_the_pane_that_asked_for_it() {
    let mut state = AppState::new();
    let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
    let parent = state
        .spawn_pane(project, HarnessId::new("claude"))
        .expect("the project exists");
    state
        .set_pane_title(parent, "Claude Code")
        .expect("it exists");

    let mut child = Pane::new(project, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.title = "tests".into();
    state.adopt_pane(child).expect("the project exists");

    let lines = render_lines(&state, 28, 6);

    let parent_row = lines
        .iter()
        .position(|l| l.contains("Claude Code"))
        .expect("the parent is listed");
    let child_row = lines
        .iter()
        .position(|l| l.contains("tests"))
        .expect("the child is listed");

    assert!(child_row > parent_row, "a child comes after its parent");
    assert!(
        column_of(&lines[child_row], "tests") > column_of(&lines[parent_row], "Claude Code"),
        "and is indented under it: {:?}",
        lines[child_row]
    );
}

#[test]
fn a_tombstone_says_it_is_closed_and_still_shows_its_children() {
    let mut state = AppState::new();
    let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
    let parent = state
        .spawn_pane(project, HarnessId::new("claude"))
        .expect("the project exists");
    state
        .set_pane_title(parent, "Claude Code")
        .expect("it exists");

    let mut child = Pane::new(project, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.durable = true;
    child.title = "bench".into();
    state.adopt_pane(child).expect("the project exists");

    state.close_pane(parent).expect("the pane exists");
    let lines = render_lines(&state, 28, 6);

    assert!(
        lines
            .iter()
            .any(|l| l.contains("Claude Code") && l.contains(CLOSED)),
        "the closed parent is marked as such: {lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("bench")),
        "its surviving child is still reachable: {lines:#?}"
    );
}

#[test]
fn a_click_on_a_pane_row_finds_that_pane() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");

    let area = Rect::new(0, 0, WIDTH, 10);
    assert_eq!(hit(&state, area, LEFT + 4, TOP + 1), Some(Hit::Pane(pane)));
}

#[test]
fn a_click_on_a_child_row_finds_the_child_rather_than_its_parent() {
    let (mut state, alpha, _) = state();
    let parent = spawn(&mut state, alpha, "claude");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(parent);
    let child = state.adopt_pane(child).expect("the project exists");

    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(
        hit(&state, area, LEFT + 6, TOP + 1),
        Some(Hit::Pane(parent))
    );
    assert_eq!(hit(&state, area, LEFT + 6, TOP + 2), Some(Hit::Pane(child)));
}

#[test]
fn a_click_anywhere_on_a_project_heading_finds_the_project() {
    // The heading names no pane, so the whole row is the project's control:
    // clicking it selects the project and folds its panes away.
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit(&state, area, LEFT, TOP), Some(Hit::Project(alpha)));
    assert_eq!(hit(&state, area, WIDTH - 2, TOP), Some(Hit::Project(alpha)));
}

#[test]
fn a_click_on_a_closed_panes_tombstone_finds_nothing() {
    // Its row is still drawn, for its surviving children's sake, but there is
    // no live pane behind it to bring into the grid.
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(pane);
    child.durable = true;
    state.adopt_pane(child).expect("the project exists");
    state.close_pane(pane).expect("the pane exists");

    let area = Rect::new(0, 0, WIDTH, 10);
    assert_eq!(hit(&state, area, LEFT + 8, TOP + 1), None);
}

#[test]
fn a_click_outside_the_sidebars_area_finds_nothing() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let area = Rect::new(0, 0, WIDTH, 10);
    assert_eq!(hit(&state, area, WIDTH + 5, TOP + 1), None);
    assert_eq!(hit(&state, area, LEFT + 4, 20), None);
}

#[test]
fn the_list_is_framed_and_titled() {
    // The sidebar abuts a pane's own output, and without an edge between them
    // a project name reads as a line the agent printed.
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);

    assert_eq!(buf.cell((0, 0)).expect("cell exists").symbol(), "┌");
    assert_eq!(buf.cell((WIDTH - 1, 0)).expect("cell exists").symbol(), "┐");
    assert_eq!(buf.cell((0, 5)).expect("cell exists").symbol(), "└");
    assert!(
        row_text(&buf, 0).contains("Projects"),
        "the frame is titled: {:?}",
        row_text(&buf, 0)
    );
}

#[test]
fn rows_are_drawn_inside_the_frame() {
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);

    assert!(
        row_text(&buf, 1).contains("alpha"),
        "the first project sits on the frame's first inner row: {:?}",
        row_text(&buf, 1)
    );
    assert_eq!(
        buf.cell((0, 1)).expect("cell exists").symbol(),
        "│",
        "the frame's own column is not written over"
    );
}

#[test]
fn the_selected_projects_whole_row_is_tinted() {
    // Emphasis on the name alone is easy to miss in a list of directory names
    // that already look alike. The tint runs the width of the list so the eye
    // finds it without reading.
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);
    let tint = Theme::fallback().tint;

    for x in LEFT..WIDTH - 1 {
        let cell = buf.cell((x, TOP)).expect("cell exists");
        assert_eq!(cell.bg, tint, "column {x} of the selected row is tinted");
        assert!(
            !cell.modifier.contains(Modifier::REVERSED),
            "and not inverted"
        );
    }

    assert_ne!(
        buf.cell((LEFT, TOP + 1)).expect("cell exists").bg,
        tint,
        "an unselected project carries no tint"
    );
}

#[test]
fn the_frame_is_not_painted_by_the_highlight() {
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);

    assert_ne!(
        buf.cell((0, TOP)).expect("cell exists").bg,
        Theme::fallback().tint,
        "the tint stops at the frame"
    );
}

#[test]
fn every_icon_has_a_blank_column_after_it() {
    // A Nerd Font glyph is routinely drawn wider than its cell; with nothing
    // after it, it runs into the next glyph or the first letter of the name.
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let buf = render(&state, WIDTH, 6);
    let blank = |x: u16, y: u16| buf.cell((x, y)).expect("cell exists").symbol() == " ";

    // The project: twisty, blank, folder, blank, name.
    assert!(blank(LEFT + 1, TOP) && blank(LEFT + 3, TOP));
    // The pane: twisty, blank, harness icon, blank, title.
    assert!(blank(LEFT + 5, TOP + 1) && blank(LEFT + 7, TOP + 1));
}

#[test]
fn the_state_glyph_keeps_a_blank_between_it_and_the_frame() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        buf.cell((WIDTH - 2, TOP + 1))
            .expect("cell exists")
            .symbol(),
        " "
    );
    assert_eq!(
        buf.cell((WIDTH - 1, TOP + 1))
            .expect("cell exists")
            .symbol(),
        "│"
    );
}

#[test]
fn a_project_with_panes_carries_a_twisty() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let buf = render(&state, WIDTH, 6);
    assert_eq!(buf.cell((LEFT, TOP)).expect("cell exists").symbol(), OPEN);

    state.toggle_project_collapsed(alpha);
    let buf = render(&state, WIDTH, 6);
    assert_eq!(buf.cell((LEFT, TOP)).expect("cell exists").symbol(), SHUT);
}

#[test]
fn a_project_with_no_panes_carries_no_twisty() {
    // There is nothing to hide, so a control that toggles nothing is a lie.
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);

    assert_eq!(buf.cell((LEFT, TOP)).expect("cell exists").symbol(), " ");
}

#[test]
fn a_pane_with_children_carries_a_twisty_and_one_without_does_not() {
    let (mut state, alpha, _) = state();
    let parent = spawn(&mut state, alpha, "claude");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(parent);
    state.adopt_pane(child).expect("the project exists");
    let lonely = spawn(&mut state, alpha, "codex");

    let buf = render(&state, WIDTH, 8);
    let twisty = |y: u16| {
        (LEFT..WIDTH)
            .map(|x| buf.cell((x, y)).expect("cell exists").symbol().to_string())
            .find(|s| s == OPEN || s == SHUT)
    };

    assert_eq!(twisty(TOP + 1), Some(OPEN.into()), "the parent has one");
    assert_eq!(twisty(TOP + 3), None, "a childless pane has none");

    state.toggle_pane_collapsed(parent);
    let buf = render(&state, WIDTH, 8);
    assert_eq!(
        buf.cell((LEFT + 4, TOP + 1)).expect("cell exists").symbol(),
        SHUT,
        "and it flips when collapsed"
    );
    let _ = lonely;
}

#[test]
fn a_collapsed_project_hides_its_panes() {
    let (mut state, alpha, beta) = state();
    spawn(&mut state, alpha, "claude");
    spawn(&mut state, beta, "codex");

    state.toggle_project_collapsed(alpha);
    let lines = render_lines(&state, WIDTH, 8);
    let text = lines.join("\n");

    assert!(!text.contains("claude"), "its panes are hidden: {lines:#?}");
    assert!(
        text.contains("alpha"),
        "the project itself stays: {lines:#?}"
    );
    assert!(
        text.contains("codex"),
        "another project is unaffected: {lines:#?}"
    );
}

#[test]
fn a_collapsed_project_hides_its_subagents_too() {
    // The children hang off a pane that is itself hidden, so leaving them
    // drawn would strand them under nothing.
    let (mut state, alpha, _) = state();
    let parent = spawn(&mut state, alpha, "claude");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.title = "tests".into();
    state.adopt_pane(child).expect("the project exists");

    state.toggle_project_collapsed(alpha);
    let text = render_lines(&state, WIDTH, 8).join("\n");

    assert!(!text.contains("tests"), "the subagent is hidden: {text:?}");
}

#[test]
fn a_collapsed_pane_hides_its_children_and_keeps_its_own_row() {
    let (mut state, alpha, _) = state();
    let parent = spawn(&mut state, alpha, "claude");
    state
        .set_pane_title(parent, "Claude Code")
        .expect("it exists");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.title = "tests".into();
    state.adopt_pane(child).expect("the project exists");

    state.toggle_pane_collapsed(parent);
    let text = render_lines(&state, WIDTH, 8).join("\n");

    assert!(text.contains("Claude Code"), "the parent stays: {text:?}");
    assert!(!text.contains("tests"), "its child is hidden: {text:?}");
}

#[test]
fn a_click_lands_on_the_row_below_a_collapsed_project() {
    // Rendering and hit testing walk the same rows, or a click answers for a
    // row the user cannot see.
    let (mut state, alpha, beta) = state();
    spawn(&mut state, alpha, "claude");
    let codex = spawn(&mut state, beta, "codex");

    state.toggle_project_collapsed(alpha);
    let area = Rect::new(0, 0, WIDTH, 10);

    // alpha, then beta's heading, then codex.
    assert_eq!(hit(&state, area, LEFT + 4, TOP + 2), Some(Hit::Pane(codex)));
}

#[test]
fn a_click_on_a_panes_twisty_toggles_it_rather_than_focusing_it() {
    let (mut state, alpha, _) = state();
    let parent = spawn(&mut state, alpha, "claude");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(parent);
    state.adopt_pane(child).expect("the project exists");

    let area = Rect::new(0, 0, WIDTH, 10);

    // The twisty sits in the row's first column, four in from the list's edge.
    assert_eq!(
        hit(&state, area, LEFT + 4, TOP + 1),
        Some(Hit::Twisty(parent))
    );
    assert_eq!(
        hit(&state, area, LEFT + 5, TOP + 1),
        Some(Hit::Pane(parent)),
        "the rest of the row still focuses the pane"
    );
}

#[test]
fn a_click_on_a_childless_panes_twisty_column_focuses_it() {
    // Nothing is drawn in that column, so there is no control to hit.
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");

    let area = Rect::new(0, 0, WIDTH, 10);
    assert_eq!(hit(&state, area, LEFT + 4, TOP + 1), Some(Hit::Pane(pane)));
}

#[test]
fn a_tombstones_twisty_still_toggles() {
    // A tombstone exists only to hold its children, so folding them away is
    // the one thing its row can still do.
    let (mut state, alpha, _) = state();
    let parent = spawn(&mut state, alpha, "claude");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.durable = true;
    state.adopt_pane(child).expect("the project exists");
    state.close_pane(parent).expect("the pane exists");

    let area = Rect::new(0, 0, WIDTH, 10);
    assert_eq!(
        hit(&state, area, LEFT + 4, TOP + 1),
        Some(Hit::Twisty(parent))
    );
}

#[test]
fn the_twisty_is_a_nerd_font_caret() {
    // The geometric triangles are East-Asian-ambiguous, which several
    // terminals render two cells wide — and a two-cell glyph in a one-cell
    // column pushes the whole row out of line.
    assert_eq!(OPEN, "\u{f0d7}");
    assert_eq!(SHUT, "\u{f0da}");
}

/// Two columns in from the frame, where a pane's state is drawn.
fn state_cell(buf: &Buffer, y: u16) -> (String, Color) {
    let cell = buf
        .cell((buf.area.width - 3, y))
        .expect("the row has a last column");
    (cell.symbol().to_string(), cell.fg)
}

#[test]
fn a_panes_state_is_one_glyph_at_the_end_of_its_row() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state
        .set_pane_status(pane, PaneStatus::Running)
        .expect("pane exists");

    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        state_cell(&buf, TOP + 1),
        (RUNNING.to_string(), Color::Green)
    );
}

#[test]
fn every_state_has_a_glyph_of_its_own() {
    // The dot said only "something is happening" and the outcome column said
    // the rest. One glyph per state is one place to look.
    let glyphs = [
        STARTING, RUNNING, IDLE, DONE, FAILED, CLOSED, BLOCKED, UNSEEN,
    ];

    for (i, glyph) in glyphs.iter().enumerate() {
        for other in &glyphs[i + 1..] {
            assert_ne!(glyph, other, "two states share a glyph");
        }
    }
}

#[test]
fn a_finished_pane_says_how_it_finished() {
    let (mut state, alpha, _) = state();
    let clean = spawn(&mut state, alpha, "claude");
    let failed = spawn(&mut state, alpha, "codex");
    state
        .set_pane_status(clean, PaneStatus::Exited(0))
        .expect("pane exists");
    state
        .set_pane_status(failed, PaneStatus::Exited(1))
        .expect("pane exists");

    let buf = render(&state, WIDTH, 6);

    assert_eq!(state_cell(&buf, TOP + 1).0, DONE);
    assert_eq!(state_cell(&buf, TOP + 2), (FAILED.to_string(), Color::Red));
}

#[test]
fn a_tombstone_says_it_is_closed() {
    let (mut state, alpha, _) = state();
    let parent = spawn(&mut state, alpha, "claude");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.durable = true;
    state.adopt_pane(child).expect("the project exists");
    state.close_pane(parent).expect("the pane exists");

    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        state_cell(&buf, TOP + 1),
        (CLOSED.to_string(), Theme::fallback().faded)
    );
}

#[test]
fn no_row_carries_a_status_dot() {
    // The state glyph carries the whole story now; a dot beside it was the
    // same fact twice.
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let text = all_text(&render(&state, WIDTH, 6));

    assert!(!text.contains('●'), "no dots: {text}");
}

/// A registry holding one harness, with `icon` as its mark.
fn registry(id: &str, icon: &str) -> dispatch_config::HarnessRegistry {
    let def = dispatch_config::HarnessDef {
        id: id.to_string(),
        display_name: id.to_string(),
        icon: Some(icon.to_string()),
        ..dispatch_config::HarnessDef::default()
    };

    [def].into_iter().collect()
}

#[test]
fn a_pane_is_marked_with_the_icon_of_its_harness() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let harnesses = registry("claude", "C");
    let area = Rect::new(0, 0, WIDTH, 6);
    let mut buf = Buffer::empty(area);
    Sidebar::new(&state)
        .with_harnesses(&harnesses)
        .render(area, &mut buf);

    // Two columns in from the row's own start, past its twisty and a blank.
    assert_eq!(
        buf.cell((LEFT + 4 + 2, TOP + 1))
            .expect("cell exists")
            .symbol(),
        "C"
    );
}

#[test]
fn a_pane_whose_harness_is_unregistered_is_marked_generically() {
    // A pane adopted from a daemon can name a harness this client has no file
    // for, and a blank column there would read as a broken row.
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "something-else");

    let harnesses = registry("claude", "C");
    let area = Rect::new(0, 0, WIDTH, 6);
    let mut buf = Buffer::empty(area);
    Sidebar::new(&state)
        .with_harnesses(&harnesses)
        .render(area, &mut buf);

    assert_eq!(
        buf.cell((LEFT + 4 + 2, TOP + 1))
            .expect("cell exists")
            .symbol(),
        dispatch_config::harness::DEFAULT_ICON
    );
}

#[test]
fn a_plain_directory_is_a_folder_and_a_repository_is_marked_as_one() {
    let mut state = AppState::new();
    state.add_project(Project::new("/tmp/plain", ProjectSource::LocalDir));
    state.add_project(Project::new(
        "/tmp/repo",
        ProjectSource::GitRepo { remote: None },
    ));

    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        buf.cell((LEFT + 2, TOP)).expect("cell exists").symbol(),
        SHUT_FOLDER
    );
    assert_eq!(
        buf.cell((LEFT + 2, TOP + 1)).expect("cell exists").symbol(),
        REPOSITORY
    );
}

#[test]
fn a_directory_whose_branch_is_known_is_marked_as_a_repository() {
    // Opened below its repository's root — a directory in a monorepo, or
    // one `git init` reached later — a project is recorded as a plain
    // directory, but its branch is found by walking up. The mark follows
    // the branch, so it never sits above a branch line as a folder.
    let mut state = AppState::new();
    state.add_project(
        Project::new("/tmp/repo/sub", ProjectSource::LocalDir).with_branch(Some("main".into())),
    );

    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        buf.cell((LEFT + 2, TOP)).expect("cell exists").symbol(),
        REPOSITORY
    );
    assert_eq!(
        column_of(&row_text(&buf, TOP + 1), "main"),
        usize::from(LEFT + NAME),
        "with its branch beneath"
    );
}

/// State with two devices, each holding one project.
fn fleet() -> (AppState, DeviceId, DeviceId) {
    let mut state = AppState::new();
    let laptop = state.add_device(Device::new("laptop"));
    let tower = state.add_device(Device::new("tower"));

    state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir).with_device(laptop));
    state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir).with_device(tower));

    (state, laptop, tower)
}

#[test]
fn one_machine_draws_no_name_line() {
    // The ordinary case. A line naming this machine would cost a row to say
    // what the user already knows.
    let mut state = AppState::new();
    let laptop = state.add_device(Device::new("laptop"));
    state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir).with_device(laptop));

    let lines = render_lines(&state, WIDTH, 8);

    assert!(
        lines[TOP as usize].contains("alpha"),
        "the project is the first row: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("laptop")),
        "and the machine is not drawn at all: {lines:#?}"
    );
}

#[test]
fn several_machines_each_get_a_section_named_on_the_line_above_it() {
    // Ten rows: frame 0 and 9, one divider, seven shared between two equal
    // weights — two each, then three split 2/1 by largest remainder.
    let (state, _, _) = fleet();
    let lines = render_lines(&state, WIDTH, 10);

    assert!(
        lines[0].starts_with('┌') && lines[0].contains("laptop"),
        "{lines:#?}"
    );
    assert!(lines[1].contains("alpha"), "{lines:#?}");
    assert!(
        lines[5].starts_with('├') && lines[5].contains("tower") && lines[5].ends_with('┤'),
        "{lines:#?}"
    );
    assert!(lines[6].contains("beta"), "{lines:#?}");
    assert!(lines[9].starts_with('└'), "{lines:#?}");
}

#[test]
fn a_machine_with_more_open_panes_gets_more_of_the_height() {
    let (mut state, laptop, _) = fleet();
    let alpha = state
        .projects()
        .iter()
        .find(|project| project.device == laptop)
        .expect("the laptop has a project")
        .id;
    for _ in 0..3 {
        spawn(&mut state, alpha, "claude");
    }

    // Twenty rows: eighteen inside, one divider, seventeen shared by weights
    // four and one — two each, then thirteen split 10.4/2.6, the spare row
    // going to the larger remainder: twelve and five.
    let lines = render_lines(&state, WIDTH, 20);
    let divider = lines
        .iter()
        .position(|line| line.contains("tower"))
        .expect("the second machine is named");

    assert_eq!(divider, 13, "{lines:#?}");
}

#[test]
fn a_folded_machine_keeps_only_the_line_naming_it() {
    let (mut state, laptop, _) = fleet();

    state.toggle_device_collapsed(laptop);
    let lines = render_lines(&state, WIDTH, 10);
    let text = lines.join("\n");

    assert!(!text.contains("alpha"), "{text}");
    assert!(lines[0].contains("laptop"), "the machine stays: {text}");
    assert!(
        lines[1].contains("tower"),
        "its section is only that line: {text}"
    );
    assert!(
        text.contains("beta"),
        "the other machine is unaffected: {text}"
    );
}

#[test]
fn a_click_on_a_machines_name_finds_the_machine() {
    let (state, laptop, tower) = fleet();
    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit(&state, area, 3, 0), Some(Hit::Device(laptop)));
    assert_eq!(hit(&state, area, 3, 5), Some(Hit::Device(tower)));
}

#[test]
fn one_machines_top_border_is_not_a_control() {
    let (state, _, _) = state();
    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit(&state, area, 3, 0), None);
}

#[test]
fn a_click_in_the_second_section_finds_its_rows() {
    let (state, _, _) = fleet();
    let beta = state
        .projects()
        .iter()
        .find(|project| project.name == "beta")
        .expect("beta is registered")
        .id;
    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit(&state, area, LEFT + 4, 6), Some(Hit::Project(beta)));
}

#[test]
fn several_machines_in_a_very_short_sidebar_still_draw_the_frame() {
    let mut state = AppState::new();
    for name in ["one", "two", "three"] {
        let device = state.add_device(Device::new(name));
        state.add_project(
            Project::new(format!("/tmp/{name}"), ProjectSource::LocalDir).with_device(device),
        );
    }

    for height in 2..6 {
        let lines = render_lines(&state, WIDTH, height);
        let last = &lines[usize::from(height) - 1];
        assert!(last.starts_with('└'), "height {height}: {lines:#?}");
    }
}

#[test]
fn heights_are_shared_by_weight_after_two_rows_each() {
    assert_eq!(section_heights(&[1, 1], &[false, false], 10), vec![5, 5]);
    assert_eq!(section_heights(&[3, 1], &[false, false], 12), vec![8, 4]);
    assert_eq!(section_heights(&[1, 1, 1], &[false; 3], 10), vec![4, 3, 3]);
}

#[test]
fn a_folded_section_gets_no_height() {
    assert_eq!(section_heights(&[1, 1], &[false, true], 10), vec![10, 0]);
    assert_eq!(section_heights(&[1, 1], &[true, true], 10), vec![0, 0]);
}

#[test]
fn too_little_height_is_handed_out_a_row_at_a_time_in_order() {
    assert_eq!(section_heights(&[1, 1], &[false, false], 3), vec![2, 1]);
    assert_eq!(section_heights(&[1, 1], &[false, false], 1), vec![1, 0]);
    assert_eq!(section_heights(&[1, 1], &[false, false], 0), vec![0, 0]);
}

#[test]
fn an_unreachable_device_says_so() {
    let (mut state, _, tower) = fleet();

    state.set_device_reachable(tower, false);
    let lines = render_lines(&state, WIDTH, 10);
    let name = lines
        .iter()
        .find(|line| line.contains("tower"))
        .expect("the machine has a name line");

    assert!(name.contains("unreachable"), "{name:?}");
}

#[test]
fn an_unreachable_device_with_a_long_name_still_says_so() {
    // `dispatchd --device` now defaults to the real hostname, which routinely
    // runs long enough that truncating "name — unreachable" as one string
    // keeps the name and cuts the word this line exists to show. The name
    // has to give way instead.
    let mut state = AppState::new();
    let long = state.add_device(Device::new(
        "Kudays-MacBook-Pro-With-A-Very-Long-Real-Hostname",
    ));
    let other = state.add_device(Device::new("tower"));
    state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir).with_device(long));
    state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir).with_device(other));

    state.set_device_reachable(long, false);
    let lines = render_lines(&state, WIDTH, 10);
    let name = lines
        .iter()
        .find(|line| line.contains("Kudays"))
        .expect("the machine has a name line");

    assert!(name.contains("unreachable"), "{name:?}");
}

#[test]
fn a_pending_device_with_no_projects_still_gets_a_section() {
    // The startup case: a registered machine is drawn before it has answered,
    // and before it has answered it has no projects either -- `roots` only
    // arrive on first connect. Its section must not wait for either.
    let mut state = AppState::new();
    let laptop = state.add_device(Device::new("laptop"));
    state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir).with_device(laptop));
    state.add_device(Device::pending("tower"));

    let lines = render_lines(&state, WIDTH, 8);
    let name = lines
        .iter()
        .find(|line| line.contains("tower"))
        .expect("the machine is named despite having no projects");

    assert!(name.contains("unreachable"), "{name:?}");
}

#[test]
fn a_reachable_machines_name_is_drawn_at_full_strength() {
    // The name is written onto the frame, which is faded; a live machine's
    // must not take that colour, or it looks as dead as an unreachable one.
    let (mut state, _, tower) = fleet();
    state.set_device_reachable(tower, false);

    let buf = render(&state, WIDTH, 10);
    let lines = render_lines(&state, WIDTH, 10);
    let divider = lines
        .iter()
        .position(|line| line.contains("tower"))
        .expect("the second machine is named");
    let first_letter = |y: usize, name: &str| {
        let x = u16::try_from(column_of(&lines[y], name)).expect("inside the sidebar");
        let y = u16::try_from(y).expect("inside the sidebar");
        buf.cell((x, y)).expect("cell exists").clone()
    };

    let laptop = first_letter(0, "laptop");
    assert_eq!(laptop.fg, Color::Reset, "{lines:#?}");
    assert!(laptop.modifier.contains(Modifier::BOLD), "{lines:#?}");

    let tower = first_letter(divider, "tower");
    assert_eq!(tower.fg, Theme::fallback().faded, "{lines:#?}");
}

#[test]
fn every_project_row_draws_its_mark_in_one_shared_column() {
    // A repository's mark and a plain directory's take the same column, so
    // their names line up with each other.
    let mut state = AppState::new();
    state.add_project(Project::new("/tmp/plain", ProjectSource::LocalDir));
    state.add_project(Project::new(
        "/tmp/repo",
        ProjectSource::GitRepo { remote: None },
    ));

    let lines = render_lines(&state, WIDTH, 6);

    assert_eq!(
        column_of(&lines[TOP as usize], "plain"),
        column_of(&lines[TOP as usize + 1], "repo")
    );
}

/// A repository on `main`, selected, with nothing in it yet.
fn repository() -> (AppState, ProjectId) {
    let mut state = AppState::new();
    let project = state.add_project(
        Project::new("/tmp/repo", ProjectSource::GitRepo { remote: None })
            .with_branch(Some("main".into())),
    );
    (state, project)
}

/// A pane of `harness` in `project`, on `branch`, titled `title`.
fn pane_on(state: &mut AppState, project: ProjectId, title: &str, branch: Option<&str>) -> PaneId {
    let pane = spawn(state, project, "claude");
    state.set_pane_title(pane, title).expect("the pane exists");
    state
        .set_pane_branch(pane, branch.map(str::to_string))
        .expect("the pane exists");
    pane
}

#[test]
fn a_projects_branch_is_drawn_faded_beneath_its_name() {
    let (state, _) = repository();
    let buf = render(&state, WIDTH, 6);

    let line = row_text(&buf, TOP + 1);
    assert_eq!(
        column_of(&line, "main"),
        usize::from(LEFT + NAME),
        "{line:?}"
    );
    assert_eq!(
        buf.cell((LEFT + NAME, TOP + 1)).expect("cell exists").fg,
        Theme::fallback().faded
    );
}

#[test]
fn panes_on_the_projects_branch_or_none_sit_beneath_it() {
    let (mut state, project) = repository();
    pane_on(&mut state, project, "on-main", Some("main"));
    pane_on(&mut state, project, "unknown", None);

    let lines = render_lines(&state, WIDTH, 8);

    assert!(lines[TOP as usize + 1].contains("main"), "{lines:#?}");
    assert!(lines[TOP as usize + 2].contains("on-main"), "{lines:#?}");
    assert!(lines[TOP as usize + 3].contains("unknown"), "{lines:#?}");
}

#[test]
fn panes_on_another_branch_are_gathered_under_it() {
    let (mut state, project) = repository();
    pane_on(&mut state, project, "tabs-one", Some("feat/tabs"));
    pane_on(&mut state, project, "on-main", Some("main"));
    pane_on(&mut state, project, "tabs-two", Some("feat/tabs"));

    let lines = render_lines(&state, WIDTH, 10);
    let at = |text: &str| {
        lines
            .iter()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("{text:?} is drawn: {lines:#?}"))
    };

    assert!(at("main") < at("on-main"));
    assert!(at("on-main") < at("feat/tabs"), "{lines:#?}");
    assert_eq!(at("tabs-one"), at("feat/tabs") + 1, "{lines:#?}");
    assert_eq!(at("tabs-two"), at("feat/tabs") + 2, "{lines:#?}");
    assert_eq!(
        column_of(&lines[at("feat/tabs")], "feat/tabs"),
        usize::from(LEFT + NAME),
        "every branch line sits where the project's does"
    );
}

#[test]
fn a_subagent_stays_under_its_parent_whatever_its_branch() {
    let (mut state, project) = repository();
    let parent = pane_on(&mut state, project, "parent", Some("main"));
    let mut child = Pane::new(project, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.title = "child".into();
    child.branch = Some("feat/elsewhere".into());
    state.adopt_pane(child).expect("the project exists");

    let lines = render_lines(&state, WIDTH, 8);
    let parent_row = lines
        .iter()
        .position(|l| l.contains("parent"))
        .expect("drawn");

    assert!(lines[parent_row + 1].contains("child"), "{lines:#?}");
    assert!(
        !lines.iter().any(|line| line.contains("feat/elsewhere")),
        "no group is opened for a subagent: {lines:#?}"
    );
}

#[test]
fn a_project_without_a_branch_draws_no_branch_row() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let lines = render_lines(&state, WIDTH, 6);

    assert!(lines[TOP as usize + 1].contains("claude"), "{lines:#?}");
}

#[test]
fn a_folded_project_keeps_its_own_branch_and_hides_the_rest() {
    let (mut state, project) = repository();
    pane_on(&mut state, project, "on-main", Some("main"));
    pane_on(&mut state, project, "tabs", Some("feat/tabs"));

    state.toggle_project_collapsed(project);
    let text = render_lines(&state, WIDTH, 8).join("\n");

    assert!(text.contains("main"), "{text}");
    assert!(
        !text.contains("on-main") && !text.contains("feat/tabs"),
        "{text}"
    );
}

#[test]
fn a_click_on_a_branch_row_is_a_click_on_its_project() {
    let (state, project) = repository();
    let area = Rect::new(0, 0, WIDTH, 6);

    assert_eq!(
        hit(&state, area, LEFT + NAME, TOP + 1),
        Some(Hit::Project(project))
    );
}

#[test]
fn a_long_branch_name_is_cut_short_inside_the_frame() {
    let mut state = AppState::new();
    state.add_project(
        Project::new("/tmp/repo", ProjectSource::GitRepo { remote: None }).with_branch(Some(
            "feature/an-extremely-long-branch-name-for-testing".into(),
        )),
    );

    let buf = render(&state, WIDTH, 5);
    let line = row_text(&buf, TOP + 1);

    assert!(
        line.ends_with('│'),
        "the frame is not overwritten: {line:?}"
    );
    assert!(line.contains('…'), "the cut is visible: {line:?}");
}

/// One machine with `count` projects named p0, p1, ….
fn many(count: usize) -> AppState {
    let mut state = AppState::new();
    for index in 0..count {
        state.add_project(Project::new(
            format!("/tmp/p{index}"),
            ProjectSource::LocalDir,
        ));
    }
    state
}

fn render_scrolled(state: &AppState, scroll: &Scroll, width: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    Sidebar::new(state)
        .with_scroll(scroll)
        .render(area, &mut buf);
    (0..height).map(|y| row_text(&buf, y)).collect()
}

#[test]
fn a_scrolled_section_starts_at_its_offset() {
    let state = many(20);
    let scroll = Scroll::from([(DeviceId::nil(), 5)]);

    let lines = render_scrolled(&state, &scroll, WIDTH, 8);

    assert!(lines[TOP as usize].contains(" p5 "), "{lines:#?}");
}

#[test]
fn an_offset_past_the_end_is_brought_back() {
    let state = many(20);
    let mut scroll = Scroll::from([(DeviceId::nil(), 99)]);

    settle(&state, Rect::new(0, 0, WIDTH, 8), &mut scroll, None);

    // Six rows inside the frame show the last six of twenty.
    assert_eq!(scroll[&DeviceId::nil()], 14);
}

#[test]
fn settling_on_a_pane_scrolls_just_far_enough_to_show_it() {
    let mut state = many(20);
    let p15 = state.projects()[15].id;
    let pane = spawn(&mut state, p15, "claude");
    let mut scroll = Scroll::new();

    settle(
        &state,
        Rect::new(0, 0, WIDTH, 8),
        &mut scroll,
        Some(Anchor::Pane(pane)),
    );

    // The pane is row 16; showing it as the last of six rows starts at 11.
    assert_eq!(scroll[&DeviceId::nil()], 11);
    let lines = render_scrolled(&state, &scroll, WIDTH, 8);
    assert!(lines[TOP as usize + 5].contains("claude"), "{lines:#?}");
}

#[test]
fn settling_without_an_anchor_leaves_the_wheels_scroll_alone() {
    let state = many(20);
    let mut scroll = Scroll::from([(DeviceId::nil(), 3)]);

    settle(&state, Rect::new(0, 0, WIDTH, 8), &mut scroll, None);

    assert_eq!(scroll[&DeviceId::nil()], 3);
}

#[test]
fn rows_hidden_below_are_counted_on_the_line_after_the_section() {
    let state = many(20);
    let lines = render_scrolled(&state, &Scroll::new(), WIDTH, 8);

    assert!(lines[7].contains("↓ 14"), "{lines:#?}");
}

#[test]
fn rows_hidden_above_are_counted_beside_the_sections_name() {
    let state = many(20);
    let scroll = Scroll::from([(DeviceId::nil(), 5)]);

    let lines = render_scrolled(&state, &scroll, WIDTH, 8);

    assert!(lines[0].contains("Projects ↑ 5"), "{lines:#?}");
}

#[test]
fn a_divider_carries_a_count_for_each_section_it_separates() {
    let mut state = AppState::new();
    let laptop = state.add_device(Device::new("laptop"));
    let tower = state.add_device(Device::new("a-tower-with-a-very-long-hostname"));
    for index in 0..10 {
        state.add_project(
            Project::new(format!("/tmp/l{index}"), ProjectSource::LocalDir).with_device(laptop),
        );
        state.add_project(
            Project::new(format!("/tmp/t{index}"), ProjectSource::LocalDir).with_device(tower),
        );
    }
    let scroll = Scroll::from([(tower, 2)]);

    // Twelve rows: ten inside, one divider, nine shared five and four.
    let lines = render_scrolled(&state, &scroll, WIDTH, 12);
    let divider = &lines[6];

    assert!(
        divider.contains("↑ 2"),
        "the tower's own count: {divider:?}"
    );
    assert!(divider.contains("↓ 5"), "the laptop's count: {divider:?}");
    assert!(divider.ends_with('┤'), "{divider:?}");
}

#[test]
fn a_click_in_a_scrolled_section_finds_the_row_drawn_there() {
    let state = many(20);
    let p5 = state.projects()[5].id;
    let scroll = Scroll::from([(DeviceId::nil(), 5)]);

    assert_eq!(
        hit_test(&state, Rect::new(0, 0, WIDTH, 8), &scroll, LEFT + 4, TOP),
        Some(Hit::Project(p5))
    );
}

#[test]
fn the_wheel_finds_the_section_under_it() {
    let (state, laptop, tower) = fleet();
    let area = Rect::new(0, 0, WIDTH, 10);
    let scroll = Scroll::new();

    assert_eq!(section_at(&state, area, &scroll, 5, 1), Some(laptop));
    assert_eq!(section_at(&state, area, &scroll, 5, 6), Some(tower));
    assert_eq!(section_at(&state, area, &scroll, 5, 5), None, "a divider");
    assert_eq!(section_at(&state, area, &scroll, 0, 1), None, "the frame");
}

#[test]
fn the_hidden_above_count_is_drawn_faded() {
    // The spec draws every hidden-row count in `faded`, above a section's
    // name and below it alike; only the name itself takes the label's own
    // style.
    let state = many(20);
    let scroll = Scroll::from([(DeviceId::nil(), 5)]);

    let area = Rect::new(0, 0, WIDTH, 8);
    let mut buf = Buffer::empty(area);
    Sidebar::new(&state)
        .with_scroll(&scroll)
        .render(area, &mut buf);
    let line = row_text(&buf, 0);

    let x = u16::try_from(column_of(&line, "↑")).expect("inside the sidebar");
    let cell = buf.cell((x, 0)).expect("cell exists");

    assert_eq!(cell.fg, Theme::fallback().faded, "{line:?}");
}

#[test]
fn a_machines_name_stays_full_strength_while_its_hidden_above_count_is_faded() {
    let (mut state, _, tower) = fleet();
    // Enough rows under the tower that an offset of 2 is not clamped away.
    for index in 0..9 {
        state.add_project(
            Project::new(format!("/tmp/t{index}"), ProjectSource::LocalDir).with_device(tower),
        );
    }
    let scroll = Scroll::from([(tower, 2)]);

    let area = Rect::new(0, 0, WIDTH, 10);
    let mut buf = Buffer::empty(area);
    Sidebar::new(&state)
        .with_scroll(&scroll)
        .render(area, &mut buf);
    let lines: Vec<String> = (0..10).map(|y| row_text(&buf, y)).collect();
    let divider = lines
        .iter()
        .position(|line| line.contains("tower"))
        .expect("the second machine is named");
    let y = u16::try_from(divider).expect("inside the sidebar");

    let name_x = u16::try_from(column_of(&lines[divider], "tower")).expect("inside the sidebar");
    let name = buf.cell((name_x, y)).expect("cell exists");
    assert_eq!(name.fg, Color::Reset, "{lines:#?}");
    assert!(name.modifier.contains(Modifier::BOLD), "{lines:#?}");

    let up_x = u16::try_from(column_of(&lines[divider], "↑")).expect("inside the sidebar");
    let up = buf.cell((up_x, y)).expect("cell exists");
    assert_eq!(up.fg, Theme::fallback().faded, "{lines:#?}");
}

#[test]
fn a_blocked_pane_says_so_in_yellow() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state
        .set_pane_status(pane, PaneStatus::Blocked)
        .expect("pane exists");

    let buf = render(&state, WIDTH, 6);
    let cell = buf.cell((WIDTH - 3, TOP + 1)).expect("cell exists");

    assert_eq!(cell.symbol(), BLOCKED);
    assert_eq!(cell.fg, Color::Yellow);
    assert!(cell.modifier.contains(Modifier::BOLD));
}

fn render_spinning(state: &AppState, frame: Option<usize>) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, 8);
    let mut buf = Buffer::empty(area);
    Sidebar::new(state)
        .with_spinner(frame)
        .render(area, &mut buf);
    buf
}

#[test]
fn a_working_pane_spins() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state
        .set_pane_status(pane, PaneStatus::Running)
        .expect("pane exists");

    assert_eq!(
        state_cell(&render_spinning(&state, Some(0)), TOP + 1).0,
        SPINNER[0]
    );
    assert_eq!(
        state_cell(&render_spinning(&state, Some(3)), TOP + 1).0,
        SPINNER[3]
    );
    assert_eq!(
        state_cell(&render_spinning(&state, Some(13)), TOP + 1).0,
        SPINNER[3],
        "the frame wraps"
    );
    assert_eq!(
        state_cell(&render_spinning(&state, None), TOP + 1),
        (RUNNING.to_string(), Color::Green),
        "with motion off, the still play glyph"
    );
}

#[test]
fn an_idle_pane_is_faded_and_one_finished_out_of_sight_is_marked() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state
        .set_pane_status(pane, PaneStatus::Idle)
        .expect("pane exists");

    assert_eq!(
        state_cell(&render(&state, WIDTH, 6), TOP + 1),
        (IDLE.to_string(), Theme::fallback().faded)
    );

    state.mark_unseen(pane);
    assert_eq!(
        state_cell(&render(&state, WIDTH, 6), TOP + 1),
        (UNSEEN.to_string(), Theme::fallback().accent)
    );
}

#[test]
fn a_folded_project_shows_its_most_urgent_pane() {
    let (mut state, alpha, _) = state();
    let working = spawn(&mut state, alpha, "claude");
    let done = spawn(&mut state, alpha, "codex");
    let blocked = spawn(&mut state, alpha, "opencode");
    state
        .set_pane_status(working, PaneStatus::Running)
        .expect("exists");
    state
        .set_pane_status(done, PaneStatus::Idle)
        .expect("exists");
    state.mark_unseen(done);
    state
        .set_pane_status(blocked, PaneStatus::Blocked)
        .expect("exists");

    state.toggle_project_collapsed(alpha);
    assert_eq!(state_cell(&render(&state, WIDTH, 6), TOP).0, BLOCKED);

    state
        .set_pane_status(blocked, PaneStatus::Idle)
        .expect("exists");
    assert_eq!(state_cell(&render(&state, WIDTH, 6), TOP).0, UNSEEN);

    state.mark_seen(done);
    assert_eq!(
        state_cell(&render_spinning(&state, Some(2)), TOP).0,
        SPINNER[2]
    );
}

#[test]
fn an_open_project_and_an_idle_one_show_no_rollup() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state
        .set_pane_status(pane, PaneStatus::Blocked)
        .expect("exists");

    assert_eq!(
        state_cell(&render(&state, WIDTH, 6), TOP).0,
        " ",
        "open: its panes say it"
    );

    state
        .set_pane_status(pane, PaneStatus::Idle)
        .expect("exists");
    state.toggle_project_collapsed(alpha);
    assert_eq!(
        state_cell(&render(&state, WIDTH, 6), TOP).0,
        " ",
        "nothing to say"
    );
}

fn render_moving(state: &AppState, motion: &SidebarMotion, height: u16) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, height);
    let mut buf = Buffer::empty(area);
    Sidebar::new(state)
        .with_motion(motion)
        .render(area, &mut buf);
    buf
}

#[test]
fn a_pulse_rises_and_falls_three_times() {
    assert_eq!(pulse_strength(0.0), 0.0);
    assert!(pulse_strength(1.0 / 6.0) > 0.99, "the first peak");
    assert!(pulse_strength(1.0 / 3.0) < 0.01, "the first trough");
    assert!(pulse_strength(1.0) < 0.01, "and it settles");
}

#[test]
fn a_pulsing_row_is_drawn_toward_the_pulse_colour() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    spawn(&mut state, alpha, "codex"); // focus moves off the first
    let theme = Theme::fallback();
    let motion = SidebarMotion {
        pulses: vec![(pane, 1.0)],
        glide: None,
    };

    let buf = render_moving(&state, &motion, 6);

    assert_eq!(
        buf.cell((LEFT + 8, TOP + 1)).expect("cell exists").bg,
        theme.blend(theme.rgb(Role::Background), theme.rgb(Role::Pulse), 1.0)
    );
}

#[test]
fn the_focus_tint_glides_through_the_rows_between() {
    let (mut state, alpha, _) = state();
    let first = spawn(&mut state, alpha, "claude");
    spawn(&mut state, alpha, "codex");
    let last = spawn(&mut state, alpha, "opencode"); // focused
    let tint = Theme::fallback().tint;
    let motion = SidebarMotion {
        pulses: Vec::new(),
        glide: Some(Glide {
            from: Anchor::Pane(first),
            t: 0.5,
        }),
    };

    let buf = render_moving(&state, &motion, 8);

    // First at TOP+1, last at TOP+3: halfway is TOP+2.
    assert_eq!(buf.cell((LEFT + 8, TOP + 2)).expect("cell").bg, tint);
    assert_ne!(
        buf.cell((LEFT + 8, TOP + 3)).expect("cell").bg,
        tint,
        "not arrived yet"
    );
    let _ = last;
}

#[test]
fn a_glide_between_sections_fades_instead() {
    let (mut state, laptop, tower) = fleet();
    let on_laptop = state
        .projects()
        .iter()
        .find(|p| p.device == laptop)
        .expect("one")
        .id;
    let on_tower = state
        .projects()
        .iter()
        .find(|p| p.device == tower)
        .expect("one")
        .id;
    let from = spawn(&mut state, on_laptop, "claude");
    let _ = state.select_project(on_tower);
    spawn(&mut state, on_tower, "codex"); // focused
    let theme = Theme::fallback();
    let motion = SidebarMotion {
        pulses: Vec::new(),
        glide: Some(Glide {
            from: Anchor::Pane(from),
            t: 0.5,
        }),
    };

    let buf = render_moving(&state, &motion, 14);
    let halfway = theme.blend(theme.rgb(Role::Background), theme.rgb(Role::Tint), 0.5);
    let row_of = |text: &str| {
        (0..buf.area.height)
            .find(|y| row_text(&buf, *y).contains(text))
            .unwrap_or_else(|| panic!("{text:?} is drawn"))
    };

    assert_eq!(
        buf.cell((LEFT + 8, row_of("claude"))).expect("cell").bg,
        halfway
    );
    assert_eq!(
        buf.cell((LEFT + 8, row_of("codex"))).expect("cell").bg,
        halfway
    );
}
