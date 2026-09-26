# C — Tabs you own, and shell panes

Status: implemented on branch `ui/tabs-shells`.
Date: 2026-09-25.
Third of four UI slices: A visual refresh (merged), B live status and motion
(merged), C this, D keybindings.
Follows: `docs/superpowers/specs/2026-09-25-live-status-motion-design.md`.

## The problem

In the user's words: "Lets have tabs on the top as well that can be added
without having to open 4 tabs", and "Lets allow for opening shells as a
window as well. I want their customizations to appear in that shell window.
For example if they have a starship config, I want it to show".

Today tabs are not objects. `App::tab_count` chunks a project's live
top-level panes four at a time, in tree order (`PANES_PER_TAB`,
`dispatch/src/app.rs`). There is no way to start a tab, and closing a pane
reflows every tab after it. The only panes are agents launched from harness
files (`claude`, `codex`, `agy`, `opencode`). Every pane inherits the
daemon's environment, including the outer terminal's `TERM` and identity
variables, which Dispatch's own emulator does not match.

### Not in this slice

- **The rest of the keybinding system.** This slice adds the tab mode and
  its direct `Alt` keys. Everything else stays on `^a` until slice D moves
  panes, scrolling and sessions to the same model and makes keys
  configurable.
- **Splits.** A tab tiles its panes with the existing balanced grid
  (`dispatch-layout`). There are no user-drawn splits.
- **Tabs that outlive the daemon.** Tabs live as long as the panes do.
  Nothing is written to disk.
- **Dragging panes or tabs with the mouse.** Clicking is enough for now.

## Decisions taken with the user

- **Tabs are owned**, not derived. You add one with `+` or a key, and new
  panes open into the tab you are on. A tab tiles at most **4** panes you
  placed; opening a fifth into a full tab starts a new tab. Closing a pane
  never moves panes on other tabs.
- **The daemon keeps tabs**, next to the panes, so they survive detach and
  reattach and look the same from every client attached to that machine.
- **A tab is kept as a daemon object** with its own id and member list. The
  daemon sends a whole-project snapshot after every change (approach 1 of 3).
- **A new tab starts from the picker, with Shell first**, so Enter gives a
  shell and one arrow gives an agent. Cancelling leaves no empty tab.
- **Tabs show a name and no number.** The name is the first pane's title
  until you rename the tab.
- **Operations**: add, rename, move a pane between tabs, close a whole tab
  (after a y/n prompt), reorder tabs, and click the tab row.
- **Keys follow zellij's model, not one prefix for everything**: `Ctrl t`
  enters a tab mode, and a few `Alt` keys act directly. That is the user's
  own zellij setup, and Dispatch will not run inside zellij, so nothing
  outside Dispatch competes for these keys.
- **Shells behave the way herdr's do**: `$SHELL`, started as a login shell
  on macOS and an interactive non-login shell elsewhere, with Dispatch's own
  `TERM` in every pane.

## The tab model

A new module, `dispatch-core/src/tabs.rs`, holds the model. The daemon uses
it for its projects; a standalone client uses the same code for its own.

```rust
id_type!(TabId);                     // a Uuid, like PaneId

pub const TAB_CAPACITY: usize = 4;

pub struct Tab {
    pub id: TabId,
    /// Set by renaming; `None` means "show the first pane's title".
    pub name: Option<String>,
    /// Members, in tiling order.
    pub panes: Vec<PaneId>,
}

/// One project's tabs, in row order.
pub struct ProjectTabs { tabs: Vec<Tab> }

pub enum Placement {
    /// No preference: the last tab if it has room, else a new tab at the
    /// end. It never back-fills an earlier tab, so it groups panes the way
    /// an older client's four-at-a-time chunking does.
    Auto,
    /// Into this tab; if it is full, a new tab straight after it.
    Into { tab: TabId },
    /// A new tab straight after this one (at the end when `None`).
    NewAfter { tab: Option<TabId> },
    /// A placement from a newer peer; treated as `Auto`.
    Unknown,
}
```

The rules:

1. **Membership.** Every top-level pane (no parent) belongs to exactly one
   tab. A delegated subagent never does. A subagent opened into the grid
   (`^a s`) is drawn on its parent's tab, straight after the parent, exactly
   as today. So a tab can tile more than four panes while subagents are
   open; the cap is on panes you place.
2. **Placing a new pane** follows its `Placement`. "Full" means four
   *running* members.
3. **Leaving.** A pane leaves its tab when it closes or its process exits.
   Nothing else moves. A tab with no running member is removed from the
   row. An exited pane stays in the sidebar, where selecting it shows what
   it printed, as today.
4. **Names.** `rename(tab, name)` trims the name, strips control
   characters and keeps at most 64 characters, so a snapshot stays small;
   an empty result sets `None`. The row shows at most 16 columns of a name,
   by display width.
5. **Moving a pane** to the previous or next tab places it at the end of
   that tab. Moving past the last tab creates a new tab at the end. Moving
   into a full tab is refused (`TabError::Full`). The first tab has no
   previous tab, so moving left from it is refused too
   (`TabError::NoSuchTab`). A pane alone on its tab that is asked to move
   past the last tab, and is already on the last tab, stays where it is
   rather than trading one tab of its own for another; asked to go after a
   different tab, or asked to go past the last tab while it is not already
   there, it moves.
6. **Closing a tab** closes every running member. The tab then leaves by
   rule 3.
7. **Reordering** moves a tab to an index, clamped to the row.

Every operation returns `Result<_, TabError>`, where `TabError` is one of
`Full`, `NoSuchTab` or `NoSuchPane`. It never panics.

## Protocol

The protocol already encodes struct fields by name and marks every field
added after 1.0 `#[serde(default)]`, and its own rule is to bump `VERSION`
only for a change an older peer cannot ignore. Nothing here is such a
change, so **`VERSION` stays 1.1**.

Client to daemon:

- `SpawnPane` gains `#[serde(default)] place: Placement`. `Auto` is the
  default, which is also what an older client gets by leaving the field out.
- `MovePane { pane, to: Placement }`
- `RenameTab { tab, name: String }`
- `CloseTab { tab }`
- `MoveTab { tab, index: usize }`

Daemon to client:

- `Tabs { project, tabs: Vec<Tab> }`, where `Tab { id, name, panes }` is
  the model's own type. It goes to every subscriber after any change to
  that project's tabs.

The subscribe replay sends one `Tabs` for **every** open project, even an
empty one, after that project's `PaneSpawned` messages. The panes a
snapshot names are therefore always already known.

When the daemon opens a project, it also sends that project's (empty)
`Tabs` straight after `ProjectOpened`, so a project opened after the replay
reads as one that keeps tabs from the start.

A refused operation gets `Error { error: ProtocolError::Other(text) }`, or
`NoSuchPane`. The client shows it in the status row.

`Placement`, `Tab` and `TabId` live in `dispatch-core` and derive
`Serialize`/`Deserialize`, like `PaneId` and `PaneStatus`. `Placement` is
tagged like the protocol's other nested enums, with `#[serde(other)]` on
`Unknown`, so a newer peer's placement cannot fail an older daemon's
frame.

## Who owns a project's tabs

- **The daemon**, for its projects. It changes them when a pane spawns (by
  `place`), moves, closes or exits, and when a project closes. A delegated
  pane is never placed. After each change it broadcasts `Tabs`.
- **A standalone client**, for its own panes. It applies the same
  operations directly, with no messages.

The client keeps one `ProjectTabs` per project in `AppState`, filled
either by snapshots or by its own operations. Drawing, keys and the mouse
read only that, so they cannot tell the two cases apart.

### Older peers

- **A new client with an old daemon.** A daemon that has sent no `Tabs` on a
  connection does not have tabs. For that machine's projects the client
  keeps today's derived four-per-tab chunking. A tab command there puts
  "this machine's Dispatch needs upgrading for tabs" in the status row and
  sends nothing — including a new-tab command, which checks this directly
  rather than through the tab on screen, since a project with no tabs kept
  has none to check there.
- **An old client with a new daemon.** It decodes `Tabs` as `Unknown` and
  ignores it, and keeps chunking. `SpawnPane` without `place` means `Auto`.

## The tab row

Row 0, right of the `D I S P A T C H` corner:

- **Label.** Each tab is drawn as ` ⠹ fix login bug `: slice B's rollup glyph
  when there is one, then the name. There is no number. The name is the
  tab's own if it has one; otherwise the first running member's title;
  otherwise its harness's display name.
- **Current tab.** It keeps slice A's faded-background tint and slice B's
  sliding tint.
- **`+`.** A faded ` + ` follows the last tab.
- **Overflow.** When the labels do not fit, the row scrolls so the current
  tab is fully visible. `‹` or `›` marks a clipped end. On a narrow row the
  `+` is pinned to the right edge rather than following the last label, and
  the tabs and marks draw only up to it, so it can never be overwritten; a
  row too narrow even for ` + ` draws no `+` at all.
- **Clicks.** A click on a tab focuses it, as selecting it does. A click on
  `+` opens the picker for a new tab after the current one. A click on `‹`
  or `›` switches to the previous or next tab. The row always scrolls to
  keep the current tab in view, so switching is what reveals the next tab
  rather than a separate scroll position the next frame would undo.
- **Status row.** It keeps `tab 2/3` when there is more than one tab:
  with no numbers in the row, it says what a digit will pick. Its key help
  gains `Ctrl t tabs`. While tab mode is on, the whole row lists the mode's
  keys instead (see Keys), with any status message shown ahead of them.

**Which tab is shown** still follows the focus and is per client, as today.
Choosing a tab (by click, digit or `Tab`) focuses the pane this client last
focused on it, or else its first member.

## Keys

### Tab mode

`Ctrl t` enters tab mode. The status row then reads
`TAB  n new  r rename  x close  ←→ switch  [ ] move pane  i o move tab  1-9 go  Esc done`,
and until the mode ends, keys go to Dispatch, not to the pane. Entering the
mode clears any old status message. A message set while the mode is on —
a refusal from a key that keeps the mode, such as `no tab to the left` —
shows between `TAB` and the key list, as
`TAB  no tab to the left  n new  r rename  …`, in the same highlighted style:

| Key | Action | Mode afterwards |
|---|---|---|
| `n` | new tab: open the picker; the pane opens with `NewAfter(current)` | ends |
| `r` | rename this tab: a one-line prompt holding the current name. Enter saves, empty means automatic, Esc cancels | ends |
| `x` | close this tab. It asks first: `Close "fix login bug" and its 3 panes? y/n` | ends |
| `←` `→`, `h` `l` | previous or next tab | stays, to step through tabs |
| `[` `]` | move the focused pane to the previous or next tab; `]` on the last tab makes a new one | stays |
| `i` `o` | move this tab left or right | stays |
| `1`–`9` | go to that tab by position | ends |
| `Tab` | the tab this client was on before | ends |
| `Esc`, `Enter` | leave tab mode | ends |
| `Ctrl t` | send `Ctrl t` itself to the focused pane, since Claude Code and fzf both use it | ends |

Any other key is ignored and the mode stays on. A mouse click ends the mode,
and then does what it would have done anyway.

### Direct keys

These work at any time, with no mode:

| Key | Action |
|---|---|
| `Alt n` | new pane in this tab: open the picker; the pane opens with `Into(current)` |
| `Alt i` / `Alt o` | move this tab left or right |
| `Alt ←` `Alt →`, `Alt h` `Alt l` | move focus left or right; at the grid's edge, go to the neighbouring tab as choosing it would (the pane last used there, else its first), and do nothing past the first or last tab |
| `Alt ↑` `Alt ↓`, `Alt j` `Alt k` | move focus up or down |

### Unchanged

`^a n` (new pane in this tab), `^a 1`–`9` and `^a Tab` keep working as
today, and so do the other `^a` commands. Slice D decides what happens to
them.

### What it costs

A key Dispatch takes never reaches the program in the pane. The shells'
defaults lose `Alt h` (zsh `run-help`), `Alt l` (lowercase word), `Alt n`
(history search) and `Alt ←`/`Alt →` (word motion in some setups); the
user's zellij already takes them. `Ctrl t` is still reachable by pressing
it twice. None of these needs the kitty keyboard protocol: `Ctrl t`
arrives as 0x14, `Alt` + a letter arrives as `ESC` + the letter, and `Alt`
+ an arrow arrives as `CSI 1;3 D`-style, all of which crossterm already
decodes.

The router in `dispatch-tui/src/input.rs` gains the mode: today it knows
"prefix armed" or not, and it adds "tab mode". `Action` gains the new tab
commands.

The prompt and the confirmation reuse the existing prompt widget
(`dispatch-tui/src/prompt.rs`).

## Motion

No new kinds of motion: tabs reuse slice B's.

- A new tab gets the sliding tint, and its first pane gets the open effect.
- Moving a pane to another tab moves the focus with it, so the tint slides
  to that tab and the pane opens there.
- Closing a tab retracts its panes as closing panes do. The row then closes
  up, and the tint slides to whichever tab now holds the focus.
- With `[interface] motion = false`, each of these goes straight to its end
  state.

## Shell panes

### The `shell` entry

`HarnessRegistry` always contains a `shell` harness. It is built in code,
not read from the harnesses folder:

- display name `Shell`, icon `\u{f120}` (nf-fa-terminal);
- no `[task]` form, so nothing can delegate to a shell;
- no `[status]` rules, so its state comes from output and the bell alone;
- no settings, so the harness manager does not list it.

A harness file whose id is `shell` replaces the built-in entry entirely, as
any user harness file does.

The picker lists `shell` first and pre-selects it. For a project on this
machine it shows the local shell's name (`Shell · zsh`); for a remote
project it shows just `Shell`. Until the shell sets a title (fish and
Starship both do), the pane's title is `Shell`.

### Which shell, and how it starts

The machine that runs the pane decides, which is the daemon or a
standalone client. A remote project therefore gets that machine's shell and
dotfiles. Resolution lives in `dispatch-os` (`shell.rs`), because it
differs by platform:

1. `[shell] command` from `config.toml`, if set.
2. `$SHELL`, if it names an executable file.
3. The user's login record (`getpwuid`), if it names an executable file.
4. `/bin/sh`.

On Windows, `$SHELL` and the login record do not apply: it is `pwsh` if it
is on `PATH`, else `powershell`.

**Login mode.** `login = "auto"` (the default) runs the shell with `-l` on
macOS and without it elsewhere. That is how Terminal.app and most Linux
terminals start shells, so the rc file that runs `starship init` (or
oh-my-zsh, p10k, …) runs here too. `always` and `never` force it either
way. Windows ignores it. Nothing is Starship-specific: the shell is
interactive because it is on a terminal, and it sources whatever the user's
setup sources.

The working directory is the project's root.

### `config.toml`

```toml
[shell]
command = "/usr/bin/fish"   # default: $SHELL, then the login record, then /bin/sh
args = []                   # added after any -l
login = "auto"              # auto | always | never
```

`Config` gains `shell: ShellConfig`, defaulting to all of the above.
`unknown_keys` learns `shell.command`, `shell.args` and `shell.login`. A
`login` value other than the three is a load error, like any bad value.

### Every pane's environment

`dispatch_pty::Pty::spawn` applies this to every pane, agents included,
before the harness's own `env`:

- Sets `TERM=xterm-256color`, `COLORTERM=truecolor`,
  `TERM_PROGRAM=dispatch` and `TERM_PROGRAM_VERSION=<crate version>`.
- Removes the outer terminal's identity variables: `TERM_SESSION_ID`,
  `ITERM_SESSION_ID`, `LC_TERMINAL`, `LC_TERMINAL_VERSION`,
  `KITTY_WINDOW_ID`, `KITTY_PID`, `KITTY_LISTEN_ON`, `WEZTERM_PANE`,
  `WEZTERM_UNIX_SOCKET`, `ALACRITTY_WINDOW_ID`, `ALACRITTY_SOCKET`,
  `WT_SESSION`, `WT_PROFILE_ID`, `VTE_VERSION`, `KONSOLE_VERSION`,
  `KONSOLE_DBUS_SESSION`, `GHOSTTY_RESOURCES_DIR`, `GHOSTTY_BIN_DIR`,
  `KITTY_INSTALLATION_DIR`, `WEZTERM_EXECUTABLE` — and those of a
  multiplexer Dispatch was started inside: `TMUX`, `TMUX_PANE`, `STY`,
  `ZELLIJ`, `ZELLIJ_SESSION_NAME`, `ZELLIJ_PANE_ID`.
- Then applies `launch.env`, so a harness can still set its own `TERM`.

Dispatch draws each pane with its own emulator. Advertising the outer
terminal makes programs send it that terminal's private sequences, and
over SSH a remote side without matching terminfo mis-draws.

## Code layout

`dispatch/src/app.rs` is already 8,800 lines. New client code goes
elsewhere:

- `dispatch-core/src/tabs.rs`: the model and its operations.
- `dispatch-os/src/shell.rs`: shell resolution and the login rule.
- `dispatch-config`: `ShellConfig` and the built-in `shell` harness.
- `dispatch-pty/src/session.rs`: the pane environment.
- `dispatch-tui/src/input.rs`: tab mode and the direct `Alt` keys.
- `dispatch/src/tabs.rs`: drawing the tab row, hit-testing it, and turning
  its clicks into tab commands.

`app.rs` then only wires these in: `tab_count`, `current_tab`,
`panes_on_tab` and `select_tab` read `ProjectTabs` instead of chunking,
with the chunking kept only for the old-daemon fallback.

## Failure cases

| Case | Behaviour |
|---|---|
| The shell command is missing or not executable | The spawn fails like any harness's: `failed to start shell: …` in the status row |
| `-l` given to a shell that rejects it | The pane exits at once with the shell's own error; `login = "never"` fixes it |
| `[` in tab mode on the first tab | Refused: `no tab to the left` |
| Moving a pane into a full tab | Refused in the client, with `that tab is full (4 panes)`; the daemon refuses too, so two clients racing for the last slot cannot both win |
| A tab command on a project whose daemon is older | `this machine's Dispatch needs upgrading for tabs`; nothing is sent |
| An operation names a tab or pane another client just removed | The daemon replies with an error; the client shows it once and carries on |
| A pane exits while being moved | The move fails with `NoSuchPane`, or the pane leaves its new tab by rule 3; either way the next snapshot is right |
| A snapshot names a pane this client does not have yet | Cannot happen: the replay order puts panes first, and a live `PaneSpawned` always precedes the `Tabs` that places it. The client skips unknown ids defensively |
| The rename prompt is given only spaces | The tab goes back to its automatic name |
| Every pane in a tab exits | The tab leaves the row; its panes stay in the sidebar |
| A malformed `[shell]` | `config.toml` fails to load with the key named, as for any section |

## Testing

- **`dispatch-core::tabs`.** Unit tests for each rule:
  - placing with `Auto`, `Into` (room and full) and `NewAfter`;
  - leaving, and a tab disappearing when nothing in it is running;
  - moving to the previous and next tab, into a full tab, and past the end;
  - renaming, clearing a name, and stripping control characters;
  - closing and reordering, including clamping;
  - the cap counting only running panes;
  - a subagent never being placed.
- **`dispatch-proto`.** Round-trips of the new messages. A `SpawnPane`
  encoded without `place` decodes as `Auto`. An enum without `Tabs`
  decodes it as `Unknown`.
- **Daemon.** A snapshot goes out after each operation. The subscribe
  replay sends `Tabs` for every project after its panes. `CloseTab` closes
  exactly its running members. A move into a full tab is refused with an
  error. An exit removes the pane from its tab.
- **Client (`dispatch`).**
  - Labels show names with no numbers.
  - Clicking `+`, a tab, `‹` and `›`.
  - Tab mode: each key's action, which keys keep the mode on and which
    end it, other keys ignored, `Ctrl t` twice sending `Ctrl t` to the
    pane, a click ending the mode, and the status row listing the keys.
  - The direct `Alt` keys, including focus crossing to the neighbouring
    tab at the grid's edge and stopping at the first and last tab.
  - The rename prompt (save, clear, cancel) and the close confirmation
    (y, n).
  - Scrolling on overflow keeps the current tab visible.
  - Standalone tabs.
  - The fallback when no `Tabs` arrives.
  - Choosing a tab refocuses the pane last used there.
- **Shell.**
  - The resolution order, with the environment and login record injected.
  - The login rule for each platform and mode.
  - `[shell]` parsing and unknown keys.
  - A Unix pty test runs `env` and checks that `TERM` and `COLORTERM` are
    set, the identity variables are gone, and a harness's own `env` wins.
- **End to end.** Standalone Dispatch opens a shell pane, types a command,
  and sees its output drawn. The picker's shared helper picks the
  `aaashell` test harness rather than the built-in `Shell`, because the
  built-in `Shell` has no `[task]` form and the same file's delegation
  tests need one that does.

## Documentation

The README gains:

- tabs: adding, naming, moving, closing and reordering them, and the
  cap of four;
- tab mode and the direct `Alt` keys, and how to send `Ctrl t` to a pane;
- shell panes and the `[shell]` section;
- the pane environment.

This is part of the work, not a follow-up.
