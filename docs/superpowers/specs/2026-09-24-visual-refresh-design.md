# A — Visual refresh: theme, chrome, a sidebar per machine, and branches

Status: approved design, not yet implemented.
Date: 2026-09-24.
First of four UI slices: A (this), B live status and motion, C user tabs and
shell windows, D keybindings.

## The problem

Dispatch draws correctly but reads as a prototype, and a few things are
simply wrong:

- **Nerd Font icons collide.** A project row packs its twisty, git mark and
  folder into three adjacent cells (`sidebar.rs`, `render_project`), and a
  pane's state glyph sits in the last column, flush against the frame. Most
  Nerd Font builds draw an icon wider than one cell, so each overflows into
  its neighbour.
- **Nothing names the program.** The top-left corner is the sidebar's frame.
- **Pane borders are rounded** (`app.rs`, `pane_block`) while everything else
  is square.
- **Selection is shown by inversion.** The active tab is black on grey, the
  selected project is `REVERSED`. Both shout; neither looks like the rest.
- **The sidebar is one list for every machine.** With a second machine
  attached, its projects are a subtree under a machine row, indented two
  columns, in a sidebar that does not scroll — rows past the bottom are
  simply not drawn.
- **Branches are invisible.** A project knows only whether it is a git
  repository; no pane knows where it is working.

The target look is herdr's: square edges, faded secondary text, and a faint
background tint for whatever is active.

### Not in this slice

- **Status detection and animation** — B. `Running`/`Idle` are still never
  set after this slice; A only restyles the glyphs.
- **User-created tabs, the `+` button, shell windows** — C. The tabs here are
  still the automatic four-panes-per-tab ones.
- **Keybindings** — D.
- **Clicking a tab.** Tabs are drawn, not yet a control; C makes them one.

## Decisions taken with the user

- **Colours follow the terminal.** Tints are mixed from the terminal's own
  background, foreground and accent, queried at startup, so the look suits
  any theme, light or dark. A built-in dark palette covers a terminal that
  does not answer.
- **One top row**: `D I S P A T C H` over the sidebar, the tab bar over the
  panes, always shown.
- **A section per machine**, filling the sidebar's height, each machine's
  share weighted by how many panes it has open.
- **Branches as group headers**: a project's branch faded beneath its name;
  panes on other branches gathered under their own faded branch line.
- **Branches detected per pane, on the machine running it**, from the
  foreground process's working directory — which catches worktrees without
  Dispatch having to create them.

## The theme

A new module, `dispatch-tui/src/theme.rs`.

```rust
/// Three colours read from the terminal, or the fallback's.
pub struct Palette {
    pub background: Rgb,
    pub foreground: Rgb,
    pub accent: Rgb,
}

/// What everything is drawn in, derived once from a palette.
pub struct Theme {
    /// Secondary text: branches, the app name, inactive tabs, unfocused
    /// borders, the status row.
    pub faded: Color,
    /// The row behind a selected project or a focused pane.
    pub tint: Color,
    /// The active tab's background.
    pub tab: Color,
    /// The focused pane's border.
    pub accent: Color,
}
```

| Role     | Derivation                                   |
|----------|----------------------------------------------|
| `faded`  | foreground mixed 45% toward background        |
| `tint`   | background mixed 10% toward foreground        |
| `tab`    | background mixed 30% toward accent            |
| `accent` | the accent itself                            |

Ordinary text is left as `Color::Reset`, so it stays the terminal's own
foreground. The state glyph colours stay ANSI (`Green`, `Yellow`, `Red`,
`Blue`): the terminal's palette already themes them.

`Theme::fallback()` is built from a fixed dark palette — background
`#16161e`, foreground `#c8c8d8`, accent `#b4a0f0` — and is what every test
renders with, so snapshots do not depend on the machine running them.

### Colour depth

When `COLORTERM` is `truecolor` or `24bit`, derived colours are
`Color::Rgb`. Otherwise each is quantised to the nearest entry of the
xterm-256 colour cube or grey ramp and emitted as `Color::Indexed`: a
terminal without 24-bit colour misreads RGB escapes rather than
approximating them.

### Asking the terminal

In `dispatch/src/terminal.rs`, immediately after raw mode is enabled and
before crossterm's event stream starts reading stdin, `TerminalGuard::acquire`
writes one burst:

```
ESC ] 10 ; ? BEL      foreground
ESC ] 11 ; ? BEL      background
ESC ] 4 ; 5 ; ? BEL   palette slot 5 (magenta), the accent
ESC [ c               primary device attributes
```

and reads replies for at most one second. Every terminal answers device
attributes, and answers in order, so its reply (`ESC [ ? … c`) arriving means
every colour reply that is coming has come: reading stops there rather than
waiting out the timeout on a terminal that ignores OSC queries. The bound is
generous because it is only reached when something is wrong: a reply that
arrived after the event loop started would be read as keystrokes. Replies
are `ESC ] N ; rgb:R/G/B` terminated by `BEL` or `ESC \`, with 1–4 hex
digits per channel, scaled to 8 bits. Any colour that did not arrive is taken
from the fallback palette.

The wait is a `poll` on stdin with a deadline; the parser is a pure function
over the bytes read, so it is tested without a terminal. On Windows, or when
stdin is not a terminal, no query is sent and the fallback is used. The
platform check lives in `dispatch-os`, beside the rest.

`App` holds the resulting `Theme` and hands it to every widget that draws
chrome.

## The top row

The screen gains one row at the top, always drawn:

```
D I S P A T C H                    1 claude   2 codex
┌ Projects ────────────────────────┐┌ claude ──────────────────
```

- **Left**, over the sidebar's column: the name letter-spaced in capitals,
  one column in, in `faded`.
- **Right**, over the panes: one tab per tab, labelled with its number and
  the title of its first pane, truncated to 16 columns. The active tab is
  drawn in bold on the `tab` background; the rest in `faded` on no
  background. One blank column separates tabs.

The rule that the tab row appears only once there is a second tab goes: the
row now carries the name, so it is never empty.

## Chrome

- `pane_block` becomes `BorderType::Plain`, `faded` when unfocused and
  `accent` when focused. The title on a focused pane's border is bold.
- The pickers, prompt, browser and approval box are already square; their
  borders take `faded`.
- The status row keeps its content and is drawn in `faded`. The `PREFIX`
  badge moves from black-on-yellow to bold on `tab`.

## The sidebar

`sidebar::WIDTH` goes from 32 to 34: the gaps below cost two columns, and the
titles keep the room they had.

### Rows

Every icon is followed by a blank column. The project row drops its
open/shut folder icon — the twisty already says whether it is open — and
shows one icon: the git mark for a repository, a folder for a plain
directory. Columns, counted from the row's start:

```
Project    ▾ ⎇ Dispatch          twisty @0, icon @2, name @4
Branch         main              @4, faded
Pane           ▸ 󰚩 refactor   ▶  twisty @4, harness icon @6, title @8
Subagent         󰚩 tests      ▶  everything two further in
```

- The state glyph moves one column in from the frame, leaving a blank
  between it and the border. A title is truncated before the blank that
  precedes the glyph.
- **Focus** is the `tint` background across the whole row plus a bold
  title; the `▌` marker goes. A **selected project** gets the same, in place
  of `REVERSED`. The tint starts at the row's own indent, so it never paints
  over another section.
- A closed pane kept as a tombstone is drawn in `faded`.
- `write` and `truncate` measure display width (`unicode-width`, already a
  workspace dependency) rather than counting `char`s, so a wide character
  cannot push the rest of a row out of line.

### Branch groups

Only for a project whose `branch` is known. Beneath the project row:

1. the project's own branch, in `faded`;
2. its top-level panes whose branch equals the project's, or is unknown;
3. for each other branch, in the order its first pane was created: that
   branch in `faded`, then its top-level panes.

Folding a project hides its panes and every other branch's line, but keeps
its own branch line: that line is part of what the project is.

A subagent is drawn under its parent whatever its own branch: the tree is
the primary structure, and grouping applies to top-level panes. A project
with no branch — a plain directory, or a daemon too old to report one —
draws no branch rows and lists its panes directly beneath, as now. A
branch row is not a control of its own: a click on one is a click on its
project.

A detached `HEAD` is written `@` plus the first seven hex digits of the
commit.

### Sections per machine

With one machine, the sidebar is one frame titled ` Projects `.

With more than one, it is one frame split into a section per machine, in
`state.devices()` order. The first machine's name is written into the top
border; every later machine's section opens with a divider carrying its
name:

```
┌ desktop ─────────────────────────┐
│ ...                              │
├ build-box ───────────────────────┤
│ ...                              │
└──────────────────────────────────┘
```

An unreachable machine's name is drawn in `faded` followed by
`— unreachable`, which is never truncated; the name gives way first, as it
does today. Clicking a machine's name folds its section down to the name
alone; the machine row's twisty goes, since the divider is the control.

**Heights.** The frame's inner height, less one row per divider, is shared
between the unfolded sections in proportion to

```
weight = open top-level panes on that machine + 1
```

so a machine with nothing open still has room. Each unfolded section gets at
least two rows when the height allows; rounding remainders go to the
sections with the largest fractional share. Sections always fill the frame:
blank space is spread across them rather than pooled at the bottom. The
allocation is a pure function, `section_heights(weights, folded, height)`.

**Scrolling.** `App` keeps a scroll offset per machine,
`sidebar_scroll: HashMap<DeviceId, u16>`, and the focus and selection it last
anchored to. Each frame, before drawing, every offset is clamped to its
section's content. Only when the focused pane or the selected project has
changed since the last anchor is its section's offset nudged so that row is
inside it — nudging every frame would undo the wheel as fast as it scrolled.
A mouse wheel over a section scrolls that section by one row per notch.

Hidden rows are counted on the lines that bound a section, in `faded`:

```
┌ desktop ↑ 3 ─────────────────────┐   desktop has 3 rows above what is shown
│ ...                              │
├ build-box ───────────────── ↓ 2 ─┤   desktop has 2 below; build-box none above
│ ...                              │
└──────────────────────────── ↓ 5 ─┘   build-box has 5 below
```

A section's `↑ n` follows its own name on its header line; its `↓ n` is
right-aligned on the line after it. A divider can carry both, one for each
of the sections it separates.

### One layout for drawing and clicking

`rows()` becomes `layout(state, area, scroll) -> Vec<Placed>`, where

```rust
struct Placed {
    y: u16,
    kind: PlacedKind, // Divider(DeviceId), Project(ProjectId), Branch(ProjectId),
                      // Pane(PaneId)
    indent: u16,
}
```

Both `render` and `hit_test` walk it, which keeps the existing guarantee
that a click can only land on a row that is actually drawn. `Hit` gains
nothing new: a divider answers `Hit::Device`, a branch row
`Hit::Project`. `hit_test` takes the scroll offsets as an argument.

The first machine's divider is the frame's top border, which today lies
outside the area `hit_test` accepts. When the sidebar is split, that row is
accepted too, and answers `Hit::Device` for the first machine; with one
machine it stays outside, as now.

## Branches

### Finding where a pane works

```rust
/// The working directory of whatever is in the foreground of the terminal
/// `pid` runs in — the program the user is looking at, which for a shell
/// running `cd wt && claude` is Claude in `wt`, not the shell.
pub fn dispatch_os::process::working_dir(pid: u32) -> Option<PathBuf>;
```

- **Linux**: field 8 of `/proc/<pid>/stat` is the terminal's foreground
  process group; `readlink /proc/<tpgid>/cwd`, falling back to
  `/proc/<pid>/cwd`.
- **macOS**: `proc_pidinfo(PROC_PIDTBSDINFO)` for `e_tpgid`, then
  `proc_pidinfo(PROC_PIDVNODEPATHINFO)` for its working directory, falling
  back to `pid`'s own.
- **Windows**: `None`.

Keyed on the pid alone, so `dispatch-pty` exposes nothing new and every
`#[cfg]` stays in `dispatch-os`. The process name is parsed from the right
of its closing parenthesis, since a command name may itself contain spaces
or parentheses.

### Reading the branch

```rust
/// The checked-out branch of the repository containing `dir`.
pub fn dispatch_os::git::head(dir: &Path) -> Option<String>;
```

Walks up from `dir` to the nearest `.git`. A directory is the git directory;
a file is a worktree or submodule, `gitdir: <path>`, resolved relative to the
file's own directory. Reads `HEAD`: `ref: refs/heads/<name>` yields `<name>`;
forty hex digits yield `@` and the first seven; anything else, or any I/O
error, yields `None`. It reads files only — no `git` subprocess, so nothing
depends on `git` being installed on a remote machine.

### Model

```rust
pub struct Pane    { /* … */ #[serde(default)] pub branch: Option<String> }
pub struct Project { /* … */ #[serde(default)] pub branch: Option<String> }
```

`AppState` gains `set_pane_branch` and `set_project_branch`, shaped like
`set_pane_status`.

### Protocol

Both additions land in an existing `Unknown` for a peer that predates them,
so `VERSION` stays `1.1`:

```rust
pub enum PaneUpdate {
    // …
    /// The branch the pane is working on changed.
    Branch { branch: Option<String> },
}

pub enum ServerMessage {
    // …
    /// Something about a project changed after it was opened.
    ProjectChanged { project: ProjectId, update: ProjectUpdate },
}

pub enum ProjectUpdate {
    /// The project root's checked-out branch changed.
    Branch { branch: Option<String> },
    #[serde(other)]
    Unknown,
}
```

A project's branch travels inside `ProjectOpened`, since `Project` carries
it. A pane's does not travel with `PaneSpawned`, so on subscribe the daemon
follows each pane that has a branch with a `PaneChanged { Branch }`, the way
it already follows an exited pane with its status.

### Who looks, and how often

**The daemon**, in `Session::pump_panes`, at most every two seconds: for each
live pane, `git::head(working_dir(pid)?)`; for each project,
`git::head(root)`. Only a value that differs from the recorded one is stored
and broadcast. The daemon's pane gains `branch: Option<String>`; its
projects already are `Project`s.

**The client** applies `PaneChanged { Branch }` with `set_pane_branch` and
`ProjectChanged { Branch }` with `set_project_branch`, for whichever machine
the message came from. For panes it runs itself with no daemon, it does the
same looking as the daemon does, in
`poll_panes`, on the same cadence, with the same two functions, for its own
panes and the projects on its own machine.

The cost is a handful of small file reads per pane every two seconds.
A pane whose directory cannot be read — a process between exiting and
being reaped, or one owned by another user — keeps the branch it last had
rather than being reported as on none. A directory that can be read but is
outside any repository is `None`. Nothing here is ever a message on screen.

## Failure cases

| Situation | Behaviour |
|---|---|
| Terminal ignores colour queries | Device-attributes reply ends the wait early; fallback colours |
| Terminal answers nothing at all | One-second timeout; fallback colours |
| No 24-bit colour | Tints quantised to xterm-256 |
| Daemon too old to report branches | No branch rows; panes listed as today |
| Pane process unreadable or gone | Branch `None`; the pane joins the project's own group |
| Branch changes (`git switch`) | Picked up within two seconds |
| Sidebar too short for every section's minimum | Sections take one row each, in order, until the height runs out; folded ones cost only their divider |

## Testing

- **Theme**: colour-reply parsing — `BEL` and `ESC \` terminators, 1–4 hex
  digits per channel, garbage between replies; mixing; xterm-256
  quantisation; the query loop stopping at the device-attributes reply and at
  the deadline, driven by a fake reader.
- **Sidebar**:
  - `section_heights` on equal weights, skewed weights, folded sections,
    minimums, and a height too small for all of them;
  - branch grouping order, subagents staying under their parent, a project
    without a branch;
  - the cell after every icon is blank, and the column between the state
    glyph and the border is blank;
  - hit tests on a divider, a branch row, and rows in a scrolled section;
  - the focused row kept inside its section; `↓ n` and `↑ n`.
  The existing sidebar tests are updated for the new columns and width.
- **App**: the top row holds the name and the tabs; pane corners are `┌`,
  never `╭`; the active tab is drawn on `tab` and never `REVERSED`.
- **`dispatch-os`**: `git::head` against temporary directories — a branch, a
  detached head, a worktree's `.git` file, a subdirectory walking up, no
  repository; on Linux, `working_dir` of a `sleep` spawned in a temporary
  directory.
- **Protocol**: round trips of `PaneUpdate::Branch` and `ProjectChanged`; an
  unknown `ProjectUpdate` decoding to `Unknown`.
- **Daemon**: open a project in a temporary repository, spawn a pane, rewrite
  `HEAD`; `ProjectChanged` and the pane's `PaneChanged { Branch }` arrive.

## Follow-ups this slice creates

- **B** can hang its status text (`working · claude`) and animation off the
  row layout here.
- **C** makes the tab row interactive and adds `+`; the row's left half is
  already reserved for the name.
- The accent is read from palette slot 5. If that proves wrong on common
  themes, a `theme.toml` override is the next step, not a guess at another
  slot.
