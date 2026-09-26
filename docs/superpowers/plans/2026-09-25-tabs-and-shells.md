# C — Tabs You Own, and Shell Panes: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tabs become objects the user makes, names, fills, reorders and closes, kept by the daemon beside the panes. A `shell` entry opens the user's own `$SHELL` in a pane, with their prompt setup (Starship and so on) intact.

**Architecture:**
- **Tab model.** A tab model in `dispatch-core` (`ProjectTabs`) is run by whoever owns a project's panes: the daemon for its projects, a standalone client for its own.
- **Protocol.** The daemon sends a whole-project `Tabs` snapshot after every change. A client that never hears one from a daemon falls back to today's four-per-tab chunking.
- **Client.** It draws named tabs with a `+`, reads a zellij-style `Ctrl t` tab mode plus direct `Alt` keys, and sends tab commands (or applies them itself when standalone).
- **Shell.** Shell resolution lives in `dispatch-os`. A built-in `shell` harness is added to every registry. Every pane gets Dispatch's own `TERM`.

**Tech Stack:** Rust 2024 workspace, ratatui 0.29, crossterm 0.28, portable-pty 0.9, serde + rmp-serde (MessagePack, named fields), thiserror.

**Spec:** `docs/superpowers/specs/2026-09-25-tabs-and-shells-design.md`

## Global Constraints

- Platform `#[cfg]` attributes live only in `dispatch-os`. Tests may carry `#[cfg(...)]`.
- CI runs three commands on `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-gnu`:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`

  Windows is already red on `main` for unrelated reasons in `dispatch-os/src/host.rs` and `ipc.rs`. Add nothing new to it.
- Every `unsafe` block carries `// SAFETY:`.
- `dispatch_proto::VERSION` stays **1.1**. Every field added to a message carries `#[serde(default)]`, and every new nested enum has a `#[serde(other)] Unknown`.
- No new external dependencies.
- Tabs:
  - A tab holds at most **4** placed panes (`TAB_CAPACITY`). An opened subagent is never placed and never counts.
  - Names are stored trimmed, with control characters stripped, at most **64** characters (`NAME_LIMIT`). The row shows at most **16** columns of a name (`TAB_TITLE`), by display width.
  - The row shows no numbers.
- Placement:
  - `Auto` means the last tab if it has room, else a new tab at the end. It never back-fills.
  - `Into { tab }` goes into that tab, or into a new tab straight after it if it is full.
  - `NewAfter { tab }` makes a new tab after `tab`, or at the end.
  - `Unknown` is treated as `Auto`.
- Status texts, verbatim:
  - `that tab is full (4 panes)`
  - `no tab to the left`
  - `this machine's Dispatch needs upgrading for tabs`
  - `a subagent stays beside the pane that asked for it`
  - `that tab is gone`
- Close prompt, verbatim: `Close "<name>" and its <n> panes? y/n` (`pane` when `n` is 1).
- Tab mode status row, verbatim: `TAB  n new  r rename  x close  ←→ switch  [ ] move pane  i o move tab  1-9 go  Esc done`
- Keys:
  - `Ctrl t` enters tab mode. In tab mode:
    - `n`, `r`, `x`, `1`–`9`, `Tab`, `Esc` and `Enter` end the mode.
    - `←` `→` `h` `l` `[` `]` `i` `o` keep it on.
    - `Ctrl t` sends `Ctrl t` to the pane and ends it.
    - Any other key is ignored and the mode stays on.
    - A mouse click ends it.
  - Direct keys: `Alt n`, `Alt i`, `Alt o`, `Alt ←` / `Alt h`, `Alt →` / `Alt l`, `Alt ↑` / `Alt k`, `Alt ↓` / `Alt j`.
  - All existing `^a` bindings are unchanged.
- Shell:
  - The harness id is `shell`, the display name `Shell`, and the icon the default `\u{f120}`.
  - The picker lists it first and pre-selects it, labelled `Shell · <program>` for a project on this machine and `Shell` for one on another machine.
  - Resolution order:
    1. `[shell] command`
    2. `$SHELL`, if executable
    3. the login record, if executable
    4. `/bin/sh`
  - Windows uses `pwsh` if it is on `PATH`, else `powershell`.
  - Login mode `auto` means `-l` on macOS only; `always` and `never` force it. Windows never gets `-l`.
- `config.toml`:

  ```toml
  [shell]
  command = "…"
  args = []
  login = "auto|always|never"
  ```

  A missing section means the defaults.
- Every pane's environment:
  - Remove `TERM_SESSION_ID`, `ITERM_SESSION_ID`, `LC_TERMINAL`, `LC_TERMINAL_VERSION`, `KITTY_WINDOW_ID`, `KITTY_PID`, `KITTY_LISTEN_ON`, `WEZTERM_PANE`, `WEZTERM_UNIX_SOCKET`, `ALACRITTY_WINDOW_ID`, `ALACRITTY_SOCKET`, `WT_SESSION`, `WT_PROFILE_ID`, `VTE_VERSION`, `KONSOLE_VERSION`, `KONSOLE_DBUS_SESSION`, `GHOSTTY_RESOURCES_DIR` and `GHOSTTY_BIN_DIR`.
  - Then set `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=dispatch` and `TERM_PROGRAM_VERSION=<crate version>`.
  - Then apply the harness's own `env`, which wins.
- Comments explain why, in the surrounding code's voice.
- Commits are `type(scope): lowercase summary`, ending with the session's attribution trailer lines.

## Review Focus

These are the five conditions most likely to bite a user that no task's tests would otherwise exercise. Each now has a test in the task named, most likely first.

1. **Two clients race for a tab's last slot.** The daemon lets the first move in and refuses the second with `that tab is full (4 panes)`. Pinned in Task 4 by `the_second_of_two_moves_into_the_last_slot_is_refused`.
2. **A client reattaches mid-session.** It sees exactly the arrangement the daemon holds. The snapshot comes after the panes it names, and an empty project still gets one. Pinned in Task 4 by `a_client_attaching_later_hears_every_projects_tabs_after_their_panes`.
3. **The only pane on the tab on screen exits.** The tab leaves the row, the view falls back to a tab that exists, and nothing panics. Pinned in Task 8 by `a_tab_whose_last_pane_exits_leaves_the_row`.
4. **A project whose daemon is older.** Panes still group four at a time. Every tab command says `this machine's Dispatch needs upgrading for tabs` and sends nothing. Pinned in Task 9 by `a_daemon_too_old_for_tabs_is_asked_nothing`.
5. **Tab mode is on when an overlay opens, or a click lands.** Keys go to the picker or prompt the user is looking at, not to tab mode. Pinned in Task 10 by `a_tab_mode_key_that_opens_the_picker_hands_it_the_keyboard` and `a_click_ends_tab_mode`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/dispatch-core/src/id.rs` | + `TabId` |
| `crates/dispatch-core/src/tabs.rs` (new) + `tabs/tests.rs` | `Tab`, `Placement`, `TabError`, `ProjectTabs`, `TAB_CAPACITY`, `NAME_LIMIT` |
| `crates/dispatch-core/src/state.rs` | `AppState` keeps `ProjectTabs` per project; panes leave tabs on close/exit |
| `crates/dispatch-proto/src/message.rs` + `message/tests.rs` | `SpawnPane.place`; `MovePane`, `RenameTab`, `CloseTab`, `MoveTab`; `ServerMessage::Tabs` |
| `crates/dispatch-daemon/src/session.rs` + `session/tests.rs` | daemon owns tabs, snapshots, handles tab commands |
| `crates/dispatch-os/src/shell.rs` (new) | `user_shell`, `login_by_default`, `takes_login_flag` |
| `crates/dispatch-config/src/config.rs` (+ tests) | `ShellConfig`, `LoginShell`, `ShellConfig::launch` |
| `crates/dispatch-config/src/lib.rs` (+ tests) | `SHELL`, `HarnessRegistry::with_shell`, `reloaded` |
| `crates/dispatch-pty/src/session.rs` (+ tests) | pane environment |
| `crates/dispatch-tui/src/input.rs` (+ tests) | tab mode, direct `Alt` keys, new `Action`s, `KeyMode` |
| `dispatch/src/tabs.rs` (new) + `tabs/tests.rs` | `TabView`, `views`, `name`, `visible_range` |
| `dispatch/src/app.rs` | wiring: snapshots, placement, tab commands, overlays, tab row, status row, picker |
| `dispatch/src/main.rs`, `dispatchd/src/main.rs` | registries get the shell |
| `dispatch/tests/end_to_end.rs` | fixture's `[shell]`; shell and tab-mode tests |
| `README.md` | tabs, keys, shell panes, `[shell]`, pane environment |

---
### Task 1: The tab model

**Files:**
- Modify: `crates/dispatch-core/src/id.rs` (add `TabId` after `ProjectId`)
- Create: `crates/dispatch-core/src/tabs.rs`
- Create: `crates/dispatch-core/src/tabs/tests.rs`
- Modify: `crates/dispatch-core/src/lib.rs`

**Interfaces:**
- Consumes: `id_type!` in `id.rs`; `PaneId`.
- Produces (used by Tasks 2–4, 8–11):
  - `dispatch_core::TabId`
  - `dispatch_core::tabs::{Tab { id: TabId, name: Option<String>, panes: Vec<PaneId> }, Placement::{Auto, Into { tab }, NewAfter { tab: Option<TabId> }, Unknown}, TabError::{Full, NoSuchTab, NoSuchPane}, ProjectTabs, TAB_CAPACITY, NAME_LIMIT}`, re-exported at the crate root.
  - `ProjectTabs` methods:
    - `new() -> Self`, `from_tabs(Vec<Tab>) -> Self`, `tabs(&self) -> &[Tab]`
    - `position(&self, TabId) -> Option<usize>`, `tab_of(&self, PaneId) -> Option<TabId>`, `is_full(&self, TabId) -> bool`
    - `place(&mut self, PaneId, Placement) -> TabId`, `remove(&mut self, PaneId) -> bool`
    - `move_pane(&mut self, PaneId, Placement) -> Result<TabId, TabError>`
    - `rename(&mut self, TabId, &str) -> Result<(), TabError>`
    - `members(&self, TabId) -> Result<Vec<PaneId>, TabError>`
    - `move_tab(&mut self, TabId, usize) -> Result<(), TabError>`

- [ ] **Step 1: Add `TabId`**

In `crates/dispatch-core/src/id.rs`, after the `ProjectId` block:

```rust
id_type! {
    /// Identifies a tab: a group of panes a project's grid shows together.
    TabId
}
```

- [ ] **Step 2: Write the failing tests**

Create `crates/dispatch-core/src/tabs/tests.rs`:

```rust
//! Tests for the tab model.

use super::*;

/// `count` fresh pane ids.
fn panes(count: usize) -> Vec<PaneId> {
    (0..count).map(|_| PaneId::new()).collect()
}

/// Each tab's panes, in row order.
fn layout(tabs: &ProjectTabs) -> Vec<Vec<PaneId>> {
    tabs.tabs().iter().map(|tab| tab.panes.clone()).collect()
}

/// Tabs holding `groups`, one tab per group, made the way a user makes them:
/// each group's first pane opens a new tab, the rest go into it.
fn tabs_of(groups: &[&[PaneId]]) -> ProjectTabs {
    let mut tabs = ProjectTabs::new();
    for group in groups {
        let mut current = None;
        for pane in *group {
            let place = match current {
                None => Placement::NewAfter {
                    tab: tabs.tabs().last().map(|tab| tab.id),
                },
                Some(tab) => Placement::Into { tab },
            };
            current = Some(tabs.place(*pane, place));
        }
    }
    tabs
}

#[test]
fn auto_fills_the_last_tab_then_starts_another() {
    let ids = panes(5);
    let mut tabs = ProjectTabs::new();
    for id in &ids {
        tabs.place(*id, Placement::Auto);
    }

    assert_eq!(layout(&tabs), vec![ids[..4].to_vec(), ids[4..].to_vec()]);
}

#[test]
fn auto_never_back_fills_an_earlier_tab() {
    // An older client groups panes four at a time, in order. Back-filling
    // would put a new pane on a tab that client draws it nowhere near.
    let ids = panes(6);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..5]]);

    tabs.place(ids[5], Placement::Auto);

    assert_eq!(
        layout(&tabs),
        vec![ids[..1].to_vec(), ids[1..5].to_vec(), vec![ids[5]]]
    );
}

#[test]
fn a_pane_asked_into_a_tab_with_room_goes_there() {
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..2]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(tabs.place(ids[2], Placement::Into { tab: first }), first);
    assert_eq!(layout(&tabs), vec![vec![ids[0], ids[2]], vec![ids[1]]]);
}

#[test]
fn a_pane_asked_into_a_full_tab_starts_a_new_one_straight_after_it() {
    let ids = panes(6);
    let mut tabs = tabs_of(&[&ids[..4], &ids[4..5]]);
    let first = tabs.tabs()[0].id;

    tabs.place(ids[5], Placement::Into { tab: first });

    assert_eq!(
        layout(&tabs),
        vec![ids[..4].to_vec(), vec![ids[5]], vec![ids[4]]]
    );
}

#[test]
fn a_new_tab_goes_straight_after_the_one_named_or_at_the_end() {
    let ids = panes(4);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..2]]);
    let first = tabs.tabs()[0].id;

    tabs.place(ids[2], Placement::NewAfter { tab: Some(first) });
    tabs.place(ids[3], Placement::NewAfter { tab: None });

    assert_eq!(
        layout(&tabs),
        vec![vec![ids[0]], vec![ids[2]], vec![ids[1]], vec![ids[3]]]
    );
}

#[test]
fn a_tab_that_is_gone_gives_way_rather_than_losing_the_pane() {
    // Another client can remove a tab between this one reading it and the
    // spawn arriving; the pane has started either way and must go somewhere.
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..1]]);
    let gone = TabId::new();

    tabs.place(ids[1], Placement::Into { tab: gone });
    tabs.place(ids[2], Placement::NewAfter { tab: Some(gone) });

    assert_eq!(layout(&tabs), vec![vec![ids[0], ids[1]], vec![ids[2]]]);
}

#[test]
fn an_unknown_placement_is_auto() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..1]]);

    tabs.place(ids[1], Placement::Unknown);

    assert_eq!(layout(&tabs), vec![ids.clone()]);
}

#[test]
fn placing_a_pane_twice_leaves_it_where_it_is() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(tabs.place(ids[0], Placement::NewAfter { tab: None }), first);
    assert_eq!(layout(&tabs), vec![vec![ids[0]], vec![ids[1]]]);
}

#[test]
fn a_pane_leaving_moves_nothing_on_other_tabs() {
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..2], &ids[2..]]);

    assert!(tabs.remove(ids[0]));
    assert_eq!(layout(&tabs), vec![vec![ids[1]], vec![ids[2]]]);
}

#[test]
fn a_tab_with_nothing_left_on_it_is_removed() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..]]);

    tabs.remove(ids[0]);

    assert_eq!(layout(&tabs), vec![vec![ids[1]]]);
}

#[test]
fn removing_a_pane_on_no_tab_changes_nothing() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);

    assert!(!tabs.remove(PaneId::new()));
    assert_eq!(layout(&tabs), vec![ids]);
}

#[test]
fn a_pane_moves_into_a_tab_with_room() {
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..2], &ids[2..]]);
    let second = tabs.tabs()[1].id;

    assert_eq!(
        tabs.move_pane(ids[0], Placement::Into { tab: second }),
        Ok(second)
    );
    assert_eq!(layout(&tabs), vec![vec![ids[1]], vec![ids[2], ids[0]]]);
}

#[test]
fn moving_into_a_full_tab_is_refused_and_moves_nothing() {
    let ids = panes(5);
    let mut tabs = tabs_of(&[&ids[..4], &ids[4..]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(
        tabs.move_pane(ids[4], Placement::Into { tab: first }),
        Err(TabError::Full)
    );
    assert_eq!(layout(&tabs), vec![ids[..4].to_vec(), vec![ids[4]]]);
}

#[test]
fn moving_into_a_tab_that_is_gone_is_refused() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);

    assert_eq!(
        tabs.move_pane(ids[0], Placement::Into { tab: TabId::new() }),
        Err(TabError::NoSuchTab)
    );
    assert_eq!(
        tabs.move_pane(
            ids[0],
            Placement::NewAfter {
                tab: Some(TabId::new())
            }
        ),
        Err(TabError::NoSuchTab)
    );
}

#[test]
fn a_pane_on_no_tab_cannot_be_moved() {
    let mut tabs = ProjectTabs::new();

    assert_eq!(
        tabs.move_pane(PaneId::new(), Placement::Auto),
        Err(TabError::NoSuchPane)
    );
}

#[test]
fn moving_past_the_last_tab_makes_a_new_one() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..]]);
    let first = tabs.tabs()[0].id;

    let new = tabs
        .move_pane(ids[1], Placement::NewAfter { tab: Some(first) })
        .expect("the move is allowed");

    assert_ne!(new, first);
    assert_eq!(layout(&tabs), vec![vec![ids[0]], vec![ids[1]]]);
}

#[test]
fn a_pane_alone_on_its_tab_is_already_on_a_new_one() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(
        tabs.move_pane(ids[0], Placement::NewAfter { tab: Some(first) }),
        Ok(first)
    );
    assert_eq!(tabs.tabs().len(), 1);
}

#[test]
fn moving_the_last_pane_off_a_tab_removes_the_tab() {
    let ids = panes(2);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..]]);
    let first = tabs.tabs()[0].id;

    tabs.move_pane(ids[1], Placement::Into { tab: first })
        .expect("there is room");

    assert_eq!(layout(&tabs), vec![ids.clone()]);
}

#[test]
fn a_name_is_cleaned_trimmed_and_capped() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);
    let tab = tabs.tabs()[0].id;

    tabs.rename(tab, "  fix\u{7}  login\n ")
        .expect("the tab exists");
    assert_eq!(tabs.tabs()[0].name.as_deref(), Some("fix  login"));

    tabs.rename(tab, &"x".repeat(100)).expect("the tab exists");
    assert_eq!(
        tabs.tabs()[0].name.as_ref().map(|name| name.chars().count()),
        Some(NAME_LIMIT)
    );
}

#[test]
fn an_empty_name_goes_back_to_the_automatic_one() {
    let ids = panes(1);
    let mut tabs = tabs_of(&[&ids[..]]);
    let tab = tabs.tabs()[0].id;

    tabs.rename(tab, "work").expect("the tab exists");
    tabs.rename(tab, " \t ").expect("the tab exists");

    assert_eq!(tabs.tabs()[0].name, None);
}

#[test]
fn renaming_a_tab_that_is_gone_is_refused() {
    let mut tabs = ProjectTabs::new();

    assert_eq!(tabs.rename(TabId::new(), "work"), Err(TabError::NoSuchTab));
}

#[test]
fn a_tabs_members_are_what_closing_it_closes() {
    let ids = panes(3);
    let tabs = tabs_of(&[&ids[..2], &ids[2..]]);
    let first = tabs.tabs()[0].id;

    assert_eq!(tabs.members(first), Ok(ids[..2].to_vec()));
    assert_eq!(tabs.members(TabId::new()), Err(TabError::NoSuchTab));
}

#[test]
fn a_tab_moves_within_the_row_and_stops_at_its_end() {
    let ids = panes(3);
    let mut tabs = tabs_of(&[&ids[..1], &ids[1..2], &ids[2..]]);
    let first = tabs.tabs()[0].id;

    tabs.move_tab(first, 1).expect("the tab exists");
    assert_eq!(layout(&tabs), vec![vec![ids[1]], vec![ids[0]], vec![ids[2]]]);

    tabs.move_tab(first, 99).expect("the tab exists");
    assert_eq!(layout(&tabs), vec![vec![ids[1]], vec![ids[2]], vec![ids[0]]]);

    assert_eq!(tabs.move_tab(TabId::new(), 0), Err(TabError::NoSuchTab));
}

#[test]
fn the_full_message_names_the_capacity() {
    assert_eq!(TabError::Full.to_string(), "that tab is full (4 panes)");
    assert_eq!(TabError::NoSuchTab.to_string(), "that tab is gone");
}

#[test]
fn a_tab_that_is_gone_has_no_room() {
    assert!(ProjectTabs::new().is_full(TabId::new()));
}

#[test]
fn the_default_placement_is_auto() {
    assert_eq!(Placement::default(), Placement::Auto);
}
```

- [ ] **Step 3: Write the module shell and see it fail**

Create `crates/dispatch-core/src/tabs.rs` containing only its doc comment, then `#[cfg(test)] mod tests;`. Add `pub mod tabs;` to `crates/dispatch-core/src/lib.rs`.

Run: `cargo test -p dispatch-core tabs`
Expected: FAIL to compile, because `ProjectTabs`, `Placement`, `TabError` and `NAME_LIMIT` are not found.

- [ ] **Step 4: Implement**

Replace `crates/dispatch-core/src/tabs.rs` with:

```rust
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
```

In `crates/dispatch-core/src/lib.rs`, add `pub mod tabs;` in alphabetical order. Then extend the re-exports:

```rust
pub use id::{DeviceId, PaneId, ProjectId, RequestId, TabId};
pub use tabs::{NAME_LIMIT, Placement, ProjectTabs, TAB_CAPACITY, Tab, TabError};
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p dispatch-core tabs`
Expected: PASS, 26 tests.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy -p dispatch-core --all-targets -- -D warnings`

```bash
git add crates/dispatch-core/src/id.rs crates/dispatch-core/src/lib.rs crates/dispatch-core/src/tabs.rs crates/dispatch-core/src/tabs/tests.rs
git commit -m "feat(core): tabs that group a project's panes and keep them"
```

---

### Task 2: `AppState` keeps each project's tabs

**Files:**
- Modify: `crates/dispatch-core/src/state.rs`. This touches the imports, the `AppState` fields, `remove_project`, `forget_device_projects`, `close_pane` and `set_pane_status`, and adds new methods and tests in the inline `mod tests`.

**Interfaces:**
- Consumes: Task 1's `ProjectTabs`, `Placement`, `TabId`.
- Produces (used by Tasks 8–11):
  - `AppState::project_tabs(&self, ProjectId) -> Option<&ProjectTabs>`
  - `AppState::project_tabs_mut(&mut self, ProjectId) -> Option<&mut ProjectTabs>`
  - `AppState::set_project_tabs(&mut self, ProjectId, ProjectTabs) -> bool`
  - `AppState::place_pane(&mut self, PaneId, Placement) -> Option<TabId>`
  - A pane leaves its tab on `close_pane`, and on `set_pane_status` with an exited status.

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` at the bottom of `crates/dispatch-core/src/state.rs`. Import `Device` and `ProjectSource` in the test module if its existing `use` lines do not already bring them in.

```rust
    /// A state with one project whose tabs are kept, and that project.
    fn state_with_tabs() -> (AppState, ProjectId) {
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/tabs", ProjectSource::LocalDir));
        assert!(state.set_project_tabs(project, ProjectTabs::new()));
        (state, project)
    }

    #[test]
    fn a_project_has_no_tabs_until_something_keeps_them() {
        // Absence is how a client knows the project's daemon is too old to
        // keep tabs, and so groups its panes itself.
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/tabs", ProjectSource::LocalDir));
        let pane = state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");

        assert!(state.project_tabs(project).is_none());
        assert_eq!(state.place_pane(pane, Placement::Auto), None);
    }

    #[test]
    fn tabs_for_a_project_not_yet_announced_are_not_kept() {
        let mut state = AppState::new();
        let unknown = ProjectId::new();

        assert!(!state.set_project_tabs(unknown, ProjectTabs::new()));
        assert!(state.project_tabs(unknown).is_none());
    }

    #[test]
    fn a_top_level_pane_is_placed_on_a_tab() {
        let (mut state, project) = state_with_tabs();
        let pane = state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");

        let tab = state
            .place_pane(pane, Placement::Auto)
            .expect("the project keeps tabs");

        assert_eq!(
            state.project_tabs(project).and_then(|tabs| tabs.tab_of(pane)),
            Some(tab)
        );
    }

    #[test]
    fn a_subagent_is_never_placed() {
        // It is tiled beside the pane that asked for it, wherever that is.
        let (mut state, project) = state_with_tabs();
        let parent = state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        let mut child = Pane::new(project, HarnessId::new("shell"));
        child.parent = Some(parent);
        let child = state.adopt_pane(child).expect("the project exists");

        assert_eq!(state.place_pane(child, Placement::Auto), None);
    }

    #[test]
    fn closing_a_pane_takes_it_off_its_tab() {
        let (mut state, project) = state_with_tabs();
        let pane = state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        state.place_pane(pane, Placement::Auto);

        state.close_pane(pane).expect("the pane exists");

        assert!(state
            .project_tabs(project)
            .is_some_and(|tabs| tabs.tabs().is_empty()));
    }

    #[test]
    fn a_pane_that_exits_gives_its_place_back() {
        let (mut state, project) = state_with_tabs();
        let pane = state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        state.place_pane(pane, Placement::Auto);

        state
            .set_pane_status(pane, PaneStatus::Exited(0))
            .expect("the pane exists");

        assert!(state
            .project_tabs(project)
            .is_some_and(|tabs| tabs.tabs().is_empty()));
    }

    #[test]
    fn working_or_waiting_keeps_a_pane_on_its_tab() {
        let (mut state, project) = state_with_tabs();
        let pane = state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        let tab = state.place_pane(pane, Placement::Auto);

        for status in [PaneStatus::Running, PaneStatus::Idle, PaneStatus::Blocked] {
            state.set_pane_status(pane, status).expect("the pane exists");
        }

        assert_eq!(
            state.project_tabs(project).and_then(|tabs| tabs.tab_of(pane)),
            tab
        );
    }

    #[test]
    fn forgetting_a_project_forgets_its_tabs() {
        let (mut state, project) = state_with_tabs();

        state
            .remove_project(project)
            .expect("the project has no panes");

        assert!(state.project_tabs(project).is_none());
    }

    #[test]
    fn forgetting_a_machines_projects_forgets_their_tabs() {
        let mut state = AppState::new();
        let device = state.add_device(Device::new("other"));
        let project = state.add_project(
            Project::new("/tmp/tabs", ProjectSource::LocalDir).with_device(device),
        );
        state.set_project_tabs(project, ProjectTabs::new());

        state.forget_device_projects(device);

        assert!(state.project_tabs(project).is_none());
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p dispatch-core state`
Expected: FAIL to compile, because there is no method `set_project_tabs`, `project_tabs` or `place_pane`.

- [ ] **Step 3: Implement**

In the imports at the top of `state.rs`:

```rust
use std::collections::{HashMap, HashSet};

use crate::device::Device;
use crate::id::{DeviceId, PaneId, ProjectId, TabId};
use crate::pane::{HarnessId, Pane, PaneRole, PaneStatus};
use crate::project::Project;
use crate::tabs::{Placement, ProjectTabs};
```

Add the field after `unseen`:

```rust
    /// Each project's tabs, while whoever runs its panes keeps them: this
    /// client for its own, the daemon for its.
    ///
    /// Absent for a project whose daemon is too old to keep tabs, and that
    /// absence is how the client knows to group the panes four at a time
    /// itself.
    tabs: HashMap<ProjectId, ProjectTabs>,
```

Add these methods after `is_unseen`:

```rust
    /// A project's tabs, when anything keeps them.
    #[must_use]
    pub fn project_tabs(&self, project: ProjectId) -> Option<&ProjectTabs> {
        self.tabs.get(&project)
    }

    /// A project's tabs, mutably, when anything keeps them.
    pub fn project_tabs_mut(&mut self, project: ProjectId) -> Option<&mut ProjectTabs> {
        self.tabs.get_mut(&project)
    }

    /// Replaces a project's tabs with what their owner says they are.
    ///
    /// Returns whether the project is known. A snapshot for one this client
    /// has not been told about is ignored: keeping it would leave tabs behind
    /// for a project that may never arrive.
    pub fn set_project_tabs(&mut self, project: ProjectId, tabs: ProjectTabs) -> bool {
        if !self.projects.iter().any(|p| p.id == project) {
            return false;
        }
        self.tabs.insert(project, tabs);
        true
    }

    /// Puts a top-level pane on one of its project's tabs, when the project
    /// keeps tabs. Returns the tab.
    ///
    /// A subagent is never placed: it is tiled beside the pane that asked for
    /// it, wherever that pane is.
    pub fn place_pane(&mut self, id: PaneId, place: Placement) -> Option<TabId> {
        let pane = self.pane(id)?;
        if pane.parent.is_some() {
            return None;
        }
        let project = pane.project;
        self.tabs.get_mut(&project).map(|tabs| tabs.place(id, place))
    }

    /// Takes a pane off its tab, if it is on one.
    fn leave_tab(&mut self, id: PaneId) {
        let Some(project) = self.pane(id).map(|pane| pane.project) else {
            return;
        };
        if let Some(tabs) = self.tabs.get_mut(&project) {
            tabs.remove(id);
        }
    }
```

In `close_pane`, call it straight after the existence check and before `self.unseen.remove(&id);`:

```rust
        self.leave_tab(id);
```

Replace `set_pane_status` with:

```rust
    /// Records a new status for a pane.
    pub fn set_pane_status(&mut self, id: PaneId, status: PaneStatus) -> Result<(), StateError> {
        let pane = self
            .panes
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(StateError::NoSuchPane(id))?;
        pane.status = status;

        // An exited pane gives its place on the tab back, as a closed one
        // does: its output stays readable from the sidebar, but it has
        // nothing left to tile.
        if !status.is_live() {
            self.leave_tab(id);
        }
        Ok(())
    }
```

In `remove_project`, next to `self.collapsed_projects.remove(&project);`:

```rust
        self.tabs.remove(&project);
```

In `forget_device_projects`, inside the `for project in projects` loop:

```rust
            self.tabs.remove(&project);
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p dispatch-core`
Expected: PASS. All existing tests are unchanged.

- [ ] **Step 5: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy -p dispatch-core --all-targets -- -D warnings`

```bash
git add crates/dispatch-core/src/state.rs
git commit -m "feat(core): keep each project's tabs beside its panes"
```

---
### Task 3: Tabs on the wire

**Files:**
- Modify: `crates/dispatch-proto/src/message.rs`: `SpawnPane`, the new client messages, `ServerMessage::Tabs`, imports.
- Modify: `crates/dispatch-proto/src/message/tests.rs`
- Modify, adding `place: Placement::Auto` to every `ClientMessage::SpawnPane { … }` literal:
  - `crates/dispatch-daemon/src/session/tests.rs` (19 sites; add `use dispatch_core::Placement;` at the top)
  - `dispatchd/tests/serves_clients.rs` (3 sites)
  - `dispatch/tests/delegate_shim.rs` (1 site)
  - `dispatch/src/app.rs` (1 site, in `spawn_pane`)
- Modify: `crates/dispatch-daemon/src/session.rs`: only the `SpawnPane` match arm, which gains `..` so it still compiles. Task 4 uses the field.

**Interfaces:**
- Consumes: Task 1's `Placement`, `Tab`, `TabId`.
- Produces (used by Tasks 4, 8, 9):
  - `ClientMessage::SpawnPane { project, harness, size, place: Placement }`
  - `ClientMessage::MovePane { pane: PaneId, to: Placement }`
  - `ClientMessage::RenameTab { tab: TabId, name: String }`
  - `ClientMessage::CloseTab { tab: TabId }`
  - `ClientMessage::MoveTab { tab: TabId, index: usize }`
  - `ServerMessage::Tabs { project: ProjectId, tabs: Vec<Tab> }`

- [ ] **Step 1: Write the failing tests**

In `crates/dispatch-proto/src/message/tests.rs`:

- Change the `use serde::Serialize;` line to `use serde::{Deserialize, Serialize};`.
- Add `use dispatch_core::{Placement, Tab, TabId};`.
- In `every_client_message_round_trips`, replace the `SpawnPane` entry with the first entry below and add the rest:

```rust
        ClientMessage::SpawnPane {
            project: ProjectId::new(),
            harness: "claude".into(),
            size: (80, 24),
            place: Placement::Into { tab: TabId::new() },
        },
        ClientMessage::MovePane {
            pane: PaneId::new(),
            to: Placement::NewAfter { tab: None },
        },
        ClientMessage::RenameTab {
            tab: TabId::new(),
            name: "work".into(),
        },
        ClientMessage::CloseTab { tab: TabId::new() },
        ClientMessage::MoveTab {
            tab: TabId::new(),
            index: 2,
        },
```

In `every_server_message_round_trips`, add:

```rust
        ServerMessage::Tabs {
            project: ProjectId::new(),
            tabs: vec![Tab {
                id: TabId::new(),
                name: Some("work".into()),
                panes: vec![PaneId::new(), PaneId::new()],
            }],
        },
```

Append these tests:

```rust
#[test]
fn a_spawn_from_an_older_client_is_placed_automatically() {
    // An older client knows nothing of tabs and says nothing about where a
    // pane goes; the daemon must still place it rather than refuse it.
    #[derive(Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Older {
        SpawnPane {
            project: ProjectId,
            harness: String,
            size: (u16, u16),
        },
    }

    let project = ProjectId::new();
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &Older::SpawnPane {
            project,
            harness: "claude".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");

    let read: ClientMessage = Frame::read(&mut buf.as_slice()).expect("reading succeeds");

    assert_eq!(
        read,
        ClientMessage::SpawnPane {
            project,
            harness: "claude".into(),
            size: (80, 24),
            place: Placement::Auto,
        }
    );
}

#[test]
fn a_placement_from_a_newer_client_is_read_as_unknown() {
    // Rather than failing the whole frame, which would drop the connection
    // and the spawn with it.
    #[derive(Serialize)]
    struct Somewhere {
        #[serde(rename = "type")]
        kind: &'static str,
    }
    #[derive(Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Newer {
        SpawnPane {
            project: ProjectId,
            harness: String,
            size: (u16, u16),
            place: Somewhere,
        },
    }

    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &Newer::SpawnPane {
            project: ProjectId::new(),
            harness: "claude".into(),
            size: (80, 24),
            place: Somewhere {
                kind: "beside_the_window",
            },
        },
    )
    .expect("writing succeeds");

    let read: ClientMessage = Frame::read(&mut buf.as_slice()).expect("reading succeeds");

    assert!(
        matches!(
            read,
            ClientMessage::SpawnPane {
                place: Placement::Unknown,
                ..
            }
        ),
        "got {read:?}"
    );
}

#[test]
fn an_older_client_skips_a_tabs_snapshot() {
    // A client from before tabs meets a daemon that sends them whenever one
    // is on a machine that was upgraded first.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OlderServerMessage {
        PaneClosed {
            pane: PaneId,
        },
        #[serde(other)]
        Unknown,
    }

    let tabs = ServerMessage::Tabs {
        project: ProjectId::new(),
        tabs: vec![Tab {
            id: TabId::new(),
            name: None,
            panes: vec![PaneId::new()],
        }],
    };
    let mut buf = Vec::new();
    Frame::write(&mut buf, &tabs).expect("writing succeeds");

    let read: OlderServerMessage =
        Frame::read(&mut buf.as_slice()).expect("an older peer still reads the frame");

    assert_eq!(read, OlderServerMessage::Unknown);
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p dispatch-proto`
Expected: FAIL to compile. There is no field `place` on `SpawnPane`, no variant `MovePane` and no variant `Tabs`.

- [ ] **Step 3: Implement**

In `crates/dispatch-proto/src/message.rs`, change the core import to:

```rust
use dispatch_core::{PaneId, PaneStatus, Placement, Project, ProjectId, RequestId, Tab, TabId};
```

Give `ClientMessage::SpawnPane` its new field, after `size`:

```rust
        /// Which tab it goes on. An older client says nothing, which is
        /// [`Placement::Auto`].
        #[serde(default)]
        place: Placement,
```

Add these variants to `ClientMessage`, straight before `Unknown`:

```rust
    /// Moves a top-level pane to another tab, or onto a new one.
    MovePane {
        /// Which pane.
        pane: PaneId,
        /// Where it goes.
        to: Placement,
    },

    /// Names a tab. A name with nothing in it goes back to the automatic one.
    RenameTab {
        /// Which tab.
        tab: TabId,
        /// The name, as typed.
        name: String,
    },

    /// Closes every pane on a tab. The tab goes with its last pane.
    CloseTab {
        /// Which tab.
        tab: TabId,
    },

    /// Moves a tab to another place in its project's row.
    MoveTab {
        /// Which tab.
        tab: TabId,
        /// Where it goes, counted from the left; past the end means the end.
        index: usize,
    },
```

Add this variant to `ServerMessage`, straight before `Unknown`:

```rust
    /// What a project's tabs are now.
    ///
    /// The whole project's, after any change to them and to every subscriber,
    /// so a client never has to reconcile a sequence of edits: whatever it
    /// held before, this is the truth. Sent after the panes it names.
    Tabs {
        /// Whose tabs.
        project: ProjectId,
        /// Every tab, in row order.
        tabs: Vec<Tab>,
    },
```

In `crates/dispatch-daemon/src/session.rs`, change the `SpawnPane` arm's pattern to `ClientMessage::SpawnPane { project, harness, size, .. } =>`. The match must still be exhaustive, so give the four new client messages a temporary arm next to `ClientMessage::Unknown`:

```rust
            // Tabs arrive in the next change to this file; until then, a
            // client asking is answered the way an older daemon would answer.
            ClientMessage::MovePane { .. }
            | ClientMessage::RenameTab { .. }
            | ClientMessage::CloseTab { .. }
            | ClientMessage::MoveTab { .. } => {}
```

In `dispatch/src/app.rs`'s `apply_from`, add `ServerMessage::Tabs { .. }` to the final arm that returns `false` (`Welcome | Pong | DelegateFinished | Unknown`). Task 8 handles it.

Add `place: Placement::Auto` to every other `ClientMessage::SpawnPane` literal listed under Files, importing `dispatch_core::Placement` where each file needs it. In `dispatch/src/app.rs` the import is `use dispatch_core::Placement;` next to its other `dispatch_core` imports.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p dispatch-proto && cargo test -p dispatch-daemon && cargo test -p dispatchd && cargo test -p dispatch --no-run`
Expected: `dispatch-proto` passes, including the three new tests. Everything else passes or compiles as before.

- [ ] **Step 5: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add crates/dispatch-proto crates/dispatch-daemon/src/session.rs crates/dispatch-daemon/src/session/tests.rs dispatchd/tests/serves_clients.rs dispatch/tests/delegate_shim.rs dispatch/src/app.rs
git commit -m "feat(proto): say where a pane goes and what a project's tabs are"
```

---

### Task 4: The daemon keeps tabs

**Files:**
- Modify: `crates/dispatch-daemon/src/session.rs`. This covers imports, the `Daemon` fields and constructor, `open_project`, `close_project_for`, the `Subscribe` replay, the `SpawnPane` and `ClosePane` arms, the placeholder tab arm from Task 3, `spawn_pane`, `pump_panes`, and these new functions: `broadcast_tabs`, `refuse`, `close_pane`, `move_pane`, `change_tab`, `close_tab`.
- Modify: `crates/dispatch-daemon/src/session/tests.rs`

**Interfaces:**
- Consumes: Task 1 (`ProjectTabs`, `Placement`, `TabError`, `TabId`) and Task 3 (the messages).
- Produces: the daemon's behaviour. After any change to a project's tabs it sends every subscriber `ServerMessage::Tabs { project, tabs }`. Its `Subscribe` replay sends one `Tabs` per open project after all `PaneSpawned`. Refusals come back as `ServerMessage::Error { error: ProtocolError::Other(text) }` with the texts in Global Constraints.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-daemon/src/session/tests.rs`:

```rust
/// Attaches interface client `id`, subscribes it, and clears its inbox.
fn subscribed(daemon: &mut Daemon, id: u64) -> Receiver<ServerMessage> {
    let inbox = daemon.attach_for_test(id);
    daemon.request_for_test(id, hello());
    daemon.request_for_test(id, ClientMessage::Subscribe);
    let _ = drain(&inbox);
    inbox
}

/// Each tab's panes from the last `Tabs` seen for `project`.
fn last_tabs(messages: &[ServerMessage], project: ProjectId) -> Option<Vec<Vec<PaneId>>> {
    messages.iter().rev().find_map(|m| match m {
        ServerMessage::Tabs { project: p, tabs } if *p == project => {
            Some(tabs.iter().map(|tab| tab.panes.clone()).collect())
        }
        _ => None,
    })
}

/// The id of tab `index` in the last `Tabs` seen for `project`.
fn tab_at(messages: &[ServerMessage], project: ProjectId, index: usize) -> TabId {
    messages
        .iter()
        .rev()
        .find_map(|m| match m {
            ServerMessage::Tabs { project: p, tabs } if *p == project => {
                tabs.get(index).map(|tab| tab.id)
            }
            _ => None,
        })
        .expect("the tab is in the last snapshot")
}

/// Spawns a pane with `place`, as client 1, and waits for the snapshot that
/// places it. Returns the pane and everything seen on the way.
fn spawn_placed(
    daemon: &mut Daemon,
    inbox: &Receiver<ServerMessage>,
    project: ProjectId,
    place: Placement,
) -> (PaneId, Vec<ServerMessage>) {
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
            place,
        },
    );

    let seen = wait_for(daemon, inbox, |m| {
        m.iter().any(|m| matches!(m, ServerMessage::Tabs { .. }))
    });
    let pane = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("a pane was spawned");
    (pane, seen)
}

/// Whether a client was refused with exactly `reason`.
fn refused_with(messages: &[ServerMessage], reason: &str) -> bool {
    messages.iter().any(|m| {
        matches!(
            m,
            ServerMessage::Error { error: ProtocolError::Other(text) } if text == reason
        )
    })
}

#[test]
fn a_spawned_pane_is_placed_and_everyone_is_told_after_the_announcement() {
    let (mut daemon, project, _dir) = daemon("tabs-spawn");
    let inbox = subscribed(&mut daemon, 1);

    let (pane, seen) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);

    assert_eq!(last_tabs(&seen, project), Some(vec![vec![pane]]));
    let announced = seen
        .iter()
        .position(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
        .expect("the pane was announced");
    let placed = seen
        .iter()
        .position(|m| matches!(m, ServerMessage::Tabs { .. }))
        .expect("the pane was placed");
    assert!(announced < placed, "a snapshot never names a pane not yet announced");
}

#[test]
fn a_spawn_asked_into_a_full_tab_opens_the_next_one() {
    let (mut daemon, project, _dir) = daemon("tabs-full-spawn");
    let inbox = subscribed(&mut daemon, 1);
    let mut all = Vec::new();
    let mut seen = Vec::new();
    for _ in 0..4 {
        let (pane, placed) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);
        all.push(pane);
        seen = placed;
    }
    let first = tab_at(&seen, project, 0);

    let (fifth, seen) = spawn_placed(&mut daemon, &inbox, project, Placement::Into { tab: first });

    assert_eq!(
        last_tabs(&seen, project),
        Some(vec![all, vec![fifth]]),
        "the full tab is untouched and the new one follows it"
    );
}

#[test]
fn a_client_attaching_later_hears_every_projects_tabs_after_their_panes() {
    let (mut daemon, project, dir) = daemon("tabs-replay");
    let inbox = subscribed(&mut daemon, 1);
    let (a, _) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);
    let (b, _) = spawn_placed(&mut daemon, &inbox, project, Placement::NewAfter { tab: None });
    let empty_root = dir.0.join("empty");
    std::fs::create_dir_all(&empty_root).expect("temp dir is writable");
    let empty = daemon.open_project(
        dispatch_os::paths::resolve(&empty_root).expect("the temp dir resolves"),
    );

    let late = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);
    let replay = drain(&late);

    assert_eq!(last_tabs(&replay, project), Some(vec![vec![a], vec![b]]));
    assert_eq!(
        last_tabs(&replay, empty),
        Some(Vec::new()),
        "an empty project still says it keeps tabs"
    );
    let last_pane = replay
        .iter()
        .rposition(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
        .expect("the panes were replayed");
    let first_tabs = replay
        .iter()
        .position(|m| matches!(m, ServerMessage::Tabs { .. }))
        .expect("the tabs were replayed");
    assert!(last_pane < first_tabs, "tabs come after every pane they name");
}

#[test]
fn moving_a_pane_tells_everyone() {
    let (mut daemon, project, _dir) = daemon("tabs-move");
    let inbox = subscribed(&mut daemon, 1);
    let (a, _) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);
    let (b, seen) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);
    let first = tab_at(&seen, project, 0);

    daemon.request_for_test(
        1,
        ClientMessage::MovePane {
            pane: b,
            to: Placement::NewAfter { tab: Some(first) },
        },
    );

    assert_eq!(last_tabs(&drain(&inbox), project), Some(vec![vec![a], vec![b]]));
}

#[test]
fn the_second_of_two_moves_into_the_last_slot_is_refused() {
    // Two clients can each see room for one more pane. The daemon decides,
    // so only one of them gets it.
    let (mut daemon, project, _dir) = daemon("tabs-race");
    let first_client = subscribed(&mut daemon, 1);
    let second_client = subscribed(&mut daemon, 2);
    let mut seen = spawn_placed(&mut daemon, &first_client, project, Placement::Auto).1;
    for _ in 0..2 {
        seen = spawn_placed(&mut daemon, &first_client, project, Placement::Auto).1;
    }
    let first = tab_at(&seen, project, 0);
    let (d, _) = spawn_placed(&mut daemon, &first_client, project, Placement::NewAfter { tab: None });
    let (e, _) = spawn_placed(&mut daemon, &first_client, project, Placement::NewAfter { tab: None });
    let _ = drain(&second_client);

    daemon.request_for_test(1, ClientMessage::MovePane { pane: d, to: Placement::Into { tab: first } });
    daemon.request_for_test(2, ClientMessage::MovePane { pane: e, to: Placement::Into { tab: first } });

    assert!(refused_with(&drain(&second_client), "that tab is full (4 panes)"));
    let tabs = last_tabs(&drain(&first_client), project).expect("the first move was told");
    assert_eq!(tabs[0].len(), 4);
    assert_eq!(tabs[0][3], d, "the first to ask got the slot");
    assert_eq!(tabs[1], vec![e]);
}

#[test]
fn a_subagent_is_never_placed_or_moved() {
    let (mut daemon, project, _dir) = daemon("tabs-subagent");
    let ui = subscribed(&mut daemon, 1);
    let (parent, _) = spawn_placed(&mut daemon, &ui, project, Placement::Auto);
    let _caller = ask(&mut daemon, parent, "echo delegated");
    let request = pending(&drain(&ui)).expect("the interface is asked");

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );
    let seen = wait_for(&mut daemon, &ui, |m| m.iter().any(m_is_child));
    let child = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned {
                pane,
                parent: Some(_),
                ..
            } => Some(*pane),
            _ => None,
        })
        .expect("the subagent was announced");

    assert!(
        !seen.iter().any(|m| matches!(
            m,
            ServerMessage::Tabs { tabs, .. } if tabs.iter().any(|tab| tab.panes.contains(&child))
        )),
        "no snapshot places the subagent"
    );

    daemon.request_for_test(1, ClientMessage::MovePane { pane: child, to: Placement::NewAfter { tab: None } });
    assert!(refused_with(&drain(&ui), "a subagent stays beside the pane that asked for it"));
}

#[test]
fn renaming_a_tab_tells_everyone() {
    let (mut daemon, project, _dir) = daemon("tabs-rename");
    let inbox = subscribed(&mut daemon, 1);
    let (_, seen) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);
    let tab = tab_at(&seen, project, 0);

    daemon.request_for_test(1, ClientMessage::RenameTab { tab, name: "  work  ".into() });

    let named = drain(&inbox).into_iter().rev().find_map(|m| match m {
        ServerMessage::Tabs { tabs, .. } => tabs.first().and_then(|tab| tab.name.clone()),
        _ => None,
    });
    assert_eq!(named.as_deref(), Some("work"));
}

#[test]
fn a_tab_that_is_gone_is_reported() {
    let (mut daemon, _project, _dir) = daemon("tabs-gone");
    let inbox = subscribed(&mut daemon, 1);

    daemon.request_for_test(1, ClientMessage::RenameTab { tab: TabId::new(), name: "work".into() });
    daemon.request_for_test(1, ClientMessage::MoveTab { tab: TabId::new(), index: 0 });
    daemon.request_for_test(1, ClientMessage::CloseTab { tab: TabId::new() });

    let refusals = drain(&inbox)
        .iter()
        .filter(|m| {
            matches!(
                m,
                ServerMessage::Error { error: ProtocolError::Other(text) } if text == "that tab is gone"
            )
        })
        .count();
    assert_eq!(refusals, 3);
}

#[test]
fn closing_a_tab_closes_exactly_its_panes() {
    let (mut daemon, project, _dir) = daemon("tabs-close");
    let inbox = subscribed(&mut daemon, 1);
    let (a, _) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);
    let (b, _) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);
    let (c, seen) = spawn_placed(&mut daemon, &inbox, project, Placement::NewAfter { tab: None });
    let first = tab_at(&seen, project, 0);

    daemon.request_for_test(1, ClientMessage::CloseTab { tab: first });

    let seen = drain(&inbox);
    for pane in [a, b] {
        assert!(
            seen.iter().any(|m| matches!(m, ServerMessage::PaneClosed { pane: p } if *p == pane)),
            "{pane} was closed"
        );
    }
    assert_eq!(last_tabs(&seen, project), Some(vec![vec![c]]));
    assert_eq!(daemon.pane_count(), 1);
}

#[test]
fn a_tab_moves_along_the_row() {
    let (mut daemon, project, _dir) = daemon("tabs-reorder");
    let inbox = subscribed(&mut daemon, 1);
    let (a, _) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);
    let (b, _) = spawn_placed(&mut daemon, &inbox, project, Placement::NewAfter { tab: None });
    let (c, seen) = spawn_placed(&mut daemon, &inbox, project, Placement::NewAfter { tab: None });
    let first = tab_at(&seen, project, 0);

    daemon.request_for_test(1, ClientMessage::MoveTab { tab: first, index: 2 });

    assert_eq!(
        last_tabs(&drain(&inbox), project),
        Some(vec![vec![b], vec![c], vec![a]])
    );
}

#[test]
fn a_pane_that_exits_leaves_its_tab() {
    let (mut daemon, project, _dir) = daemon("tabs-exit");
    let inbox = subscribed(&mut daemon, 1);
    let (pane, _) = spawn_placed(&mut daemon, &inbox, project, Placement::Auto);

    daemon.request_for_test(1, ClientMessage::WritePane { pane, bytes: b"exit 0\r".to_vec() });

    let seen = wait_for(&mut daemon, &inbox, |m| {
        last_tabs(m, project).is_some_and(|tabs| tabs.is_empty())
    });
    assert!(seen.iter().any(|m| matches!(
        m,
        ServerMessage::PaneChanged { update: PaneUpdate::Status { status: PaneStatus::Exited(_) }, .. }
    )));
    assert_eq!(daemon.pane_count(), 1, "the pane itself stays until closed");
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p dispatch-daemon tab`
Expected: FAIL. `spawn_placed` times out waiting for a `Tabs` message: "timed out; saw […]".

- [ ] **Step 3: Implement**

In `crates/dispatch-daemon/src/session.rs`, extend the core import:

```rust
use dispatch_core::{
    PaneId, PaneStatus, Placement, Project, ProjectId, ProjectSource, ProjectTabs, RequestId,
    TabError, TabId,
};
```

Add the field to `Daemon`, after `projects`:

```rust
    /// Each project's tabs. Kept here because the panes are: every client
    /// attached to this machine sees one arrangement, and it outlives any of
    /// them.
    tabs: HashMap<ProjectId, ProjectTabs>,
```

Initialise it in `with_limits` with `tabs: HashMap::new(),`.

In `open_project`, after `self.projects.insert(id, project);`:

```rust
        self.tabs.insert(id, ProjectTabs::new());
```

In `close_project_for`, after `self.projects.remove(&project);`:

```rust
        self.tabs.remove(&project);
```

In the `Subscribe` replay, straight after the `for pane in self.panes.values() { … }` loop:

```rust
                // Every project's tabs, empty ones included: a client that
                // hears none from a daemon takes it for one too old to keep
                // them. After the panes, so every pane a snapshot names is one
                // the client has already been told about.
                for (project, tabs) in &self.tabs {
                    existing.push(ServerMessage::Tabs {
                        project: *project,
                        tabs: tabs.tabs().to_vec(),
                    });
                }
```

Change the `SpawnPane` arm to pass the placement on:

```rust
            ClientMessage::SpawnPane {
                project,
                harness,
                size,
                place,
            } => self.spawn_pane(id, project, &harness, Size::new(size.0, size.1), place),
```

Replace the whole body of the `ClientMessage::ClosePane { pane } => { … }` arm with `self.close_pane(id, pane),`. Replace Task 3's temporary tab arm with:

```rust
            ClientMessage::MovePane { pane, to } => self.move_pane(id, pane, to),
            ClientMessage::RenameTab { tab, name } => {
                self.change_tab(id, tab, |tabs| tabs.rename(tab, &name));
            }
            ClientMessage::CloseTab { tab } => self.close_tab(id, tab),
            ClientMessage::MoveTab { tab, index } => {
                self.change_tab(id, tab, |tabs| tabs.move_tab(tab, index));
            }
```

Give `spawn_pane` a `place: Placement` parameter, the last one. After its `self.broadcast(ServerMessage::PaneSpawned { … });`, add:

```rust
        // After the announcement, so a snapshot never names a pane a client
        // has not heard of.
        self.tabs.entry(project).or_default().place(id, place);
        self.broadcast_tabs(project);
```

In `pump_panes`:
- Make the exited list carry the project: `exited.push((*id, pane.project, code));`.
- Change the loop that builds `PaneChanged` messages to `for (id, _, code) in &exited`.
- Straight after `for message in messages { self.broadcast(message); }`, add:

```rust
        // An exited pane gives its place on a tab back. Its output stays
        // readable from the sidebar until someone closes it.
        let mut changed: Vec<ProjectId> = Vec::new();
        for (id, project, _) in &exited {
            if self
                .tabs
                .get_mut(project)
                .is_some_and(|tabs| tabs.remove(*id))
                && !changed.contains(project)
            {
                changed.push(*project);
            }
        }
        for project in changed {
            self.broadcast_tabs(project);
        }
```

Add these methods to `impl Daemon`, after `spawn_pane`:

```rust
    /// Tells every client what `project`'s tabs are now.
    fn broadcast_tabs(&mut self, project: ProjectId) {
        let tabs = self
            .tabs
            .get(&project)
            .map(|tabs| tabs.tabs().to_vec())
            .unwrap_or_default();
        self.broadcast(ServerMessage::Tabs { project, tabs });
    }

    /// Tells one client why what it asked for was not done.
    fn refuse(&mut self, client: ClientId, reason: impl Into<String>) {
        self.send(
            client,
            ServerMessage::Error {
                error: ProtocolError::Other(reason.into()),
            },
        );
    }

    /// Closes a pane and terminates its process tree.
    fn close_pane(&mut self, client: ClientId, pane: PaneId) {
        let Some(mut target) = self.panes.remove(&pane) else {
            self.send(
                client,
                ServerMessage::Error {
                    error: ProtocolError::NoSuchPane(pane),
                },
            );
            return;
        };
        let project = target.project;

        // Moved here unchanged from the `ClosePane` arm, so closing a tab
        // closes each pane exactly as closing it alone does.
        //
        // The pane is being killed, not allowed to finish; its caller, if it
        // has one, is answered here or not at all.
        self.answer_for_a_closed_subagent(&mut target);
        target.session.terminate();
        // A pane that is gone can be asked for nothing more, so its blanket
        // approval goes with it.
        self.blanket.remove(&pane);
        self.broadcast(ServerMessage::PaneClosed { pane });
        self.refuse_requests_from(pane);
        self.drop_children_of(pane);

        // Its place on a tab goes with it.
        if self
            .tabs
            .get_mut(&project)
            .is_some_and(|tabs| tabs.remove(pane))
        {
            self.broadcast_tabs(project);
        }
    }

    /// Moves a top-level pane between its project's tabs.
    fn move_pane(&mut self, client: ClientId, pane: PaneId, to: Placement) {
        let Some((project, subagent)) = self
            .panes
            .get(&pane)
            .map(|target| (target.project, target.parent.is_some()))
        else {
            self.send(
                client,
                ServerMessage::Error {
                    error: ProtocolError::NoSuchPane(pane),
                },
            );
            return;
        };
        // A subagent is tiled beside the pane that asked for it, wherever
        // that pane is, so it has no tab of its own to move between.
        if subagent {
            self.refuse(client, "a subagent stays beside the pane that asked for it");
            return;
        }

        match self.tabs.entry(project).or_default().move_pane(pane, to) {
            Ok(_) => self.broadcast_tabs(project),
            Err(error) => self.refuse(client, error.to_string()),
        }
    }

    /// Makes `change` to whichever project's tabs hold `tab`, telling every
    /// client the result, or the asker why not.
    fn change_tab(
        &mut self,
        client: ClientId,
        tab: TabId,
        change: impl FnOnce(&mut ProjectTabs) -> Result<(), TabError>,
    ) {
        let Some((project, tabs)) = self
            .tabs
            .iter_mut()
            .find(|(_, tabs)| tabs.position(tab).is_some())
        else {
            self.refuse(client, TabError::NoSuchTab.to_string());
            return;
        };

        let project = *project;
        match change(tabs) {
            Ok(()) => self.broadcast_tabs(project),
            Err(error) => self.refuse(client, error.to_string()),
        }
    }

    /// Closes every pane on a tab. The tab goes with its last pane.
    fn close_tab(&mut self, client: ClientId, tab: TabId) {
        let Some(members) = self.tabs.values().find_map(|tabs| tabs.members(tab).ok()) else {
            self.refuse(client, TabError::NoSuchTab.to_string());
            return;
        };

        for pane in members {
            self.close_pane(client, pane);
        }
    }
```

If an existing replay test asserts an exact message count or sequence (for example `a_client_attaching_later_is_told_what_already_exists` or `a_subscriber_is_told_the_projects_before_the_panes`) and now fails only because the replay also carries `Tabs`, adjust that assertion to allow the `Tabs` messages. List every such change in the report. The same applies to `dispatchd/tests/serves_clients.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p dispatch-daemon && cargo test -p dispatchd`
Expected: PASS, including the 11 new tests.

- [ ] **Step 5: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add crates/dispatch-daemon dispatchd/tests/serves_clients.rs
git commit -m "feat(daemon): keep each project's tabs and tell every client"
```

---
### Task 5: Finding the user's shell

**Files:**
- Create: `crates/dispatch-os/src/shell.rs`, with its tests inline, as `host.rs` has.
- Modify: `crates/dispatch-os/src/lib.rs` (`pub mod shell;`)

**Interfaces:**
- Produces (used by Task 6):
  - `dispatch_os::shell::user_shell() -> String`
  - `dispatch_os::shell::login_by_default() -> bool`
  - `dispatch_os::shell::takes_login_flag() -> bool`

- [ ] **Step 1: Write the module with its tests and see them fail**

Create `crates/dispatch-os/src/shell.rs` with only its doc comment and this test module, and add `pub mod shell;` to `lib.rs`:

```rust
//! The user's own shell, for a pane that runs one.
//!
//! Asked of the machine that runs the pane, so a project on another machine
//! gets that machine's shell and dotfiles rather than the client's.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_environments_shell_comes_first() {
        assert_eq!(
            pick(Some("/usr/bin/zsh"), Some("/bin/bash"), |_| true),
            "/usr/bin/zsh"
        );
    }

    #[test]
    fn a_shell_that_is_not_there_falls_to_the_login_record() {
        let there = |path: &Path| path == Path::new("/bin/bash");

        assert_eq!(pick(Some("/nope/zsh"), Some("/bin/bash"), there), "/bin/bash");
        assert_eq!(pick(Some(""), Some("/bin/bash"), there), "/bin/bash");
    }

    #[test]
    fn with_nothing_usable_it_is_the_posix_shell() {
        assert_eq!(pick(None, None, |_| true), "/bin/sh");
        assert_eq!(pick(Some("/nope"), Some("/nope"), |_| false), "/bin/sh");
    }

    #[test]
    fn windows_prefers_powershell_seven_when_it_is_installed() {
        assert_eq!(powershell(true), "pwsh");
        assert_eq!(powershell(false), "powershell");
    }

    #[test]
    fn the_shell_found_here_can_be_started() {
        let shell = user_shell();

        assert!(!shell.is_empty());
        if cfg!(unix) {
            assert!(
                Path::new(&shell).is_file(),
                "{shell} is a file this machine has"
            );
        }
    }
}
```

Run: `cargo test -p dispatch-os shell`
Expected: FAIL to compile, because `pick`, `powershell` and `user_shell` are not found.

- [ ] **Step 2: Implement**

Put this above the test module. `Path` is spelled out in `pick` rather than imported: on a Windows build `pick` is compiled out, and an import only it uses would be unused there.

```rust
/// The shell to run when the user has not named one.
///
/// `$SHELL` first, as a terminal emulator does. Then the login record, for a
/// daemon started by something that never set it. Then `/bin/sh`. On
/// Windows, PowerShell: `pwsh` if it is installed, the `powershell` every
/// Windows has if not.
#[must_use]
pub fn user_shell() -> String {
    imp::user_shell()
}

/// Whether a shell starts as a login shell when the user has not said.
///
/// macOS terminals start login shells and Linux ones do not, and each
/// platform's dotfiles are written for its own habit: that is where the rc
/// file that sets up a prompt like Starship gets read.
#[must_use]
pub fn login_by_default() -> bool {
    cfg!(target_os = "macos")
}

/// Whether this platform's shells take `-l`. PowerShell does not.
#[must_use]
pub fn takes_login_flag() -> bool {
    cfg!(unix)
}

/// The first of the environment's shell and the login record's that names
/// an executable file, or `/bin/sh`.
#[cfg(any(unix, test))]
fn pick(
    env_shell: Option<&str>,
    login_record: Option<&str>,
    executable: impl Fn(&std::path::Path) -> bool,
) -> String {
    [env_shell, login_record]
        .into_iter()
        .flatten()
        .find(|candidate| !candidate.is_empty() && executable(std::path::Path::new(candidate)))
        .map_or_else(|| "/bin/sh".to_string(), str::to_string)
}

/// PowerShell 7 when it is installed, else the Windows PowerShell every
/// Windows has.
#[cfg(any(windows, test))]
fn powershell(pwsh_installed: bool) -> String {
    if pwsh_installed { "pwsh" } else { "powershell" }.to_string()
}

#[cfg(unix)]
mod imp {
    use std::path::Path;

    pub(super) fn user_shell() -> String {
        let env_shell = std::env::var("SHELL").ok();
        let record = login_record();
        super::pick(env_shell.as_deref(), record.as_deref(), is_executable)
    }

    fn is_executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;

        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }

    /// The shell named in this user's password entry.
    fn login_record() -> Option<String> {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };

        let mut buf = vec![0 as libc::c_char; 4096];
        // SAFETY: an all-zero passwd is a valid value for getpwuid_r to
        // overwrite: every field is an integer or a pointer.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();

        // SAFETY: entry, buf and result are valid for the whole call, and
        // buf.len() is the buffer's true length. getpwuid_r writes the
        // entry's strings into buf and points result at entry on success.
        let status = unsafe {
            libc::getpwuid_r(uid, &mut entry, buf.as_mut_ptr(), buf.len(), &mut result)
        };
        if status != 0 || result.is_null() || entry.pw_shell.is_null() {
            return None;
        }

        // SAFETY: on success pw_shell points at a NUL-terminated string
        // inside buf, which is still alive here.
        let shell = unsafe { std::ffi::CStr::from_ptr(entry.pw_shell) };
        shell.to_str().ok().map(str::to_string)
    }
}

#[cfg(windows)]
mod imp {
    pub(super) fn user_shell() -> String {
        super::powershell(on_path("pwsh.exe"))
    }

    fn on_path(exe: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(exe).is_file()))
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p dispatch-os shell`
Expected: PASS, 5 tests.

- [ ] **Step 4: Lint on every platform and commit**

Run:
- `cargo fmt --all --check && cargo clippy -p dispatch-os --all-targets -- -D warnings`
- `cargo clippy -p dispatch-os --all-targets --target aarch64-apple-darwin -- -D warnings`
- `cargo clippy -p dispatch-os --all-targets --target x86_64-pc-windows-gnu -- -D warnings 2>&1 | grep shell.rs`

Expected: the first two are clean, and the third prints nothing. `dispatch-os` fails on Windows already in `host.rs` and `ipc.rs`, so only `shell.rs` must add nothing.

```bash
git add crates/dispatch-os/src/lib.rs crates/dispatch-os/src/shell.rs
git commit -m "feat(os): find the user's own shell"
```

---

### Task 6: `[shell]`, and a built-in `shell` harness

**Files:**
- Modify: `crates/dispatch-config/src/config.rs`. Add `LoginShell`, `ShellConfig` and `Config.shell`, and extend `unknown_keys`. `Config` loses `Copy`.
- Modify: `crates/dispatch-config/src/config/tests.rs`
- Modify: `crates/dispatch-config/src/lib.rs`. Add `SHELL`, a `HarnessRegistry.shell` field, `with_shell`, `reloaded`, and the re-exports.
- Modify: `crates/dispatch-config/src/tests.rs`
- Modify: `dispatch/src/main.rs`, `dispatchd/src/main.rs` (registries get the shell)
- Modify: `dispatch/src/app.rs`. Only `choose`'s `OverlayKind::Register` reload changes.

**Interfaces:**
- Consumes: Task 5's `dispatch_os::shell::{user_shell, login_by_default, takes_login_flag}`.
- Produces (used by Task 12):
  - `dispatch_config::{ShellConfig { command: Option<String>, args: Vec<String>, login: LoginShell }, LoginShell::{Auto, Always, Never}, SHELL}`
  - `ShellConfig::launch(&self) -> Launch`
  - `Config.shell`
  - `HarnessRegistry::with_shell(self, &ShellConfig) -> Self`
  - `HarnessRegistry::reloaded(&self, &Path) -> Result<Self, ConfigError>`

- [ ] **Step 1: Write the failing config tests**

Append to `crates/dispatch-config/src/config/tests.rs`. It already imports `crate::testing::TempDir`, whose `config(text)` writes a `config.toml` and returns its path.

```rust
/// Loads `text` as a `config.toml`.
fn load(label: &str, text: &str) -> Config {
    let dir = TempDir::new(label);
    Config::load(&dir.config(text)).expect("the file parses")
}

#[test]
fn a_shell_section_is_read() {
    let config = load(
        "shell",
        "[shell]\ncommand = \"/usr/bin/fish\"\nargs = [\"--private\"]\nlogin = \"always\"\n",
    );

    assert_eq!(
        config.shell,
        ShellConfig {
            command: Some("/usr/bin/fish".into()),
            args: vec!["--private".into()],
            login: LoginShell::Always,
        }
    );
}

#[test]
fn without_a_shell_section_the_shell_is_found_and_login_decided_for_this_platform() {
    let config = load("no-shell", "");

    assert_eq!(config.shell, ShellConfig::default());
    assert_eq!(config.shell.login, LoginShell::Auto);
    assert_eq!(config.shell.command, None);
}

#[test]
fn a_login_value_that_is_not_one_of_the_three_is_an_error() {
    let dir = TempDir::new("bad-login");
    let path = dir.config("[shell]\nlogin = \"sometimes\"\n");

    assert!(Config::load(&path).is_err());
}

#[test]
fn an_unknown_shell_key_is_reported_by_name() {
    let dir = TempDir::new("shell-unknown");
    let path = dir.config("[shell]\ncommand = \"sh\"\nprompt = \"starship\"\n");

    let loaded = Config::load_reporting(&path).expect("the file loads");

    assert_eq!(loaded.unknown, vec!["shell.prompt".to_string()]);
}

#[test]
fn a_named_shell_is_run_as_named() {
    let shell = ShellConfig {
        command: Some("/opt/bin/nu".into()),
        args: vec!["--no-history".into()],
        login: LoginShell::Never,
    };

    let launch = shell.launch_with(|| unreachable!("not asked"), true, true);

    assert_eq!(launch.command, "/opt/bin/nu");
    assert_eq!(launch.args, vec!["--no-history".to_string()]);
}

#[test]
fn with_no_shell_named_the_machines_own_is_used() {
    let launch = ShellConfig::default().launch_with(|| "/usr/bin/zsh".into(), false, true);

    assert_eq!(launch.command, "/usr/bin/zsh");
    assert!(launch.args.is_empty(), "no -l where logins are not the habit");
}

#[test]
fn auto_logs_in_where_that_is_the_platforms_habit_and_args_follow_the_flag() {
    let shell = ShellConfig {
        command: None,
        args: vec!["--extra".into()],
        login: LoginShell::Auto,
    };

    let launch = shell.launch_with(|| "/bin/zsh".into(), true, true);

    assert_eq!(launch.args, vec!["-l".to_string(), "--extra".to_string()]);
}

#[test]
fn always_and_never_override_the_platform() {
    let always = ShellConfig {
        login: LoginShell::Always,
        ..ShellConfig::default()
    };
    let never = ShellConfig {
        login: LoginShell::Never,
        ..ShellConfig::default()
    };

    assert_eq!(always.launch_with(|| "sh".into(), false, true).args, vec!["-l".to_string()]);
    assert!(never.launch_with(|| "sh".into(), true, true).args.is_empty());
}

#[test]
fn a_shell_that_takes_no_login_flag_is_never_given_one() {
    // PowerShell has no `-l`; passing it would stop the pane starting.
    let always = ShellConfig {
        login: LoginShell::Always,
        ..ShellConfig::default()
    };

    assert!(always.launch_with(|| "pwsh".into(), true, false).args.is_empty());
}

#[test]
fn a_blank_command_counts_as_none() {
    let blank = ShellConfig {
        command: Some("  ".into()),
        ..ShellConfig::default()
    };

    assert_eq!(blank.launch_with(|| "/bin/sh".into(), false, true).command, "/bin/sh");
}
```

- [ ] **Step 2: Write the failing registry tests**

Append to `crates/dispatch-config/src/tests.rs`, which already imports `crate::testing::TempDir`:

```rust
#[test]
fn every_registry_can_offer_the_users_shell() {
    let registry = HarnessRegistry::default().with_shell(&ShellConfig {
        command: Some("/usr/bin/fish".into()),
        ..ShellConfig::default()
    });

    let shell = registry.get(SHELL).expect("the shell is registered");
    assert_eq!(shell.display_name, "Shell");
    assert_eq!(shell.launch.command, "/usr/bin/fish");
    assert!(shell.task.is_none(), "nothing can delegate to a shell");
    assert!(shell.settings.is_empty());
    assert_eq!(shell.icon(), crate::harness::DEFAULT_ICON);
    assert!(registry.status_rules(SHELL).is_empty(), "its state comes from output alone");
}

#[test]
fn a_harness_file_named_shell_wins_over_the_built_in_one() {
    let dir = TempDir::new("shell-file");
    std::fs::write(
        dir.path().join("shell.toml"),
        "id = \"shell\"\ndisplay_name = \"My shell\"\ncommand = \"sh\"\n",
    )
    .expect("temp dir is writable");

    let registry = HarnessRegistry::load_from_dir(dir.path())
        .expect("loading succeeds")
        .with_shell(&ShellConfig::default());

    assert_eq!(
        registry.get(SHELL).map(|def| def.display_name.as_str()),
        Some("My shell")
    );
}

#[test]
fn reloading_keeps_the_shell() {
    // Registering a harness reloads the directory; the shell is not in it and
    // must not vanish from the picker because of that.
    let dir = TempDir::new("shell-reload");
    let registry = HarnessRegistry::load_from_dir(dir.path())
        .expect("loading succeeds")
        .with_shell(&ShellConfig {
            command: Some("/bin/zsh".into()),
            ..ShellConfig::default()
        });

    let reloaded = registry.reloaded(dir.path()).expect("reloading succeeds");

    assert_eq!(
        reloaded.get(SHELL).map(|def| def.launch.command.as_str()),
        Some("/bin/zsh")
    );
}
```

Run: `cargo test -p dispatch-config`
Expected: FAIL to compile, because `ShellConfig`, `LoginShell`, `SHELL`, `with_shell`, `reloaded` and `launch_with` are not found.

- [ ] **Step 3: Implement the config**

In `crates/dispatch-config/src/config.rs`:
- Add `use std::collections::BTreeMap;` and `use crate::harness::Launch;`.
- Drop `Copy` from `Config`'s derive, since it now holds strings.
- Add these types before `Config`:

```rust
/// Whether the user's shell starts as a login shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginShell {
    /// As this platform's terminals do: a login shell on macOS, not
    /// elsewhere.
    #[default]
    Auto,
    /// Always pass `-l`.
    Always,
    /// Never pass `-l`.
    Never,
}

/// The shell a `shell` pane runs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// The program. `None` means the machine's own: `$SHELL`, then the
    /// login record, then `/bin/sh`.
    pub command: Option<String>,
    /// Arguments, after any `-l`.
    pub args: Vec<String>,
    /// Whether it starts as a login shell.
    pub login: LoginShell,
}

impl ShellConfig {
    /// How to start the shell on this machine.
    #[must_use]
    pub fn launch(&self) -> Launch {
        self.launch_with(
            dispatch_os::shell::user_shell,
            dispatch_os::shell::login_by_default(),
            dispatch_os::shell::takes_login_flag(),
        )
    }

    /// [`Self::launch`], with what it would ask the machine handed in, so
    /// every platform's rules can be tested on any one of them.
    pub(crate) fn launch_with(
        &self,
        user_shell: impl FnOnce() -> String,
        login_by_default: bool,
        takes_login_flag: bool,
    ) -> Launch {
        let command = self
            .command
            .clone()
            .filter(|command| !command.trim().is_empty())
            .unwrap_or_else(user_shell);

        let login = match self.login {
            LoginShell::Auto => login_by_default,
            LoginShell::Always => true,
            LoginShell::Never => false,
        };

        let mut args = Vec::new();
        if login && takes_login_flag {
            args.push("-l".to_string());
        }
        args.extend(self.args.iter().cloned());

        Launch {
            command,
            args,
            env: BTreeMap::new(),
        }
    }
}
```

Add a field to `Config`, after `interface`:

```rust
    /// The shell a `shell` pane runs. Read by whichever side starts panes:
    /// the daemon, or a standalone client.
    pub shell: ShellConfig,
```

In `unknown_keys`, add `const SHELL: [&str; 3] = ["command", "args", "login"];` next to `DELEGATION`, and a match arm:

```rust
            ("shell", toml::Value::Table(table)) => {
                for key in table.keys() {
                    if !SHELL.contains(&key.as_str()) {
                        unknown.push(format!("shell.{key}"));
                    }
                }
            }
```

- [ ] **Step 4: Implement the registry's shell**

In `crates/dispatch-config/src/lib.rs`, re-export the new types next to `Config`'s existing re-export:

```rust
pub use config::{Config, DelegationLimits, InterfaceConfig, LoadedConfig, LoginShell, ShellConfig};
```

Keep whatever that line already exports. Then add:

```rust
/// The id of the harness that runs the user's own shell.
pub const SHELL: &str = "shell";
```

Add a field to `HarnessRegistry`:

```rust
    /// The shell this registry was given, so reloading the directory can
    /// give it back.
    shell: Option<ShellConfig>,
```

Set `shell: None` in `with`, then add these methods to `impl HarnessRegistry`:

```rust
    /// This registry plus the user's own shell, as the `shell` harness.
    ///
    /// Built in code rather than written to the harnesses directory: what it
    /// runs depends on the machine and on `[shell]`, and a file written once
    /// would go stale the day either changed. A harness file with the id
    /// `shell` wins, as a user's file always does.
    #[must_use]
    pub fn with_shell(mut self, shell: &ShellConfig) -> Self {
        self.shell = Some(shell.clone());
        if !self.harnesses.contains_key(SHELL) {
            let def = HarnessDef {
                id: SHELL.to_string(),
                display_name: "Shell".to_string(),
                launch: shell.launch(),
                ..HarnessDef::default()
            };
            self.rules.insert(
                SHELL.to_string(),
                std::sync::Arc::new(status::StatusRules::for_harness(SHELL, None)),
            );
            self.harnesses.insert(SHELL.to_string(), def);
        }
        self
    }

    /// Loads `dir` again, keeping the shell this registry was given.
    pub fn reloaded(&self, dir: &Path) -> Result<Self, ConfigError> {
        let fresh = Self::load_from_dir(dir)?;
        Ok(match &self.shell {
            Some(shell) => fresh.with_shell(shell),
            None => fresh,
        })
    }
```

- [ ] **Step 5: Give both binaries' registries the shell**

In `dispatch/src/main.rs`:
- `harnesses` is loaded before `config.toml`. After `loaded` has been read and its unknown keys logged, rebind it: `let harnesses = harnesses.with_shell(&loaded.config.shell);`.
- Make sure `App::attached(harnesses, …)` and `App::new(harnesses)` use the rebound value.

In `dispatchd/src/main.rs`, after `loaded` is read:

```rust
    let harnesses = harnesses.with_shell(&loaded.config.shell);
```

It must come before `Daemon::with_limits(harnesses, …)`.

In `dispatch/src/app.rs`, `choose`'s `OverlayKind::Register` branch, replace the reload with:

```rust
                self.harnesses = self
                    .harnesses
                    .reloaded(&dir)
                    .context("failed to reload harness definitions")?;
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p dispatch-config && cargo build -p dispatch -p dispatchd`
Expected: PASS, including 10 new config tests and 3 new registry tests. Both binaries build.

- [ ] **Step 7: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add crates/dispatch-config dispatch/src/main.rs dispatchd/src/main.rs dispatch/src/app.rs
git commit -m "feat(config): a [shell] section and the user's shell as a harness"
```

---

### Task 7: Every pane is told the terminal it is in

**Files:**
- Modify: `crates/dispatch-pty/src/session.rs`. Add the constants, `apply_pane_env`, and a call to it in `Pty::spawn`.
- Modify: `crates/dispatch-pty/src/session/tests.rs`

**Interfaces:**
- Produces: every spawned pane gets the environment in Global Constraints. The harness's `launch.env` is applied after it.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-pty/src/session/tests.rs`:

```rust
#[test]
fn a_pane_is_told_about_dispatchs_terminal_not_the_one_outside() {
    // Dispatch draws the pane with its own emulator. A program told it is in
    // kitty would send kitty's private sequences through, and over SSH a
    // remote side without that terminfo mis-draws.
    let mut command = CommandBuilder::new("true");
    command.env("TERM", "xterm-kitty");
    command.env("KITTY_WINDOW_ID", "7");
    command.env("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty");

    apply_pane_env(&mut command);

    assert_eq!(command.get_env("TERM"), Some(std::ffi::OsStr::new("xterm-256color")));
    assert_eq!(command.get_env("COLORTERM"), Some(std::ffi::OsStr::new("truecolor")));
    assert_eq!(command.get_env("TERM_PROGRAM"), Some(std::ffi::OsStr::new("dispatch")));
    assert_eq!(
        command.get_env("TERM_PROGRAM_VERSION"),
        Some(std::ffi::OsStr::new(env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(command.get_env("KITTY_WINDOW_ID"), None);
    assert_eq!(command.get_env("GHOSTTY_RESOURCES_DIR"), None);
}

#[test]
#[cfg(unix)]
fn a_child_sees_dispatchs_terminal() {
    let mut session = PtySession::spawn(
        &shell("echo T=$TERM C=$COLORTERM P=$TERM_PROGRAM"),
        &cwd(),
        Size::new(80, 24),
    )
    .expect("the shell starts");

    assert!(wait_until(&mut session, TIMEOUT, |session| {
        visible(session)
            .iter()
            .any(|line| line.contains("T=xterm-256color C=truecolor P=dispatch"))
    }));
}

#[test]
#[cfg(unix)]
fn a_harnesss_own_environment_still_wins() {
    let mut launch = shell("echo T=$TERM");
    launch.env.insert("TERM".into(), "vt100".into());

    let mut session =
        PtySession::spawn(&launch, &cwd(), Size::new(80, 24)).expect("the shell starts");

    assert!(wait_until(&mut session, TIMEOUT, |session| {
        visible(session).iter().any(|line| line.contains("T=vt100"))
    }));
}
```

`wait_until`, `visible`, `shell`, `cwd` and `TIMEOUT` are the file's existing helpers.

Run: `cargo test -p dispatch-pty session`
Expected: FAIL to compile, because `apply_pane_env` is not found.

- [ ] **Step 2: Implement**

In `crates/dispatch-pty/src/session.rs`, above `impl Pty`:

```rust
/// What every pane is told about the terminal it is in.
///
/// Dispatch draws each pane with its own emulator, not the terminal it was
/// started from, so a program has to be told about this one: xterm's
/// terminfo, which every machine a pane might SSH to has, and 24-bit colour,
/// which the emulator draws.
const PANE_TERMINAL: [(&str, &str); 3] = [
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("TERM_PROGRAM", "dispatch"),
];

/// Variables naming the terminal Dispatch was started from.
///
/// Left in, they send a program's kitty- or iTerm-only tricks through an
/// emulator that is neither.
const HOST_TERMINAL: [&str; 18] = [
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_LISTEN_ON",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "VTE_VERSION",
    "KONSOLE_VERSION",
    "KONSOLE_DBUS_SESSION",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
];

/// Tells `command` it runs in Dispatch's terminal, not the one outside.
fn apply_pane_env(command: &mut CommandBuilder) {
    for key in HOST_TERMINAL {
        command.env_remove(key);
    }
    for (key, value) in PANE_TERMINAL {
        command.env(key, value);
    }
    command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
}
```

In `Pty::spawn`, call it after `command.cwd(cwd);` and before the loop over `launch.env`. The loop stays last, so a harness's own `env` still wins:

```rust
        apply_pane_env(&mut command);
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p dispatch-pty`
Expected: PASS, including the 3 new tests (1 on Windows).

- [ ] **Step 4: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy -p dispatch-pty --all-targets -- -D warnings`

```bash
git add crates/dispatch-pty/src/session.rs crates/dispatch-pty/src/session/tests.rs
git commit -m "feat(pty): tell every pane it is in dispatch's terminal"
```

---
### Task 8: The client shows the tabs it is told about, and places new panes

**Files:**
- Create: `dispatch/src/tabs.rs`, `dispatch/src/tabs/tests.rs`
- Modify: `dispatch/src/main.rs` (`mod tabs;`)
- Modify: `dispatch/src/app.rs`:
  - imports; remove `PANES_PER_TAB`;
  - new `App` fields `placing` and `tab_focus`;
  - `add_project`, `spawn_pane`, `open_harness_picker`, `apply_from` (the `Tabs` arm), `select_tab`, `tab_count`, `current_tab`, `panes_on_tab`, `draw`, `draw_tabs`;
  - new methods `tab_views`, `current_tab_id` and `open_picker_placing`;
  - tests.

**Interfaces:**
- Consumes:
  - Task 2's `AppState::{project_tabs, set_project_tabs, place_pane}`
  - Task 1's `ProjectTabs::from_tabs`, `Placement`, `TabId`
  - Task 3's `ServerMessage::Tabs` and `SpawnPane.place`
- Produces (used by Tasks 9–12):
  - `tabs::TabView { id: Option<TabId>, name: Option<String>, panes: Vec<PaneId> }`
  - `tabs::views(&AppState, Option<ProjectId>, &[PaneId]) -> Vec<TabView>`, never empty
  - `App::tab_views(&self) -> Vec<TabView>`, `App::current_tab_id(&self) -> Option<TabId>`
  - `App::open_picker_placing(&mut self, Placement)`
  - `App.placing: Placement`, `App.tab_focus: HashMap<TabId, PaneId>`
  - `select_tab(index)` focuses the pane this client last used on that tab, else its first.
  - `tab_count`, `current_tab` and `panes_on_tab` read `tab_views`.

- [ ] **Step 1: Write the failing view tests**

Create `dispatch/src/tabs/tests.rs`:

```rust
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
```

Add `mod tabs;` to `dispatch/src/main.rs`, and create `dispatch/src/tabs.rs` holding only its doc comment and `#[cfg(test)] mod tests;`.

Run: `cargo test -p dispatch tabs::`
Expected: FAIL to compile, because `views` and `TabView` are not found.

- [ ] **Step 2: Implement the views**

Replace `dispatch/src/tabs.rs` with:

```rust
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

#[cfg(test)]
mod tests;
```

Run: `cargo test -p dispatch tabs::`
Expected: PASS, 6 tests.

- [ ] **Step 3: Write the failing app tests**

In `dispatch/src/app.rs`'s test module, add `Tab` to the `dispatch_core` test import, then add:

```rust
    /// `attached_app`, with a `shell` harness for the picker to offer.
    fn attached_app_with_shell() -> (
        App,
        ProjectId,
        Sender<ServerMessage>,
        Receiver<ClientMessage>,
    ) {
        let (client, daemon, sent) = Client::for_test();
        let shell = dispatch_config::HarnessDef {
            id: "shell".to_string(),
            display_name: "Shell".to_string(),
            ..dispatch_config::HarnessDef::default()
        };
        let mut app = App::attached([shell].into_iter().collect(), client);

        let project = Project::new("/tmp/attached", ProjectSource::LocalDir);
        let id = project.id;
        daemon
            .send(ServerMessage::ProjectOpened { project })
            .expect("the app is listening");
        app.poll_daemon();

        (app, id, daemon, sent)
    }

    /// Sends `project`'s tabs as the daemon does, one tab per group, and
    /// returns their ids.
    fn send_tabs(
        app: &mut App,
        daemon: &Sender<ServerMessage>,
        project: ProjectId,
        groups: &[&[PaneId]],
    ) -> Vec<TabId> {
        let tabs: Vec<Tab> = groups
            .iter()
            .map(|panes| Tab {
                id: TabId::new(),
                name: None,
                panes: panes.to_vec(),
            })
            .collect();
        let ids = tabs.iter().map(|tab| tab.id).collect();
        daemon
            .send(ServerMessage::Tabs { project, tabs })
            .expect("the app is listening");
        app.poll_daemon();
        ids
    }

    /// Where the last pane this app asked its daemon for was to go.
    fn placed(sent: &Receiver<ClientMessage>) -> Option<Placement> {
        sent.try_iter()
            .filter_map(|message| match message {
                ClientMessage::SpawnPane { place, .. } => Some(place),
                _ => None,
            })
            .last()
    }

    fn a_terminal() -> ratatui::Terminal<ratatui::backend::TestBackend> {
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created")
    }

    #[test]
    fn the_daemons_snapshot_decides_which_tab_each_pane_is_on() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 3);

        send_tabs(&mut app, &daemon, project, &[&panes[..1], &panes[1..]]);

        assert_eq!(app.tab_count(), 2);
        app.focus_pane(panes[1]);
        assert_eq!(app.current_tab(), 1);
        assert_eq!(app.panes_on_tab(), panes[1..].to_vec());
    }

    #[test]
    fn a_new_pane_is_asked_into_the_tab_on_screen() {
        let (mut app, project, daemon, sent) = attached_app_with_shell();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes]);

        command(&mut app, 'n');
        press(&mut app, KeyCode::Enter);

        assert_eq!(placed(&sent), Some(Placement::Into { tab: tabs[0] }));
    }

    #[test]
    fn a_daemon_that_keeps_no_tabs_is_asked_for_no_particular_tab() {
        let (mut app, project, daemon, sent) = attached_app_with_shell();
        spawn_several(&mut app, &daemon, project, 1);

        command(&mut app, 'n');
        press(&mut app, KeyCode::Enter);

        assert_eq!(placed(&sent), Some(Placement::Auto));
    }

    #[test]
    fn choosing_a_tab_goes_back_to_the_pane_last_used_there() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 3);
        send_tabs(&mut app, &daemon, project, &[&panes[..2], &panes[2..]]);
        let mut terminal = a_terminal();

        app.focus_pane(panes[1]);
        drawn(&mut app, &mut terminal);
        app.select_tab(1);
        drawn(&mut app, &mut terminal);
        assert_eq!(app.state.focused_pane(), Some(panes[2]));

        app.select_tab(0);
        assert_eq!(
            app.state.focused_pane(),
            Some(panes[1]),
            "where the user left it, not the tab's first pane"
        );
    }

    #[test]
    fn a_pane_the_snapshot_has_not_placed_yet_is_still_shown() {
        let (mut app, project, daemon, _sent) = attached_app();
        let first = spawn_several(&mut app, &daemon, project, 1);
        send_tabs(&mut app, &daemon, project, &[&first]);

        let fresh = spawn_several(&mut app, &daemon, project, 1)[0];

        assert_eq!(app.tab_count(), 1);
        app.focus_pane(fresh);
        assert_eq!(app.panes_on_tab(), vec![first[0], fresh]);
    }

    #[test]
    fn a_tab_whose_last_pane_exits_leaves_the_row() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        send_tabs(&mut app, &daemon, project, &[&panes[..1], &panes[1..]]);
        app.focus_pane(panes[1]);
        let mut terminal = a_terminal();
        drawn(&mut app, &mut terminal);

        daemon
            .send(ServerMessage::PaneChanged {
                pane: panes[1],
                update: PaneUpdate::Status {
                    status: PaneStatus::Exited(0),
                },
            })
            .expect("the app is listening");
        app.poll_daemon();

        assert_eq!(app.tab_count(), 1);
        assert_eq!(app.current_tab(), 0);
        assert_eq!(app.panes_on_tab(), vec![panes[0]]);
        drawn(&mut app, &mut terminal);
    }

    #[test]
    fn a_standalone_project_keeps_its_own_tabs() {
        let shell = dispatch_config::HarnessDef {
            id: "sh".to_string(),
            display_name: "Sh".to_string(),
            launch: Launch {
                command: if cfg!(windows) { "cmd.exe" } else { "sh" }.to_string(),
                ..Launch::default()
            },
            ..dispatch_config::HarnessDef::default()
        };
        let mut app = App::new([shell].into_iter().collect());
        let root = scratch("standalone-tabs");
        app.add_project(root.clone());
        let project = app.state.selected_project().expect("the project is selected");
        assert!(
            app.state.project_tabs(project).is_some(),
            "a standalone client keeps its own panes' tabs"
        );

        app.spawn_pane("sh", Size::new(80, 24)).expect("the pane starts");
        app.placing = Placement::NewAfter {
            tab: app.current_tab_id(),
        };
        app.spawn_pane("sh", Size::new(80, 24)).expect("the pane starts");

        assert_eq!(app.tab_count(), 2, "the second pane opened a tab of its own");

        // Real shells: stopped here rather than left to outlive the test.
        for (_, mut pane) in app.panes.drain() {
            pane.backend.terminate();
        }
        let _ = std::fs::remove_dir_all(&root);
    }
```

Run: `cargo test -p dispatch app::tests::`
Expected: FAIL to compile, because `current_tab_id`, `placing` and the `Tabs` handling don't exist yet.

- [ ] **Step 4: Implement in `app.rs`**

**Imports.**
- Add `use crate::tabs::{self, TabView};`.
- Add `Placement`, `ProjectTabs` and `TabId` to the `dispatch_core` import.
- Delete the `PANES_PER_TAB` constant and its doc comment. Its reason now lives on `dispatch_core::TAB_CAPACITY`.

**Fields.** Add to `App`, after `tab_from`:

```rust
    /// Where the next pane chosen in the picker goes, set by whatever opened
    /// the picker.
    placing: Placement,
    /// The pane this client last focused on each tab, so coming back to a
    /// tab lands where the user left it.
    tab_focus: HashMap<TabId, PaneId>,
```

Initialise them in `App::new` with `placing: Placement::Auto,` and `tab_focus: HashMap::new(),`.

**Adding a project.** In `add_project`'s standalone path, replace the final `self.state.add_project(…);` statement with:

```rust
        let id = self.state.add_project(
            Project::new(root, source)
                .with_branch(branch)
                .with_device(device),
        );
        // This client runs a standalone project's panes, so it keeps their
        // tabs as well.
        if self.state.project_tabs(id).is_none() {
            self.state.set_project_tabs(id, ProjectTabs::new());
        }
```

**Spawning.** In `spawn_pane`:
- As its first statement, add:

  ```rust
          // Taken whatever happens next, so a placement meant for this pane
          // can never land a later one somewhere the user did not ask.
          let place = std::mem::take(&mut self.placing);
  ```

- Put `place,` in the `ClientMessage::SpawnPane { … }` it sends, replacing Task 3's `place: Placement::Auto`.
- In the standalone path, after `let id = self.state.spawn_pane(…)…;`, add:

  ```rust
          self.state.place_pane(id, place);
  ```

**Opening the picker.** Replace `open_harness_picker` with the pair below. The body of `open_picker_placing` is the old function's, plus the one line that records `place`:

```rust
    /// Opens the picker for a pane on the tab on screen.
    fn open_harness_picker(&mut self) {
        let place = self
            .current_tab_id()
            .map_or(Placement::Auto, |tab| Placement::Into { tab });
        self.open_picker_placing(place);
    }

    /// Opens the picker for a pane that goes where `place` says.
    fn open_picker_placing(&mut self, place: Placement) {
        let items: Vec<Item> = self
            .harnesses
            .all()
            .map(|h| Item::new(&h.id, &h.display_name).with_detail(&h.launch.command))
            .collect();

        if items.is_empty() {
            self.status = "no harnesses registered; press ^a H to add one".into();
            return;
        }

        self.placing = place;
        self.overlay = Some(Overlay::Harness(Picker::new("New pane", items)));
    }
```

**The snapshot.** In `apply_from`, remove `ServerMessage::Tabs { .. }` from the final ignore arm and add:

```rust
            // The daemon's word on its own project's tabs, whole: whatever
            // this client held before is replaced rather than merged.
            ServerMessage::Tabs { project, tabs } => self
                .state
                .set_project_tabs(project, ProjectTabs::from_tabs(tabs)),
```

**Tab queries.** Replace `tab_count`, `current_tab`, `panes_on_tab` and `select_tab` with:

```rust
    /// The tabs of the project on screen.
    fn tab_views(&self) -> Vec<TabView> {
        tabs::views(&self.state, self.state.selected_project(), &self.tileable())
    }

    /// How many tabs the project on screen has. Always at least one.
    fn tab_count(&self) -> usize {
        self.tab_views().len()
    }

    /// The tab on screen: the one holding the focused pane.
    ///
    /// Derived rather than stored, because a stored tab and the focus can
    /// disagree — a pane spawning, exiting, or being adopted from the daemon
    /// all move focus without going anywhere near a tab — and a view showing
    /// one tab while typing went to another would be the worst bug here.
    fn current_tab(&self) -> usize {
        let Some(focused) = self.state.focused_pane() else {
            return 0;
        };
        self.tab_views()
            .iter()
            .position(|view| view.panes.contains(&focused))
            .unwrap_or(0)
    }

    /// The id of the tab on screen, when the project keeps tabs.
    fn current_tab_id(&self) -> Option<TabId> {
        self.tab_views()
            .into_iter()
            .nth(self.current_tab())
            .and_then(|view| view.id)
    }

    /// The panes on the tab being shown.
    fn panes_on_tab(&self) -> Vec<PaneId> {
        self.tab_views()
            .into_iter()
            .nth(self.current_tab())
            .map(|view| view.panes)
            .unwrap_or_default()
    }

    /// Shows a tab by focusing a pane on it: the one this client last used
    /// there, else its first.
    ///
    /// Focusing is how a tab is shown at all, since the view follows the
    /// focus. Out of range wraps to the first, so `^a 9` on a two-tab fleet
    /// lands somewhere real rather than doing nothing.
    fn select_tab(&mut self, index: usize) {
        let views = self.tab_views();
        let index = if index < views.len() { index } else { 0 };
        let Some(view) = views.get(index) else {
            return;
        };

        let remembered = view
            .id
            .and_then(|id| self.tab_focus.get(&id).copied())
            .filter(|pane| view.panes.contains(pane));
        if let Some(pane) = remembered.or_else(|| view.panes.first().copied()) {
            let _ = self.state.focus(pane);
        }
    }
```

**Remembering focus per tab.** In `draw`, straight after `self.notice_focus(now);`:

```rust
        // Remembered once a frame rather than on each way focus can move:
        // there are many ways, and the frame sees the result of all of them.
        if let Some(focused) = self.state.focused_pane()
            && let Some(tab) = self
                .tab_views()
                .into_iter()
                .find(|view| view.panes.contains(&focused))
                .and_then(|view| view.id)
        {
            self.tab_focus.insert(tab, focused);
        }
```

**The tab row.** In `draw_tabs`, replace the chunking with the views. Task 11 replaces the labels; here they stay as they are.
- Delete `let tileable = self.tileable();` and add `let views = self.tab_views();`.
- Loop with `for (index, view) in views.iter().enumerate()`.
- Compute the title as `view.panes.first().and_then(|id| self.state.pane(*id)).map(|pane| truncate(&pane.title, TAB_TITLE))`.
- Compute the rollup as `sidebar::Rollup::of(&self.state, view.panes.iter().filter_map(|id| self.state.pane(*id)))`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p dispatch`
Expected: PASS, including 7 new app tests and 6 view tests. The existing tab tests (`a_fifth_pane_opens_a_second_tab_rather_than_shrinking_the_other_four`, `the_tab_shown_is_the_one_holding_the_focused_pane`) still pass unchanged: their daemon sends no snapshot, so they run on the fallback grouping.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add dispatch/src/tabs.rs dispatch/src/tabs/tests.rs dispatch/src/main.rs dispatch/src/app.rs
git commit -m "feat(dispatch): show the tabs the daemon keeps and place new panes on them"
```

---

### Task 9: Tab commands

**Files:**
- Modify: `dispatch/src/tabs.rs`, adding `name`, and its tests.
- Modify: `dispatch/src/app.rs`:
  - add `NEEDS_UPGRADE`;
  - add the `Overlay::RenameTab` and `Overlay::CloseTab` variants, plus the arms for them in `picker`, `picker_mut`, `kind` and `set_border`;
  - handle them in `handle_overlay` and `draw_overlay`;
  - add fields `tab_shown` and `tab_back`, and track them in `draw`;
  - add methods `change_tabs`, `apply_tab_change`, `tab_to_change`, `move_focused_pane`, `move_current_tab`, `open_rename_tab`, `handle_rename_tab_key`, `open_close_tab`, `handle_close_tab_key`, `select_previous_tab`, `select_last_tab`, `focus_or_tab`, `open_new_tab_picker` and `close_pane`, and make `close_focused` use `close_pane`;
  - add tests.

**Interfaces:**
- Consumes: Task 8's `tab_views`, `current_tab_id`, `open_picker_placing` and `select_tab`; Task 3's client messages.
- Produces (wired to keys and clicks by Tasks 10–11). All of these take `&mut self`:
  - `open_new_tab_picker()`, `move_focused_pane(step: isize)`, `move_current_tab(step: isize)`
  - `open_rename_tab()`, `open_close_tab()`
  - `select_previous_tab()`, `select_last_tab()`, `focus_or_tab(direction: Direction)`
- Produces: `tabs::name(&AppState, &TabView) -> String`.

- [ ] **Step 1: Write the failing tests**

Append to `dispatch/src/tabs/tests.rs`:

```rust
#[test]
fn a_tab_is_called_by_its_name_or_else_its_first_panes_title() {
    let (mut state, project) = state();
    let pane = top(&mut state, project);
    state.set_pane_title(pane, "fix login").expect("the pane exists");
    let mut view = TabView {
        id: None,
        name: None,
        panes: vec![pane],
    };

    assert_eq!(name(&state, &view), "fix login");

    view.name = Some("work".into());
    assert_eq!(name(&state, &view), "work");
}
```

Append to `dispatch/src/app.rs`'s test module:

```rust
    /// Every tab command the app has sent its daemon.
    fn tab_commands(sent: &Receiver<ClientMessage>) -> Vec<ClientMessage> {
        sent.try_iter()
            .filter(|message| {
                matches!(
                    message,
                    ClientMessage::MovePane { .. }
                        | ClientMessage::RenameTab { .. }
                        | ClientMessage::CloseTab { .. }
                        | ClientMessage::MoveTab { .. }
                )
            })
            .collect()
    }

    #[test]
    fn moving_a_pane_to_the_next_tab_asks_the_daemon() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes[..1], &panes[1..]]);
        app.focus_pane(panes[0]);

        app.move_focused_pane(1);

        assert_eq!(
            tab_commands(&sent),
            vec![ClientMessage::MovePane {
                pane: panes[0],
                to: Placement::Into { tab: tabs[1] },
            }]
        );
    }

    #[test]
    fn moving_past_the_last_tab_asks_for_a_new_one() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes[..1], &panes[1..]]);
        app.focus_pane(panes[1]);

        app.move_focused_pane(1);

        assert_eq!(
            tab_commands(&sent),
            vec![ClientMessage::MovePane {
                pane: panes[1],
                to: Placement::NewAfter { tab: Some(tabs[1]) },
            }]
        );
    }

    #[test]
    fn moving_left_from_the_first_tab_is_refused_here() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        send_tabs(&mut app, &daemon, project, &[&panes]);
        app.focus_pane(panes[0]);

        app.move_focused_pane(-1);

        assert_eq!(app.status, "no tab to the left");
        assert!(tab_commands(&sent).is_empty());
    }

    #[test]
    fn moving_into_a_full_tab_is_refused_here() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 5);
        send_tabs(&mut app, &daemon, project, &[&panes[..4], &panes[4..]]);
        app.focus_pane(panes[4]);

        app.move_focused_pane(-1);

        assert_eq!(app.status, "that tab is full (4 panes)");
        assert!(tab_commands(&sent).is_empty());
    }

    #[test]
    fn a_subagent_is_not_moved_between_tabs() {
        let (mut app, project, daemon, sent) = attached_app();
        let parent = spawn_several(&mut app, &daemon, project, 1)[0];
        send_tabs(&mut app, &daemon, project, &[&[parent]]);
        let kid = PaneId::new();
        daemon
            .send(spawned(kid, project, "shell", Some(parent), false))
            .expect("the app is listening");
        app.poll_daemon();
        app.focus_pane(kid);

        app.move_focused_pane(1);

        assert_eq!(app.status, "a subagent stays beside the pane that asked for it");
        assert!(tab_commands(&sent).is_empty());
    }

    #[test]
    fn a_daemon_too_old_for_tabs_is_asked_nothing() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        app.focus_pane(panes[0]);

        app.move_focused_pane(1);
        assert_eq!(app.status, NEEDS_UPGRADE);
        app.status.clear();
        app.open_rename_tab();
        assert_eq!(app.status, NEEDS_UPGRADE);
        app.status.clear();
        app.open_close_tab();
        assert_eq!(app.status, NEEDS_UPGRADE);
        app.status.clear();
        app.move_current_tab(1);
        assert_eq!(app.status, NEEDS_UPGRADE);

        assert!(app.overlay.is_none());
        assert!(tab_commands(&sent).is_empty());
    }

    #[test]
    fn renaming_asks_the_daemon_with_what_was_typed() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes]);
        app.focus_pane(panes[0]);

        app.open_rename_tab();
        for c in "work".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);

        assert_eq!(
            tab_commands(&sent),
            vec![ClientMessage::RenameTab {
                tab: tabs[0],
                name: "work".into(),
            }]
        );
    }

    #[test]
    fn a_cancelled_rename_changes_nothing() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        send_tabs(&mut app, &daemon, project, &[&panes]);
        app.focus_pane(panes[0]);

        app.open_rename_tab();
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Esc);

        assert!(app.overlay.is_none());
        assert!(tab_commands(&sent).is_empty());
    }

    #[test]
    fn closing_a_tab_asks_first() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes]);
        app.rename(panes[0], "fix login");
        app.focus_pane(panes[0]);

        app.open_close_tab();
        let Some(Overlay::CloseTab { prompt, .. }) = &app.overlay else {
            panic!("the confirmation is open");
        };
        assert_eq!(prompt.title(), "Close \"fix login\" and its 2 panes? y/n");
        press(&mut app, KeyCode::Char('n'));
        assert!(app.overlay.is_none());
        assert!(tab_commands(&sent).is_empty());

        app.open_close_tab();
        press(&mut app, KeyCode::Char('y'));

        assert_eq!(
            tab_commands(&sent),
            vec![ClientMessage::CloseTab { tab: tabs[0] }]
        );
    }

    #[test]
    fn reordering_asks_for_the_new_position_and_stops_at_the_ends() {
        let (mut app, project, daemon, sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes[..1], &panes[1..]]);
        app.focus_pane(panes[0]);

        app.move_current_tab(-1);
        app.move_current_tab(1);

        assert_eq!(
            tab_commands(&sent),
            vec![ClientMessage::MoveTab {
                tab: tabs[0],
                index: 1,
            }]
        );
    }

    #[test]
    fn a_standalone_client_changes_its_own_tabs() {
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/standalone", ProjectSource::LocalDir));
        app.state.set_project_tabs(project, ProjectTabs::new());
        let panes: Vec<PaneId> = (0..2)
            .map(|_| {
                let id = app
                    .state
                    .spawn_pane(project, HarnessId::new("shell"))
                    .expect("the project exists");
                app.state.place_pane(id, Placement::Auto);
                id
            })
            .collect();
        app.focus_pane(panes[1]);

        app.move_focused_pane(1);
        assert_eq!(app.tab_count(), 2, "the pane moved onto a tab of its own");

        app.open_rename_tab();
        for c in "work".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.tab_views()[app.current_tab()].name.as_deref(),
            Some("work")
        );

        app.open_close_tab();
        press(&mut app, KeyCode::Char('y'));
        assert!(
            app.state.pane(panes[1]).is_none(),
            "closing the tab closed its pane"
        );
        assert_eq!(app.tab_count(), 1);
    }

    #[test]
    fn the_previous_tab_wraps_and_the_last_tab_is_the_one_before() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 3);
        send_tabs(
            &mut app,
            &daemon,
            project,
            &[&panes[..1], &panes[1..2], &panes[2..]],
        );
        let mut terminal = a_terminal();
        app.focus_pane(panes[0]);
        drawn(&mut app, &mut terminal);

        app.select_previous_tab();
        assert_eq!(app.current_tab(), 2, "left of the first is the last");
        drawn(&mut app, &mut terminal);

        app.select_last_tab();
        assert_eq!(app.current_tab(), 0, "back to the tab it came from");
    }

    #[test]
    fn focus_crosses_to_the_next_tab_at_the_grids_edge_and_stops_at_the_last() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        send_tabs(&mut app, &daemon, project, &[&panes[..1], &panes[1..]]);
        let mut terminal = a_terminal();
        app.focus_pane(panes[0]);
        drawn(&mut app, &mut terminal);

        app.focus_or_tab(Direction::Right);
        assert_eq!(app.state.focused_pane(), Some(panes[1]));
        drawn(&mut app, &mut terminal);

        app.focus_or_tab(Direction::Right);
        assert_eq!(app.state.focused_pane(), Some(panes[1]), "no tab past the last");

        app.focus_or_tab(Direction::Left);
        assert_eq!(app.state.focused_pane(), Some(panes[0]));
    }
```

Run: `cargo test -p dispatch`
Expected: FAIL to compile, because `move_focused_pane`, `NEEDS_UPGRADE`, `Overlay::CloseTab`, `select_last_tab`, `tabs::name` and the other new items are not found.

- [ ] **Step 2: Implement `tabs::name`**

Add to `dispatch/src/tabs.rs`, after `views`:

```rust
/// What a tab is called: the name it was given, else its first pane's title.
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
```

- [ ] **Step 3: Implement the overlays**

In `app.rs`, next to the other constants:

```rust
/// What a tab command says on a project whose daemon keeps no tabs.
const NEEDS_UPGRADE: &str = "this machine's Dispatch needs upgrading for tabs";
```

Add to `enum Overlay`:

```rust
    /// A new name for a tab being typed.
    RenameTab {
        /// The tab being renamed.
        tab: TabId,
        /// What has been typed.
        prompt: Prompt,
    },
    /// Closing a tab, and every pane on it, waiting on a yes.
    CloseTab {
        /// The tab to close.
        tab: TabId,
        /// The question, as a prompt with nothing to type.
        prompt: Prompt,
    },
```

Give the variants their arms:
- In `picker` and `picker_mut`, add `| Overlay::RenameTab { .. } | Overlay::CloseTab { .. }` to the arm returning `None`.
- In `kind`, add the same to the arm returning `None`.
- In `set_border`, add `Overlay::RenameTab { prompt, .. } | Overlay::CloseTab { prompt, .. } => prompt.set_border(style),`.
- In `draw_overlay`, widen the `OpenOn` render to `if let Overlay::OpenOn { prompt, .. } | Overlay::RenameTab { prompt, .. } | Overlay::CloseTab { prompt, .. } = overlay {`.
- In `handle_overlay`'s paste branch, add `Some(Overlay::RenameTab { prompt, .. })` to the arm that pushes non-newline characters into an `OpenOn` prompt.
- Next to the `OpenOn` early return, add:

```rust
        if matches!(self.overlay, Some(Overlay::RenameTab { .. })) {
            self.handle_rename_tab_key(key);
            return Ok(());
        }

        if matches!(self.overlay, Some(Overlay::CloseTab { .. })) {
            self.handle_close_tab_key(key);
            return Ok(());
        }
```

- [ ] **Step 4: Implement the commands**

Add these fields to `App`, after `tab_focus`:

```rust
    /// The tab on screen at the last frame, and the one before it, for
    /// going back to the tab the user came from.
    tab_shown: Option<TabId>,
    tab_back: Option<TabId>,
```

Initialise both to `None`. In `draw`, next to the `last_tab` bookkeeping, add:

```rust
        let shown = self.current_tab_id();
        if shown != self.tab_shown {
            self.tab_back = self.tab_shown;
            self.tab_shown = shown;
        }
```

Split `close_focused`:

```rust
    fn close_focused(&mut self) {
        let Some(id) = self.state.focused_pane() else {
            return;
        };
        self.close_pane(id);
    }

    /// Closes one pane, terminating its process.
    fn close_pane(&mut self, id: PaneId) {
        // Refused rather than done locally: the machine still has the process,
        // and a row taken off this client's screen is a running agent nobody
        // can find again.
        if !self.reachable_for_pane(id) {
            return;
        }

        if let Some(mut pane) = self.panes.remove(&id) {
            pane.backend.terminate();
        }

        let _ = self.state.close_pane(id);
        // A closed pane cannot be brought into the grid, so it has nothing
        // left to be expanded into.
        self.expanded.remove(&id);
    }
```

This is the old `close_focused` body, moved unchanged.

Add these methods to `impl App`:

```rust
    /// The tab on screen, when its project keeps tabs; otherwise says why
    /// nothing can be done with it, and gives `None`.
    fn tab_to_change(&mut self) -> Option<TabId> {
        let keeps_tabs = self
            .state
            .selected_project()
            .is_some_and(|project| self.state.project_tabs(project).is_some());
        if !keeps_tabs {
            self.status = NEEDS_UPGRADE.into();
            return None;
        }
        self.current_tab_id()
    }

    /// Makes a change to the selected project's tabs: here for a project this
    /// client runs itself, by asking its daemon otherwise.
    ///
    /// The change is the message a daemon would be sent either way, so the
    /// two paths cannot drift apart in what they mean.
    fn change_tabs(&mut self, change: ClientMessage) {
        let Some(project) = self.state.selected_project() else {
            return;
        };

        if matches!(self.mode, Mode::Standalone) {
            self.apply_tab_change(project, change);
            return;
        }

        if let Some(daemon) = self
            .reachable_for_project(project)
            .map(|attachment| attachment.client.handle())
        {
            daemon.send(change);
        }
    }

    /// Makes a tab change to a project this client runs: what the daemon does
    /// for its own.
    fn apply_tab_change(&mut self, project: ProjectId, change: ClientMessage) {
        if let ClientMessage::CloseTab { tab } = change {
            let members = self
                .state
                .project_tabs(project)
                .and_then(|tabs| tabs.members(tab).ok())
                .unwrap_or_default();
            for pane in members {
                self.close_pane(pane);
            }
            return;
        }

        let Some(tabs) = self.state.project_tabs_mut(project) else {
            return;
        };
        let changed = match change {
            ClientMessage::MovePane { pane, to } => tabs.move_pane(pane, to).map(|_| ()),
            ClientMessage::RenameTab { tab, name } => tabs.rename(tab, &name),
            ClientMessage::MoveTab { tab, index } => tabs.move_tab(tab, index),
            // Only tab changes are made here.
            _ => Ok(()),
        };
        if let Err(error) = changed {
            self.status = error.to_string();
        }
    }

    /// Opens the picker for a pane on a new tab, straight after the one on
    /// screen.
    fn open_new_tab_picker(&mut self) {
        self.open_picker_placing(Placement::NewAfter {
            tab: self.current_tab_id(),
        });
    }

    /// Moves the focused pane to the tab `step` away: `-1` the previous, `1`
    /// the next, or a new one past the last.
    ///
    /// Refused here, with the reason, when the answer is already known, so a
    /// round trip to the daemon is not what tells the user a tab is full.
    fn move_focused_pane(&mut self, step: isize) {
        let Some(pane) = self.state.focused_pane() else {
            return;
        };
        if self
            .state
            .pane(pane)
            .is_some_and(|pane| pane.parent.is_some())
        {
            self.status = "a subagent stays beside the pane that asked for it".into();
            return;
        }
        let Some(current) = self.tab_to_change() else {
            return;
        };

        let views = self.tab_views();
        let here = self.current_tab();
        let to = match here.checked_add_signed(step) {
            None => {
                self.status = "no tab to the left".into();
                return;
            }
            Some(index) if index >= views.len() => Placement::NewAfter { tab: Some(current) },
            Some(index) => {
                let Some(tab) = views[index].id else {
                    return;
                };
                let full = self
                    .state
                    .selected_project()
                    .and_then(|project| self.state.project_tabs(project))
                    .is_some_and(|tabs| tabs.is_full(tab));
                if full {
                    self.status = dispatch_core::TabError::Full.to_string();
                    return;
                }
                Placement::Into { tab }
            }
        };

        self.change_tabs(ClientMessage::MovePane { pane, to });
    }

    /// Moves the tab on screen `step` places along the row. Past either end
    /// it stays where it is.
    fn move_current_tab(&mut self, step: isize) {
        let Some(tab) = self.tab_to_change() else {
            return;
        };
        let Some(index) = self.current_tab().checked_add_signed(step) else {
            return;
        };
        if index >= self.tab_count() {
            return;
        }
        self.change_tabs(ClientMessage::MoveTab { tab, index });
    }

    /// Opens the prompt that renames the tab on screen, holding its name.
    fn open_rename_tab(&mut self) {
        let Some(tab) = self.tab_to_change() else {
            return;
        };
        let current = self
            .tab_views()
            .into_iter()
            .find(|view| view.id == Some(tab))
            .and_then(|view| view.name)
            .unwrap_or_default();

        self.overlay = Some(Overlay::RenameTab {
            tab,
            prompt: Prompt::new("Rename tab", "empty goes back to the first pane's title")
                .with_input(current),
        });
    }

    /// Acts on one key while a tab's new name is being typed.
    fn handle_rename_tab_key(&mut self, key: &KeyEvent) {
        let Some(Overlay::RenameTab { tab, prompt }) = &mut self.overlay else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Backspace => prompt.backspace(),
            // Enter on nothing is a choice here, unlike a path: it clears the
            // name, and the tab goes back to its first pane's title.
            KeyCode::Enter => {
                let change = ClientMessage::RenameTab {
                    tab: *tab,
                    name: prompt.input().to_string(),
                };
                self.overlay = None;
                self.change_tabs(change);
            }
            KeyCode::Char(c) if !ctrl => prompt.push(c),
            _ => {}
        }
    }

    /// Asks before closing the tab on screen: it can stop running agents.
    fn open_close_tab(&mut self) {
        let Some(tab) = self.tab_to_change() else {
            return;
        };
        let Some(view) = self
            .tab_views()
            .into_iter()
            .find(|view| view.id == Some(tab))
        else {
            return;
        };
        let count = self
            .state
            .selected_project()
            .and_then(|project| self.state.project_tabs(project))
            .and_then(|tabs| tabs.members(tab).ok())
            .map_or(0, |members| members.len());
        let noun = if count == 1 { "pane" } else { "panes" };
        let name = tabs::name(&self.state, &view);

        self.overlay = Some(Overlay::CloseTab {
            tab,
            prompt: Prompt::new(
                format!("Close \"{name}\" and its {count} {noun}? y/n"),
                "y closes them, n keeps them",
            ),
        });
    }

    /// Acts on one key while closing a tab waits on an answer.
    fn handle_close_tab_key(&mut self, key: &KeyEvent) {
        let Some(Overlay::CloseTab { tab, .. }) = &self.overlay else {
            return;
        };
        let tab = *tab;

        match key.code {
            KeyCode::Char('y') => {
                self.overlay = None;
                self.change_tabs(ClientMessage::CloseTab { tab });
            }
            KeyCode::Char('n') | KeyCode::Esc => self.overlay = None,
            _ => {}
        }
    }

    /// Shows the tab to the left, wrapping to the last.
    fn select_previous_tab(&mut self) {
        let count = self.tab_count();
        self.select_tab((self.current_tab() + count - 1) % count);
    }

    /// Shows the tab this client was on before the one on screen.
    fn select_last_tab(&mut self) {
        let Some(back) = self.tab_back else {
            return;
        };
        if let Some(index) = self
            .tab_views()
            .iter()
            .position(|view| view.id == Some(back))
        {
            self.select_tab(index);
        }
    }

    /// Moves focus left or right, going on to the neighbouring tab at the
    /// grid's edge, and staying put past the first or last tab.
    fn focus_or_tab(&mut self, direction: Direction) {
        let before = self.state.focused_pane();
        self.focus_direction(direction);
        if self.state.focused_pane() != before {
            return;
        }

        let current = self.current_tab();
        let next = match direction {
            Direction::Left => current.checked_sub(1),
            Direction::Right => Some(current + 1).filter(|index| *index < self.tab_count()),
            Direction::Up | Direction::Down => None,
        };
        if let Some(index) = next {
            self.select_tab(index);
        }
    }
```

Nothing calls `open_new_tab_picker`, `select_previous_tab`, `select_last_tab` or `focus_or_tab` until Task 10 wires the keys. Put `#[allow(dead_code)] // wired to keys in the next change` on each of those four, and remove the attributes in Task 10.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p dispatch`
Expected: PASS, including 13 new app tests and 1 new view test.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add dispatch/src/tabs.rs dispatch/src/tabs/tests.rs dispatch/src/app.rs
git commit -m "feat(dispatch): move panes between tabs, and rename, close and reorder tabs"
```

---
### Task 10: Tab mode and the direct `Alt` keys

**Files:**
- Modify: `crates/dispatch-tui/src/input.rs`:
  - add `KeyMode`, and the new `Action` variants;
  - add `InputRouter.mode`, `key_mode` and `leave_mode`;
  - change `handle` and `handle_key`;
  - add `tab_key`, `is_tab_mode_key` and `direct`.
- Modify: `crates/dispatch-tui/src/input/tests.rs`
- Modify: `crates/dispatch-tui/src/lib.rs` (re-export `KeyMode`)
- Modify: `dispatch/src/app.rs`:
  - `handle`: a click leaves tab mode, and the new action arms are added;
  - remove Task 9's four `#[allow(dead_code)]`;
  - tests.

**Interfaces:**
- Consumes: Task 9's `open_new_tab_picker`, `open_rename_tab`, `open_close_tab`, `select_previous_tab`, `select_last_tab`, `move_focused_pane`, `move_current_tab` and `focus_or_tab`.
- Produces (used by Task 11):
  - `dispatch_tui::KeyMode::{Normal, Tabs}`
  - `InputRouter::key_mode(&self) -> KeyMode`, `InputRouter::leave_mode(&mut self)`
  - `Action::{NewTab, RenameTab, CloseTab, PreviousTab, LastTab, MovePaneLeft, MovePaneRight, MoveTabLeft, MoveTabRight, FocusOrTab(Direction)}`

- [ ] **Step 1: Write the failing router tests**

Append to `crates/dispatch-tui/src/input/tests.rs`:

```rust
fn ctrl_t() -> Event {
    press_with(KeyCode::Char('t'), KeyModifiers::CONTROL)
}

fn alt(code: KeyCode) -> Event {
    press_with(code, KeyModifiers::ALT)
}

fn mouse_down() -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton_::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn ctrl_t_enters_tab_mode_and_sends_nothing() {
    let mut router = router();

    assert_eq!(router.handle(&ctrl_t(), &[]), Action::None);
    assert_eq!(router.key_mode(), KeyMode::Tabs);
}

#[test]
fn every_tab_mode_key_is_bound_and_says_whether_the_mode_stays() {
    let cases = [
        (KeyCode::Char('n'), Action::NewTab, KeyMode::Normal),
        (KeyCode::Char('r'), Action::RenameTab, KeyMode::Normal),
        (KeyCode::Char('x'), Action::CloseTab, KeyMode::Normal),
        (KeyCode::Left, Action::PreviousTab, KeyMode::Tabs),
        (KeyCode::Char('h'), Action::PreviousTab, KeyMode::Tabs),
        (KeyCode::Right, Action::NextTab, KeyMode::Tabs),
        (KeyCode::Char('l'), Action::NextTab, KeyMode::Tabs),
        (KeyCode::Char('['), Action::MovePaneLeft, KeyMode::Tabs),
        (KeyCode::Char(']'), Action::MovePaneRight, KeyMode::Tabs),
        (KeyCode::Char('i'), Action::MoveTabLeft, KeyMode::Tabs),
        (KeyCode::Char('o'), Action::MoveTabRight, KeyMode::Tabs),
        (KeyCode::Char('3'), Action::SelectTab(2), KeyMode::Normal),
        (KeyCode::Tab, Action::LastTab, KeyMode::Normal),
        (KeyCode::Esc, Action::None, KeyMode::Normal),
        (KeyCode::Enter, Action::None, KeyMode::Normal),
    ];

    for (code, action, after) in cases {
        let mut router = router();
        router.handle(&ctrl_t(), &[]);

        assert_eq!(router.handle(&press(code), &[]), action, "tab mode then {code:?}");
        assert_eq!(router.key_mode(), after, "the mode after {code:?}");
    }
}

#[test]
fn any_other_key_in_tab_mode_is_ignored_and_the_mode_stays() {
    // A stray key must neither reach a pane nor drop the user out of what
    // they were doing.
    let mut router = router();
    router.handle(&ctrl_t(), &[]);

    for event in [
        press(KeyCode::Char('q')),
        press(KeyCode::Char('z')),
        ctrl_a(),
        alt(KeyCode::Char('n')),
    ] {
        assert_eq!(router.handle(&event, &[]), Action::None);
    }
    assert_eq!(router.key_mode(), KeyMode::Tabs);
}

#[test]
fn ctrl_t_twice_sends_ctrl_t_to_the_pane() {
    // Claude Code's task list and a shell's fzf both use it.
    let mut router = router();
    router.handle(&ctrl_t(), &[]);

    assert_eq!(
        router.handle(&ctrl_t(), &[]),
        Action::SendKey(
            Key::Char('t'),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            }
        )
    );
    assert_eq!(router.key_mode(), KeyMode::Normal);
}

#[test]
fn a_click_ends_tab_mode() {
    let mut router = router();
    router.handle(&ctrl_t(), &[]);

    router.handle(&mouse_down(), &[]);

    assert_eq!(router.key_mode(), KeyMode::Normal);
}

#[test]
fn tab_mode_does_not_start_while_the_prefix_is_armed() {
    let mut router = router();
    router.handle(&ctrl_a(), &[]);

    assert_eq!(router.handle(&ctrl_t(), &[]), Action::None);
    assert_eq!(router.key_mode(), KeyMode::Normal);
}

#[test]
fn the_direct_alt_keys_reach_dispatch() {
    let cases = [
        (KeyCode::Char('n'), Action::NewPane),
        (KeyCode::Char('i'), Action::MoveTabLeft),
        (KeyCode::Char('o'), Action::MoveTabRight),
        (KeyCode::Left, Action::FocusOrTab(Direction::Left)),
        (KeyCode::Char('h'), Action::FocusOrTab(Direction::Left)),
        (KeyCode::Right, Action::FocusOrTab(Direction::Right)),
        (KeyCode::Char('l'), Action::FocusOrTab(Direction::Right)),
        (KeyCode::Up, Action::FocusDirection(Direction::Up)),
        (KeyCode::Char('k'), Action::FocusDirection(Direction::Up)),
        (KeyCode::Down, Action::FocusDirection(Direction::Down)),
        (KeyCode::Char('j'), Action::FocusDirection(Direction::Down)),
    ];

    for (code, expected) in cases {
        let mut router = router();
        assert_eq!(router.handle(&alt(code), &[]), expected, "Alt {code:?}");
    }
}

#[test]
fn an_alt_key_dispatch_does_not_bind_still_reaches_the_pane() {
    let mut router = router();

    assert_eq!(
        router.handle(&alt(KeyCode::Char('b')), &[]),
        Action::SendKey(
            Key::Char('b'),
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            }
        )
    );
    assert!(matches!(
        router.handle(
            &press_with(KeyCode::Char('N'), KeyModifiers::ALT | KeyModifiers::SHIFT),
            &[]
        ),
        Action::SendKey(..)
    ));
}
```

Run: `cargo test -p dispatch-tui input`
Expected: FAIL to compile, because `KeyMode`, `key_mode`, `Action::NewTab` and the other new items are not found.

- [ ] **Step 2: Implement the router**

In `crates/dispatch-tui/src/input.rs`, add these variants to `Action` after `NextTab`:

```rust
    /// Show the tab to the left, wrapping to the last.
    PreviousTab,
    /// Show the tab this client was on before this one.
    LastTab,
    /// Open the picker for a pane on a new tab, straight after this one.
    NewTab,
    /// Rename the tab on screen.
    RenameTab,
    /// Close every pane on the tab on screen, once the user says yes.
    CloseTab,
    /// Move the focused pane to the previous tab.
    MovePaneLeft,
    /// Move the focused pane to the next tab, or a new one past the last.
    MovePaneRight,
    /// Move the tab on screen one place left.
    MoveTabLeft,
    /// Move the tab on screen one place right.
    MoveTabRight,
    /// Move focus left or right, going on to the neighbouring tab at the
    /// grid's edge.
    FocusOrTab(Direction),
```

Add after `Direction`:

```rust
/// Which keys the router is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyMode {
    /// Keys go to the focused pane. The prefix and a few `Alt` keys reach
    /// Dispatch.
    #[default]
    Normal,
    /// Keys are tab commands, until one of them ends the mode.
    Tabs,
}
```

Add the field `mode: KeyMode,` to `InputRouter`, with its doc comment `/// Which keys it is reading.`, and add `mode: KeyMode::Normal,` in `with_prefix`. Then add these methods next to `is_armed`:

```rust
    /// Which keys the router is reading, for the status row to say.
    #[must_use]
    pub fn key_mode(&self) -> KeyMode {
        self.mode
    }

    /// Leaves tab mode, for a click the router itself never sees.
    pub fn leave_mode(&mut self) {
        self.mode = KeyMode::Normal;
    }
```

In `handle`, replace the mouse arm:

```rust
            Event::Mouse(mouse) => {
                // A click ends tab mode, then does what it would have anyway.
                if matches!(mouse.kind, MouseEventKind::Down(_)) {
                    self.leave_mode();
                }
                self.handle_mouse(mouse, panes)
            }
```

In `handle_key`, right after the press check, add:

```rust
        if self.mode == KeyMode::Tabs {
            return self.tab_key(event);
        }
```

After the `if self.prefix.matches(event) { … }` block, and before `translate`, add:

```rust
        if is_tab_mode_key(event) {
            self.mode = KeyMode::Tabs;
            return Action::None;
        }

        if let Some(action) = direct(event) {
            return action;
        }
```

Add these to `impl InputRouter`:

```rust
    /// What a key means in tab mode, and whether the mode stays on after it.
    ///
    /// Stepping keys keep it on, so a tab or a pane can be walked several
    /// places along; keys that open something or jump somewhere end it.
    fn tab_key(&mut self, event: &KeyEvent) -> Action {
        // Twice sends it through, the way the prefix does: Claude Code and a
        // shell's fzf both use Ctrl t, and this is how they still get it.
        if is_tab_mode_key(event) {
            self.mode = KeyMode::Normal;
            return Action::SendKey(Key::Char('t'), modifiers_of(KeyModifiers::CONTROL));
        }

        // Shift is allowed through: some terminals report it for `[` and `]`.
        if !(event.modifiers - KeyModifiers::SHIFT).is_empty() {
            return Action::None;
        }

        let (action, stays) = match event.code {
            KeyCode::Char('n') => (Action::NewTab, false),
            KeyCode::Char('r') => (Action::RenameTab, false),
            KeyCode::Char('x') => (Action::CloseTab, false),
            KeyCode::Left | KeyCode::Char('h') => (Action::PreviousTab, true),
            KeyCode::Right | KeyCode::Char('l') => (Action::NextTab, true),
            KeyCode::Char('[') => (Action::MovePaneLeft, true),
            KeyCode::Char(']') => (Action::MovePaneRight, true),
            KeyCode::Char('i') => (Action::MoveTabLeft, true),
            KeyCode::Char('o') => (Action::MoveTabRight, true),
            KeyCode::Char(digit @ '1'..='9') => (
                Action::SelectTab(digit.to_digit(10).unwrap_or(1) as usize - 1),
                false,
            ),
            KeyCode::Tab => (Action::LastTab, false),
            KeyCode::Esc | KeyCode::Enter => (Action::None, false),
            // Anything else is ignored and the mode stays on: a stray key must
            // neither reach a pane nor drop the user out of what they were
            // doing.
            _ => return Action::None,
        };

        if !stays {
            self.mode = KeyMode::Normal;
        }
        action
    }
```

And these free functions, after `command_for`:

```rust
/// `Ctrl t`, the key that enters tab mode, as it does in zellij.
fn is_tab_mode_key(event: &KeyEvent) -> bool {
    event.code == KeyCode::Char('t') && event.modifiers == KeyModifiers::CONTROL
}

/// The keys that reach Dispatch with no mode and no prefix, as zellij's `Alt`
/// keys do. `Alt` with anything else held still goes to the pane.
fn direct(event: &KeyEvent) -> Option<Action> {
    if event.modifiers != KeyModifiers::ALT {
        return None;
    }

    Some(match event.code {
        KeyCode::Char('n') => Action::NewPane,
        KeyCode::Char('i') => Action::MoveTabLeft,
        KeyCode::Char('o') => Action::MoveTabRight,
        KeyCode::Left | KeyCode::Char('h') => Action::FocusOrTab(Direction::Left),
        KeyCode::Right | KeyCode::Char('l') => Action::FocusOrTab(Direction::Right),
        KeyCode::Up | KeyCode::Char('k') => Action::FocusDirection(Direction::Up),
        KeyCode::Down | KeyCode::Char('j') => Action::FocusDirection(Direction::Down),
        _ => return None,
    })
}
```

In `crates/dispatch-tui/src/lib.rs`: `pub use input::{Action, Direction, InputRouter, KeyMode, Prefix};`.

If an existing router test sent a now-bound `Alt` key or `Ctrl t` to a pane and fails only because Dispatch now takes it, change that assertion and list it in the report.

- [ ] **Step 3: Write the failing app tests**

Append to `dispatch/src/app.rs`'s test module:

```rust
    fn key_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle(&Event::Key(KeyEvent::new(code, modifiers)), Size::new(100, 30))
            .expect("a keystroke is handled");
    }

    #[test]
    fn ctrl_t_then_n_asks_for_a_new_tab_after_the_one_on_screen() {
        let (mut app, project, daemon, sent) = attached_app_with_shell();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes]);

        key_with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Enter);

        assert_eq!(
            placed(&sent),
            Some(Placement::NewAfter { tab: Some(tabs[0]) })
        );
    }

    #[test]
    fn a_tab_mode_key_that_opens_the_picker_hands_it_the_keyboard() {
        let (mut app, project, daemon, _sent) = attached_app_with_shell();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        send_tabs(&mut app, &daemon, project, &[&panes]);

        key_with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('n'));

        assert_eq!(app.router.key_mode(), KeyMode::Normal);
        press(&mut app, KeyCode::Char('x'));
        assert!(
            matches!(app.overlay, Some(Overlay::Harness(_))),
            "x went to the picker, not to closing a tab"
        );
    }

    #[test]
    fn a_click_ends_tab_mode() {
        let (mut app, project, daemon, _sent) = attached_app();
        spawn_several(&mut app, &daemon, project, 1);
        let mut terminal = a_terminal();
        drawn(&mut app, &mut terminal);

        key_with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        click(&mut app, 2, 5);

        assert_eq!(app.router.key_mode(), KeyMode::Normal);
    }

    #[test]
    fn tab_mode_arrows_step_along_the_tabs() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 3);
        send_tabs(
            &mut app,
            &daemon,
            project,
            &[&panes[..1], &panes[1..2], &panes[2..]],
        );
        app.focus_pane(panes[0]);

        key_with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);

        assert_eq!(app.current_tab(), 2);
        assert_eq!(app.router.key_mode(), KeyMode::Tabs, "still stepping");
    }

    #[test]
    fn alt_n_opens_the_picker_for_this_tab() {
        let (mut app, project, daemon, sent) = attached_app_with_shell();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes]);

        key_with(&mut app, KeyCode::Char('n'), KeyModifiers::ALT);
        press(&mut app, KeyCode::Enter);

        assert_eq!(placed(&sent), Some(Placement::Into { tab: tabs[0] }));
    }

    #[test]
    fn ctrl_t_twice_types_ctrl_t_into_the_pane() {
        let (mut app, project, daemon, sent) = attached_app();
        let pane = spawn_several(&mut app, &daemon, project, 1)[0];

        key_with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        key_with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);

        assert!(sent.try_iter().any(|message| matches!(
            message,
            ClientMessage::WritePane { pane: p, bytes } if p == pane && bytes == [0x14]
        )));
    }
```

`click(app, column, row)` is the test module's existing helper. Row 5, column 2 lies on the sidebar, which the app handles before the router sees the event. That is why the app has to end the mode itself.

Run: `cargo test -p dispatch`
Expected: FAIL to compile. The new `Action` variants aren't handled by `App::handle`'s match, and `KeyMode` isn't imported.

- [ ] **Step 4: Wire the keys**

In `dispatch/src/app.rs`:
- Add `KeyMode` to the `dispatch_tui::input` import.
- Remove the four `#[allow(dead_code)]` lines from Task 9.
- In `App::handle`, as its very first statement (before the overlay check), add:

```rust
        // A click ends tab mode whatever it lands on. The sidebar and the tab
        // row are resolved here, before the router sees the event, so the
        // router cannot end it for them.
        if let Event::Mouse(mouse) = event
            && matches!(mouse.kind, MouseEventKind::Down(_))
        {
            self.router.leave_mode();
        }
```

Add these arms to the `match action`:

```rust
            Action::NewTab => self.open_new_tab_picker(),
            Action::RenameTab => self.open_rename_tab(),
            Action::CloseTab => self.open_close_tab(),
            Action::PreviousTab => self.select_previous_tab(),
            Action::LastTab => self.select_last_tab(),
            Action::MovePaneLeft => self.move_focused_pane(-1),
            Action::MovePaneRight => self.move_focused_pane(1),
            Action::MoveTabLeft => self.move_current_tab(-1),
            Action::MoveTabRight => self.move_current_tab(1),
            Action::FocusOrTab(direction) => self.focus_or_tab(direction),
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p dispatch-tui && cargo test -p dispatch`
Expected: PASS, including 8 new router tests and 6 new app tests.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add crates/dispatch-tui/src/input.rs crates/dispatch-tui/src/input/tests.rs crates/dispatch-tui/src/lib.rs dispatch/src/app.rs
git commit -m "feat(tui): a ctrl t tab mode and direct alt keys"
```

---

### Task 11: The tab row, its clicks, and the status row

**Files:**
- Modify: `dispatch/src/tabs.rs`: add `TabHit`, `MARK`, `PLUS` and `visible_range`, plus tests.
- Modify: `dispatch/src/app.rs`:
  - add fields `tab_row` and `tab_hits`;
  - rewrite `draw_tabs` (now `&mut self`);
  - handle tab-row clicks in `handle`;
  - in `draw_status`, add the tab-mode line and the help text change;
  - add `TAB_MODE_HELP`;
  - tests.

**Interfaces:**
- Consumes:
  - Task 8's `tab_views`, `current_tab`, `select_tab`
  - Task 9's `tabs::name`, `open_new_tab_picker`, `select_previous_tab`
  - Task 10's `KeyMode`, `InputRouter::key_mode`
- Produces:
  - `tabs::visible_range(&[u16], usize, u16) -> Range<usize>`
  - `tabs::TabHit::{Tab(usize), New, Previous, Next}`
  - `tabs::{MARK, PLUS}`

- [ ] **Step 1: Write the failing tests**

Append to `dispatch/src/tabs/tests.rs`:

```rust
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
```

Append to `dispatch/src/app.rs`'s test module:

```rust
    /// Row 0 of what was drawn: the name and the tab row.
    fn top_row(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        rendered_text(terminal)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The last row of what was drawn: the status row.
    fn bottom_row(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        rendered_text(terminal)
            .lines()
            .last()
            .unwrap_or_default()
            .to_string()
    }

    /// The screen column `needle` starts at in `row`, counted in cells.
    fn cell_of(row: &str, needle: &str) -> u16 {
        u16::try_from(column_of(row, needle)).expect("the row is narrow")
    }

    #[test]
    fn a_tab_is_labelled_by_its_first_panes_title_with_no_number() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        app.rename(panes[0], "alpha");
        app.rename(panes[1], "beta");
        send_tabs(&mut app, &daemon, project, &[&panes[..1], &panes[1..]]);
        let mut terminal = a_terminal();

        drawn(&mut app, &mut terminal);

        let row = top_row(&terminal);
        assert!(row.contains(" alpha ") && row.contains(" beta "), "{row:?}");
        assert!(!row.contains("1 alpha") && !row.contains("2 beta"), "{row:?}");
    }

    #[test]
    fn a_name_given_to_a_tab_replaces_its_panes_title() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        app.rename(panes[0], "alpha");
        daemon
            .send(ServerMessage::Tabs {
                project,
                tabs: vec![Tab {
                    id: TabId::new(),
                    name: Some("work".into()),
                    panes: panes.clone(),
                }],
            })
            .expect("the app is listening");
        app.poll_daemon();
        let mut terminal = a_terminal();

        drawn(&mut app, &mut terminal);

        let row = top_row(&terminal);
        assert!(row.contains(" work ") && !row.contains("alpha"), "{row:?}");
    }

    #[test]
    fn a_wide_name_is_cut_to_sixteen_columns() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        let wide = "界".repeat(20);
        daemon
            .send(ServerMessage::Tabs {
                project,
                tabs: vec![Tab {
                    id: TabId::new(),
                    name: Some(wide.clone()),
                    panes,
                }],
            })
            .expect("the app is listening");
        app.poll_daemon();
        let mut terminal = a_terminal();

        drawn(&mut app, &mut terminal);

        // Sixteen columns: seven two-column characters and the ellipsis.
        let row = top_row(&terminal);
        assert_eq!(row.matches('界').count(), 7, "{row:?}");
        assert!(row.contains('…'), "{row:?}");
    }

    #[test]
    fn the_plus_opens_the_picker_for_a_new_tab() {
        let (mut app, project, daemon, sent) = attached_app_with_shell();
        let panes = spawn_several(&mut app, &daemon, project, 1);
        let tabs = send_tabs(&mut app, &daemon, project, &[&panes]);
        let mut terminal = a_terminal();
        drawn(&mut app, &mut terminal);

        let plus = cell_of(&top_row(&terminal), " + ") + 1;
        click(&mut app, plus, 0);
        press(&mut app, KeyCode::Enter);

        assert_eq!(
            placed(&sent),
            Some(Placement::NewAfter { tab: Some(tabs[0]) })
        );
    }

    #[test]
    fn clicking_a_tab_shows_it() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 2);
        app.rename(panes[0], "alpha");
        app.rename(panes[1], "beta");
        send_tabs(&mut app, &daemon, project, &[&panes[..1], &panes[1..]]);
        app.focus_pane(panes[0]);
        let mut terminal = a_terminal();
        drawn(&mut app, &mut terminal);

        click(&mut app, cell_of(&top_row(&terminal), "beta"), 0);

        assert_eq!(app.current_tab(), 1);
    }

    #[test]
    fn with_more_tabs_than_fit_the_one_on_screen_stays_in_view() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 10);
        for (index, pane) in panes.iter().enumerate() {
            app.rename(*pane, &format!("tab-number-{index:02}"));
        }
        let groups: Vec<&[PaneId]> = panes.chunks(1).collect();
        send_tabs(&mut app, &daemon, project, &groups);
        let mut terminal = a_terminal();

        app.focus_pane(panes[9]);
        drawn(&mut app, &mut terminal);
        let row = top_row(&terminal);
        assert!(row.contains("tab-number-09") && row.contains('‹'), "{row:?}");

        app.focus_pane(panes[0]);
        drawn(&mut app, &mut terminal);
        let row = top_row(&terminal);
        assert!(row.contains("tab-number-00") && row.contains('›'), "{row:?}");
    }

    #[test]
    fn tab_mode_is_spelled_out_on_the_status_row() {
        let (mut app, project, daemon, _sent) = attached_app();
        spawn_several(&mut app, &daemon, project, 1);
        let mut terminal = a_terminal();

        drawn(&mut app, &mut terminal);
        assert!(bottom_row(&terminal).contains("Ctrl t tabs"));

        key_with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        drawn(&mut app, &mut terminal);
        assert!(bottom_row(&terminal).starts_with(TAB_MODE_HELP));
    }
```

`column_of(line, needle)` is the test module's existing helper. It counts characters, and for these ASCII rows a character is a cell.

Run: `cargo test -p dispatch`
Expected: FAIL to compile, because `visible_range`, `TAB_MODE_HELP` and `tabs::TabHit` are not found.

- [ ] **Step 2: Implement the range and the hits**

Add to `dispatch/src/tabs.rs`, after `name`:

```rust
/// Columns a `‹` or `›` takes, with the blank beside it.
pub const MARK: u16 = 2;

/// Columns ` + ` takes.
pub const PLUS: u16 = 3;

/// What a click on the tab row lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHit {
    /// The tab at this position.
    Tab(usize),
    /// The `+`, which opens a new tab.
    New,
    /// The `‹` before the first tab drawn.
    Previous,
    /// The `›` after the last tab drawn.
    Next,
}

/// Which tabs to draw in `width` columns so the current one is in view.
///
/// `widths` are the tabs' label widths; one blank separates two tabs. Room is
/// kept for ` + ` and the blank before it, for `‹` when the range does not
/// start at the first tab, and for `›` when it does not reach the last.
/// Starts from the first tab while the current one fits that way, so the row
/// only scrolls once it has to. A current tab too wide to fit alone is still
/// the one shown, cut off at the edge.
pub fn visible_range(widths: &[u16], current: usize, width: u16) -> std::ops::Range<usize> {
    let count = widths.len();
    if count == 0 {
        return 0..0;
    }
    let current = current.min(count - 1);
    let room = u32::from(width.saturating_sub(PLUS + 1));

    let fits = |start: usize, end: usize| {
        let labels: u32 = widths[start..end].iter().map(|w| u32::from(*w)).sum();
        let gaps = u32::try_from((end - start).saturating_sub(1)).unwrap_or(u32::MAX);
        let marks = u32::from(MARK) * (u32::from(start > 0) + u32::from(end < count));
        labels.saturating_add(gaps).saturating_add(marks) <= room
    };

    let mut end = 0;
    while end < count && fits(0, end + 1) {
        end += 1;
    }
    if current < end {
        return 0..end;
    }

    let mut start = current;
    while start > 0 && fits(start - 1, current + 1) {
        start -= 1;
    }
    let mut end = current + 1;
    while end < count && fits(start, end + 1) {
        end += 1;
    }
    start..end
}
```

- [ ] **Step 3: Draw the row**

In `dispatch/src/app.rs`:
- Import `tabs::TabHit` (`use crate::tabs::{self, TabHit, TabView};`) and `KeyMode`.
- Add a constant:

```rust
/// The status row while tab mode is on: every key it takes, since nothing
/// else on screen says what they are.
const TAB_MODE_HELP: &str =
    "TAB  n new  r rename  x close  ←→ switch  [ ] move pane  i o move tab  1-9 go  Esc done";
```

Add these fields to `App`, after `tab_back`:

```rust
    /// Where the tab row was drawn last frame, and what each stretch of it
    /// is, for a click to be matched against.
    tab_row: Rect,
    tab_hits: Vec<(u16, u16, TabHit)>,
```

Initialise them with `tab_row: Rect::default(), tab_hits: Vec::new(),`. Then replace `draw_tabs` whole:

```rust
    /// Draws the row of tabs above the grid.
    ///
    /// Each is its rollup glyph and its name: the one it was given, else its
    /// first pane's title. No number: a digit still picks one by position, and
    /// the status row says which position is on screen. The one on screen
    /// sits on a tint rather than being inverted: it should read as the one
    /// you are in, not as a warning.
    fn draw_tabs(&mut self, frame: &mut Frame<'_>, area: Rect, now: Instant) {
        self.tab_hits.clear();
        self.tab_row = area;
        if area.height == 0 || area.width == 0 {
            return;
        }

        let views = self.tab_views();
        let current = self.current_tab();
        let sliding = self.animations.value(Target::Tab, now).is_some();
        let spinner = self.spinner_frame();

        // A project with nothing to tile has a tab to be on but nothing to
        // call it: it draws no label, only the `+`.
        let labels: Vec<Vec<Span<'static>>> = views
            .iter()
            .enumerate()
            .map(|(index, view)| {
                if view.panes.is_empty() {
                    return Vec::new();
                }
                // `tab` is mixed from the palette, the fallback's dark one
                // when the terminal did not answer, so the text on it comes
                // from the palette too. While the tint slides it is laid on
                // afterwards, across whichever columns it has reached.
                let style = if index == current {
                    let style = Style::default()
                        .fg(self.theme.text)
                        .add_modifier(Modifier::BOLD);
                    if sliding {
                        style
                    } else {
                        style.bg(self.theme.tab)
                    }
                } else {
                    Style::default().fg(self.theme.faded)
                };

                let mut spans = Vec::new();
                if let Some(rollup) = sidebar::Rollup::of(
                    &self.state,
                    view.panes.iter().filter_map(|id| self.state.pane(*id)),
                ) {
                    let (glyph, glyph_style) = rollup.glyph(spinner, &self.theme);
                    spans.push(Span::styled(" ", style));
                    spans.push(Span::styled(glyph, style.patch(glyph_style)));
                }
                let name = truncate(&tabs::name(&self.state, view), TAB_TITLE);
                spans.push(Span::styled(format!(" {name} "), style));
                spans
            })
            .collect();
        let widths: Vec<u16> = labels
            .iter()
            .map(|spans| {
                spans
                    .iter()
                    .map(|span| u16::try_from(span.width()).unwrap_or(u16::MAX))
                    .sum()
            })
            .collect();
        let shown = tabs::visible_range(&widths, current, area.width);

        let faded = Style::default().fg(self.theme.faded);
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut column = area.x;
        // Where each tab sits in the row, for the sliding tint.
        let mut extents: Vec<Option<(u16, u16)>> = vec![None; views.len()];

        if shown.start > 0 {
            spans.push(Span::styled("‹ ", faded));
            self.tab_hits.push((column, 1, TabHit::Previous));
            column = column.saturating_add(tabs::MARK);
        }
        let mut drawn_any = false;
        for index in shown.clone() {
            if widths[index] == 0 {
                continue;
            }
            // The gap between two tabs belongs to neither.
            if drawn_any {
                spans.push(Span::raw(" "));
                column = column.saturating_add(1);
            }
            drawn_any = true;
            extents[index] = Some((column, widths[index]));
            self.tab_hits.push((column, widths[index], TabHit::Tab(index)));
            spans.extend(labels[index].iter().cloned());
            column = column.saturating_add(widths[index]);
        }
        if shown.end < views.len() {
            spans.push(Span::styled(" ›", faded));
            self.tab_hits
                .push((column.saturating_add(1), 1, TabHit::Next));
            column = column.saturating_add(tabs::MARK);
        }
        Paragraph::new(Line::from(spans)).render(area, frame.buffer_mut());

        // Straight after the tabs while they all fit; pinned to the right edge
        // once the row scrolls, so it is always in the same place to reach for.
        let right = area.x.saturating_add(area.width);
        let plus_x = if shown.start == 0 && shown.end == views.len() {
            column.saturating_add(u16::from(drawn_any))
        } else {
            right.saturating_sub(tabs::PLUS)
        };
        if plus_x < right {
            let plus = Rect::new(plus_x, area.y, tabs::PLUS.min(right - plus_x), 1);
            Paragraph::new(Span::styled(" + ", faded)).render(plus, frame.buffer_mut());
            self.tab_hits.push((plus_x, plus.width, TabHit::New));
        }

        if let Some(t) = self.animations.value(Target::Tab, now) {
            let (from_x, from_w) = extents
                .get(self.tab_from)
                .copied()
                .flatten()
                .unwrap_or((area.x, 0));
            let (to_x, to_w) = extents
                .get(current)
                .copied()
                .flatten()
                .unwrap_or((area.x, 0));
            let lerp =
                |a: u16, b: u16| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u16;
            let (x, width) = (lerp(from_x, to_x), lerp(from_w, to_w));
            for column in x..x.saturating_add(width).min(right) {
                if let Some(cell) = frame.buffer_mut().cell_mut((column, area.y)) {
                    cell.set_bg(self.theme.tab);
                }
            }
        }
    }
```

In `App::handle`, straight after the overlay check and before the sidebar click, add:

```rust
        // The tab row is not part of input routing either: a click on it is
        // resolved against what was drawn there last frame.
        if let Event::Mouse(mouse) = event
            && matches!(mouse.kind, MouseEventKind::Down(_))
            && self.tab_row.height > 0
            && mouse.row == self.tab_row.y
            && let Some(hit) = self
                .tab_hits
                .iter()
                .find(|(x, width, _)| mouse.column >= *x && mouse.column < x.saturating_add(*width))
                .map(|(_, _, hit)| *hit)
        {
            match hit {
                TabHit::Tab(index) => self.select_tab(index),
                TabHit::New => self.open_new_tab_picker(),
                TabHit::Previous => self.select_previous_tab(),
                TabHit::Next => self.select_tab(self.current_tab() + 1),
            }
            return Ok(());
        }
```

- [ ] **Step 4: The status row**

In `draw_status`:
- Change the `let text = if self.router.is_armed() { "PREFIX".to_string() } else { … }` chain to:

```rust
        let text = if self.router.is_armed() {
            // A prefix that armed invisibly is how a keystroke goes missing
            // with no explanation.
            "PREFIX".to_string()
        } else if self.router.key_mode() == KeyMode::Tabs {
            // The same goes for a mode, and it has keys of its own to spell out.
            TAB_MODE_HELP.to_string()
        } else {
```

- Change the tab counter to `format!("  tab {}/{}", self.current_tab() + 1, self.tab_count())`.
- Change the help to:

```rust
                let help = format!(
                    "{panes} pane(s){where_}{tabs}  ^a n new  Ctrl t tabs  ^a x close  ^a z zoom  ^a s child  ^a c collapse  ^a q quit"
                );
```

- Make the highlighted style apply in tab mode as well: `let style = if self.router.is_armed() || self.router.key_mode() == KeyMode::Tabs {`.

If an existing test asserted the old `^a 1-9` counter or the old help text, update it to the new text and list the change in the report.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p dispatch`
Expected: PASS, including 5 new range tests and 7 new app tests.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add dispatch/src/tabs.rs dispatch/src/tabs/tests.rs dispatch/src/app.rs
git commit -m "feat(dispatch): named tabs with a plus, scrolled into view and clickable"
```

---

### Task 12: Shell first in the picker, end-to-end, and the README

**Files:**
- Modify: `dispatch/src/app.rs`: `open_picker_placing`, the new `shell_label` and `project_is_local`, and tests.
- Modify: `dispatch/tests/end_to_end.rs`: the `Fixture` writes `[shell]`, the comments above `SHELL_HARNESS` and in `Fixture::new` change, and two tests are added.
- Modify: `README.md`

**Interfaces:**
- Consumes: Task 6's `dispatch_config::SHELL` and Task 8's `open_picker_placing`.

- [ ] **Step 1: Write the failing app tests**

Append to `dispatch/src/app.rs`'s test module:

```rust
    /// A registry holding `claude` and the user's shell, `command`.
    fn registry_with_shell(command: &str) -> HarnessRegistry {
        [
            dispatch_config::HarnessDef {
                id: "claude".to_string(),
                display_name: "Claude Code".to_string(),
                ..dispatch_config::HarnessDef::default()
            },
            dispatch_config::HarnessDef {
                id: dispatch_config::SHELL.to_string(),
                display_name: "Shell".to_string(),
                launch: Launch {
                    command: command.to_string(),
                    ..Launch::default()
                },
                ..dispatch_config::HarnessDef::default()
            },
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn the_picker_offers_the_users_shell_first_named_after_its_program() {
        let mut app = App::new(registry_with_shell("/usr/bin/zsh"));
        app.state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));

        app.open_harness_picker();

        let Some(Overlay::Harness(picker)) = &app.overlay else {
            panic!("the picker is open");
        };
        assert_eq!(picker.items()[0].label, "Shell · zsh");
        assert_eq!(
            picker.selected().map(|item| item.id.as_str()),
            Some("shell"),
            "Enter gives a shell"
        );
    }

    #[test]
    fn a_shell_on_another_machine_is_not_named_after_this_ones() {
        // The picker is this machine's, but a shell runs where the project
        // is: its program is only this machine's to name when that is here.
        let (client, daemon, _sent) = Client::for_test();
        let mut app = App::new(registry_with_shell("/usr/bin/zsh"));
        app.attach_named(client, Some("box".into()), Vec::new());
        daemon
            .send(ServerMessage::ProjectOpened {
                project: Project::new("/srv/app", ProjectSource::LocalDir),
            })
            .expect("the app is listening");
        app.poll_daemon();

        app.open_harness_picker();

        let Some(Overlay::Harness(picker)) = &app.overlay else {
            panic!("the picker is open");
        };
        assert_eq!(picker.items()[0].label, "Shell");
        assert_eq!(picker.items()[0].detail, None);
    }
```

Run: `cargo test -p dispatch picker_offers`
Expected: FAIL. The first item is `Claude Code`: items sort by id and nothing puts the shell first.

- [ ] **Step 2: Implement**

In `dispatch/src/app.rs`, add `SHELL` to the `dispatch_config` import. Then replace the `items` construction in `open_picker_placing` with:

```rust
        let local = self.project_is_local();
        let mut items: Vec<Item> = self
            .harnesses
            .all()
            .map(|h| {
                if h.id == SHELL && !local {
                    // The picker is this machine's, but a shell runs where the
                    // project is, and this machine cannot say which one that
                    // machine will start.
                    return Item::new(&h.id, &h.display_name);
                }
                let label = if h.id == SHELL {
                    shell_label(&h.launch.command)
                } else {
                    h.display_name.clone()
                };
                Item::new(&h.id, label).with_detail(&h.launch.command)
            })
            .collect();
        // The user's own shell first, and so chosen: Enter on a new tab gives
        // a shell, one arrow an agent.
        items.sort_by_key(|item| item.id != SHELL);
```

Add a free function next to `pane_title`:

```rust
/// The picker's name for the user's shell, after its program: `Shell · zsh`.
fn shell_label(command: &str) -> String {
    let program = Path::new(command)
        .file_stem()
        .map_or_else(|| command.to_string(), |stem| stem.to_string_lossy().into_owned());
    format!("Shell · {program}")
}
```

Add a method to `impl App`:

```rust
    /// Whether the selected project's panes run on this machine.
    fn project_is_local(&self) -> bool {
        match &self.mode {
            Mode::Standalone => true,
            Mode::Attached(_) => self
                .state
                .selected_project()
                .and_then(|project| self.attachment_for_project(project))
                .is_some_and(|attachment| !attachment.remote),
        }
    }
```

- [ ] **Step 3: End to end**

In `dispatch/tests/end_to_end.rs`, in `Fixture::new`, after the harness file is written:

```rust
        // The built-in shell is offered first. Pinned to plain `sh` here, so
        // no test runs the developer's own shell and its prompt.
        std::fs::write(
            config.path().join("config.toml"),
            "[shell]\ncommand = \"sh\"\nlogin = \"never\"\n",
        )
        .expect("temp dir is writable");
```

Update the comments above `SHELL_HARNESS` and above its `std::fs::write`. They say it sorts first so "new pane" selects it. It now sorts first among the harness files, below the built-in Shell, and it remains the harness the delegation tests delegate to.

Append these tests:

```rust
#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn the_picker_offers_the_users_shell_first() {
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));

    app.send(b"\x01n");
    assert!(
        app.wait_for(|lines| contains(lines, "Shell · sh")),
        "the user's shell is offered by name"
    );

    app.send(b"\r");
    assert!(app.wait_for(|lines| panes_shown(lines) > 0));
    app.send(b"echo from-the-users-shell\r");
    assert!(
        app.wait_for(|lines| contains(lines, "from-the-users-shell")),
        "Enter started the shell, and it answers"
    );
}

#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn tab_mode_opens_a_new_tab_with_a_pane_of_its_own() {
    let mut app = Harness::start(Size::new(100, 30));
    assert!(app.wait_for(|lines| contains(lines, "pane(s)")));
    app.spawn_shell();

    // Ctrl t, then n: the picker, for a pane on a new tab.
    app.send(b"\x14n");
    assert!(app.wait_for(|lines| contains(lines, "New pane")));
    app.send(b"\r");

    assert!(
        app.wait_for(|lines| contains(lines, "tab 2/2")),
        "the new pane is on a tab of its own"
    );
}
```

- [ ] **Step 4: The README**

In `README.md`, replace the second paragraph of `## The grid` (the one beginning "At most four panes are tiled at once") with:

~~~markdown
At most four panes are tiled at once, on a tab. Tabs are yours: a new pane
opens on the tab you are on, and a fifth on a full tab opens the next one.
Closing a pane never moves panes on other tabs. The sidebar always lists
every pane, whichever tab it is on.

## Tabs

The row across the top names each tab after its first pane's title, or the
name you give it, with `+` at its end for a new one. Click a tab to go to
it. When there are more tabs than fit, the row scrolls to keep yours in view.

`Ctrl t` enters tab mode, and the status row lists its keys:

| Key | Does |
|---|---|
| `n` | new tab: the picker, with your shell first |
| `r` | rename the tab (empty goes back to the first pane's title) |
| `x` | close the tab and every pane on it, after a y/n |
| `←` `→` / `h` `l` | previous / next tab |
| `[` `]` | move the focused pane to the previous / next tab (a new one past the last) |
| `i` `o` | move the tab left / right |
| `1`–`9` | go to a tab by position |
| `Tab` | the tab you were on before |
| `Esc` / `Enter` | leave tab mode |
| `Ctrl t` | send `Ctrl t` itself to the pane (Claude Code and fzf use it) |

Some keys work without a mode: `Alt n` opens a new pane on this tab,
`Alt i` / `Alt o` move the tab, and `Alt` with an arrow or `h` `j` `k` `l`
moves focus, going on to the next tab at the grid's edge. `^a 1`–`^a 9` and
`^a Tab` still work too.

A daemon keeps its projects' tabs, so they survive detaching and look the
same from every client. A daemon older than tabs still works: its panes are
grouped four at a time, as before.

## Shell panes

The picker's first entry is your own shell — `Shell · zsh`, or whatever
`$SHELL` is on the machine the project is on — so `Enter` opens a terminal
in the project's directory. It starts the way your terminal starts it (a
login shell on macOS, a plain interactive one elsewhere), so your rc file
runs and your prompt, Starship or otherwise, looks as it does anywhere else.
To choose it yourself:

```toml
# ~/.config/dispatch/config.toml
[shell]
command = "/usr/bin/fish"   # default: $SHELL, then your login record, then /bin/sh
args = []
login = "auto"              # auto | always | never
```

Every pane is told it is in Dispatch's terminal — `TERM=xterm-256color`,
`COLORTERM=truecolor`, `TERM_PROGRAM=dispatch` — and not the one Dispatch
runs in, so a program never sends it another terminal's private sequences.
A harness's own `env` still wins.
~~~

- [ ] **Step 5: Run the tests**

Run: `cargo test -p dispatch`
Expected: PASS, including 2 new app tests and 2 new end-to-end tests. Every existing end-to-end test still passes, because the built-in shell runs `sh` in the fixture.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add dispatch/src/app.rs dispatch/tests/end_to_end.rs README.md
git commit -m "feat(dispatch): offer the user's shell first, and document tabs and shells"
```

---

### Task 13: Verify the whole branch

**Files:** none, unless a check fails. Then fix it, and commit as `fix(dispatch): what verifying tabs and shells turned up`.

- [ ] **Step 1: The whole suite**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --no-fail-fast`
Expected: all clean, and every test passes. Report the total.

- [ ] **Step 2: The other platforms**

Run:
- `cargo clippy -p dispatch-tui -p dispatch-config -p dispatch-pty -p dispatch-core -p dispatch-proto --all-targets --target aarch64-apple-darwin -- -D warnings`
- `cargo clippy -p dispatch-os --all-targets --target aarch64-apple-darwin -- -D warnings`
- `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings 2>&1 | grep -E "^(error|warning)" -A 3 | grep -E "\-\->" | sort -u`

Expected: macOS is clean. On Windows, only the errors already on `main` (`dispatch-os/src/host.rs`, `ipc.rs`, and the daemon's test helpers) appear, and nothing is listed from a file this branch changed.

- [ ] **Step 3: Run it**

Build, then drive the real binary in a pseudoterminal and look at the screen. `SCRATCH` is the session scratchpad.

```bash
cargo build -p dispatch
mkdir -p "$SCRATCH/c-smoke/project" "$SCRATCH/c-smoke/config"
printf '[shell]\ncommand = "sh"\nlogin = "never"\n' > "$SCRATCH/c-smoke/config/config.toml"
cd "$SCRATCH/c-smoke/project"
( sleep 2; printf '\x01n'; sleep 1; printf '\r'; sleep 1; printf 'echo smoke-one\r'; sleep 1; printf '\x14n'; sleep 1; printf '\r'; sleep 1; printf '\x14r'; sleep 1; printf 'second\r'; sleep 1; printf '\x01q' ) \
  | DISPATCH_CONFIG_DIR="$SCRATCH/c-smoke/config" script -qfc "$OLDPWD/target/debug/dispatch ." "$SCRATCH/c-smoke/look.txt" >/dev/null
cd "$OLDPWD"
python3 -c "import re,sys; s=open(sys.argv[1],errors='replace').read(); s=re.sub(r'\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\x1b.',' ',s); print(' '.join(s.split())[-2500:])" "$SCRATCH/c-smoke/look.txt"
pgrep -f target/debug/dispatch || echo "no dispatch left running"
```

Expected, in the extracted text: `Shell · sh`, `smoke-one`, `TAB  n new`, `second` (the renamed tab), `tab 2/2` and `no dispatch left running`. Report the extracted text.

- [ ] **Step 4: The spec against the code**

Read `docs/superpowers/specs/2026-09-25-tabs-and-shells-design.md` section by section against the code. Wherever they disagree, fix whichever is wrong: the code if it misses the spec, or the spec's wording if the code shows a refinement made during the build. Update the spec's status line to "implemented on branch `ui/tabs-shells`". List every change in the report.

- [ ] **Step 5: Commit, if anything changed**

```bash
git add -A
git commit -m "fix(dispatch): what verifying tabs and shells turned up"
```

`git add -A` is safe here only because the tree was clean before this task. Run `git status --short` first and stage by name if anything unexpected shows.
