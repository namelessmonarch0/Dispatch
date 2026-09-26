//! Tests for the tabs a project's grid is shown as.

use super::*;

use dispatch_core::{HarnessId, Pane, Placement, Project, ProjectSource};

/// A state with one selected project, and that project.
fn state() -> (AppState, ProjectId) {
    let mut state = AppState::new();
    let project = state.add_project(Project::new("/tmp/tabs", ProjectSource::LocalDir));
    (state, project)
}

/// Adds a top-level pane.
fn top(state: &mut AppState, project: ProjectId) -> PaneId {
    state
        .spawn_pane(project, HarnessId::new("shell"))
        .expect("the project exists")
}

/// Adds a subagent under `parent`.
fn child(state: &mut AppState, project: ProjectId, parent: PaneId) -> PaneId {
    let mut pane = Pane::new(project, HarnessId::new("shell"));
    pane.parent = Some(parent);
    state.adopt_pane(pane).expect("the project exists")
}

/// Each view's panes.
fn panes(views: &[TabView]) -> Vec<Vec<PaneId>> {
    views.iter().map(|view| view.panes.clone()).collect()
}

#[test]
fn a_project_whose_tabs_nobody_keeps_is_grouped_four_at_a_time() {
    let (mut state, project) = state();
    let ids: Vec<PaneId> = (0..5).map(|_| top(&mut state, project)).collect();

    let views = views(&state, Some(project), &ids);

    assert_eq!(panes(&views), vec![ids[..4].to_vec(), ids[4..].to_vec()]);
    assert!(views.iter().all(|view| view.id.is_none()));
}

#[test]
fn kept_tabs_tile_their_members_in_the_order_kept() {
    let (mut state, project) = state();
    let ids: Vec<PaneId> = (0..3).map(|_| top(&mut state, project)).collect();
    state.set_project_tabs(project, ProjectTabs::new());
    state.place_pane(ids[2], Placement::Auto);
    state.place_pane(ids[0], Placement::NewAfter { tab: None });
    state.place_pane(ids[1], Placement::Auto);

    let views = views(&state, Some(project), &ids);

    assert_eq!(panes(&views), vec![vec![ids[2]], vec![ids[0], ids[1]]]);
    assert!(views.iter().all(|view| view.id.is_some()));
}

#[test]
fn an_opened_subagent_is_tiled_beside_the_pane_that_asked_for_it() {
    let (mut state, project) = state();
    let parent = top(&mut state, project);
    let other = top(&mut state, project);
    let kid = child(&mut state, project, parent);
    state.set_project_tabs(project, ProjectTabs::new());
    state.place_pane(other, Placement::Auto);
    state.place_pane(parent, Placement::NewAfter { tab: None });

    // Tree order, as `App::tileable` gives it: each top-level pane, then its
    // opened subagents.
    let views = views(&state, Some(project), &[parent, kid, other]);

    assert_eq!(panes(&views), vec![vec![other], vec![parent, kid]]);
}

#[test]
fn a_pane_not_placed_yet_goes_on_the_last_tab() {
    // It started a moment before the snapshot that places it arrives.
    let (mut state, project) = state();
    let placed = top(&mut state, project);
    let fresh = top(&mut state, project);
    state.set_project_tabs(project, ProjectTabs::new());
    state.place_pane(placed, Placement::Auto);

    let views = views(&state, Some(project), &[placed, fresh]);

    assert_eq!(panes(&views), vec![vec![placed, fresh]]);
}

#[test]
fn a_tab_with_nothing_on_the_grid_is_not_shown() {
    // Its only pane exited a moment before the snapshot that removes it.
    let (mut state, project) = state();
    let ids: Vec<PaneId> = (0..2).map(|_| top(&mut state, project)).collect();
    state.set_project_tabs(project, ProjectTabs::new());
    state.place_pane(ids[0], Placement::Auto);
    state.place_pane(ids[1], Placement::NewAfter { tab: None });

    let views = views(&state, Some(project), &ids[1..]);

    assert_eq!(panes(&views), vec![vec![ids[1]]]);
}

#[test]
fn a_project_with_nothing_to_tile_still_has_a_tab() {
    let (mut state, project) = state();
    state.set_project_tabs(project, ProjectTabs::new());

    assert_eq!(views(&state, Some(project), &[]).len(), 1);
    assert_eq!(views(&state, None, &[]).len(), 1);
}

#[test]
fn a_tab_is_called_by_its_name_or_else_its_first_panes_title() {
    let (mut state, project) = state();
    let pane = top(&mut state, project);
    state
        .set_pane_title(pane, "fix login")
        .expect("the pane exists");
    let mut view = TabView {
        id: None,
        name: None,
        panes: vec![pane],
    };

    assert_eq!(name(&state, &view), "fix login");

    view.name = Some("work".into());
    assert_eq!(name(&state, &view), "work");
}

#[test]
fn every_tab_is_shown_when_they_all_fit() {
    assert_eq!(visible_range(&[10, 10, 10], 1, 80), 0..3);
}

#[test]
fn the_row_starts_at_the_first_tab_while_the_current_one_fits_that_way() {
    // 60 columns: 4 go to ` + ` and its gap, 2 to `›`. Two 20-column tabs
    // and their gap fit; a third does not.
    assert_eq!(visible_range(&[20; 6], 1, 60), 0..2);
}

#[test]
fn the_row_scrolls_to_keep_the_current_tab_in_view() {
    assert_eq!(visible_range(&[20; 6], 5, 60), 4..6);
}

#[test]
fn a_tab_too_wide_to_fit_alone_is_still_the_one_shown() {
    assert_eq!(visible_range(&[100, 10], 0, 40), 0..1);
}

#[test]
fn no_tabs_shows_nothing() {
    assert_eq!(visible_range(&[], 0, 80), 0..0);
}
