//! The tabs a project's grid is shown as.
//!
//! Worked out fresh from whoever keeps the project's tabs and from what
//! there is to tile, rather than stored beside them: a stored copy and the
//! focus can disagree, and a view showing one tab while typing went to
//! another would be the worst bug here.

use std::collections::HashSet;

use dispatch_core::{AppState, PaneId, ProjectId, ProjectTabs, TAB_CAPACITY, TabId};

/// One tab, as this client shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabView {
    /// Its id, or `None` for a group this client made up itself because the
    /// project's daemon is too old to keep tabs.
    pub id: Option<TabId>,
    /// The name the user gave it.
    pub name: Option<String>,
    /// What it tiles, in order: each member, then the subagents opened
    /// beside it.
    pub panes: Vec<PaneId>,
}

/// The tabs of `project`, made from `tileable`: every pane to tile, each
/// top-level pane followed by the subagents opened beside it.
///
/// Never empty: a project with nothing to tile still has a tab to be on.
pub fn views(state: &AppState, project: Option<ProjectId>, tileable: &[PaneId]) -> Vec<TabView> {
    let mut views = match project.and_then(|project| state.project_tabs(project)) {
        Some(tabs) => kept(state, tabs, tileable),
        // A daemon too old to keep tabs: grouped four at a time, in order,
        // as every client grouped them before tabs were kept.
        None => tileable
            .chunks(TAB_CAPACITY)
            .map(|chunk| TabView {
                id: None,
                name: None,
                panes: chunk.to_vec(),
            })
            .collect(),
    };

    if views.is_empty() {
        views.push(TabView {
            id: None,
            name: None,
            panes: Vec::new(),
        });
    }
    views
}

/// The tabs their owner keeps, each tiling its members and the subagents
/// opened beside them.
fn kept(state: &AppState, tabs: &ProjectTabs, tileable: &[PaneId]) -> Vec<TabView> {
    // `tileable` runs a top-level pane, then its opened subagents, then the
    // next top-level pane: split it into those runs, one per member.
    let mut runs: Vec<Vec<PaneId>> = Vec::new();
    for &id in tileable {
        let top_level = state.pane(id).is_some_and(|pane| pane.parent.is_none());
        match runs.last_mut() {
            Some(run) if !top_level => run.push(id),
            _ => runs.push(vec![id]),
        }
    }

    let mut shown = HashSet::new();
    let mut views: Vec<TabView> = tabs
        .tabs()
        .iter()
        .map(|tab| {
            let panes: Vec<PaneId> = tab
                .panes
                .iter()
                .filter_map(|member| runs.iter().find(|run| run.first() == Some(member)))
                .flatten()
                .copied()
                .collect();
            shown.extend(panes.iter().copied());
            TabView {
                id: Some(tab.id),
                name: tab.name.clone(),
                panes,
            }
        })
        .collect();

    // A tab with nothing on the grid: its panes exited or closed a moment
    // before the snapshot that removes it.
    views.retain(|view| !view.panes.is_empty());

    // A pane no tab holds yet, having started a moment before the snapshot
    // that places it, goes on the last tab rather than nowhere.
    let stray: Vec<PaneId> = tileable
        .iter()
        .copied()
        .filter(|id| !shown.contains(id))
        .collect();
    if !stray.is_empty() {
        match views.last_mut() {
            Some(last) => last.panes.extend(stray),
            None => views.push(TabView {
                id: None,
                name: None,
                panes: stray,
            }),
        }
    }

    views
}

/// What a tab is called: the name it was given, else its first pane's title.
#[allow(dead_code)] // used once open_close_tab is wired to a key
pub fn name(state: &AppState, view: &TabView) -> String {
    view.name
        .clone()
        .or_else(|| {
            view.panes
                .first()
                .and_then(|id| state.pane(*id))
                .map(|pane| pane.title.clone())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
