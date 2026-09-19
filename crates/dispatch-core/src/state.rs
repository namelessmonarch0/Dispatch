//! The state every Dispatch client renders.
//!
//! Mutated only through the methods below rather than by reaching into the
//! fields. A later slice moves this into a daemon and replays the same
//! transitions from the wire, so each one has to be a single named operation
//! with its invariants enforced in one place.

use crate::id::{PaneId, ProjectId};
use crate::pane::{HarnessId, Pane, PaneStatus};
use crate::project::Project;

/// Rejected state transitions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    /// The named project is not registered.
    #[error("no project with id {0}")]
    NoSuchProject(ProjectId),
    /// The named pane does not exist.
    #[error("no pane with id {0}")]
    NoSuchPane(PaneId),
}

/// Projects, their panes, and what the user is currently looking at.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    projects: Vec<Project>,
    /// Every pane across every project, in spawn order. Spawn order is also
    /// the tiling order, so the grid does not reshuffle when unrelated state
    /// changes.
    panes: Vec<Pane>,
    selected_project: Option<ProjectId>,
    focused_pane: Option<PaneId>,
    zoomed_pane: Option<PaneId>,
}

impl AppState {
    /// Creates empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a project, selecting it if it is the first.
    ///
    /// A project already known by that id is updated in place rather than
    /// added again. The daemon announces its projects on every subscribe, and
    /// two rows for one checkout would give its panes two places to be drawn.
    pub fn add_project(&mut self, project: Project) -> ProjectId {
        let id = project.id;

        if let Some(existing) = self.projects.iter_mut().find(|p| p.id == id) {
            *existing = project;
            return id;
        }

        self.projects.push(project);
        if self.selected_project.is_none() {
            self.selected_project = Some(id);
        }
        id
    }

    /// All registered projects, in the order they were added.
    #[must_use]
    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    /// The project whose panes are on screen.
    #[must_use]
    pub fn selected_project(&self) -> Option<ProjectId> {
        self.selected_project
    }

    /// Switches which project is on screen.
    ///
    /// Focus moves to that project's first pane, because focus always has to
    /// name a visible pane.
    pub fn select_project(&mut self, project: ProjectId) -> Result<(), StateError> {
        if !self.projects.iter().any(|p| p.id == project) {
            return Err(StateError::NoSuchProject(project));
        }

        self.selected_project = Some(project);
        // Zoom is a property of looking at one pane, so it does not survive
        // looking somewhere else.
        self.zoomed_pane = None;
        self.focused_pane = self.panes_for(project).first().map(|p| p.id);
        Ok(())
    }

    /// Panes belonging to `project`, in spawn order.
    #[must_use]
    pub fn panes_for(&self, project: ProjectId) -> Vec<&Pane> {
        self.panes.iter().filter(|p| p.project == project).collect()
    }

    /// Panes currently on screen, in tiling order.
    ///
    /// Excludes tombstones: a closed pane has no process behind it, so there
    /// is nothing for the grid to draw.
    #[must_use]
    pub fn visible_panes(&self) -> Vec<&Pane> {
        self.panes
            .iter()
            .filter(|p| !p.closed && Some(p.project) == self.selected_project)
            .collect()
    }

    /// Looks up one pane.
    #[must_use]
    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    /// The panes delegated by `parent`, in spawn order.
    #[must_use]
    pub fn children_of(&self, parent: PaneId) -> Vec<&Pane> {
        self.panes
            .iter()
            .filter(|p| p.parent == Some(parent))
            .collect()
    }

    /// How many of `parent`'s children are still running.
    ///
    /// What the delegation cap counts: work in progress, not work that has
    /// been done.
    #[must_use]
    pub fn live_children(&self, parent: PaneId) -> usize {
        self.children_of(parent)
            .iter()
            .filter(|p| p.status.is_live())
            .count()
    }

    /// Adds a pane to `project` and focuses it.
    ///
    /// A newly spawned agent is what the user is about to interact with, so it
    /// takes focus.
    pub fn spawn_pane(
        &mut self,
        project: ProjectId,
        harness: HarnessId,
    ) -> Result<PaneId, StateError> {
        self.adopt_pane(Pane::new(project, harness))
    }

    /// Adds a pane that already exists, keeping its identity.
    ///
    /// The daemon names the panes it owns, so a client attaching to one takes
    /// the ids it is given rather than minting its own: two clients looking at
    /// the same fleet have to agree on what each pane is called.
    pub fn adopt_pane(&mut self, pane: Pane) -> Result<PaneId, StateError> {
        let project = pane.project;
        if !self.projects.iter().any(|p| p.id == project) {
            return Err(StateError::NoSuchProject(project));
        }

        let id = pane.id;
        if self.panes.iter().any(|p| p.id == id) {
            return Ok(id);
        }

        self.panes.push(pane);

        if self.selected_project == Some(project) {
            self.focused_pane = Some(id);
            // A new pane changes the grid, so a zoomed pane would hide it.
            self.zoomed_pane = None;
        }

        Ok(id)
    }

    /// Removes a pane, repairing focus and zoom.
    ///
    /// A pane with live durable children is marked closed and kept instead: a
    /// blanket-approved subagent outlives its caller, and has to stay reachable
    /// through something. Children that were one-off, or that have already
    /// finished, go with their parent — their transcripts were reachable
    /// through the pane being closed, and rows for finished work under a pane
    /// that no longer exists are debris.
    pub fn close_pane(&mut self, id: PaneId) -> Result<(), StateError> {
        if !self.panes.iter().any(|p| p.id == id) {
            return Err(StateError::NoSuchPane(id));
        }

        let survivors: Vec<PaneId> = self
            .children_of(id)
            .iter()
            .filter(|p| p.durable && p.status.is_live())
            .map(|p| p.id)
            .collect();

        let doomed: Vec<PaneId> = self
            .children_of(id)
            .iter()
            .filter(|p| !survivors.contains(&p.id))
            .map(|p| p.id)
            .collect();

        for child in doomed {
            self.remove_pane(child);
        }

        if survivors.is_empty() {
            // Read before the child is removed: once it is gone, its parent
            // link goes with it.
            let tombstone = self.parent_tombstone(id);
            self.remove_pane(id);

            // A tombstone exists only for its children. Closing the last one
            // takes the row with it.
            if let Some(parent) = tombstone {
                self.remove_pane(parent);
            }
        } else if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
            pane.closed = true;
            if self.focused_pane == Some(id) {
                self.focused_pane = None;
            }
            if self.zoomed_pane == Some(id) {
                self.zoomed_pane = None;
            }
        }

        Ok(())
    }

    /// The closed parent of `child`, when that parent is only still present to
    /// hold children and this was its last live one.
    fn parent_tombstone(&self, child: PaneId) -> Option<PaneId> {
        let parent = self
            .panes
            .iter()
            .find(|p| p.id == child)
            .and_then(|p| p.parent)?;

        let pane = self.panes.iter().find(|p| p.id == parent)?;
        if !pane.closed {
            return None;
        }

        (self.live_children(parent) <= 1).then_some(parent)
    }

    /// Removes one pane and repairs focus and zoom around it.
    ///
    /// A tombstone is never a repair target: it has no process behind it, so
    /// focus or zoom landing on one would point at nothing the grid draws.
    fn remove_pane(&mut self, id: PaneId) {
        let Some(index) = self.panes.iter().position(|p| p.id == id) else {
            return;
        };
        let closed = self.panes.remove(index);

        if self.zoomed_pane == Some(id) {
            self.zoomed_pane = None;
        }

        if self.focused_pane == Some(id) {
            // Prefer the pane that slid into this one's position, so repeated
            // closes walk along the grid instead of jumping to the start.
            let siblings: Vec<&Pane> = self
                .panes
                .iter()
                .filter(|p| p.project == closed.project && !p.closed)
                .collect();
            self.focused_pane = siblings
                .get(index.min(siblings.len().saturating_sub(1)))
                .map(|p| p.id);
        }
    }

    /// The focused pane, which receives keystrokes.
    #[must_use]
    pub fn focused_pane(&self) -> Option<PaneId> {
        self.focused_pane
    }

    /// Focuses a pane. It must belong to the selected project.
    pub fn focus(&mut self, id: PaneId) -> Result<(), StateError> {
        let pane = self.pane(id).ok_or(StateError::NoSuchPane(id))?;
        if Some(pane.project) != self.selected_project {
            return Err(StateError::NoSuchPane(id));
        }

        self.focused_pane = Some(id);
        Ok(())
    }

    /// The pane filling the whole grid, if any.
    #[must_use]
    pub fn zoomed_pane(&self) -> Option<PaneId> {
        self.zoomed_pane
    }

    /// Zooms the focused pane to fill the grid, or restores the grid.
    ///
    /// Does nothing when no pane is focused: there would be nothing to zoom.
    pub fn toggle_zoom(&mut self) {
        self.zoomed_pane = match (self.zoomed_pane, self.focused_pane) {
            (Some(_), _) => None,
            (None, Some(focused)) => Some(focused),
            (None, None) => None,
        };
    }

    /// Records a new status for a pane.
    pub fn set_pane_status(&mut self, id: PaneId, status: PaneStatus) -> Result<(), StateError> {
        let pane = self
            .panes
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(StateError::NoSuchPane(id))?;
        pane.status = status;
        Ok(())
    }

    /// Records a new title for a pane, as set by its terminal.
    pub fn set_pane_title(
        &mut self,
        id: PaneId,
        title: impl Into<String>,
    ) -> Result<(), StateError> {
        let pane = self
            .panes
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(StateError::NoSuchPane(id))?;
        pane.title = title.into();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_announced_twice_is_one_project() {
        // The daemon announces its projects on every subscribe; two rows for
        // one checkout would give its panes two places to be drawn.
        let mut state = AppState::new();
        let project = Project::new("/tmp/one", ProjectSource::LocalDir);
        let id = project.id;

        assert_eq!(state.add_project(project.clone()), id);
        assert_eq!(state.add_project(project.with_name("renamed")), id);

        assert_eq!(state.projects().len(), 1);
        assert_eq!(state.projects()[0].name, "renamed");
    }

    #[test]
    fn a_pane_from_the_daemon_keeps_its_id() {
        // Two clients on the same fleet have to call each pane the same thing.
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));

        let mut pane = Pane::new(project, HarnessId::new("claude"));
        pane.id = PaneId::new();
        let id = pane.id;

        assert_eq!(state.adopt_pane(pane).expect("the project exists"), id);
        assert_eq!(state.pane(id).map(|p| p.id), Some(id));
    }

    #[test]
    fn adopting_a_pane_twice_is_not_a_second_pane() {
        // The daemon announces its panes on every subscribe, and a client that
        // resubscribes must not double them.
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let pane = Pane::new(project, HarnessId::new("claude"));

        let id = state.adopt_pane(pane.clone()).expect("the project exists");
        assert_eq!(state.adopt_pane(pane).expect("the project exists"), id);
        assert_eq!(state.visible_panes().len(), 1);
    }

    #[test]
    fn a_pane_for_an_unknown_project_is_refused() {
        let mut state = AppState::new();
        let pane = Pane::new(ProjectId::new(), HarnessId::new("claude"));

        assert!(matches!(
            state.adopt_pane(pane),
            Err(StateError::NoSuchProject(_))
        ));
    }

    use crate::project::ProjectSource;

    /// State with one project selected, plus that project's id.
    fn with_project() -> (AppState, ProjectId) {
        let mut state = AppState::new();
        let id = state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir));
        (state, id)
    }

    fn harness(name: &str) -> HarnessId {
        HarnessId::new(name)
    }

    #[test]
    fn the_first_project_added_is_selected() {
        let (state, id) = with_project();
        assert_eq!(state.selected_project(), Some(id));
    }

    #[test]
    fn adding_a_second_project_does_not_steal_the_selection() {
        let (mut state, first) = with_project();
        state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));
        assert_eq!(state.selected_project(), Some(first));
    }

    #[test]
    fn spawning_a_pane_focuses_it() {
        let (mut state, project) = with_project();
        let pane = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");
        assert_eq!(state.focused_pane(), Some(pane));
    }

    #[test]
    fn panes_keep_spawn_order() {
        let (mut state, project) = with_project();
        let first = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");
        let second = state
            .spawn_pane(project, harness("codex"))
            .expect("project exists");
        let third = state
            .spawn_pane(project, harness("agy"))
            .expect("project exists");

        let order: Vec<_> = state.visible_panes().iter().map(|p| p.id).collect();
        assert_eq!(order, vec![first, second, third]);
    }

    #[test]
    fn panes_of_other_projects_are_not_visible() {
        let (mut state, alpha) = with_project();
        let beta = state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));

        let in_alpha = state
            .spawn_pane(alpha, harness("claude"))
            .expect("alpha exists");
        state
            .spawn_pane(beta, harness("codex"))
            .expect("beta exists");

        let visible: Vec<_> = state.visible_panes().iter().map(|p| p.id).collect();
        assert_eq!(visible, vec![in_alpha]);
    }

    #[test]
    fn spawning_into_an_unselected_project_does_not_move_focus() {
        let (mut state, alpha) = with_project();
        let beta = state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));

        let focused = state
            .spawn_pane(alpha, harness("claude"))
            .expect("alpha exists");
        state
            .spawn_pane(beta, harness("codex"))
            .expect("beta exists");

        assert_eq!(state.focused_pane(), Some(focused));
    }

    #[test]
    fn closing_the_focused_pane_focuses_the_one_that_takes_its_place() {
        let (mut state, project) = with_project();
        let first = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");
        let second = state
            .spawn_pane(project, harness("codex"))
            .expect("project exists");
        let third = state
            .spawn_pane(project, harness("agy"))
            .expect("project exists");

        state.focus(second).expect("second is visible");
        state.close_pane(second).expect("second exists");

        // third slid into second's slot.
        assert_eq!(state.focused_pane(), Some(third));
        let remaining: Vec<_> = state.visible_panes().iter().map(|p| p.id).collect();
        assert_eq!(remaining, vec![first, third]);
    }

    #[test]
    fn closing_the_last_pane_falls_back_to_the_previous_one() {
        let (mut state, project) = with_project();
        let first = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");
        let last = state
            .spawn_pane(project, harness("codex"))
            .expect("project exists");

        state.focus(last).expect("last is visible");
        state.close_pane(last).expect("last exists");

        assert_eq!(state.focused_pane(), Some(first));
    }

    #[test]
    fn closing_the_only_pane_clears_focus() {
        let (mut state, project) = with_project();
        let only = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");

        state.close_pane(only).expect("pane exists");

        assert_eq!(state.focused_pane(), None);
        assert!(state.visible_panes().is_empty());
    }

    #[test]
    fn closing_an_unfocused_pane_leaves_focus_alone() {
        let (mut state, project) = with_project();
        let first = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");
        let second = state
            .spawn_pane(project, harness("codex"))
            .expect("project exists");

        state.focus(second).expect("second is visible");
        state.close_pane(first).expect("first exists");

        assert_eq!(state.focused_pane(), Some(second));
    }

    #[test]
    fn zoom_toggles_the_focused_pane() {
        let (mut state, project) = with_project();
        let pane = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");

        assert_eq!(state.zoomed_pane(), None);
        state.toggle_zoom();
        assert_eq!(state.zoomed_pane(), Some(pane));
        state.toggle_zoom();
        assert_eq!(state.zoomed_pane(), None);
    }

    #[test]
    fn zoom_does_nothing_without_a_focused_pane() {
        let (mut state, _) = with_project();
        state.toggle_zoom();
        assert_eq!(state.zoomed_pane(), None);
    }

    #[test]
    fn closing_the_zoomed_pane_restores_the_grid() {
        let (mut state, project) = with_project();
        state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");
        let zoomed = state
            .spawn_pane(project, harness("codex"))
            .expect("project exists");

        state.toggle_zoom();
        assert_eq!(state.zoomed_pane(), Some(zoomed));

        state.close_pane(zoomed).expect("pane exists");
        assert_eq!(state.zoomed_pane(), None);
    }

    #[test]
    fn spawning_a_pane_unzooms_so_the_new_pane_is_visible() {
        let (mut state, project) = with_project();
        state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");
        state.toggle_zoom();
        assert!(state.zoomed_pane().is_some());

        state
            .spawn_pane(project, harness("codex"))
            .expect("project exists");
        assert_eq!(state.zoomed_pane(), None);
    }

    #[test]
    fn switching_projects_moves_focus_to_the_new_projects_first_pane() {
        let (mut state, alpha) = with_project();
        let beta = state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));

        state
            .spawn_pane(alpha, harness("claude"))
            .expect("alpha exists");
        let first_in_beta = state
            .spawn_pane(beta, harness("codex"))
            .expect("beta exists");
        state.spawn_pane(beta, harness("agy")).expect("beta exists");

        state.select_project(beta).expect("beta exists");

        assert_eq!(state.focused_pane(), Some(first_in_beta));
    }

    #[test]
    fn switching_to_an_empty_project_clears_focus() {
        let (mut state, alpha) = with_project();
        let beta = state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));
        state
            .spawn_pane(alpha, harness("claude"))
            .expect("alpha exists");

        state.select_project(beta).expect("beta exists");

        assert_eq!(state.focused_pane(), None);
    }

    #[test]
    fn switching_projects_clears_zoom() {
        let (mut state, alpha) = with_project();
        let beta = state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));
        state
            .spawn_pane(alpha, harness("claude"))
            .expect("alpha exists");
        state.toggle_zoom();

        state.select_project(beta).expect("beta exists");

        assert_eq!(state.zoomed_pane(), None);
    }

    #[test]
    fn a_pane_in_another_project_cannot_be_focused() {
        let (mut state, alpha) = with_project();
        let beta = state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));
        state
            .spawn_pane(alpha, harness("claude"))
            .expect("alpha exists");
        let hidden = state
            .spawn_pane(beta, harness("codex"))
            .expect("beta exists");

        assert_eq!(state.focus(hidden), Err(StateError::NoSuchPane(hidden)));
    }

    #[test]
    fn unknown_ids_are_rejected() {
        let (mut state, _) = with_project();
        let ghost_pane = PaneId::new();
        let ghost_project = ProjectId::new();

        assert_eq!(
            state.close_pane(ghost_pane),
            Err(StateError::NoSuchPane(ghost_pane))
        );
        assert_eq!(
            state.focus(ghost_pane),
            Err(StateError::NoSuchPane(ghost_pane))
        );
        assert_eq!(
            state.select_project(ghost_project),
            Err(StateError::NoSuchProject(ghost_project))
        );
        assert_eq!(
            state.spawn_pane(ghost_project, harness("claude")),
            Err(StateError::NoSuchProject(ghost_project))
        );
    }

    #[test]
    fn status_and_title_updates_are_recorded() {
        let (mut state, project) = with_project();
        let pane = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");

        state
            .set_pane_status(pane, PaneStatus::Exited(1))
            .expect("pane exists");
        state.set_pane_title(pane, "building").expect("pane exists");

        let pane = state.pane(pane).expect("pane exists");
        assert_eq!(pane.status, PaneStatus::Exited(1));
        assert_eq!(pane.title, "building");
    }

    #[test]
    fn an_exited_pane_stays_visible_until_it_is_closed() {
        let (mut state, project) = with_project();
        let pane = state
            .spawn_pane(project, harness("claude"))
            .expect("project exists");

        state
            .set_pane_status(pane, PaneStatus::Exited(0))
            .expect("pane exists");

        assert_eq!(state.visible_panes().len(), 1);
        assert_eq!(state.focused_pane(), Some(pane));
    }

    /// A project with a parent pane and one child, returning both ids.
    fn parent_and_child(durable: bool) -> (AppState, PaneId, PaneId) {
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));

        let parent = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut child = Pane::new(project, HarnessId::new("claude"));
        child.parent = Some(parent);
        child.durable = durable;
        let child_id = child.id;
        state.adopt_pane(child).expect("the project exists");

        (state, parent, child_id)
    }

    #[test]
    fn a_child_is_listed_under_its_parent() {
        let (state, parent, child) = parent_and_child(false);

        let children: Vec<PaneId> = state.children_of(parent).iter().map(|p| p.id).collect();
        assert_eq!(children, vec![child]);
        assert_eq!(state.live_children(parent), 1);
        assert!(
            state.children_of(child).is_empty(),
            "a child has no children of its own"
        );
    }

    #[test]
    fn an_exited_child_is_not_a_live_child() {
        // The cap counts what is running, not what has run.
        let (mut state, parent, child) = parent_and_child(false);
        state
            .set_pane_status(child, PaneStatus::Exited(0))
            .expect("the pane exists");

        assert_eq!(state.live_children(parent), 0);
        assert_eq!(state.children_of(parent).len(), 1, "the row stays");
    }

    #[test]
    fn closing_a_parent_takes_its_one_off_children_with_it() {
        // A one-off subagent exists to answer a caller that has just gone.
        let (mut state, parent, child) = parent_and_child(false);

        state.close_pane(parent).expect("the pane exists");

        assert!(state.pane(parent).is_none(), "the parent is gone");
        assert!(state.pane(child).is_none(), "and so is its child");
    }

    #[test]
    fn closing_a_parent_leaves_a_tombstone_over_a_durable_child() {
        // Blanket-approved work keeps running, and has to stay reachable.
        let (mut state, parent, child) = parent_and_child(true);

        state.close_pane(parent).expect("the pane exists");

        let row = state.pane(parent).expect("the parent row stays");
        assert!(row.closed, "marked closed rather than removed");
        assert!(state.pane(child).is_some(), "the child keeps running");
        assert!(
            !state.visible_panes().iter().any(|p| p.id == parent),
            "a tombstone is a row, not a pane to draw"
        );
    }

    #[test]
    fn a_tombstone_goes_when_its_last_child_does() {
        let (mut state, parent, child) = parent_and_child(true);
        state.close_pane(parent).expect("the pane exists");

        state.close_pane(child).expect("the pane exists");

        assert!(
            state.pane(parent).is_none(),
            "nothing is left to hold the row open"
        );
    }

    #[test]
    fn closing_a_parent_drops_children_that_have_already_finished() {
        // Their transcripts were reachable through the pane just closed; rows
        // for finished work under a pane that is gone are debris.
        let (mut state, parent, child) = parent_and_child(true);
        state
            .set_pane_status(child, PaneStatus::Exited(0))
            .expect("the pane exists");

        state.close_pane(parent).expect("the pane exists");

        assert!(state.pane(parent).is_none());
        assert!(state.pane(child).is_none());
    }
}
