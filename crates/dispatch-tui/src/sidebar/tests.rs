//! Tests for the project sidebar.

use super::*;

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
/// pane's focus marker and its twisty are both blanks when they have nothing
/// to say, so leading-whitespace counting cannot tell "no marker" apart from
/// "less indented".
fn column_of(line: &str, text: &str) -> usize {
    let byte = line
        .find(text)
        .unwrap_or_else(|| panic!("expected {text:?} in {line:?}"));
    line[..byte].chars().count()
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
fn the_focused_pane_is_marked() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");
    spawn(&mut state, alpha, "codex");

    // Spawning focuses the new pane, so codex is focused.
    let buf = render(&state, WIDTH, 10);

    assert!(
        !row_text(&buf, TOP + 1).contains('▌'),
        "claude is not focused"
    );
    assert!(row_text(&buf, TOP + 2).contains('▌'), "codex is focused");
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
    assert_eq!(
        hit_test(&state, area, LEFT + 4, TOP + 1),
        Some(Hit::Pane(pane))
    );
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
        hit_test(&state, area, LEFT + 6, TOP + 1),
        Some(Hit::Pane(parent))
    );
    assert_eq!(
        hit_test(&state, area, LEFT + 6, TOP + 2),
        Some(Hit::Pane(child))
    );
}

#[test]
fn a_click_anywhere_on_a_project_heading_finds_the_project() {
    // The heading names no pane, so the whole row is the project's control:
    // clicking it selects the project and folds its panes away.
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit_test(&state, area, LEFT, TOP), Some(Hit::Project(alpha)));
    assert_eq!(
        hit_test(&state, area, WIDTH - 2, TOP),
        Some(Hit::Project(alpha))
    );
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
    assert_eq!(hit_test(&state, area, LEFT + 4, TOP + 1), None);
}

#[test]
fn a_click_outside_the_sidebars_area_finds_nothing() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let area = Rect::new(0, 0, WIDTH, 10);
    assert_eq!(hit_test(&state, area, WIDTH + 5, TOP + 1), None);
    assert_eq!(hit_test(&state, area, LEFT + 4, 20), None);
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
fn the_selected_projects_whole_row_is_highlighted() {
    // Emphasis on the name alone is easy to miss in a list of directory names
    // that already look alike. The bar runs the width of the list so the eye
    // finds it without reading.
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);

    for x in LEFT..WIDTH - 1 {
        assert!(
            buf.cell((x, TOP))
                .expect("cell exists")
                .modifier
                .contains(Modifier::REVERSED),
            "column {x} of the selected row is part of the bar"
        );
    }

    assert!(
        !buf.cell((LEFT, TOP + 1))
            .expect("cell exists")
            .modifier
            .contains(Modifier::REVERSED),
        "an unselected project carries no bar"
    );
}

#[test]
fn the_frame_is_not_painted_by_the_highlight() {
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);

    assert!(
        !buf.cell((0, TOP))
            .expect("cell exists")
            .modifier
            .contains(Modifier::REVERSED),
        "the bar stops at the frame"
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
        buf.cell((LEFT + 2, TOP + 1)).expect("cell exists").symbol(),
        SHUT,
        "and it flips when collapsed"
    );
    let _ = lonely;
}

#[test]
fn the_focus_marker_is_not_a_twisty() {
    // Two different `▸` in one row read as one control.
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let buf = render(&state, WIDTH, 6);
    let row = row_text(&buf, TOP + 1);

    assert!(row.contains('▌'), "the focused pane is marked: {row:?}");
    assert!(!row.contains(SHUT), "and not with a twisty: {row:?}");
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
    assert_eq!(
        hit_test(&state, area, LEFT + 4, TOP + 2),
        Some(Hit::Pane(codex))
    );
}

#[test]
fn a_click_on_a_panes_twisty_toggles_it_rather_than_focusing_it() {
    let (mut state, alpha, _) = state();
    let parent = spawn(&mut state, alpha, "claude");
    let mut child = Pane::new(alpha, HarnessId::new("claude"));
    child.parent = Some(parent);
    state.adopt_pane(child).expect("the project exists");

    let area = Rect::new(0, 0, WIDTH, 10);

    // The twisty sits in the row's first column, two in from the list's edge.
    assert_eq!(
        hit_test(&state, area, LEFT + 2, TOP + 1),
        Some(Hit::Twisty(parent))
    );
    assert_eq!(
        hit_test(&state, area, LEFT + 3, TOP + 1),
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
    assert_eq!(
        hit_test(&state, area, LEFT + 2, TOP + 1),
        Some(Hit::Pane(pane))
    );
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
        hit_test(&state, area, LEFT + 2, TOP + 1),
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

/// The last column inside the frame, where a pane's state is drawn.
fn state_cell(buf: &Buffer, y: u16) -> (String, Color) {
    let cell = buf
        .cell((buf.area.width - 2, y))
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
    let glyphs = [STARTING, RUNNING, IDLE, DONE, FAILED, CLOSED];

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
        (CLOSED.to_string(), Color::DarkGray)
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

    // Two columns in from the row's own edge: past its twisty and its focus
    // marker.
    assert_eq!(
        buf.cell((LEFT + 2 + 2, TOP + 1))
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
        buf.cell((LEFT + 2 + 2, TOP + 1))
            .expect("cell exists")
            .symbol(),
        dispatch_config::harness::DEFAULT_ICON
    );
}

#[test]
fn a_project_folder_is_open_while_its_panes_are_shown() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let buf = render(&state, WIDTH, 6);
    assert_eq!(
        buf.cell((LEFT + 2, TOP)).expect("cell exists").symbol(),
        OPEN_FOLDER
    );

    state.toggle_project_collapsed(alpha);
    let buf = render(&state, WIDTH, 6);
    assert_eq!(
        buf.cell((LEFT + 2, TOP)).expect("cell exists").symbol(),
        SHUT_FOLDER
    );
}

#[test]
fn a_project_with_nothing_in_it_is_a_shut_folder() {
    // There is nothing inside it to be looking at.
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        buf.cell((LEFT + 2, TOP)).expect("cell exists").symbol(),
        SHUT_FOLDER
    );
}

#[test]
fn a_repository_carries_a_git_mark_beside_its_folder() {
    let mut state = AppState::new();
    state.add_project(Project::new("/tmp/plain", ProjectSource::LocalDir));
    state.add_project(Project::new(
        "/tmp/repo",
        ProjectSource::GitRepo { remote: None },
    ));

    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        buf.cell((LEFT + 1, TOP)).expect("cell exists").symbol(),
        " ",
        "a plain directory has no git mark"
    );
    assert_eq!(
        buf.cell((LEFT + 1, TOP + 1)).expect("cell exists").symbol(),
        REPOSITORY,
        "and a repository does"
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
fn one_machine_draws_no_device_row() {
    // The ordinary case. A lone row naming this machine costs a line and
    // indents everything under it to say what the user already knows.
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
fn several_machines_each_get_a_row_above_their_projects() {
    let (state, _, _) = fleet();
    let lines = render_lines(&state, WIDTH, 10);

    let laptop = lines
        .iter()
        .position(|line| line.contains("laptop"))
        .expect("the first machine has a row");
    let alpha = lines
        .iter()
        .position(|line| line.contains("alpha"))
        .expect("its project is listed");
    let tower = lines
        .iter()
        .position(|line| line.contains("tower"))
        .expect("the second machine has a row");

    assert!(laptop < alpha && alpha < tower, "{lines:#?}");
    assert!(
        column_of(&lines[alpha], "alpha") > column_of(&lines[laptop], "laptop"),
        "a project is indented under its machine: {lines:#?}"
    );
}

#[test]
fn a_collapsed_device_hides_its_projects() {
    let (mut state, laptop, _) = fleet();

    state.toggle_device_collapsed(laptop);
    let text = render_lines(&state, WIDTH, 10).join("\n");

    assert!(!text.contains("alpha"), "{text}");
    assert!(text.contains("laptop"), "the machine stays: {text}");
    assert!(
        text.contains("beta"),
        "the other machine is unaffected: {text}"
    );
}

#[test]
fn an_unreachable_device_says_so() {
    let (mut state, _, tower) = fleet();

    state.set_device_reachable(tower, false);
    let lines = render_lines(&state, WIDTH, 10);
    let row = lines
        .iter()
        .find(|line| line.contains("tower"))
        .expect("the machine has a row");

    assert!(row.contains("unreachable"), "{row:?}");
}

#[test]
fn an_unreachable_device_with_a_long_name_still_says_so() {
    // `dispatchd --device` now defaults to the real hostname, which routinely
    // runs long enough that truncating "name — unreachable" as one string
    // keeps the name and cuts the word this row exists to show. The name has
    // to give way instead.
    let mut state = AppState::new();
    let long = state.add_device(Device::new(
        "Kudays-MacBook-Pro-With-A-Very-Long-Real-Hostname",
    ));
    let other = state.add_device(Device::new("tower"));
    state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir).with_device(long));
    state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir).with_device(other));

    state.set_device_reachable(long, false);
    let lines = render_lines(&state, WIDTH, 10);
    let row = lines
        .iter()
        .find(|line| line.contains("Kudays"))
        .expect("the machine has a row");

    assert!(row.contains("unreachable"), "{row:?}");
}

#[test]
fn a_pending_device_with_no_projects_still_draws_a_row() {
    // The startup case: a registered machine is drawn before it has answered,
    // and before it has answered it has no projects either -- `roots` only
    // arrive on first connect. The row must not wait for either.
    let mut state = AppState::new();
    let laptop = state.add_device(Device::new("laptop"));
    state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir).with_device(laptop));
    state.add_device(Device::pending("tower"));

    let lines = render_lines(&state, WIDTH, 8);
    let row = lines
        .iter()
        .find(|line| line.contains("tower"))
        .expect("the machine has a row despite having no projects");

    assert!(row.contains("unreachable"), "{row:?}");
}

#[test]
fn a_click_on_a_device_row_finds_the_device() {
    let (state, laptop, _) = fleet();
    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit_test(&state, area, LEFT, TOP), Some(Hit::Device(laptop)));
}

#[test]
fn the_git_mark_column_is_reserved_on_every_project_row() {
    // Reserved rather than inserted, so a repository and a plain directory
    // line their names up with each other.
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
