//! The state every Dispatch client renders.
//!
//! Mutated only through the methods below rather than by reaching into the
//! fields. A later slice moves this into a daemon and replays the same
//! transitions from the wire, so each one has to be a single named operation
//! with its invariants enforced in one place.

use std::collections::HashSet;

use crate::id::{PaneId, ProjectId};
use crate::pane::{HarnessId, Pane, PaneRole, PaneStatus};
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
    /// The project still has panes, so removing it would strand them.
    #[error("project {0} still has panes")]
    ProjectInUse(ProjectId),
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
    /// Projects whose panes the sidebar hides. Absence means expanded, so a
    /// project is expanded the moment it is added and nothing has to be
    /// recorded for the ordinary case.
    collapsed_projects: HashSet<ProjectId>,
    /// Panes whose children the sidebar hides, on the same rule.
    collapsed_panes: HashSet<PaneId>,
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

    /// Forgets a project.
    ///
    /// Refused while it still has panes, tombstones included: an agent whose
    /// project row has gone is running with no way left to reach it. Closing
    /// them is the user's decision to make, one pane at a time.
    ///
    /// The view moves to whatever project is left, because the selection has
    /// to name one that exists.
    pub fn remove_project(&mut self, project: ProjectId) -> Result<(), StateError> {
        let Some(index) = self.projects.iter().position(|p| p.id == project) else {
            return Err(StateError::NoSuchProject(project));
        };

        if self.panes.iter().any(|pane| pane.project == project) {
            return Err(StateError::ProjectInUse(project));
        }

        self.projects.remove(index);
        self.collapsed_projects.remove(&project);

        if self.selected_project == Some(project) {
            self.zoomed_pane = None;
            self.focused_pane = None;
            self.selected_project = self.projects.first().map(|p| p.id);

            if let Some(next) = self.selected_project {
                self.focused_pane = self.panes_for(next).first().map(|p| p.id);
            }
        }

        Ok(())
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
    ///
    /// A pane named as this one's parent becomes an
    /// [`PaneRole::Orchestrator`]: that is the whole rule, and this is the one
    /// moment it can be applied, because a pane's first approved child is
    /// exactly what arrives here.
    pub fn adopt_pane(&mut self, pane: Pane) -> Result<PaneId, StateError> {
        let project = pane.project;
        if !self.projects.iter().any(|p| p.id == project) {
            return Err(StateError::NoSuchProject(project));
        }

        let id = pane.id;
        if self.panes.iter().any(|p| p.id == id) {
            return Ok(id);
        }

        if let Some(parent) = pane.parent
            && let Some(delegating) = self.panes.iter_mut().find(|p| p.id == parent)
        {
            delegating.role = PaneRole::Orchestrator;
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
    ///
    /// That same rule applies all the way down: a doomed child can itself have
    /// live durable children (reachable once `max_depth` allows a delegated
    /// pane to delegate again), so cleaning it up asks the same survivor
    /// question rather than deleting it outright — and a child that survives
    /// that question *as a tombstone* keeps this pane's row open too, since a
    /// row whose parent is missing is drawn nowhere at all.
    pub fn close_pane(&mut self, id: PaneId) -> Result<(), StateError> {
        if !self.panes.iter().any(|p| p.id == id) {
            return Err(StateError::NoSuchPane(id));
        }

        let mut judged = HashSet::new();
        self.close_or_tombstone(id, &mut judged);

        Ok(())
    }

    /// Closes `id`, recursively applying the same survivor rule to whatever it
    /// dooms.
    ///
    /// `judged` guards the walk against a corrupt cycle in `parent` links:
    /// each pane is judged at most once per call, so a cycle ends the walk
    /// instead of recursing forever.
    fn close_or_tombstone(&mut self, id: PaneId, judged: &mut HashSet<PaneId>) {
        if !judged.insert(id) {
            return;
        }

        let doomed: Vec<PaneId> = self
            .children_of(id)
            .iter()
            .filter(|p| !(p.durable && p.status.is_live()))
            .map(|p| p.id)
            .collect();

        for child in doomed {
            self.close_or_tombstone(child, judged);
        }

        // Counted after the recursion, not before it: a doomed child with live
        // durable work of its own has just become a tombstone rather than being
        // removed, and it is still here naming this pane as its parent. Removing
        // this pane anyway would leave that tombstone's `parent` pointing at
        // nothing, and the sidebar draws neither a row whose parent is absent
        // nor anything below it — so a *running* subagent would go invisible
        // and unreachable.
        if self.children_of(id).is_empty() {
            // Read before the pane is removed: once it is gone, its parent
            // link goes with it.
            let tombstone = self.parent_tombstone(id);
            self.remove_pane(id);

            // A tombstone exists only for its children. Closing the last one
            // takes the row with it — and may do the same to the tombstone
            // above that, so the collapse has to walk, not just look once.
            self.collapse_tombstones(tombstone);
            return;
        }

        if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
            pane.closed = true;
        }
        if self.focused_pane == Some(id) {
            self.focused_pane = None;
        }
        if self.zoomed_pane == Some(id) {
            self.zoomed_pane = None;
        }
    }

    /// The closed parent of `child`, when that parent is only still present to
    /// hold children and losing `child`'s branch leaves it holding nothing that
    /// is still running.
    ///
    /// The question is asked over the whole subtree rather than one level down.
    /// A direct child that has finished can still have a blanket-approved
    /// subagent of its own running beneath it — reachable as soon as
    /// `max_depth >= 2` — and a tombstone judged spent is collapsed by
    /// [`Self::collapse_tombstones`], which sweeps everything below it. Counting
    /// only direct children would call that row spent and delete a pane whose
    /// process the daemon still owns and whose closing nobody ever announced.
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

        (!self.has_live_work(parent, child)).then_some(parent)
    }

    /// Whether anything under `parent` is still running, ignoring the branch
    /// rooted at `skip` — the pane on its way out, which takes everything below
    /// it along.
    ///
    /// A tombstone is not work: it has no process behind it, and its `status` is
    /// only whatever it was when the row was kept.
    ///
    /// Walks with a visited set for the reason the removal walks do: a corrupt
    /// cycle in `parent` links must end the search rather than recurse forever.
    fn has_live_work(&self, parent: PaneId, skip: PaneId) -> bool {
        let mut visited = HashSet::from([parent, skip]);
        let mut pending = vec![parent];

        while let Some(id) = pending.pop() {
            for child in self.children_of(id) {
                if !visited.insert(child.id) {
                    continue;
                }
                if !child.closed && child.status.is_live() {
                    return true;
                }
                pending.push(child.id);
            }
        }

        false
    }

    /// Walks a chain of tombstones upward, removing each one that the removal
    /// below it just left holding no live children of its own.
    ///
    /// An iterative loop with a visited set rather than recursion: a corrupt
    /// cycle in `parent` links must not spin this forever or overflow a call
    /// stack, so each candidate is removed at most once and a repeat ends the
    /// walk instead.
    fn collapse_tombstones(&mut self, first: Option<PaneId>) {
        let mut current = first;
        let mut visited = HashSet::new();

        while let Some(id) = current {
            if !visited.insert(id) {
                break;
            }

            // Read before this tombstone is removed, for the same reason as
            // in `close_or_tombstone`: its parent link goes with it.
            let next = self.parent_tombstone(id);
            self.remove_pane(id);
            // A tombstone can hold more than the one child whose ending
            // collapsed it — a sibling that exited earlier is still a row under
            // it — and those go with the row they were reachable through rather
            // than being left naming a parent that no longer exists.
            self.remove_descendants(id, &mut visited);
            current = next;
        }
    }

    /// Removes everything still naming `parent`, and everything under that.
    ///
    /// Unconditional, so it may only be called where [`Self::parent_tombstone`]
    /// has already established that nothing in that subtree is running: it does
    /// not ask the survivor question itself, and a live pane swept from here
    /// would vanish from the tree while its process ran on in the daemon.
    ///
    /// `removed` guards the walk the way `close_or_tombstone`'s `judged` does:
    /// each pane is removed at most once, so a corrupt cycle in `parent` links
    /// ends the walk rather than recursing forever.
    fn remove_descendants(&mut self, parent: PaneId, removed: &mut HashSet<PaneId>) {
        let children: Vec<PaneId> = self.children_of(parent).iter().map(|p| p.id).collect();

        for child in children {
            if !removed.insert(child) {
                continue;
            }
            self.remove_descendants(child, removed);
            self.remove_pane(child);
        }
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
        self.collapsed_panes.remove(&id);

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

    /// Whether the sidebar hides `project`'s panes.
    #[must_use]
    pub fn is_project_collapsed(&self, project: ProjectId) -> bool {
        self.collapsed_projects.contains(&project)
    }

    /// Hides `project`'s panes, or shows them again.
    ///
    /// Takes an unknown project without complaint: this answers a click on a
    /// row, and a row for a project that has just gone is not worth an error
    /// path of its own.
    pub fn toggle_project_collapsed(&mut self, project: ProjectId) {
        if !self.collapsed_projects.remove(&project) {
            self.collapsed_projects.insert(project);
        }
    }

    /// Whether the sidebar hides `pane`'s children.
    #[must_use]
    pub fn is_pane_collapsed(&self, pane: PaneId) -> bool {
        self.collapsed_panes.contains(&pane)
    }

    /// Hides `pane`'s children, or shows them again.
    pub fn toggle_pane_collapsed(&mut self, pane: PaneId) {
        if !self.collapsed_panes.remove(&pane) {
            self.collapsed_panes.insert(pane);
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
        assert!(
            state.children_of(child).is_empty(),
            "a child has no children of its own"
        );
    }

    #[test]
    fn an_exited_child_keeps_its_row_under_its_parent() {
        // Its transcript is the only record of what the subagent did, so the row
        // stays until someone closes it.
        let (mut state, parent, child) = parent_and_child(false);
        state
            .set_pane_status(child, PaneStatus::Exited(0))
            .expect("the pane exists");

        assert_eq!(state.children_of(parent).len(), 1);
        assert!(!state.children_of(parent)[0].status.is_live());
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

    #[test]
    fn closing_a_parent_judges_each_child_on_its_own_terms() {
        // One survivor, two that go: the rule is per-child, and a mixed family is
        // the normal case rather than an edge one.
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut durable_live = Pane::new(project, HarnessId::new("claude"));
        durable_live.parent = Some(parent);
        durable_live.durable = true;
        let survivor = durable_live.id;
        state.adopt_pane(durable_live).expect("the project exists");

        let mut one_off = Pane::new(project, HarnessId::new("claude"));
        one_off.parent = Some(parent);
        let doomed_live = one_off.id;
        state.adopt_pane(one_off).expect("the project exists");

        let mut durable_done = Pane::new(project, HarnessId::new("claude"));
        durable_done.parent = Some(parent);
        durable_done.durable = true;
        durable_done.status = PaneStatus::Exited(0);
        let doomed_finished = durable_done.id;
        state.adopt_pane(durable_done).expect("the project exists");

        state.close_pane(parent).expect("the pane exists");

        assert!(
            state.pane(survivor).is_some(),
            "live durable work continues"
        );
        assert!(
            state.pane(doomed_live).is_none(),
            "a one-off goes with its caller"
        );
        assert!(
            state.pane(doomed_finished).is_none(),
            "finished work goes with the pane it was reachable through"
        );
        assert!(
            state.pane(parent).expect("the row stays").closed,
            "the parent stays only as a tombstone"
        );
    }

    #[test]
    fn a_doomed_child_does_not_orphan_its_own_children() {
        // Reachable whenever max_depth is raised: without recursion the grandchild's
        // parent id names a pane that no longer exists, and nothing ever cleans it.
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut child = Pane::new(project, HarnessId::new("claude"));
        child.parent = Some(parent);
        let child_id = child.id;
        state.adopt_pane(child).expect("the project exists");

        let mut grandchild = Pane::new(project, HarnessId::new("claude"));
        grandchild.parent = Some(child_id);
        grandchild.durable = true;
        let grandchild_id = grandchild.id;
        state.adopt_pane(grandchild).expect("the project exists");

        state.close_pane(parent).expect("the pane exists");

        assert!(
            state.pane(child_id).is_some_and(|p| p.closed),
            "a doomed child with live durable work of its own becomes a tombstone"
        );
        assert!(
            state.pane(grandchild_id).is_some(),
            "and its work continues"
        );
        assert!(
            state.pane(parent).is_some_and(|p| p.closed),
            "so the pane above it stays as a tombstone too: a row whose parent \
             is gone is drawn nowhere, and the grandchild is still running"
        );
    }

    #[test]
    fn every_row_of_a_surviving_chain_can_be_found_from_the_top() {
        // What the orphan above actually costs: the sidebar walks down from
        // top-level panes, so a chain with a link missing is not merely untidy
        // — every row below the gap is invisible and unreachable, including the
        // running subagent the whole tombstone machinery exists to keep.
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut child = Pane::new(project, HarnessId::new("claude"));
        child.parent = Some(parent);
        let child_id = child.id;
        state.adopt_pane(child).expect("the project exists");

        let mut grandchild = Pane::new(project, HarnessId::new("claude"));
        grandchild.parent = Some(child_id);
        grandchild.durable = true;
        let grandchild_id = grandchild.id;
        state.adopt_pane(grandchild).expect("the project exists");

        state.close_pane(parent).expect("the pane exists");

        for pane in state.panes_for(project) {
            if let Some(above) = pane.parent {
                assert!(
                    state.pane(above).is_some(),
                    "every row that names a parent must have one to be drawn under"
                );
            }
        }
        assert!(
            state
                .children_of(child_id)
                .iter()
                .any(|p| p.id == grandchild_id),
            "and the running subagent is still reachable from the row above it"
        );
    }

    #[test]
    fn a_collapsing_tombstone_takes_its_finished_children_with_it() {
        // A tombstone holding one live child and one that has already exited:
        // reachable at the default max_depth of 1 with two `A`-approved
        // subagents, the second of which finished first. Collapsing the row
        // when the live one ends must take the finished sibling too — left
        // behind, it names a parent that no longer exists and is drawn nowhere
        // for the rest of the session.
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut first = Pane::new(project, HarnessId::new("claude"));
        first.parent = Some(parent);
        first.durable = true;
        let first_id = first.id;
        state.adopt_pane(first).expect("the project exists");

        let mut second = Pane::new(project, HarnessId::new("claude"));
        second.parent = Some(parent);
        second.durable = true;
        let second_id = second.id;
        state.adopt_pane(second).expect("the project exists");

        // Both are live when the parent closes, so both survive it.
        state.close_pane(parent).expect("the pane exists");
        assert!(
            state.pane(parent).expect("the row stays").closed,
            "set up: the parent is a tombstone over two live children"
        );

        // Then one finishes, and the user closes the other.
        state
            .set_pane_status(second_id, PaneStatus::Exited(0))
            .expect("the pane exists");
        state.close_pane(first_id).expect("the pane exists");

        assert!(state.pane(first_id).is_none(), "the closed child is gone");
        assert!(
            state.pane(parent).is_none(),
            "nothing live is left to hold the tombstone open"
        );
        assert!(
            state.pane(second_id).is_none(),
            "and the finished sibling goes with the row it was drawn under, \
             rather than being orphaned under a parent that no longer exists"
        );
    }

    #[test]
    fn a_pane_becomes_an_orchestrator_when_its_first_child_is_adopted() {
        // The role nothing ever constructed. Nobody declares a pane an
        // orchestrator: having a subagent approved under it is what makes it
        // one.
        let (mut state, parent, _child) = parent_and_child(false);

        assert_eq!(
            state.pane(parent).map(|p| p.role),
            Some(PaneRole::Orchestrator),
            "the pane that was delegated from is an orchestrator"
        );

        let plain = state
            .spawn_pane(
                state.selected_project().expect("a project is selected"),
                HarnessId::new("codex"),
            )
            .expect("the project exists");
        assert_eq!(
            state.pane(plain).map(|p| p.role),
            Some(PaneRole::Worker),
            "a pane with no children of its own is still a worker"
        );
    }

    #[test]
    fn a_chain_of_tombstones_collapses_together() {
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let parent = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let mut child = Pane::new(project, HarnessId::new("claude"));
        child.parent = Some(parent);
        child.durable = true;
        let child_id = child.id;
        state.adopt_pane(child).expect("the project exists");

        let mut grandchild = Pane::new(project, HarnessId::new("claude"));
        grandchild.parent = Some(child_id);
        grandchild.durable = true;
        let grandchild_id = grandchild.id;
        state.adopt_pane(grandchild).expect("the project exists");

        state.close_pane(parent).expect("the pane exists");
        state.close_pane(child_id).expect("the pane exists");

        // Closing the last live descendant should take both tombstones with it.
        state.close_pane(grandchild_id).expect("the pane exists");

        assert!(state.pane(grandchild_id).is_none());
        assert!(state.pane(child_id).is_none(), "its tombstone goes too");
        assert!(
            state.pane(parent).is_none(),
            "and so does the one above that"
        );
    }

    /// Adopts a delegated pane under `parent`, returning its id.
    fn delegated(state: &mut AppState, parent: PaneId, durable: bool) -> PaneId {
        let project = state.pane(parent).expect("the parent exists").project;
        let mut pane = Pane::new(project, HarnessId::new("claude"));
        pane.parent = Some(parent);
        pane.durable = durable;
        let id = pane.id;
        state.adopt_pane(pane).expect("the project exists");
        id
    }

    /// A tombstone over a live durable child and a finished durable one that
    /// still has a live durable child of its own — the deepest shape
    /// `max_depth >= 2` allows, and the one the collapse used to sweep away.
    fn tombstone_over_deep_work() -> (AppState, PaneId, PaneId, PaneId, PaneId) {
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let top = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        let live = delegated(&mut state, top, true);
        let middle = delegated(&mut state, top, true);
        let deep = delegated(&mut state, middle, true);

        // Both children are live when the top pane closes, so both survive it
        // and the row above them is kept as a tombstone.
        state.close_pane(top).expect("the pane exists");
        assert!(
            state.pane(top).expect("the row stays").closed,
            "set up: a tombstone over two live children"
        );

        // Then the middle one finishes while its own subagent keeps working.
        state
            .set_pane_status(middle, PaneStatus::Exited(0))
            .expect("the pane exists");

        (state, top, live, middle, deep)
    }

    #[test]
    fn a_collapsing_tombstone_keeps_live_work_below_its_children() {
        // The collapse gate used to count only direct children, so the finished
        // middle row made the tombstone look spent — and the sweep that followed
        // deleted a blanket-approved subagent whose process the daemon still
        // owns and for which no PaneClosed was ever broadcast. The client's tree
        // lost a running pane until the next reattach.
        let (mut state, top, live, middle, deep) = tombstone_over_deep_work();

        state.close_pane(live).expect("the pane exists");

        assert!(state.pane(live).is_none(), "the closed child is gone");
        assert!(
            state.pane(deep).is_some(),
            "the running subagent two levels down is still here"
        );
        assert!(
            state.pane(middle).is_some(),
            "so is the row it is drawn under"
        );
        assert!(
            state.pane(top).is_some_and(|p| p.closed),
            "and the tombstone above that, since a row whose parent is missing \
             is drawn nowhere at all"
        );

        // What the sweep actually cost: reachability from the top of the tree.
        for pane in state.panes_for(state.selected_project().expect("a project")) {
            if let Some(above) = pane.parent {
                assert!(
                    state.pane(above).is_some(),
                    "every row that names a parent must have one to be drawn under"
                );
            }
        }
        assert!(
            state.children_of(middle).iter().any(|p| p.id == deep),
            "the running subagent is reachable from the row above it"
        );
    }

    #[test]
    fn a_tombstone_over_a_wholly_finished_subtree_still_collapses() {
        // The other half of the rule: widening the gate must not keep rows for
        // work that is over. Nothing below this tombstone is running, so closing
        // its last live child takes the whole subtree with it.
        let (mut state, top, live, middle, deep) = tombstone_over_deep_work();
        state
            .set_pane_status(deep, PaneStatus::Exited(0))
            .expect("the pane exists");

        state.close_pane(live).expect("the pane exists");

        assert!(state.pane(live).is_none(), "the closed child is gone");
        assert!(
            state.pane(top).is_none(),
            "nothing live is left to hold the tombstone open"
        );
        assert!(
            state.pane(middle).is_none(),
            "the finished child goes with the row it was drawn under"
        );
        assert!(
            state.pane(deep).is_none(),
            "and so does the finished work below it"
        );
    }

    #[test]
    fn a_cycle_in_parent_links_ends_the_close_walk() {
        // Nothing should be able to build this, which is exactly why the walks
        // must not trust it: two tombstones naming each other as parent would
        // otherwise spin the collapse forever or overflow its stack. The claim
        // under test is termination, not any particular surviving shape.
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));

        let mut first = Pane::new(project, HarnessId::new("claude"));
        let first_id = first.id;
        let mut second = Pane::new(project, HarnessId::new("claude"));
        let second_id = second.id;

        first.parent = Some(second_id);
        first.closed = true;
        first.durable = true;
        second.parent = Some(first_id);
        second.closed = true;
        second.durable = true;
        state.adopt_pane(first).expect("the project exists");
        state.adopt_pane(second).expect("the project exists");

        // A leaf under the cycle, so closing it asks the collapse question and
        // the answer has to walk the ring.
        let leaf = delegated(&mut state, first_id, true);

        state.close_pane(leaf).expect("the pane exists");

        assert!(
            state.pane(leaf).is_none(),
            "the walk finished and the leaf went"
        );
    }

    #[test]
    fn a_project_is_expanded_until_it_is_collapsed() {
        let (mut state, project) = with_project();

        assert!(!state.is_project_collapsed(project));

        state.toggle_project_collapsed(project);
        assert!(state.is_project_collapsed(project));

        state.toggle_project_collapsed(project);
        assert!(!state.is_project_collapsed(project));
    }

    #[test]
    fn a_pane_is_expanded_until_it_is_collapsed() {
        let (mut state, project) = with_project();
        let pane = state
            .spawn_pane(project, harness("claude"))
            .expect("the project exists");

        assert!(!state.is_pane_collapsed(pane));

        state.toggle_pane_collapsed(pane);
        assert!(state.is_pane_collapsed(pane));

        state.toggle_pane_collapsed(pane);
        assert!(!state.is_pane_collapsed(pane));
    }

    #[test]
    fn closing_a_pane_forgets_that_it_was_collapsed() {
        // Ids are not reused, but a set that only ever grows is a leak, and a
        // pane adopted from the daemon must start expanded like any other.
        let (mut state, project) = with_project();
        let pane = state
            .spawn_pane(project, harness("claude"))
            .expect("the project exists");

        state.toggle_pane_collapsed(pane);
        state.close_pane(pane).expect("the pane exists");

        assert!(!state.is_pane_collapsed(pane));
    }

    #[test]
    fn a_project_with_no_panes_can_be_removed() {
        let (mut state, first) = with_project();
        let second = state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir));

        state.remove_project(first).expect("it has no panes");

        assert_eq!(state.projects().len(), 1);
        assert_eq!(
            state.selected_project(),
            Some(second),
            "the view moves to what is left"
        );
    }

    #[test]
    fn removing_the_last_project_leaves_nothing_selected() {
        let (mut state, only) = with_project();

        state.remove_project(only).expect("it has no panes");

        assert_eq!(state.selected_project(), None);
    }

    #[test]
    fn a_project_still_running_panes_is_not_removed() {
        // Removing it would take its agents off the screen while they carried
        // on running, with no row left to reach them by.
        let (mut state, project) = with_project();
        state
            .spawn_pane(project, harness("claude"))
            .expect("the project exists");

        assert!(matches!(
            state.remove_project(project),
            Err(StateError::ProjectInUse(_))
        ));
        assert_eq!(state.projects().len(), 1);
    }

    #[test]
    fn removing_a_project_that_is_not_there_is_refused() {
        let (mut state, _) = with_project();

        assert!(matches!(
            state.remove_project(ProjectId::new()),
            Err(StateError::NoSuchProject(_))
        ));
    }
}
