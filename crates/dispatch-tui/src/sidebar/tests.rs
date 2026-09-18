//! Tests for the project sidebar.

use super::*;

use dispatch_core::{HarnessId, PaneId, Project, ProjectSource};

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

#[test]
fn projects_are_listed_in_the_order_they_were_added() {
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 10);

    assert!(row_text(&buf, 0).contains("alpha"));
    assert!(row_text(&buf, 1).contains("beta"));
}

#[test]
fn panes_are_listed_under_their_project() {
    let (mut state, alpha, beta) = state();
    spawn(&mut state, alpha, "claude");
    spawn(&mut state, beta, "codex");

    let buf = render(&state, WIDTH, 10);

    assert!(row_text(&buf, 0).contains("alpha"));
    assert!(row_text(&buf, 1).contains("claude"));
    assert!(row_text(&buf, 2).contains("beta"));
    assert!(row_text(&buf, 3).contains("codex"));
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

    // "alpha" starts after the reserved status column and its spacer.
    let selected = buf.cell((2, 0)).expect("cell exists");
    let unselected = buf.cell((2, 1)).expect("cell exists");

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

    assert!(!row_text(&buf, 1).contains('▸'), "claude is not focused");
    assert!(row_text(&buf, 2).contains('▸'), "codex is focused");
}

#[test]
fn every_project_has_a_reserved_status_column() {
    // The federation slice lights this per device. It must already be in the
    // layout so adding that does not shift every row.
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 10);

    assert_eq!(buf.cell((0, 0)).expect("cell exists").symbol(), DOT);
    assert_eq!(buf.cell((0, 1)).expect("cell exists").symbol(), DOT);
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

    // The dot sits two columns after the focus marker.
    let find_dot = |y: u16| {
        (0..WIDTH)
            .find(|x| buf.cell((*x, y)).expect("cell exists").symbol() == DOT)
            .map(|x| buf.cell((x, y)).expect("cell exists").fg)
    };

    assert_eq!(find_dot(1), Some(Color::Green), "a running pane");
    assert_eq!(find_dot(2), Some(Color::Red), "a pane that failed");
}

#[test]
fn a_pane_that_exited_cleanly_is_dimmed_rather_than_red() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state
        .set_pane_status(pane, PaneStatus::Exited(0))
        .expect("pane exists");

    let buf = render(&state, WIDTH, 10);
    let dot_x = (0..WIDTH)
        .find(|x| buf.cell((*x, 1)).expect("cell exists").symbol() == DOT)
        .expect("the pane has a status dot");

    assert_eq!(
        buf.cell((dot_x, 1)).expect("cell exists").fg,
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
    let line = row_text(&buf, 0);

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
    assert!(row_text(&buf, 0).contains("p0"));
}

#[test]
fn an_empty_state_paints_nothing() {
    let state = AppState::new();
    let buf = render(&state, WIDTH, 5);

    assert_eq!(all_text(&buf).trim(), "");
}

#[test]
fn a_zero_sized_area_paints_nothing() {
    let (state, _, _) = state();
    let mut buf = Buffer::empty(Rect::new(0, 0, WIDTH, 5));

    Sidebar::new(&state).render(Rect::new(0, 0, 0, 5), &mut buf);

    assert_eq!(all_text(&buf).trim(), "");
}
