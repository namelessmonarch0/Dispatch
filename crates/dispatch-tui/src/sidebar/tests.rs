//! Tests for the project sidebar.

use super::*;

use dispatch_core::{HarnessId, Pane, PaneId, Project, ProjectSource};

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

/// The column a row's status dot is drawn in.
fn dot_column(buf: &Buffer, y: u16) -> Option<u16> {
    (0..buf.area.width).find(|x| buf.cell((*x, y)).expect("cell exists").symbol() == DOT)
}

/// How many columns a rendered row is indented.
///
/// Measured by the status dot's column rather than by counting leading
/// spaces: a pane's own focus marker is a blank space when that pane is not
/// focused, so on an unfocused row plain leading-whitespace counting cannot
/// tell "no marker" apart from "less indented" — the dot is drawn at a fixed
/// offset from the row's indent regardless of focus, so it is unambiguous.
fn indent(line: &str) -> usize {
    line.find(DOT).unwrap_or(0)
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

    // The name starts two columns after the row's status dot.
    let name_x = |y: u16| dot_column(&buf, y).expect("the row has a dot") + 2;
    let selected = buf.cell((name_x(TOP), TOP)).expect("cell exists");
    let unselected = buf.cell((name_x(TOP + 1), TOP + 1)).expect("cell exists");

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
fn every_project_has_a_reserved_status_column() {
    // The federation slice lights this per device. It must already be in the
    // layout so adding that does not shift every row.
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 10);

    assert_eq!(dot_column(&buf, TOP), dot_column(&buf, TOP + 1));
    assert!(
        dot_column(&buf, TOP).is_some_and(|x| x >= LEFT),
        "the column is inside the frame"
    );
}

#[test]
fn pane_status_is_colour_coded() {
    let (mut state, alpha, _) = state();
    let running = spawn(&mut state, alpha, "claude");
    let failed = spawn(&mut state, alpha, "codex");

    state
        .set_pane_status(running, PaneStatus::Running)
        .expect("pane exists");
    state
        .set_pane_status(failed, PaneStatus::Exited(1))
        .expect("pane exists");

    let buf = render(&state, WIDTH, 10);

    let colour = |y: u16| dot_column(&buf, y).map(|x| buf.cell((x, y)).expect("cell exists").fg);

    assert_eq!(colour(TOP + 1), Some(Color::Green), "a running pane");
    assert_eq!(colour(TOP + 2), Some(Color::Red), "a pane that failed");
}

#[test]
fn a_pane_that_exited_cleanly_is_dimmed_rather_than_red() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state
        .set_pane_status(pane, PaneStatus::Exited(0))
        .expect("pane exists");

    let buf = render(&state, WIDTH, 10);
    let dot_x = dot_column(&buf, TOP + 1).expect("the pane has a status dot");

    assert_eq!(
        buf.cell((dot_x, TOP + 1)).expect("cell exists").fg,
        Color::DarkGray
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
        indent(&lines[child_row]) > indent(&lines[parent_row]),
        "and is indented under it: {:?}",
        lines[child_row]
    );
}

#[test]
fn a_finished_subagent_shows_how_it_ended() {
    let mut state = AppState::new();
    let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
    let parent = state
        .spawn_pane(project, HarnessId::new("claude"))
        .expect("the project exists");

    let mut clean = Pane::new(project, HarnessId::new("claude"));
    clean.parent = Some(parent);
    clean.title = "tests".into();
    clean.status = PaneStatus::Exited(0);
    state.adopt_pane(clean).expect("the project exists");

    let mut failed = Pane::new(project, HarnessId::new("claude"));
    failed.parent = Some(parent);
    failed.title = "docs".into();
    failed.status = PaneStatus::Exited(1);
    state.adopt_pane(failed).expect("the project exists");

    let lines = render_lines(&state, 28, 6);

    assert!(
        lines.iter().any(|l| l.contains("tests") && l.contains('✓')),
        "a clean exit is marked: {lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("docs") && l.contains('!')),
        "a failure is marked differently: {lines:#?}"
    );
}

#[test]
fn a_pane_nobody_delegated_carries_no_outcome_glyph() {
    // The status dot already says what an ordinary pane is doing. A glyph
    // beside it repeats the fact, and a column of them against every shell
    // buries the one row that is actually reporting a subagent's result.
    let mut state = AppState::new();
    let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));

    let mut shell = Pane::new(project, HarnessId::new("shell"));
    shell.title = "a shell".into();
    state.adopt_pane(shell).expect("the project exists");

    let lines = render_lines(&state, 28, 4);
    let row = lines
        .iter()
        .find(|line| line.contains("a shell"))
        .expect("the pane has a row");

    for glyph in ['⋯', '✓', '!'] {
        assert!(
            !row.contains(glyph),
            "{glyph:?} does not belong on an undelegated pane: {row:?}"
        );
    }
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
            .any(|l| l.contains("Claude Code") && l.contains('⊘')),
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
    assert_eq!(buf.cell((LEFT, TOP)).expect("cell exists").symbol(), "▾");

    state.toggle_project_collapsed(alpha);
    let buf = render(&state, WIDTH, 6);
    assert_eq!(buf.cell((LEFT, TOP)).expect("cell exists").symbol(), "▸");
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
            .find(|s| s == "▾" || s == "▸")
    };

    assert_eq!(twisty(TOP + 1), Some("▾".into()), "the parent has one");
    assert_eq!(twisty(TOP + 3), None, "a childless pane has none");

    state.toggle_pane_collapsed(parent);
    let buf = render(&state, WIDTH, 8);
    assert_eq!(
        buf.cell((LEFT + 2, TOP + 1)).expect("cell exists").symbol(),
        "▸",
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
    assert!(!row.contains('▸'), "and not with a twisty: {row:?}");
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
