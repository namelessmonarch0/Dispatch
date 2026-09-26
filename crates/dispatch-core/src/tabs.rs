//! Tabs: the groups of panes a project's grid shows one at a time.
//!
//! Owned rather than derived. A tab is made on purpose and keeps its panes
//! until they leave, so closing one pane never moves panes on another tab.
//! The daemon keeps these for its projects and a standalone client for its
//! own, and both run the same operations here, so the rules live in one
//! place.

use serde::{Deserialize, Serialize};

use crate::id::{PaneId, TabId};

/// How many placed panes a tab tiles.
///
/// Four is the most that stays readable in a terminal: past it every pane is
/// too narrow for a wrapped line of code and too short for a prompt and its
/// answer. A subagent opened beside its parent is not placed and does not
/// count.
pub const TAB_CAPACITY: usize = 4;

/// The longest name a tab keeps, in characters.
///
/// Every tab travels in every snapshot, so a pasted paragraph would travel
/// with every change to any tab in the project.
pub const NAME_LIMIT: usize = 64;

/// One tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    /// Stable identifier.
    pub id: TabId,
    /// The name the user gave it. `None` shows its first pane's title.
    #[serde(default)]
    pub name: Option<String>,
    /// Its panes, in tiling order.
    #[serde(default)]
    pub panes: Vec<PaneId>,
}

/// Where a new or moved pane goes.
///
/// Tagged like the protocol's other nested enums, with somewhere for a newer
/// peer's variant to land: this travels inside a message, and one this build
/// cannot read would otherwise fail the whole frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Placement {
    /// No preference: the last tab if it has room, else a new tab at the end.
    ///
    /// Never back-fills an earlier tab, so panes group the way an older
    /// client's four-at-a-time chunking groups them.
    #[default]
    Auto,
    /// Into `tab`; if it is full, a new tab straight after it.
    Into {
        /// The tab asked for.
        tab: TabId,
    },
    /// A new tab straight after `tab`, or at the end when `None`.
    NewAfter {
        /// The tab to follow.
        tab: Option<TabId>,
    },
    /// A placement from a newer peer, treated as [`Placement::Auto`].
    #[serde(other)]
    Unknown,
}

/// Why a tab operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TabError {
    /// The tab already holds [`TAB_CAPACITY`] panes.
    #[error("that tab is full ({max} panes)", max = TAB_CAPACITY)]
    Full,
    /// No tab has that id: another client may just have removed it.
    #[error("that tab is gone")]
    NoSuchTab,
    /// The pane is on no tab.
    #[error("that pane is not on a tab")]
    NoSuchPane,
}

/// Where a pane is about to go.
///
/// Named by id rather than by index: taking a pane off its old tab can
/// remove that tab and shift every index after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A tab that exists.
    Existing(TabId),
    /// A new tab after this one, or at the end.
    New(Option<TabId>),
}

/// One project's tabs, in the order the row shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectTabs {
    tabs: Vec<Tab>,
}

impl ProjectTabs {
    /// No tabs yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Tabs as a snapshot from their owner describes them.
    #[must_use]
    pub fn from_tabs(tabs: Vec<Tab>) -> Self {
        Self { tabs }
    }

    /// Every tab, in row order.
    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Where `tab` is in the row.
    #[must_use]
    pub fn position(&self, tab: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == tab)
    }

    /// The tab `pane` is on.
    #[must_use]
    pub fn tab_of(&self, pane: PaneId) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|t| t.panes.contains(&pane))
            .map(|t| t.id)
    }

    /// Whether `tab` has no room for another pane. A tab that is gone has none.
    #[must_use]
    pub fn is_full(&self, tab: TabId) -> bool {
        self.tabs
            .iter()
            .find(|t| t.id == tab)
            .is_none_or(|t| t.panes.len() >= TAB_CAPACITY)
    }

    /// Puts a new pane on a tab, and returns which.
    ///
    /// Never refused: a pane that has started has to be tiled somewhere. A
    /// full or vanished tab gives way to a new one, and a pane already on a
    /// tab stays where it is.
    pub fn place(&mut self, pane: PaneId, place: Placement) -> TabId {
        if let Some(tab) = self.tab_of(pane) {
            return tab;
        }

        let slot = match place {
            Placement::Into { tab } if self.position(tab).is_some() => {
                if self.is_full(tab) {
                    Slot::New(Some(tab))
                } else {
                    Slot::Existing(tab)
                }
            }
            Placement::NewAfter { tab } => {
                Slot::New(tab.filter(|tab| self.position(*tab).is_some()))
            }
            Placement::Into { .. } | Placement::Auto | Placement::Unknown => self.auto(),
        };

        self.put(pane, slot)
    }

    /// Takes `pane` off its tab, removing the tab if nothing is left on it.
    ///
    /// Returns whether it was on one.
    pub fn remove(&mut self, pane: PaneId) -> bool {
        let Some(index) = self.tabs.iter().position(|t| t.panes.contains(&pane)) else {
            return false;
        };

        self.tabs[index].panes.retain(|p| *p != pane);
        if self.tabs[index].panes.is_empty() {
            self.tabs.remove(index);
        }
        true
    }

    /// Moves a pane that is on a tab to another tab, or onto a new one.
    ///
    /// Refused rather than redirected, unlike [`Self::place`]: the user asked
    /// for a particular tab, and quietly putting the pane somewhere else would
    /// lose it.
    pub fn move_pane(&mut self, pane: PaneId, to: Placement) -> Result<TabId, TabError> {
        let from = self.tab_of(pane).ok_or(TabError::NoSuchPane)?;
        let alone = self
            .tabs
            .iter()
            .find(|t| t.id == from)
            .is_some_and(|t| t.panes.len() == 1);

        let slot = match to {
            Placement::Into { tab } if tab == from => return Ok(from),
            Placement::Into { tab } => {
                if self.position(tab).is_none() {
                    return Err(TabError::NoSuchTab);
                }
                if self.is_full(tab) {
                    return Err(TabError::Full);
                }
                Slot::Existing(tab)
            }
            Placement::NewAfter { tab: Some(tab) } if self.position(tab).is_none() => {
                return Err(TabError::NoSuchTab);
            }
            // Alone on its tab already: a new tab of its own is the one it has.
            Placement::NewAfter { .. } if alone => return Ok(from),
            Placement::NewAfter { tab } => Slot::New(tab),
            Placement::Auto | Placement::Unknown => match self.auto() {
                Slot::Existing(tab) if tab == from => return Ok(from),
                slot => slot,
            },
        };

        self.remove(pane);
        Ok(self.put(pane, slot))
    }

    /// Names a tab.
    ///
    /// A name with nothing left in it once control characters and the space
    /// around it are gone clears the name, so the tab shows its first pane's
    /// title again.
    pub fn rename(&mut self, tab: TabId, name: &str) -> Result<(), TabError> {
        let tab = self
            .tabs
            .iter_mut()
            .find(|t| t.id == tab)
            .ok_or(TabError::NoSuchTab)?;

        let visible: String = name.chars().filter(|c| !c.is_control()).collect();
        let kept: String = visible.trim().chars().take(NAME_LIMIT).collect();
        tab.name = (!kept.is_empty()).then_some(kept);
        Ok(())
    }

    /// The panes on `tab`, which is what closing it closes.
    pub fn members(&self, tab: TabId) -> Result<Vec<PaneId>, TabError> {
        self.tabs
            .iter()
            .find(|t| t.id == tab)
            .map(|t| t.panes.clone())
            .ok_or(TabError::NoSuchTab)
    }

    /// Moves a tab to `index` in the row, or to the end past it.
    pub fn move_tab(&mut self, tab: TabId, index: usize) -> Result<(), TabError> {
        let from = self.position(tab).ok_or(TabError::NoSuchTab)?;
        let moved = self.tabs.remove(from);
        let to = index.min(self.tabs.len());
        self.tabs.insert(to, moved);
        Ok(())
    }

    /// Where [`Placement::Auto`] puts a pane.
    fn auto(&self) -> Slot {
        match self.tabs.last() {
            Some(last) if last.panes.len() < TAB_CAPACITY => Slot::Existing(last.id),
            _ => Slot::New(None),
        }
    }

    /// Puts `pane` in `slot`, and returns the tab it landed on.
    fn put(&mut self, pane: PaneId, slot: Slot) -> TabId {
        match slot {
            Slot::Existing(tab) => {
                if let Some(existing) = self.tabs.iter_mut().find(|t| t.id == tab) {
                    existing.panes.push(pane);
                    return tab;
                }
                // Only a tab that vanished between choosing it and now; a new
                // tab beats losing the pane.
                self.put(pane, Slot::New(None))
            }
            Slot::New(after) => {
                let index = after
                    .and_then(|tab| self.position(tab))
                    .map_or(self.tabs.len(), |at| at + 1);
                let tab = Tab {
                    id: TabId::new(),
                    name: None,
                    panes: vec![pane],
                };
                let id = tab.id;
                self.tabs.insert(index, tab);
                id
            }
        }
    }
}

#[cfg(test)]
mod tests;
