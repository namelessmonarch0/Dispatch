# B — Live status and motion

Status: approved design, not yet implemented.
Date: 2026-09-25.
Second of four UI slices: A visual refresh (merged), B this, C user tabs and
shell windows, D keybindings.
Follows: `docs/superpowers/specs/2026-09-24-visual-refresh-design.md`.

## The problem

The sidebar's status glyph never changes while an agent works.
`PaneStatus` has `Starting`, `Running`, `Idle` and `Exited`, but nothing in
the codebase ever sets `Running` or `Idle`: a pane sits on `Starting` until
its process exits. The user cannot tell which agent is busy, which is
waiting on them, and which finished while they were looking elsewhere.

The interface also has no motion: every change — focus, a pane opening, a
tab switch — is a jump.

### Not in this slice

- **Hooks or plugins inside agents.** herdr, which solved this problem at
  scale, found that Claude Code and Codex hooks "miss permission approval
  results, escape interrupts, or other transitions" and removed them as a
  source of state; this slice reads the screen instead, as herdr does.
- **Remote rule updates**, an `explain` command, per-agent process
  detection. Dispatch already knows each pane's harness.
- **Status in the daemon or on the wire.** Detection runs in the client;
  the protocol does not change.
- **User-created tabs** — C. The tab-switch animation works on whatever tabs
  exist.

## Decisions taken with the user

- **Detect state the way herdr does, with no hooks**: per-harness screen and
  title rules, plus output activity and the bell, in the client.
- **Rules live in each harness's TOML** under `[status]`, with built-in
  defaults for `claude`, `codex`, `opencode` and `agy` that a `[status]`
  section in the user's file replaces. Adding the `regex` crate for them is
  accepted.
- **States**: working, idle, blocked, and "done until seen".
- **All four kinds of motion**: a working spinner, an attention pulse, focus
  and selection easing, and open/close/tab-switch transitions — with a
  setting to turn motion off.

## What herdr taught us

Read from a clone of `ogulcancelik/herdr` (Apache-2.0, as Dispatch is):

1. State for Claude Code and Codex comes from **screen manifests**: per-agent
   rule files matched against the bottom of the live buffer, the terminal
   title, and the `OSC 9;4` progress sequence, highest priority winning.
2. Hooks are trusted only where they cover the whole lifecycle, and never
   mixed with screen rules for the same pane.
3. **Blocked is strict**: only a known approval or question UI counts; an
   unknown prompt falls back to idle.
4. **Working → idle is damped** (confirmed over up to 700 ms), so a pause
   between output chunks does not flicker.
5. **Done** is a finished agent you have not looked at yet; the sidebar's
   attention order is blocked > done > working > idle.

The built-in rules below are adapted from herdr's `claude.toml`,
`codex.toml`, `opencode.toml` and `antigravity.toml` manifests, credited in
the file that holds them.

## States

`PaneStatus` gains one variant:

```rust
pub enum PaneStatus {
    Starting,
    /// The agent is working.
    Running,
    /// The agent is waiting on the user.
    Idle,
    /// The agent is waiting on a decision only the user can make — a
    /// permission prompt, a question.
    Blocked,
    Exited(i32),
}
```

`Running` and `Idle` finally mean what their docs always said. Only the
client sets `Running`, `Idle` and `Blocked`; the daemon still sends only
`Exited`, which overrides everything and ends detection for that pane. The
enum travels inside `PaneUpdate::Status`, but a daemon never sends
`Blocked`, so no older client ever meets it.

**Done** is not a status. `AppState` gains `unseen: HashSet<PaneId>`, with
`mark_unseen`, `is_unseen`, and `mark_seen`. `AppState::focus` itself leaves
the mark alone; the client calls `mark_seen` for the focused pane on each
activity poll, so the mark clears shortly after focus rather than at the
keystroke. It is client state, like the folded rows.

## Signals

Per pane, in the client — which already keeps an emulator for every pane,
remote ones included:

- **Output activity**: when bytes last arrived. Output within 150 ms of a key,
  paste or pointer event this client sent to that pane is echo and does not
  count; nor does output within 500 ms of this client resizing the pane,
  which is the program repainting to fit. A resize follows every pane
  opening or closing beside it and every change of terminal size, and the
  longer window covers a remote pane's round trip through the daemon.
- **Title**: the raw title the program last set, spinner mark included. The
  client already strips the mark for display; it now keeps the raw string
  beside it.
- **Progress**: the payload of the last `OSC 9;4` sequence (`4;0`, `4;1;40`…).
- **Bell**: a `BEL` byte outside any escape sequence.

`dispatch_pty::TitleScanner` grows into a signal scanner:

```rust
pub struct Signals {
    /// The last title completed in the chunk.
    pub title: Option<String>,
    /// The last `OSC 9;4` payload completed in the chunk, after `9;`.
    pub progress: Option<String>,
    /// Whether a bare `BEL` rang.
    pub bell: bool,
}

impl TitleScanner {
    pub fn scan_signals(&mut self, bytes: &[u8]) -> Signals;
    /// Unchanged: the title part of `scan_signals`.
    pub fn scan(&mut self, bytes: &[u8]) -> Option<String>;
}
```

A `BEL` that terminates an OSC is a terminator, not a bell; one inside
`ESC [` … is not reachable (CSI has no BEL). Chunk boundaries are handled as
titles already are.

## Rules

### Format

In a harness file:

```toml
[[status.rules]]
state = "blocked"                        # working | idle | blocked
region = "bottom:12"                     # title | progress | bottom:N | screen
contains = ["do you want to proceed?"]   # every one must appear
any = ["1. yes", "2. no"]                # at least one must appear
not = ["esc to interrupt"]               # none may appear
regex = ['^\s*❯?\s*1\.\s*yes\b']         # at least one matches some line
priority = 850
```

- `region`: `title` is the raw title; `progress` the last `OSC 9;4` payload;
  `bottom:N` the last N non-blank lines of the live screen; `screen` every
  line of it.
- `contains`, `any` and `not` compare case-insensitively against the region's
  text; `regex` is tested against each line of the region separately and
  is case-sensitive unless it says `(?i)`.
- A rule matches when every condition it states holds; a rule stating none
  never matches.
- Rules are evaluated highest `priority` first; the first match decides.
  Equal priorities keep file order.

### Where rules come from

`HarnessDef` gains `status: Option<StatusDef>` (`#[serde(default)]`), with
`StatusDef { rules: Vec<RuleDef> }`. At registry load each harness gets
compiled rules:

1. its own `[status]` section, when the file has one — even an empty
   `rules = []`, which means "activity only";
2. otherwise the built-in rules for its id, when there are some;
3. otherwise none — activity alone decides.

Dispatch never rewrites an existing harness file, so users who already have
`claude.toml` get the built-in rules through (2) without editing anything,
exactly as a file with no `icon` falls back on its id.

A rule whose regex does not compile, whose region or state is unknown, or
which states no condition is logged with the harness id and skipped; the
harness still loads. The built-ins live in
`crates/dispatch-config/src/status/builtin.rs` as TOML strings parsed at
load, so they are the same format a user copies into their own file.

### Built-in rules

Adapted from herdr's manifests; exact patterns in the plan. In outline:

| Harness | Blocked | Working | Idle |
|---|---|---|---|
| `claude` | permission prompt (`do you want to proceed?` with numbered yes/no); `esc to cancel` with `enter to confirm`/`enter to select` forms | title starts with a braille or `◐◑◒◓` spinner; `esc to interrupt` in the bottom lines | title starts with `✳ `; `OSC 9;4;0` |
| `codex` | `Action Required`; `allow command?`; `[y/n]`; trust-directory prompt | a braille spinner glyph standing alone; `… to interrupt)` timers | — (falls to idle) |
| `opencode` | `permission required` | `esc to interrupt`/`esc interrupt`; `■■■■`/`⬝⬝⬝⬝` progress bar | — |
| `agy` | `requesting permission for:` | a braille spinner before an `…ing` word | — |

## The verdict

A small state machine per pane, in `dispatch-tui/src/activity.rs`:

```rust
pub struct Tracker { /* rules, timings, last signals, current state */ }

impl Tracker {
    pub fn new(rules: Arc<StatusRules>) -> Self;
    /// We sent the pane a keystroke, paste or pointer event.
    pub fn input(&mut self, now: Instant);
    /// We told the pane its new size; its repaint within 500 ms is ignored.
    pub fn resized(&mut self, now: Instant);
    /// Output arrived; within 150 ms of our own input it is echo and ignored.
    pub fn output(&mut self, now: Instant);
    pub fn signals(&mut self, signals: &Signals);
    /// Re-evaluates against the live screen; returns a change, if any.
    pub fn evaluate(&mut self, now: Instant, screen: &[String]) -> Option<Verdict>;
}

pub enum Verdict { Working, Idle, Blocked }
```

Each evaluation, the raw verdict is:

1. a matching **blocked** rule → `Blocked`;
2. otherwise a matching **working** rule, or non-echo output in the last
   second → `Working`;
3. otherwise (a matching idle rule, or nothing) → `Idle`.

Then, stabilised:

- `Working` → `Idle` is reported only once the raw verdict has been `Idle`
  continuously for **700 ms**.
- Entering or leaving `Blocked`, and `Idle` → `Working`, are reported at once.

The app evaluates a pane after an output burst — at most every **100 ms** per
pane — and every pane on a **250 ms** tick, so an idle verdict can confirm
with nothing arriving. While a pane is scrolled back its screen is not the
live one, so it is not evaluated: it keeps its last state.

A **subagent** — a pane with a parent — always runs its harness's one-shot
`[task]` form, whose output says nothing about whether it is still at work:
`claude -p` prints nothing until its answer, and `codex exec` goes quiet
between model calls. It is `Running` from adoption until it exits, `Blocked`
while its rules say so, and never `Idle`, so going quiet never marks it
done. Its exit is shown as any pane's is.

### Done, and the startup grace

- A pane whose status goes `Running` → `Idle` while it is not the focused
  pane is marked unseen.
- A bell from a pane that is not focused marks it unseen, whatever its state.
- Focusing a pane clears its mark — on the activity poll that follows, not
  the keystroke itself, since that poll is what touches the mark.
- **Grace**: for the first **3 s** after a pane is adopted nothing marks it
  unseen. A reattaching client is replayed every pane's recent output, and
  without the grace every pane would come back "done".

## Presentation

### Glyphs

In the sidebar's state column (one cell, the blank from A before the border):

| State | Glyph | Colour |
|---|---|---|
| Starting | `\u{f252}` hourglass | yellow |
| Working | braille spinner `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`; `\u{f04b}` play with motion off | green |
| Idle | `\u{f04c}` pause | `faded` |
| Blocked | `\u{f071}` warning | yellow, bold |
| Done (idle, unseen) | `\u{f058}` check-circle | `accent` |
| Exited(0) | `\u{f00c}` check | `faded` |
| Exited(n) | `\u{f00d}` cross | red |
| Closed | `\u{f05e}` ban | `faded` |

### Rollups

Attention order: blocked > done > working > idle.

- A **folded project** row draws the most urgent state among its panes and
  their subagents in its own state column; nothing when all are idle,
  starting or exited. An open project draws none — its panes are right
  there.
- Each **tab** label is prefixed with its panes' most urgent state glyph and
  a space — `⠋ 1 claude`, `\u{f071} 2 codex` — or nothing for an all-idle
  tab. The glyph keeps its state colour on the tab's own background.
- Machine names get no rollup.

### The status row

When any pane is blocked and no overlay is open, the status row shows
`N waiting on you`, beside the existing delegation reminder, so a blocked
pane on another tab or in a folded project is still noticed. Both reminders
come after a disconnect notice or status message and ahead of the key help,
which alone runs past eighty columns: whatever follows it is cut off on
common terminal widths.

## Motion

### Engine

`dispatch-tui/src/motion.rs`:

```rust
pub struct Tween { start: Instant, duration: Duration }

impl Tween {
    pub fn new(start: Instant, duration: Duration) -> Self;
    /// 0.0 at start, 1.0 at and after the end, eased out (cubic).
    pub fn progress(&self, now: Instant) -> f32;
    pub fn done(&self, now: Instant) -> bool;
}
```

`Theme` keeps its palette and depth, and gains
`blend(&self, from: Rgb, to: Rgb, t: f32) -> Color`, mixing then converting
at the theme's depth (so 256-colour terminals step through the nearest
entries). It also exposes the RGB behind each role so animations can mix
between roles, and `tween(&self, from: Role, to: Role, t: f32) -> Color`,
which at either end is exactly the colour the role is drawn in at rest —
in 256 colours the accent at rest is palette slot 5 itself, which no blend
reaches. A fill rising out of the background (a pulse, a cross-fading
tint) paints nothing until it has visibly left it: the background at rest
is the terminal's own, and the palette's is only a guess at it.

The app keeps its tweens in one place (`App::motion`), keyed by what they
animate; starting a tween on something already animating replaces the old
one, continuing from where it had got to, so fast focus changes never queue.

### Clock

`App` reads time through `clock: Box<dyn Fn() -> Instant>` — `Instant::now` in
the binary; tests supply one they advance by hand, so animations and the
detection timings (150 ms echo, 500 ms repaint, 1 s activity, 700 ms damping,
3 s grace) are tested exactly, without sleeping.

### Frame pacing

The loop draws today only after input or pane output. `App` gains:

```rust
/// How long until the next frame something on screen needs.
pub fn next_frame(&self, now: Instant) -> Option<Duration>;
```

— 33 ms while any tween remains in the store, 100 ms while a spinner is on
screen, `None` otherwise. A tween that has finished but not yet been swept
up still asks: the frame that sweeps it is the one that shows its end, and
a closed pane's tile holds the grid until that frame is drawn. `main.rs`
polls for input no longer than that and draws when it passes. A Dispatch
with nothing moving still draws nothing.

### The animations

| Action | Motion | Duration |
|---|---|---|
| Working | the spinner glyph (sidebar and tab) advances one braille frame per 100 ms, derived from the clock so all spinners are in step | continuous |
| Attention (a pane turns blocked, or is marked unseen) | its sidebar row's background pulses three times between the background and a strong accent mix (background 55% toward accent), then settles; the glyph stays. Only an unfocused pane pulses; focusing it while it pulses stops the pulse at once — it is what the pulse was for. | 1.2 s |
| Focus moves | the new pane's border eases `faded` → `accent`, the old one's `accent` → `faded`, each starting from what the previous frame showed as focused rather than the state read fresh, so an ease already under way continues instead of restarting; the sidebar's focus tint glides row by row from the old row to the new when both are drawn in one section, and cross-fades (old out, new in) otherwise. A glide replaced mid-way starts from the row the previous one was heading to. | 150 ms |
| Pane opens | its border is revealed clockwise from the top-left corner | 200 ms |
| Pane closes or exits the grid | its tile keeps its place, interior cleared, while its border retracts anticlockwise; then the grid reflows | 150 ms |
| Tab switch | the active tab's tint slides from the old tab's position and width to the new one's | 150 ms |

For the 150 ms a tile is closing, the grid keeps its old shape for drawing
and input alike — the other panes keep their tiles, so a click lands where
the eye sees it — while the closing tile itself takes no input and is gone
from the pane list. When the tween ends the grid reflows and the remaining
panes are resized, once.

The hold is stamped with the area its grid was laid out for, not the area
of whatever frame started it, so a terminal that has since resized is
noticed: the hold is dropped and the grid reflows at once rather than
holding tiles that no longer fit. A switch to another project drops the
hold the same way instead of retracting it — that project's panes going
off screen is not them closing.

### Motion off

`config.toml` gains:

```toml
[interface]
motion = false   # default true
```

The client loads `config.toml` for the first time (the daemon already does,
and ignores `[interface]`); `interface.motion` joins the known keys, so it is
not reported as unknown. With motion off every tween is complete from its
first frame, the spinner is the static play glyph, and there is no pulse —
the warning glyph and the done mark remain.

## Failure cases

| Situation | Behaviour |
|---|---|
| A rule's regex does not compile | Logged with harness id and rule index; rule skipped; harness loads |
| A harness file's `[status]` is malformed TOML | The harness fails to load as today, with the parse error |
| No rules for a harness, or a plain shell | Activity alone: working while output flows, idle a second after it stops |
| An agent's UI changes and no rule matches | Falls to activity and idle, never to blocked |
| A prompt the rules do not know | Idle, not blocked — strict, as herdr |
| Typing into a pane | Echo within 150 ms of our own input is not activity |
| A pane resized — a sibling opened or closed, the terminal resized | Its repaint within 500 ms is not activity |
| Reattaching to a daemon | Replayed output flashes working briefly; no done marks within 3 s |
| Scrolled back in a pane | Detection holds its last state |
| Terminal without 24-bit colour | Animated colours step through the 256-colour palette, and start and end on the colours drawn at rest |
| `motion = false` | Everything static, all state still shown |

## Testing

- **Scanner**: title, `OSC 9;4` payload and bare bell in one chunk and split
  across chunks; a `BEL` terminating an OSC is not a bell.
- **Rules**: parsing every field; each region; `contains`/`any`/`not`
  case-insensitive; `regex` per line; priority and file order; bad regex and
  unknown region/state skipped with the rest loaded; a file's `[status]`
  (including an empty one) replacing the built-ins; the built-ins parsing
  and matching representative screens for each of the four agents
  (recorded screen text in test fixtures).
- **Tracker**, on a hand-advanced clock: output → working; quiet 1 s →
  idle only after a further 700 ms; echo ignored; blocked immediate in and
  out; idle-rule screen with fresh output stays working.
- **App**, on a hand-advanced clock: a pane going working → idle unfocused is
  marked done, and the next activity poll after it is focused clears the
  mark; a bell marks an unfocused pane; no marks inside the 3 s grace;
  scrolled-back panes are not evaluated; the status row's "waiting on you".
- **Presentation**: each glyph and colour; spinner frame follows the clock;
  folded-project rollup order; tab prefix; no rollup on an open project.
- **Motion**: `Tween` easing and bounds; `next_frame` for tweens, spinners,
  and nothing; replacing a running tween continues from its current value;
  each animation's intermediate frame (border colour mid-ease, glide row,
  pulse strength, sweep length, tab tint position); `motion = false` makes
  every animation complete at once.
- **Config**: `[interface] motion` parsed, defaulted, and not reported
  unknown.

## Follow-ups this slice creates

- **C** can rely on shell panes getting activity-only status for free, and
  on the tab-switch animation for its user tabs.
- If an agent's UI moves on, its fix is a `[status]` edit in the user's
  harness file; shipping the fix for everyone is a change to
  `status/builtin.rs`.
