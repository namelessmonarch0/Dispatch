# Live Status and Motion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make each pane's sidebar status live — working, idle, blocked, done-until-seen — detected the way herdr does it, and give the interface motion: a working spinner, an attention pulse, focus easing, and open/close/tab-switch transitions.

**Architecture:** A signal scanner in `dispatch-pty` reports titles, `OSC 9;4` progress and bells; a rules engine in `dispatch-config` matches per-harness screen/title rules (a harness file's `[status]`, else built-ins adapted from herdr); a per-pane `Tracker` in `dispatch-tui` turns rules, output activity and time into a damped verdict; the client (`dispatch/src/app.rs`) wires it, keeps the done marks, and drives a small tween engine (`dispatch-tui::motion`) with frame pacing. No protocol change.

**Tech Stack:** Rust 2024, ratatui 0.29, crossterm 0.28, the `regex` crate (new, approved), `toml`, `serde`.

**Spec:** `docs/superpowers/specs/2026-09-25-live-status-motion-design.md`

## Global Constraints

- Platform `#[cfg]` lives only in `dispatch-os`; tests may carry `#[cfg(...)]`.
- CI runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` on `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-gnu` (the Windows target is already red on `main` for unrelated reasons in `dispatch-os/src/host.rs`/`ipc.rs`; add nothing new to it).
- Every `unsafe` block carries `// SAFETY:`; none are expected in this plan.
- `dispatch_proto::VERSION` stays `1.1`; the protocol does not change.
- The only new dependency is `regex = "1"`, in `dispatch-config`.
- Timings, verbatim from the spec: echo window **150 ms**; output counts as activity for **1 s**; working→idle settles over **700 ms**; evaluate at most every **100 ms** per pane after output and every pane every **250 ms**; startup grace **3 s**; spinner **100 ms** per frame; tween frames **33 ms**; focus/tint/tab/close **150 ms**; open **200 ms**; pulse **1.2 s**, three pulses, peak colour background **55%** toward accent.
- Glyphs, verbatim: working spinner `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` (static `\u{f04b}` with motion off); idle `\u{f04c}` faded; blocked `\u{f071}` yellow bold; done `\u{f058}` accent; starting `\u{f252}` yellow; exited(0) `\u{f00c}` faded; exited(n) `\u{f00d}` red; closed `\u{f05e}` faded. Attention order blocked > done > working > idle.
- `config.toml`: `[interface] motion = true|false`, default `true`.
- Built-in rules are adapted from herdr's manifests (`github.com/ogulcancelik/herdr`, Apache-2.0) and say so in `status/builtin.rs`.
- Comments explain why, in the surrounding code's voice. Commits: `type(scope): lowercase summary`, ending with the session's attribution trailer lines.

## Review Focus

1. **Reattaching to a daemon replays every pane's output** — no pane may come back marked done, and panes must settle to idle once the replay stops. Test in Task 6.
2. **A pane that exits mid-work** — it stays `Exited`, is never overwritten by a later verdict, and is not marked done or pulsed. Test in Task 6.
3. **Resizing the terminal while a closed pane's tile is still retracting** — the held grid must be dropped and the panes laid out for the new size at once, never drawn outside it. Test in Task 11.
4. **Closing two panes in quick succession** — both tiles retract, the grid reflows once after the last one, and nothing panics when the grid ends up empty. Test in Task 11.
5. **Rapid focus changes (a held `h`/`l` key)** — no animations queue; each change continues from where the border and tint were; the final frame shows exactly the last focus. Test in Task 10.

---

### Task 1: A signal scanner — titles, progress and bells in one pass

**Files:**
- Modify: `crates/dispatch-pty/src/title.rs`
- Modify: `crates/dispatch-pty/src/title/tests.rs`
- Modify: `crates/dispatch-pty/src/lib.rs` (re-export)

**Interfaces:**
- Produces: `dispatch_pty::Signals { pub title: Option<String>, pub progress: Option<String>, pub bell: bool }` (`Debug, Clone, Default, PartialEq, Eq`); `TitleScanner::scan_signals(&mut self, bytes: &[u8]) -> Signals`. `TitleScanner::scan` is unchanged in behaviour (it returns `scan_signals(bytes).title`).

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-pty/src/title/tests.rs`:

```rust
fn signals(bytes: &[u8]) -> Signals {
    TitleScanner::new().scan_signals(bytes)
}

#[test]
fn a_bare_bell_is_reported() {
    assert!(signals(b"done\x07").bell);
}

#[test]
fn a_bell_that_ends_a_title_is_not_a_bell() {
    let found = signals(b"\x1b]0;Claude Code\x07");

    assert_eq!(found.title.as_deref(), Some("Claude Code"));
    assert!(!found.bell, "BEL terminated the sequence, it did not ring");
}

#[test]
fn a_progress_report_is_read_after_its_nine() {
    assert_eq!(
        signals(b"\x1b]9;4;1;40\x07").progress.as_deref(),
        Some("4;1;40")
    );
    assert_eq!(
        signals(b"\x1b]9;4;0\x1b\\").progress.as_deref(),
        Some("4;0"),
        "either terminator ends it"
    );
}

#[test]
fn a_notification_is_not_progress() {
    // `OSC 9` alone is iTerm2's notification; only `9;4` is progress.
    assert_eq!(signals(b"\x1b]9;build finished\x07").progress, None);
}

#[test]
fn a_progress_report_split_across_writes_is_still_read() {
    let mut scanner = TitleScanner::new();

    assert_eq!(scanner.scan_signals(b"\x1b]9;4;").progress, None);
    assert_eq!(
        scanner.scan_signals(b"1;75\x07").progress.as_deref(),
        Some("4;1;75")
    );
}

#[test]
fn a_title_a_progress_report_and_a_bell_arrive_together() {
    let found = signals(b"\x1b]2;\xe2\xa0\x8b working\x07\x1b]9;4;3\x07ready\x07");

    assert_eq!(found.title.as_deref(), Some("\u{280b} working"));
    assert_eq!(found.progress.as_deref(), Some("4;3"));
    assert!(found.bell);
}

#[test]
fn scan_still_returns_only_the_title() {
    let mut scanner = TitleScanner::new();

    assert_eq!(
        scanner.scan(b"\x1b]9;4;1;10\x07\x1b]0;name\x07\x07").as_deref(),
        Some("name")
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-pty title::`
Expected: compile error — no `Signals`, no `scan_signals`.

- [ ] **Step 3: Implement**

In `title.rs`:

1. Rename `State::Title` to `State::Payload` with the doc `/// Inside a title's or a progress report's text.` and add a field to `TitleScanner`:

```rust
    /// What the payload being read will be, once it ends.
    collecting: Collecting,
```

initialised `collecting: Collecting::Title` in `new`, with:

```rust
/// Which kind of payload the scanner is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Collecting {
    /// A title, from `OSC 0`, `OSC 1` or `OSC 2`.
    Title,
    /// A progress report, from `OSC 9;4`.
    Progress,
}

/// What one byte finished.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Title(String),
    Progress(String),
    Bell,
}

/// What a chunk of output said besides its text.
///
/// The title is how an agent names itself and — for several — how it says it
/// is busy; progress (`OSC 9;4`) is how some report a running turn; a bare
/// bell is how a program asks to be looked at. All three are read off the
/// same bytes the screen is drawn from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signals {
    /// The last title completed in the chunk.
    pub title: Option<String>,
    /// The last `OSC 9;4` payload completed in the chunk, after `9;`: `4;0`,
    /// `4;1;40`, …
    pub progress: Option<String>,
    /// Whether a bare `BEL` rang.
    pub bell: bool,
}

/// The OSC number that carries progress reports, as `9;4;…`.
const NOTIFY: u16 = 9;
```

2. Replace `scan` and add `scan_signals`:

```rust
    /// Scans `bytes`, returning the last title completed in them.
    ///
    /// The last rather than every one: a child that sets several titles in one
    /// write means only the newest, and the caller wants what to display.
    pub fn scan(&mut self, bytes: &[u8]) -> Option<String> {
        self.scan_signals(bytes).title
    }

    /// Scans `bytes` for everything they say besides their text.
    pub fn scan_signals(&mut self, bytes: &[u8]) -> Signals {
        let mut signals = Signals::default();

        for &byte in bytes {
            match self.step(byte) {
                Some(Event::Title(title)) => signals.title = Some(title),
                Some(Event::Progress(progress)) => signals.progress = Some(progress),
                Some(Event::Bell) => signals.bell = true,
                None => {}
            }
        }

        signals
    }
```

3. `step` returns `Option<Event>`. Changes inside it:
   - `State::Text`: `if byte == ESC { self.state = State::Escape; } else if byte == BEL { return Some(Event::Bell); }`
   - `State::Command`, on `b';'`:

```rust
                b';' => {
                    self.pending.clear();
                    self.overran = false;
                    // `OSC 0` sets both icon name and title, `OSC 1` the icon
                    // name, `OSC 2` the title. All three are what a child means
                    // by "call me this". `OSC 9` is read too, for the progress
                    // reports that travel as `9;4;…`.
                    self.state = if self.numbered && self.command <= 2 {
                        self.collecting = Collecting::Title;
                        State::Payload
                    } else if self.numbered && self.command == NOTIFY {
                        self.collecting = Collecting::Progress;
                        State::Payload
                    } else {
                        State::Other
                    };
                }
```

   - `State::Payload` is the old `State::Title` arm unchanged except that `BEL` returns `self.finish()` (now `Option<Event>`).
   - `State::Terminator`: unchanged logic, returning `self.finish()`.

4. `finish` returns `Option<Event>`:

```rust
    /// Takes the finished payload, if there is one worth reporting.
    fn finish(&mut self) -> Option<Event> {
        let bytes = std::mem::take(&mut self.pending);
        let overran = std::mem::take(&mut self.overran);

        // Lossy: a child that writes a broken byte in its title should get a
        // replacement character in the sidebar, not have the title dropped.
        let text = String::from_utf8_lossy(&bytes).trim().to_string();

        if overran || text.is_empty() {
            return None;
        }

        match self.collecting {
            Collecting::Title => Some(Event::Title(text)),
            // `OSC 9` alone is a notification; only `9;4` reports progress.
            Collecting::Progress => text.starts_with("4;").then_some(Event::Progress(text)),
        }
    }
```

   (Keep whatever the existing `finish` did beyond this — read it first; if it has further checks for titles, keep them in the `Title` arm.)

5. In `lib.rs`: `pub use title::{Signals, TitleScanner};`

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-pty title::`
Expected: every title test passes, old and new.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-pty/src
git commit -m "feat(pty): read progress reports and bells along with titles"
```

---

### Task 2: Status rules — the format, compiling, matching, and the registry

**Files:**
- Modify: `Cargo.toml` (workspace dependency), `crates/dispatch-config/Cargo.toml`
- Create: `crates/dispatch-config/src/status.rs`, `crates/dispatch-config/src/status/builtin.rs`, `crates/dispatch-config/src/status/tests.rs`
- Modify: `crates/dispatch-config/src/harness.rs` (`HarnessDef::status`)
- Modify: `crates/dispatch-config/src/lib.rs` (module, `HarnessRegistry` rules)

**Interfaces:**
- Produces (`dispatch_config::status`): `RuleState { Working, Idle, Blocked }`; `RuleDef` and `StatusDef` (serde, `Default`, `PartialEq, Eq`); `StatusRules` (`Debug, Clone, Default`) with `StatusRules::compile(id: &str, def: &StatusDef) -> StatusRules`, `StatusRules::for_harness(id: &str, own: Option<&StatusDef>) -> StatusRules`, `is_empty(&self) -> bool`, `evaluate(&self, input: &StatusInput<'_>) -> Option<RuleState>`; `StatusInput<'a> { pub title: &'a str, pub progress: &'a str, pub screen: &'a [String] }` (`Clone, Copy, Default`). `HarnessDef::status: Option<StatusDef>`. `HarnessRegistry::status_rules(&self, id: &str) -> std::sync::Arc<StatusRules>`.

- [ ] **Step 1: Add the dependency**

Workspace `Cargo.toml`, under `[workspace.dependencies]` in the `# Data` group: `regex = "1"`. In `crates/dispatch-config/Cargo.toml` `[dependencies]`: `regex = { workspace = true }`.

- [ ] **Step 2: Write the failing tests**

Create `crates/dispatch-config/src/status/tests.rs`:

```rust
//! Tests for status rules.

use super::*;

/// Rules as a harness file would write them.
fn rules(text: &str) -> StatusRules {
    #[derive(serde::Deserialize)]
    struct File {
        status: StatusDef,
    }

    let file: File = toml::from_str(text).expect("the test's TOML parses");
    StatusRules::compile("test", &file.status)
}

fn lines(text: &[&str]) -> Vec<String> {
    text.iter().map(|line| (*line).to_string()).collect()
}

fn on_screen(rules: &StatusRules, screen: &[&str]) -> Option<RuleState> {
    let screen = lines(screen);
    rules.evaluate(&StatusInput {
        screen: &screen,
        ..StatusInput::default()
    })
}

#[test]
fn contains_matches_regardless_of_case() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "blocked"
        region = "screen"
        contains = ["do you want to proceed?"]
        "#,
    );

    assert_eq!(
        on_screen(&rules, &["  Do you want to PROCEED?"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(on_screen(&rules, &["nothing here"]), None);
}

#[test]
fn every_contains_must_appear_and_one_any() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "blocked"
        region = "screen"
        contains = ["esc to cancel"]
        any = ["enter to confirm", "enter to select"]
        "#,
    );

    assert_eq!(
        on_screen(&rules, &["Pick one", "enter to select · esc to cancel"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        on_screen(&rules, &["esc to cancel"]),
        None,
        "no `any` string appeared"
    );
}

#[test]
fn not_vetoes_a_match() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "working"
        region = "screen"
        contains = ["thinking"]
        not = ["waiting for permission"]
        "#,
    );

    assert_eq!(on_screen(&rules, &["thinking…"]), Some(RuleState::Working));
    assert_eq!(
        on_screen(&rules, &["thinking…", "Waiting for permission"]),
        None
    );
}

#[test]
fn a_regex_is_tested_against_each_line() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "blocked"
        region = "screen"
        regex = ['^\s*❯?\s*1\.\s*Yes\b']
        "#,
    );

    assert_eq!(
        on_screen(&rules, &["Do you want to proceed?", " ❯ 1. Yes", "   2. No"]),
        Some(RuleState::Blocked),
        "`^` anchors at the start of the second line, not of the screen"
    );
}

#[test]
fn bottom_reads_only_the_last_non_blank_lines() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "working"
        region = "bottom:2"
        contains = ["esc to interrupt"]
        "#,
    );

    assert_eq!(
        on_screen(&rules, &["esc to interrupt", "a", "", "b", ""]),
        None,
        "the phrase is above the last two non-blank lines"
    );
    assert_eq!(
        on_screen(&rules, &["a", "esc to interrupt", "", "b", ""]),
        Some(RuleState::Working)
    );
}

#[test]
fn the_title_and_progress_regions_read_their_signal() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "working"
        region = "title"
        regex = ['^[\x{2800}-\x{28FF}] ']

        [[status.rules]]
        state = "idle"
        region = "progress"
        regex = ['^4;0']
        "#,
    );

    let working = rules.evaluate(&StatusInput {
        title: "\u{280b} Refactor",
        ..StatusInput::default()
    });
    let idle = rules.evaluate(&StatusInput {
        progress: "4;0",
        ..StatusInput::default()
    });

    assert_eq!(working, Some(RuleState::Working));
    assert_eq!(idle, Some(RuleState::Idle));
}

#[test]
fn the_highest_priority_match_decides_and_ties_keep_file_order() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "idle"
        region = "screen"
        contains = ["x"]
        priority = 10

        [[status.rules]]
        state = "blocked"
        region = "screen"
        contains = ["x"]
        priority = 20

        [[status.rules]]
        state = "working"
        region = "screen"
        contains = ["x"]
        priority = 20
        "#,
    );

    assert_eq!(on_screen(&rules, &["x"]), Some(RuleState::Blocked));
}

#[test]
fn a_rule_that_cannot_be_used_is_skipped_and_the_rest_still_work() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "blocked"
        region = "screen"
        regex = ['(unclosed']

        [[status.rules]]
        state = "sleeping"
        region = "screen"
        contains = ["x"]

        [[status.rules]]
        state = "working"
        region = "sideways"
        contains = ["x"]

        [[status.rules]]
        state = "working"
        region = "screen"
        not = ["y"]

        [[status.rules]]
        state = "working"
        region = "screen"
        contains = ["x"]
        "#,
    );

    assert_eq!(on_screen(&rules, &["x"]), Some(RuleState::Working));
}

#[test]
fn no_rules_match_nothing() {
    assert!(StatusRules::default().is_empty());
    assert_eq!(on_screen(&StatusRules::default(), &["anything"]), None);
}

#[test]
fn a_harness_file_carries_a_status_section() {
    let def: crate::HarnessDef = toml::from_str(
        r#"
        id = "custom"
        display_name = "Custom"
        command = "custom"

        [[status.rules]]
        state = "working"
        region = "screen"
        contains = ["busy"]
        "#,
    )
    .expect("the harness parses");

    let rules = StatusRules::for_harness(&def.id, def.status.as_ref());
    assert_eq!(on_screen(&rules, &["busy"]), Some(RuleState::Working));
}

#[test]
fn an_empty_status_section_means_activity_only() {
    let own = StatusDef::default();

    assert!(StatusRules::for_harness("claude", Some(&own)).is_empty());
}

#[test]
fn an_unknown_harness_with_no_section_has_no_rules() {
    assert!(StatusRules::for_harness("never-heard-of-it", None).is_empty());
}

#[test]
fn the_registry_hands_out_each_harnesss_rules() {
    let def: crate::HarnessDef = toml::from_str(
        r#"
        id = "custom"
        display_name = "Custom"
        command = "custom"

        [[status.rules]]
        state = "blocked"
        region = "screen"
        contains = ["stop"]
        "#,
    )
    .expect("the harness parses");
    let registry: crate::HarnessRegistry = [def].into_iter().collect();

    let screen = lines(&["stop"]);
    let input = StatusInput {
        screen: &screen,
        ..StatusInput::default()
    };
    assert_eq!(
        registry.status_rules("custom").evaluate(&input),
        Some(RuleState::Blocked)
    );
    assert!(registry.status_rules("unknown").is_empty());
}
```

Create `status.rs` with the module doc and `#[cfg(test)] mod tests;`, `mod builtin;`, and `builtin.rs` holding only:

```rust
//! The rules Dispatch ships for the agents it knows.

/// The built-in `[status]` section for harness `id`, as TOML.
pub(super) fn builtin(_id: &str) -> Option<&'static str> {
    None
}
```

Add `pub mod status;` to `crates/dispatch-config/src/lib.rs`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p dispatch-config status::`
Expected: compile errors — `RuleState`, `StatusDef`, `StatusRules`, … not found.

- [ ] **Step 4: Implement**

`crates/dispatch-config/src/status.rs`:

```rust
//! Rules that read what a pane is doing off its screen.
//!
//! An agent's terminal says whether it is busy — a spinner in the title, an
//! "esc to interrupt" footer — and whether it is waiting on the user — a
//! permission prompt. Each harness carries rules that recognise those, in its
//! file's `[status]` section or, when it has none, the built-in set for its
//! id. The approach, and the built-in rules, follow herdr's.

use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};

mod builtin;

/// What a rule says a pane is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleState {
    /// Busy.
    Working,
    /// Waiting for the next thing to do.
    Idle,
    /// Waiting on a decision only the user can make.
    Blocked,
}

/// One rule, as a harness file writes it.
///
/// `state` and `region` are strings rather than enums so a value this build
/// does not know skips the one rule instead of failing the whole file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleDef {
    /// `working`, `idle` or `blocked`.
    pub state: String,
    /// `title`, `progress`, `bottom:N` or `screen`.
    pub region: String,
    /// Every one must appear, case-insensitively.
    pub contains: Vec<String>,
    /// At least one must appear, case-insensitively.
    pub any: Vec<String>,
    /// None may appear, case-insensitively.
    pub not: Vec<String>,
    /// At least one must match some line of the region.
    pub regex: Vec<String>,
    /// Higher is tried first; ties keep file order.
    pub priority: i32,
}

/// A harness file's `[status]` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusDef {
    /// The rules, in file order.
    pub rules: Vec<RuleDef>,
}

/// Where on the terminal a rule looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Title,
    Progress,
    Bottom(usize),
    Screen,
}

impl Region {
    fn parse(text: &str) -> Option<Region> {
        match text {
            "title" => Some(Region::Title),
            "progress" => Some(Region::Progress),
            "screen" => Some(Region::Screen),
            _ => text
                .strip_prefix("bottom:")
                .and_then(|count| count.parse().ok())
                .filter(|count| *count > 0)
                .map(Region::Bottom),
        }
    }
}

/// A rule ready to match.
#[derive(Debug, Clone)]
struct Rule {
    state: RuleState,
    region: Region,
    contains: Vec<String>,
    any: Vec<String>,
    not: Vec<String>,
    regex: Vec<Regex>,
    priority: i32,
}

/// What rules are matched against.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusInput<'a> {
    /// The raw title the program last set.
    pub title: &'a str,
    /// The last `OSC 9;4` payload, after `9;`.
    pub progress: &'a str,
    /// The live screen, top to bottom.
    pub screen: &'a [String],
}

/// A harness's rules, compiled, highest priority first.
#[derive(Debug, Clone, Default)]
pub struct StatusRules {
    rules: Vec<Rule>,
}

impl StatusRules {
    /// Compiles `def`'s rules for harness `id`.
    ///
    /// A rule that cannot be used — a regex that does not compile, a state or
    /// region this build does not know, or no positive condition at all — is
    /// logged and left out. The harness still loads with the rest: one bad
    /// pattern should cost that pattern, not the agent's whole status.
    #[must_use]
    pub fn compile(id: &str, def: &StatusDef) -> StatusRules {
        let mut rules: Vec<Rule> = def
            .rules
            .iter()
            .enumerate()
            .filter_map(|(index, rule)| match compile_rule(rule) {
                Ok(rule) => Some(rule),
                Err(reason) => {
                    tracing::warn!(harness = id, rule = index, %reason, "skipping a status rule");
                    None
                }
            })
            .collect();

        // Stable, so equal priorities keep the order the file gave them.
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        StatusRules { rules }
    }

    /// The rules harness `id` gets: its file's own `[status]` when it has
    /// one, else the built-in set for its id, else none.
    ///
    /// An empty `[status]` counts as having one: it is how a user says "go by
    /// activity alone" for an agent whose built-ins misread it.
    #[must_use]
    pub fn for_harness(id: &str, own: Option<&StatusDef>) -> StatusRules {
        if let Some(own) = own {
            return StatusRules::compile(id, own);
        }

        #[derive(Deserialize)]
        struct File {
            status: StatusDef,
        }

        builtin::builtin(id)
            .and_then(|text| match toml::from_str::<File>(text) {
                Ok(file) => Some(StatusRules::compile(id, &file.status)),
                Err(error) => {
                    tracing::error!(harness = id, %error, "built-in status rules do not parse");
                    None
                }
            })
            .unwrap_or_default()
    }

    /// Whether there are no rules, so activity alone decides.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The state the first matching rule names, trying the highest priority
    /// first; `None` when nothing matches.
    #[must_use]
    pub fn evaluate(&self, input: &StatusInput<'_>) -> Option<RuleState> {
        self.rules
            .iter()
            .find(|rule| rule.matches(input))
            .map(|rule| rule.state)
    }
}

impl Rule {
    fn matches(&self, input: &StatusInput<'_>) -> bool {
        let lines = region_lines(self.region, input);
        let text = lines.join("\n").to_lowercase();

        self.contains.iter().all(|needle| text.contains(needle.as_str()))
            && (self.any.is_empty() || self.any.iter().any(|needle| text.contains(needle.as_str())))
            && !self.not.iter().any(|needle| text.contains(needle.as_str()))
            && (self.regex.is_empty()
                || self
                    .regex
                    .iter()
                    .any(|pattern| lines.iter().any(|line| pattern.is_match(line))))
    }
}

/// The lines a region covers.
fn region_lines<'a>(region: Region, input: &StatusInput<'a>) -> Vec<&'a str> {
    match region {
        Region::Title => vec![input.title],
        Region::Progress => vec![input.progress],
        Region::Screen => input.screen.iter().map(String::as_str).collect(),
        Region::Bottom(count) => {
            let mut kept: Vec<&str> = input
                .screen
                .iter()
                .rev()
                .map(String::as_str)
                .filter(|line| !line.trim().is_empty())
                .take(count)
                .collect();
            kept.reverse();
            kept
        }
    }
}

/// Turns one rule as written into one ready to match, or says why not.
fn compile_rule(def: &RuleDef) -> Result<Rule, String> {
    let state = match def.state.as_str() {
        "working" => RuleState::Working,
        "idle" => RuleState::Idle,
        "blocked" => RuleState::Blocked,
        other => return Err(format!("unknown state {other:?}")),
    };
    let region =
        Region::parse(&def.region).ok_or_else(|| format!("unknown region {:?}", def.region))?;

    let lower = |values: &[String]| -> Vec<String> {
        values
            .iter()
            .filter(|value| !value.is_empty())
            .map(|value| value.to_lowercase())
            .collect()
    };
    let contains = lower(&def.contains);
    let any = lower(&def.any);
    let not = lower(&def.not);
    let regex = def
        .regex
        .iter()
        .map(|pattern| Regex::new(pattern).map_err(|error| format!("bad regex: {error}")))
        .collect::<Result<Vec<_>, _>>()?;

    if contains.is_empty() && any.is_empty() && regex.is_empty() {
        return Err("no condition to match on".to_string());
    }

    Ok(Rule {
        state,
        region,
        contains,
        any,
        not,
        regex,
        priority: def.priority,
    })
}

#[cfg(test)]
mod tests;
```

(`Arc` is used by the registry below; if `status.rs` does not use it, drop the import there.)

In `harness.rs`, add to `HarnessDef` after `settings`:

```rust
    /// Rules that read this agent's state off its screen. Absent means the
    /// built-in rules for its id, if Dispatch has some.
    #[serde(default)]
    pub status: Option<crate::status::StatusDef>,
```

In `lib.rs`, `HarnessRegistry` gains the compiled rules:

```rust
/// Every harness Dispatch knows about, keyed by id.
#[derive(Debug, Clone, Default)]
pub struct HarnessRegistry {
    harnesses: BTreeMap<String, HarnessDef>,
    /// Each harness's status rules, compiled once as it is registered.
    rules: BTreeMap<String, std::sync::Arc<status::StatusRules>>,
}
```

with one constructor used by both `from_iter` and `load_from_dir`:

```rust
impl HarnessRegistry {
    /// A registry over `harnesses`, with each one's status rules compiled.
    fn with(harnesses: BTreeMap<String, HarnessDef>) -> Self {
        let rules = harnesses
            .values()
            .map(|def| {
                (
                    def.id.clone(),
                    std::sync::Arc::new(status::StatusRules::for_harness(
                        &def.id,
                        def.status.as_ref(),
                    )),
                )
            })
            .collect();

        Self { harnesses, rules }
    }

    /// The status rules for harness `id`.
    ///
    /// A pane can name a harness this client has no file for — one adopted
    /// from a daemon — and still gets the built-in rules for that id.
    #[must_use]
    pub fn status_rules(&self, id: &str) -> std::sync::Arc<status::StatusRules> {
        self.rules.get(id).cloned().unwrap_or_else(|| {
            std::sync::Arc::new(status::StatusRules::for_harness(id, None))
        })
    }
}
```

Replace the three `Self { harnesses }` / `Self { harnesses: … }` constructions in `from_iter` and `load_from_dir` with `Self::with(…)`. Re-export: `pub use status::{RuleState, StatusInput, StatusRules};` beside the other `pub use`s.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p dispatch-config`
Expected: all pass, including the 13 new status tests.

- [ ] **Step 6: Lint and commit**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add Cargo.toml Cargo.lock crates/dispatch-config
git commit -m "feat(config): status rules that read an agent's state off its screen"
```

---

### Task 3: Built-in rules for claude, codex, opencode and agy

**Files:**
- Modify: `crates/dispatch-config/src/status/builtin.rs`
- Modify: `crates/dispatch-config/src/status/tests.rs`

**Interfaces:**
- Consumes: `StatusRules::for_harness`, `StatusInput`, `RuleState` (Task 2).
- Produces: built-in rules for ids `claude`, `codex`, `opencode`, `agy`.

- [ ] **Step 1: Write the failing tests**

Append to `status/tests.rs`:

```rust
fn builtin(id: &str) -> StatusRules {
    let rules = StatusRules::for_harness(id, None);
    assert!(!rules.is_empty(), "{id} has built-in rules");
    rules
}

fn verdict(rules: &StatusRules, title: &str, progress: &str, screen: &[&str]) -> Option<RuleState> {
    let screen = lines(screen);
    rules.evaluate(&StatusInput {
        title,
        progress,
        screen: &screen,
    })
}

#[test]
fn claude_is_working_while_its_title_spins() {
    let claude = builtin("claude");

    assert_eq!(
        verdict(&claude, "\u{2802} Refactor the sidebar", "", &["> "]),
        Some(RuleState::Working)
    );
    assert_eq!(
        verdict(&claude, "\u{25d0} Refactor the sidebar", "", &["> "]),
        Some(RuleState::Working),
        "the newer half-circle spinner too"
    );
}

#[test]
fn claude_is_working_while_its_turn_footer_shows() {
    assert_eq!(
        verdict(
            &builtin("claude"),
            "",
            "",
            &["✻ Thinking… (12s · ↑ 1.2k tokens)", "", "⏵⏵ accept edits on · esc to interrupt"]
        ),
        Some(RuleState::Working)
    );
}

#[test]
fn claude_is_blocked_on_a_permission_prompt() {
    assert_eq!(
        verdict(
            &builtin("claude"),
            "\u{2733} Claude Code",
            "",
            &[
                "Bash command",
                "  rm -rf target",
                "Do you want to proceed?",
                "❯ 1. Yes",
                "  2. No, and tell Claude what to do differently (esc)",
            ]
        ),
        Some(RuleState::Blocked)
    );
}

#[test]
fn claude_is_blocked_on_a_choice_form() {
    assert_eq!(
        verdict(
            &builtin("claude"),
            "",
            "",
            &["Which approach?", "❯ 1. Fast", "  2. Thorough", "Enter to select · ↑/↓ to navigate · Esc to cancel"]
        ),
        Some(RuleState::Blocked)
    );
}

#[test]
fn claude_is_idle_at_rest() {
    let claude = builtin("claude");

    assert_eq!(
        verdict(&claude, "\u{2733} Claude Code", "", &["╭────╮", "│ >  │", "╰────╯"]),
        Some(RuleState::Idle)
    );
    assert_eq!(verdict(&claude, "", "4;0", &[">"]), Some(RuleState::Idle));
}

#[test]
fn codex_reads_its_title() {
    let codex = builtin("codex");

    assert_eq!(verdict(&codex, "\u{280b} dispatch", "", &[]), Some(RuleState::Working));
    assert_eq!(
        verdict(&codex, "Action Required", "", &[]),
        Some(RuleState::Blocked)
    );
}

#[test]
fn codex_is_blocked_on_its_prompts() {
    let codex = builtin("codex");

    assert_eq!(
        verdict(&codex, "", "", &["Run `cargo test`? [y/n]"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        verdict(&codex, "", "", &["Allow command?", "  cargo build", "Press enter to confirm or esc to cancel"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        verdict(&codex, "", "", &["> You are in /home/me/app", "Do you trust the contents of this directory?"]),
        Some(RuleState::Blocked)
    );
}

#[test]
fn codex_is_working_while_its_timer_runs() {
    assert_eq!(
        verdict(&builtin("codex"), "", "", &["• Working (12s • esc to interrupt)"]),
        Some(RuleState::Working)
    );
}

#[test]
fn opencode_reads_its_screen() {
    let opencode = builtin("opencode");

    assert_eq!(
        verdict(&opencode, "", "", &["△ Permission required", "  edit src/main.rs"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        verdict(&opencode, "", "", &["Building… esc to interrupt"]),
        Some(RuleState::Working)
    );
    assert_eq!(
        verdict(&opencode, "", "", &["■■■■⬝⬝⬝⬝"]),
        Some(RuleState::Working)
    );
}

#[test]
fn agy_reads_its_screen() {
    let agy = builtin("agy");

    assert_eq!(
        verdict(&agy, "", "", &["Agent is requesting permission for:", "  npm install", "Do you want to proceed?"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        verdict(&agy, "", "", &["⠙ Searching the codebase"]),
        Some(RuleState::Working)
    );
}

#[test]
fn a_harness_files_own_section_replaces_the_built_ins() {
    let own: StatusDef = toml::from_str(
        r#"
        [[rules]]
        state = "working"
        region = "screen"
        contains = ["my marker"]
        "#,
    )
    .expect("the section parses");
    let rules = StatusRules::for_harness("claude", Some(&own));

    assert_eq!(
        verdict(&rules, "\u{2733} Claude Code", "", &[]),
        None,
        "the built-in idle rule is gone"
    );
    assert_eq!(verdict(&rules, "", "", &["my marker"]), Some(RuleState::Working));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-config status::`
Expected: the new built-in tests fail ("has built-in rules").

- [ ] **Step 3: Implement**

Replace `status/builtin.rs`:

```rust
//! The rules Dispatch ships for the agents it knows.
//!
//! Adapted from herdr's detection manifests
//! (<https://github.com/ogulcancelik/herdr>, Apache-2.0), simplified to the
//! regions and conditions Dispatch's rules have. Written as TOML — the same
//! format a harness file's `[status]` section uses — so any of these can be
//! copied into that file and edited when an agent's interface moves on.

/// The built-in `[status]` section for harness `id`, as TOML.
pub(super) fn builtin(id: &str) -> Option<&'static str> {
    match id {
        "claude" => Some(CLAUDE),
        "codex" => Some(CODEX),
        "opencode" => Some(OPENCODE),
        "agy" => Some(AGY),
        _ => None,
    }
}

const CLAUDE: &str = r#"
# The title's first glyph spins while Claude Code works: braille through
# 2.1.227, half circles since.
[[status.rules]]
state = "working"
region = "title"
regex = ['^[\x{2800}-\x{28FF}\x{25D0}-\x{25D3}] ']
priority = 1100

# A permission prompt.
[[status.rules]]
state = "blocked"
region = "bottom:15"
contains = ["do you want to proceed?"]
regex = ['(?i)^\s*❯?\s*1\.\s*yes\b']
priority = 990

# A form waiting on a choice.
[[status.rules]]
state = "blocked"
region = "bottom:15"
contains = ["esc to cancel"]
any = ["enter to confirm", "enter to select"]
priority = 980

# The live turn's footer.
[[status.rules]]
state = "working"
region = "bottom:12"
contains = ["esc to interrupt"]
priority = 970

# The live turn's activity line: a star glyph, a verb, an ellipsis.
[[status.rules]]
state = "working"
region = "bottom:12"
regex = ['^\s*[\x{002A}\x{00B7}\x{2722}\x{2733}\x{2736}\x{273B}\x{273D}]\s+\S.*…(?:\s+\(\d+[smh]|\s*$)']
priority = 965

# At rest the title carries a still mark, and progress is cleared.
[[status.rules]]
state = "idle"
region = "title"
regex = ['^\x{2733} ']
priority = 250

[[status.rules]]
state = "idle"
region = "progress"
regex = ['^4;0']
priority = 250
"#;

const CODEX: &str = r#"
[[status.rules]]
state = "blocked"
region = "title"
contains = ["action required"]
priority = 1100

# A braille spinner glyph standing on its own in the title.
[[status.rules]]
state = "working"
region = "title"
regex = ['(?:^| )[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏](?: |$)']
priority = 1050

[[status.rules]]
state = "blocked"
region = "screen"
contains = ["do you trust the contents of this directory?"]
priority = 950

[[status.rules]]
state = "blocked"
region = "bottom:20"
any = [
  "press enter to confirm or esc to cancel",
  "enter to submit answer",
  "enter to submit all",
  "allow command?",
]
priority = 900

[[status.rules]]
state = "blocked"
region = "bottom:20"
any = ["[y/n]", "yes (y)"]
priority = 600

# The running turn's timer.
[[status.rules]]
state = "working"
region = "bottom:12"
regex = ['\((?:[0-9]+[hm] )*[0-9]+s • [^)]*to interrupt\)']
priority = 500
"#;

const OPENCODE: &str = r#"
[[status.rules]]
state = "blocked"
region = "screen"
any = ["△ permission required"]
priority = 300

[[status.rules]]
state = "blocked"
region = "screen"
contains = ["esc dismiss"]
any = ["enter confirm", "enter submit", "enter toggle"]
priority = 290

[[status.rules]]
state = "working"
region = "screen"
any = ["esc to interrupt", "ctrl+c to interrupt", "esc interrupt"]
priority = 110

# The progress bar under a running turn.
[[status.rules]]
state = "working"
region = "screen"
regex = ['(■|⬝){4,}']
priority = 100
"#;

const AGY: &str = r#"
[[status.rules]]
state = "blocked"
region = "screen"
contains = ["requesting permission for:"]
any = ["do you want to proceed?", "edit command"]
priority = 300

# A braille spinner before an "-ing" word.
[[status.rules]]
state = "working"
region = "screen"
regex = ['^\s*[\x{2800}-\x{28FF}]+\s+\p{Alphabetic}+\w*ing\b']
priority = 100

[[status.rules]]
state = "working"
region = "bottom:5"
regex = ['(?i)·\s*[1-9][0-9]*\s+task']
priority = 90
"#;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-config status::`
Expected: all pass. If a fixture fails, fix the **rule** (the fixtures are the agents' real UI shapes as herdr records them), and say which in the report.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-config/src/status
git commit -m "feat(config): built-in status rules for claude, codex, opencode and agy"
```

---

### Task 4: The tracker — a damped verdict per pane

**Files:**
- Create: `crates/dispatch-tui/src/activity.rs`, `crates/dispatch-tui/src/activity/tests.rs`
- Modify: `crates/dispatch-tui/src/lib.rs`
- Modify: `docs/superpowers/specs/2026-09-25-live-status-motion-design.md` (the `Tracker` sketch)

**Interfaces:**
- Consumes: `dispatch_config::status::{RuleState, StatusInput, StatusRules}` (Task 2), `dispatch_pty::Signals` (Task 1).
- Produces (`dispatch_tui::activity`): constants `ECHO: Duration` (150 ms), `ACTIVE_FOR: Duration` (1 s), `SETTLE: Duration` (700 ms); `enum Verdict { Working, Idle, Blocked }` (`Debug, Clone, Copy, PartialEq, Eq`); `struct Tracker` with `Tracker::new(rules: Arc<StatusRules>) -> Tracker`, `input(&mut self, now: Instant)`, `output(&mut self, now: Instant)`, `signals(&mut self, signals: &Signals)`, `evaluate(&mut self, now: Instant, screen: &[String]) -> Option<Verdict>`.

- [ ] **Step 1: Write the failing tests**

`crates/dispatch-tui/src/activity/tests.rs`:

```rust
//! Tests for the activity tracker.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dispatch_config::status::{StatusDef, StatusRules};
use dispatch_pty::Signals;

use super::*;

fn rules(text: &str) -> Arc<StatusRules> {
    let def: StatusDef = toml::from_str(text).expect("the rules parse");
    Arc::new(StatusRules::compile("test", &def))
}

fn none() -> Arc<StatusRules> {
    Arc::new(StatusRules::default())
}

const MS: fn(u64) -> Duration = Duration::from_millis;

#[test]
fn the_first_evaluation_reports_at_once() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());

    assert_eq!(tracker.evaluate(start, &[]), Some(Verdict::Idle));
    assert_eq!(tracker.evaluate(start + MS(300), &[]), None, "no change, no report");
}

#[test]
fn output_makes_a_pane_working() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());
    tracker.evaluate(start, &[]);

    tracker.output(start + MS(10));
    assert_eq!(tracker.evaluate(start + MS(20), &[]), Some(Verdict::Working));
}

#[test]
fn quiet_makes_it_idle_only_once_it_has_settled() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());
    tracker.output(start);
    assert_eq!(tracker.evaluate(start, &[]), Some(Verdict::Working));

    // A second of quiet makes the raw verdict idle...
    assert_eq!(tracker.evaluate(start + MS(1100), &[]), None);
    // ...which is reported only once it has held for the settling time.
    assert_eq!(tracker.evaluate(start + MS(1500), &[]), None);
    assert_eq!(tracker.evaluate(start + MS(1800), &[]), Some(Verdict::Idle));
}

#[test]
fn output_while_settling_keeps_it_working() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());
    tracker.output(start);
    tracker.evaluate(start, &[]);

    tracker.evaluate(start + MS(1100), &[]);
    tracker.output(start + MS(1300));
    assert_eq!(tracker.evaluate(start + MS(1900), &[]), None, "still working");

    // The clock restarts from the last output.
    assert_eq!(tracker.evaluate(start + MS(2400), &[]), None);
    assert_eq!(tracker.evaluate(start + MS(3100), &[]), Some(Verdict::Idle));
}

#[test]
fn the_echo_of_input_is_not_activity() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());
    tracker.evaluate(start, &[]);

    tracker.input(start + MS(100));
    tracker.output(start + MS(140));
    assert_eq!(tracker.evaluate(start + MS(200), &[]), None, "still idle");

    tracker.output(start + MS(400));
    assert_eq!(
        tracker.evaluate(start + MS(410), &[]),
        Some(Verdict::Working),
        "output long after the keystroke is the program's own"
    );
}

#[test]
fn blocked_is_reported_at_once_and_left_at_once() {
    let start = Instant::now();
    let mut tracker = Tracker::new(rules(
        r#"
        [[rules]]
        state = "blocked"
        region = "screen"
        contains = ["proceed?"]
        "#,
    ));
    tracker.output(start);
    tracker.evaluate(start, &[]);

    let prompt = vec!["Do you want to proceed?".to_string()];
    assert_eq!(tracker.evaluate(start + MS(50), &prompt), Some(Verdict::Blocked));
    assert_eq!(
        tracker.evaluate(start + MS(1500), &[]),
        Some(Verdict::Idle),
        "leaving blocked is not damped"
    );
}

#[test]
fn a_working_rule_holds_without_output() {
    let start = Instant::now();
    let mut tracker = Tracker::new(rules(
        r#"
        [[rules]]
        state = "working"
        region = "title"
        regex = ['^\x{280b} ']
        "#,
    ));
    tracker.signals(&Signals {
        title: Some("\u{280b} thinking".to_string()),
        ..Signals::default()
    });

    assert_eq!(tracker.evaluate(start, &[]), Some(Verdict::Working));
    assert_eq!(tracker.evaluate(start + MS(5000), &[]), None, "still working");
}

#[test]
fn an_idle_rule_with_fresh_output_stays_working() {
    let start = Instant::now();
    let mut tracker = Tracker::new(rules(
        r#"
        [[rules]]
        state = "idle"
        region = "screen"
        contains = [">"]
        "#,
    ));
    tracker.output(start);

    assert_eq!(
        tracker.evaluate(start + MS(10), &[">".to_string()]),
        Some(Verdict::Working),
        "a prompt on screen does not outrank output arriving"
    );
}

#[test]
fn progress_and_title_are_kept_until_replaced() {
    let start = Instant::now();
    let mut tracker = Tracker::new(rules(
        r#"
        [[rules]]
        state = "idle"
        region = "progress"
        regex = ['^4;0']
        "#,
    ));
    tracker.signals(&Signals {
        progress: Some("4;0".to_string()),
        ..Signals::default()
    });
    tracker.signals(&Signals::default());

    assert_eq!(tracker.evaluate(start, &[]), Some(Verdict::Idle));
}
```

Create `activity.rs` with the module doc and `#[cfg(test)] mod tests;`; add `pub mod activity;` to `dispatch-tui/src/lib.rs`. Add `toml = { workspace = true }` under `[dev-dependencies]` in `crates/dispatch-tui/Cargo.toml` (the tests parse rules; `toml` is already a workspace dependency, so this adds nothing new to the build).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui activity::`
Expected: compile errors — `Tracker`, `Verdict`, … not found.

- [ ] **Step 3: Implement**

`crates/dispatch-tui/src/activity.rs`:

```rust
//! What a pane is doing, worked out from what it prints and what is on its
//! screen.
//!
//! The harness's rules read the screen and the title; output arriving says
//! the program is busy even where no rule knows its interface; and time
//! damps the change from working to idle, so a pause between two chunks of a
//! reply does not flicker the sidebar.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dispatch_config::status::{RuleState, StatusInput, StatusRules};
use dispatch_pty::Signals;

/// Output this soon after our own input is its echo, not the program at work.
pub const ECHO: Duration = Duration::from_millis(150);

/// How long after output a pane still counts as working.
pub const ACTIVE_FOR: Duration = Duration::from_secs(1);

/// How long an idle verdict must hold before a working pane is called idle.
pub const SETTLE: Duration = Duration::from_millis(700);

/// What a pane is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Busy.
    Working,
    /// Waiting for the next thing to do.
    Idle,
    /// Waiting on a decision only the user can make.
    Blocked,
}

/// One pane's evidence, and the verdict last reported from it.
#[derive(Debug)]
pub struct Tracker {
    rules: Arc<StatusRules>,
    title: String,
    progress: String,
    last_input: Option<Instant>,
    last_output: Option<Instant>,
    reported: Option<Verdict>,
    /// When the raw verdict turned idle under a reported working one.
    idle_since: Option<Instant>,
}

impl Tracker {
    /// A tracker reading a pane with `rules`.
    #[must_use]
    pub fn new(rules: Arc<StatusRules>) -> Self {
        Self {
            rules,
            title: String::new(),
            progress: String::new(),
            last_input: None,
            last_output: None,
            reported: None,
            idle_since: None,
        }
    }

    /// We sent the pane a keystroke or a paste.
    pub fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// The pane printed something.
    ///
    /// Within [`ECHO`] of our own input it is the terminal echoing what was
    /// typed, and does not count: typing into a pane is not the agent working.
    pub fn output(&mut self, now: Instant) {
        let echo = self
            .last_input
            .is_some_and(|at| now.saturating_duration_since(at) < ECHO);

        if !echo {
            self.last_output = Some(now);
        }
    }

    /// The pane's title or progress changed.
    pub fn signals(&mut self, signals: &Signals) {
        if let Some(title) = &signals.title {
            self.title.clone_from(title);
        }
        if let Some(progress) = &signals.progress {
            self.progress.clone_from(progress);
        }
    }

    /// Works out the pane's state against its live `screen`, returning it
    /// when it differs from the last one reported.
    pub fn evaluate(&mut self, now: Instant, screen: &[String]) -> Option<Verdict> {
        let rule = self.rules.evaluate(&StatusInput {
            title: &self.title,
            progress: &self.progress,
            screen,
        });
        let active = self
            .last_output
            .is_some_and(|at| now.saturating_duration_since(at) < ACTIVE_FOR);

        let raw = match rule {
            Some(RuleState::Blocked) => Verdict::Blocked,
            Some(RuleState::Working) => Verdict::Working,
            _ if active => Verdict::Working,
            _ => Verdict::Idle,
        };

        if raw != Verdict::Idle {
            self.idle_since = None;
        }

        // Only working to idle is damped: a prompt appearing, a turn
        // starting, or leaving a prompt are all worth showing the moment they
        // happen, and none of them flickers.
        let next = if self.reported == Some(Verdict::Working) && raw == Verdict::Idle {
            let since = *self.idle_since.get_or_insert(now);
            if now.saturating_duration_since(since) < SETTLE {
                return None;
            }
            Verdict::Idle
        } else {
            raw
        };

        if self.reported == Some(next) {
            return None;
        }

        self.reported = Some(next);
        self.idle_since = None;
        Some(next)
    }
}

#[cfg(test)]
mod tests;
```

In the spec's "The verdict" section, replace the `Tracker` sketch's `output(&mut self, now: Instant, echo: bool)` line and its comment with:

```rust
    /// We sent the pane a keystroke or paste.
    pub fn input(&mut self, now: Instant);
    /// Output arrived; within 150 ms of our own input it is echo and ignored.
    pub fn output(&mut self, now: Instant);
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-tui activity::`
Expected: 9 passed.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui docs/superpowers/specs/2026-09-25-live-status-motion-design.md
git commit -m "feat(tui): track what a pane is doing from its output, screen and title"
```

---

### Task 5: `Blocked`, and the done marks, in the model

**Files:**
- Modify: `crates/dispatch-core/src/pane.rs` (`PaneStatus::Blocked`)
- Modify: `crates/dispatch-core/src/state.rs` (`unseen`, three methods, `close_pane`)
- Modify: `crates/dispatch-tui/src/sidebar.rs` (`BLOCKED` glyph arm — needed for the match to compile)
- Test: `crates/dispatch-core/src/state.rs`, `crates/dispatch-tui/src/sidebar/tests.rs`

**Interfaces:**
- Produces: `PaneStatus::Blocked` (live); `AppState::mark_unseen(&mut self, PaneId)`, `mark_seen(&mut self, PaneId)`, `is_unseen(&self, PaneId) -> bool`; `sidebar::BLOCKED: &str = "\u{f071}"`.

- [ ] **Step 1: Write the failing tests**

In `state.rs` tests:

```rust
    #[test]
    fn a_pane_can_be_marked_unseen_and_seen_again() {
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let pane = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        assert!(!state.is_unseen(pane));
        state.mark_unseen(pane);
        assert!(state.is_unseen(pane));
        state.mark_seen(pane);
        assert!(!state.is_unseen(pane));
    }

    #[test]
    fn closing_a_pane_forgets_its_mark() {
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let pane = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        state.mark_unseen(pane);
        state.close_pane(pane).expect("the pane exists");

        assert!(!state.is_unseen(pane));
    }
```

In `pane.rs` tests, add `assert!(PaneStatus::Blocked.is_live());` to the existing liveness test.

In `sidebar/tests.rs`:

```rust
#[test]
fn a_blocked_pane_says_so_in_yellow() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state
        .set_pane_status(pane, PaneStatus::Blocked)
        .expect("pane exists");

    let buf = render(&state, WIDTH, 6);
    let cell = buf.cell((WIDTH - 3, TOP + 1)).expect("cell exists");

    assert_eq!(cell.symbol(), BLOCKED);
    assert_eq!(cell.fg, Color::Yellow);
    assert!(cell.modifier.contains(Modifier::BOLD));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-core && cargo test -p dispatch-tui sidebar::`
Expected: compile errors — no `Blocked`, `mark_unseen`, `BLOCKED`.

- [ ] **Step 3: Implement**

`pane.rs`, in `PaneStatus` after `Idle`:

```rust
    /// The agent is waiting on a decision only the user can make — a
    /// permission prompt, a question.
    ///
    /// Only a client sets this, from what it reads on the pane's screen; a
    /// daemon never sends it, so no older client meets it on the wire.
    Blocked,
```

`state.rs`, in `AppState` after `collapsed_devices`:

```rust
    /// Panes that finished, or rang for attention, while the user was looking
    /// elsewhere. Client state, like the folds: nothing on the wire.
    unseen: HashSet<PaneId>,
```

and after `set_pane_title`:

```rust
    /// Marks a pane as finished, or asking for attention, out of the user's
    /// sight.
    pub fn mark_unseen(&mut self, id: PaneId) {
        self.unseen.insert(id);
    }

    /// Clears that mark: the user is looking at the pane now.
    pub fn mark_seen(&mut self, id: PaneId) {
        self.unseen.remove(&id);
    }

    /// Whether a pane is marked as finished out of sight.
    #[must_use]
    pub fn is_unseen(&self, id: PaneId) -> bool {
        self.unseen.contains(&id)
    }
```

In `close_pane`, alongside its other bookkeeping for `id`, add `self.unseen.remove(&id);`.

`sidebar.rs`: after `IDLE`:

```rust
/// Waiting on a decision only the user can make.
pub const BLOCKED: &str = "\u{f071}";
```

and in `state_glyph` add the arm

```rust
        PaneStatus::Blocked => (
            BLOCKED,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
```

If any other `match` over `PaneStatus` fails to compile (the build will say), add `PaneStatus::Blocked` beside `PaneStatus::Idle` there.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-core crates/dispatch-tui
git commit -m "feat(core): a blocked pane, and a mark for one that finished out of sight"
```

---

### Task 6: The client works out every pane's state

**Files:**
- Modify: `dispatch/src/app.rs`

**Interfaces:**
- Consumes: `Signals`, `TitleScanner::scan_signals` (Task 1); `HarnessRegistry::status_rules` (Task 2); `Tracker`, `Verdict` (Task 4); `PaneStatus::Blocked`, `AppState::{mark_unseen, mark_seen, is_unseen}` (Task 5).
- Produces: `App::clock: Box<dyn Fn() -> Instant>` (private; tests replace it) and `App::now(&self) -> Instant`; `Pane::activity: Tracker`, `Pane::adopted: Instant`, `Pane::evaluated: Option<Instant>`, `Pane::dirty: bool`; `App::refresh_activity(&mut self) -> bool`, called at the end of `poll_panes`; constants `EVALUATE_AFTER_OUTPUT` (100 ms), `EVALUATE_EVERY` (250 ms), `GRACE` (3 s). A status change is reported by `poll_panes` returning `true`. Task 10 hooks the attention pulse into `refresh_activity` where a pane turns blocked or is marked unseen.

- [ ] **Step 1: Write the failing tests**

In `app.rs`'s test module (add `use std::cell::Cell; use std::rc::Rc;` there):

```rust
    /// A clock the test moves by hand, installed in `app`.
    fn hand_clock(app: &mut App) -> Rc<Cell<Instant>> {
        let now = Rc::new(Cell::new(Instant::now()));
        let reading = Rc::clone(&now);
        app.clock = Box::new(move || reading.get());
        now
    }

    fn advance(clock: &Rc<Cell<Instant>>, by: Duration) {
        clock.set(clock.get() + by);
    }

    /// Delivers `bytes` as `pane`'s output and lets the app take it in.
    fn print(app: &mut App, daemon: &Sender<ServerMessage>, pane: PaneId, bytes: &[u8]) {
        daemon
            .send(ServerMessage::PaneOutput {
                pane,
                bytes: bytes.to_vec(),
            })
            .expect("the app is listening");
        app.poll_daemon();
        app.poll_panes();
    }

    fn status_of(app: &App, pane: PaneId) -> PaneStatus {
        app.state.pane(pane).expect("the pane exists").status
    }

    /// Lets a pane that has stopped printing settle: a second of quiet makes
    /// it idle, which is reported once it has held for the settling time.
    fn settle(app: &mut App, clock: &Rc<Cell<Instant>>) {
        advance(clock, Duration::from_millis(1100));
        app.poll_panes();
        advance(clock, Duration::from_millis(800));
        app.poll_panes();
    }

    #[test]
    fn output_makes_a_pane_working_and_quiet_makes_it_idle() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let pane = spawn_several(&mut app, &daemon, project, 1)[0];

        advance(&clock, Duration::from_secs(4));
        print(&mut app, &daemon, pane, b"compiling\r\n");
        assert_eq!(status_of(&app, pane), PaneStatus::Running);

        advance(&clock, Duration::from_millis(1100));
        app.poll_panes();
        assert_eq!(status_of(&app, pane), PaneStatus::Running, "still settling");

        advance(&clock, Duration::from_millis(800));
        app.poll_panes();
        assert_eq!(status_of(&app, pane), PaneStatus::Idle);
    }

    #[test]
    fn a_pane_finishing_out_of_focus_is_marked_done_until_focused() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let (background, foreground) = (panes[0], panes[1]);
        assert_eq!(app.state.focused_pane(), Some(foreground));

        advance(&clock, Duration::from_secs(4));
        print(&mut app, &daemon, background, b"working\r\n");
        settle(&mut app, &clock);

        assert_eq!(status_of(&app, background), PaneStatus::Idle);
        assert!(app.state.is_unseen(background), "it finished out of sight");
        assert!(!app.state.is_unseen(foreground));

        app.focus_pane(background);
        app.poll_panes();
        assert!(!app.state.is_unseen(background), "looking at it clears the mark");
    }

    #[test]
    fn a_reattach_replay_marks_nothing_done() {
        // A client attaching is replayed every pane's recent output at once;
        // without the grace every pane would come back "done".
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 3);

        for pane in &panes {
            print(&mut app, &daemon, *pane, b"history\r\n");
        }
        settle(&mut app, &clock);

        for pane in &panes {
            assert_eq!(status_of(&app, *pane), PaneStatus::Idle);
            assert!(!app.state.is_unseen(*pane));
        }
    }

    #[test]
    fn a_bell_from_a_pane_out_of_focus_marks_it() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 2);

        advance(&clock, Duration::from_secs(4));
        print(&mut app, &daemon, panes[0], b"\x07");

        assert!(app.state.is_unseen(panes[0]));
    }

    #[test]
    fn typing_into_a_pane_is_not_work() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let pane = spawn_several(&mut app, &daemon, project, 1)[0];

        advance(&clock, Duration::from_secs(4));
        app.poll_panes();
        assert_eq!(status_of(&app, pane), PaneStatus::Idle);

        app.send_key(dispatch_pty::Key::Char('l'), dispatch_pty::Modifiers::default());
        advance(&clock, Duration::from_millis(40));
        print(&mut app, &daemon, pane, b"l");
        advance(&clock, Duration::from_millis(200));
        app.poll_panes();

        assert_eq!(
            status_of(&app, pane),
            PaneStatus::Idle,
            "the echo of a keystroke is not the agent working"
        );
    }

    #[test]
    fn a_pane_scrolled_back_keeps_its_state() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let pane = spawn_several(&mut app, &daemon, project, 1)[0];

        advance(&clock, Duration::from_secs(4));
        print(&mut app, &daemon, pane, b"output\r\n");
        assert_eq!(status_of(&app, pane), PaneStatus::Running);

        app.panes.get_mut(&pane).expect("adopted").scrolled_back = true;
        settle(&mut app, &clock);

        assert_eq!(
            status_of(&app, pane),
            PaneStatus::Running,
            "the screen is not the live one, so it is not read"
        );
    }

    #[test]
    fn a_pane_that_exits_mid_work_stays_exited_and_unmarked() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 2);

        advance(&clock, Duration::from_secs(4));
        print(&mut app, &daemon, panes[0], b"working\r\n");
        daemon
            .send(ServerMessage::PaneChanged {
                pane: panes[0],
                update: PaneUpdate::Status {
                    status: PaneStatus::Exited(0),
                },
            })
            .expect("the app is listening");
        app.poll_daemon();
        settle(&mut app, &clock);

        assert_eq!(status_of(&app, panes[0]), PaneStatus::Exited(0));
        assert!(!app.state.is_unseen(panes[0]));
    }

    #[test]
    fn a_harnesss_rules_decide_blocked() {
        let def = dispatch_config::HarnessDef {
            id: "shell".to_string(),
            display_name: "Shell".to_string(),
            status: Some(
                toml::from_str(
                    r#"
                    [[rules]]
                    state = "blocked"
                    region = "screen"
                    contains = ["proceed?"]
                    "#,
                )
                .expect("the rules parse"),
            ),
            ..dispatch_config::HarnessDef::default()
        };
        let (client, daemon, _sent) = Client::for_test();
        let mut app = App::attached([def].into_iter().collect(), client);
        let project = Project::new("/tmp/rules", ProjectSource::LocalDir);
        let project_id = project.id;
        daemon
            .send(ServerMessage::ProjectOpened { project })
            .expect("the app is listening");
        app.poll_daemon();
        let clock = hand_clock(&mut app);
        let pane = spawn_several(&mut app, &daemon, project_id, 1)[0];

        advance(&clock, Duration::from_secs(4));
        print(&mut app, &daemon, pane, b"Do you want to proceed?\r\n");

        assert_eq!(status_of(&app, pane), PaneStatus::Blocked);
    }
```

(If `toml` is not a dev-dependency of `dispatch`, add `toml = { workspace = true }` under its `[dev-dependencies]`.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch -- output_makes finishing_out reattach_replay bell_from typing_into scrolled_back exits_mid rules_decide`
Expected: compile errors — no `clock` field.

- [ ] **Step 3: Implement**

Imports: `use dispatch_pty::Signals;` (beside the existing `dispatch_pty` import) and `use dispatch_tui::activity::{Tracker, Verdict};`.

Constants beside `FRAME`:

```rust
/// How soon after output a pane's state is looked at again.
const EVALUATE_AFTER_OUTPUT: Duration = Duration::from_millis(100);

/// How often every pane's state is looked at, output or not: an idle verdict
/// needs time to pass, not bytes, to be confirmed.
const EVALUATE_EVERY: Duration = Duration::from_millis(250);

/// How long a new pane is kept from being marked done. A client attaching is
/// replayed every pane's recent output at once, and without this every pane
/// would come back "finished while you were away".
const GRACE: Duration = Duration::from_secs(3);
```

`Pane` gains:

```rust
    /// What the pane is doing, from its output, screen and title.
    activity: Tracker,
    /// When this client took the pane on, for the grace before done marks.
    adopted: Instant,
    /// When its state was last worked out.
    evaluated: Option<Instant>,
    /// Whether it printed since then.
    dirty: bool,
```

`App` gains `clock: Box<dyn Fn() -> Instant>,` (initialised `clock: Box::new(Instant::now),` in `App::new`), with:

```rust
    /// The time, read through the clock the app was given: the real one in
    /// the binary, one moved by hand in tests, so every timing here can be
    /// tested without sleeping.
    fn now(&self) -> Instant {
        (self.clock)()
    }
```

`adopt` builds the tracker from the pane's harness:

```rust
        let rules = self
            .state
            .pane(id)
            .map(|pane| self.harnesses.status_rules(pane.harness.as_str()))
            .unwrap_or_default();
        let now = self.now();
```

and adds to the `Pane { .. }` literal: `activity: Tracker::new(rules), adopted: now, evaluated: None, dirty: false,`.

A helper used by both output paths:

```rust
    /// Takes in what one burst of a pane's output said besides its text.
    ///
    /// Returns the new title, for the caller to rename the pane with, and
    /// marks the pane unseen on a bell when the user is looking elsewhere.
    fn take_output(&mut self, id: PaneId, bytes: &[u8]) -> Option<String> {
        let now = self.now();
        let focused = self.state.focused_pane();
        let pane = self.panes.get_mut(&id)?;

        let signals: Signals = pane.titles.scan_signals(bytes);
        pane.activity.output(now);
        pane.activity.signals(&signals);
        pane.dirty = true;

        let graced = now.saturating_duration_since(pane.adopted) < GRACE;
        if signals.bell && focused != Some(id) && !graced {
            self.state.mark_unseen(id);
        }

        signals.title
    }
```

In `poll_panes`, the loop body's `if let Some(title) = pane.titles.scan(&output) { renamed.push((*id, title)); }` becomes collecting the output: `if !output.is_empty() { changed = true; outputs.push((*id, output)); … screen read as now … }` — i.e. keep the screen read inside the loop, move the scan out:

```rust
        let mut outputs = Vec::new();
        for (id, pane) in &mut self.panes {
            let output = pane.backend.drain();

            if !output.is_empty() {
                changed = true;

                if let Ok(screen) = pane.reader.read(pane.backend.terminal()) {
                    pane.screen = screen;
                }
                outputs.push((*id, output));
            }

            if let RunState::Exited(code) = pane.backend.state() {
                exited.push((*id, code));
            }
        }

        for (id, output) in outputs {
            if let Some(title) = self.take_output(id, &output) {
                self.rename(id, &title);
            }
        }
```

(delete the old `renamed` vector and its loop). At the end, beside `refresh_local_branches`:

```rust
        if self.refresh_activity() {
            changed = true;
        }
```

In `apply_from`'s `ServerMessage::PaneOutput` arm, replace `let title = target.titles.scan(&bytes); if let Some(title) = title { self.rename(pane, &title); }` with:

```rust
                if let Some(title) = self.take_output(pane, &bytes) {
                    self.rename(pane, &title);
                }
```

(the `target` borrow must end before this call — move the screen read above it, as it already is).

In `send_key` and `paste`, after the successful write to the pane, add `pane.activity.input(now);` — read `let now = self.now();` at the top of each function, before `self.panes.get_mut`.

The refresh:

```rust
    /// Works out what every pane is doing, and records what changed.
    ///
    /// A pane is looked at soon after it prints, and every pane on a slower
    /// tick so a quiet one can settle to idle. A pane scrolled back is left
    /// alone: its screen is history, not the live one. Returns whether any
    /// status changed.
    fn refresh_activity(&mut self) -> bool {
        let now = self.now();
        let focused = self.state.focused_pane();

        if let Some(focused) = focused {
            self.state.mark_seen(focused);
        }

        let mut verdicts = Vec::new();
        for (id, pane) in &mut self.panes {
            if pane.scrolled_back {
                continue;
            }

            let due = pane.evaluated.is_none_or(|at| {
                let since = now.saturating_duration_since(at);
                (pane.dirty && since >= EVALUATE_AFTER_OUTPUT) || since >= EVALUATE_EVERY
            });
            if !due {
                continue;
            }

            pane.evaluated = Some(now);
            pane.dirty = false;

            if let Some(verdict) = pane.activity.evaluate(now, &pane.screen.text_lines()) {
                verdicts.push((*id, verdict, pane.adopted));
            }
        }

        let mut changed = false;
        for (id, verdict, adopted) in verdicts {
            let Some(before) = self.state.pane(id).map(|pane| pane.status) else {
                continue;
            };
            // An exited pane's last word is its exit; nothing read off its
            // final screen may overwrite that.
            if !before.is_live() {
                continue;
            }

            let status = match verdict {
                Verdict::Working => PaneStatus::Running,
                Verdict::Idle => PaneStatus::Idle,
                Verdict::Blocked => PaneStatus::Blocked,
            };
            if status == before {
                continue;
            }

            let _ = self.state.set_pane_status(id, status);
            changed = true;

            let graced = now.saturating_duration_since(adopted) < GRACE;
            if before == PaneStatus::Running
                && status == PaneStatus::Idle
                && focused != Some(id)
                && !graced
            {
                self.state.mark_unseen(id);
            }
        }

        changed
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch`
Expected: all pass, the 8 new ones included. An existing test that asserted a pane's status stays `Starting` indefinitely may now see `Idle` after a `poll_panes` — correct it only if that is the whole reason, and say so in the report.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add dispatch
git commit -m "feat(dispatch): work out whether each pane is working, idle or blocked"
```

---

### Task 7: The `motion` setting, read by the client

**Files:**
- Modify: `crates/dispatch-config/src/config.rs`, `crates/dispatch-config/src/config/tests.rs`
- Modify: `crates/dispatch-config/src/lib.rs` (re-export)
- Modify: `dispatch/src/main.rs`, `dispatch/src/app.rs`

**Interfaces:**
- Produces: `dispatch_config::InterfaceConfig { pub motion: bool }` (default `true`), `Config::interface`; `App::set_motion(&mut self, on: bool)` and a private `App::motion: bool` (default `true`) that later tasks read.

- [ ] **Step 1: Write the failing tests**

In `crates/dispatch-config/src/config/tests.rs` (follow its existing helpers for writing a temporary config file; the ones below use `toml::from_str` directly for the typed parse and `Config::load_reporting` for unknown keys, writing through the file's existing scratch-file helper):

```rust
#[test]
fn motion_is_on_unless_turned_off() {
    assert!(Config::default().interface.motion);

    let config: Config = toml::from_str("[interface]\nmotion = false\n").expect("parses");
    assert!(!config.interface.motion);
}

#[test]
fn the_interface_section_is_not_reported_unknown() {
    let raw: toml::Table = toml::from_str("[interface]\nmotion = false\n").expect("parses");
    assert!(unknown_keys(&raw).is_empty());

    let raw: toml::Table = toml::from_str("[interface]\nsparkles = true\n").expect("parses");
    assert_eq!(unknown_keys(&raw), vec!["interface.sparkles".to_string()]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-config config::`
Expected: compile error — no field `interface`.

- [ ] **Step 3: Implement**

`config.rs`:

```rust
/// How the interface draws itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceConfig {
    /// Whether things move: spinners, pulses, easing, transitions. Off, every
    /// change is shown at once and nothing animates.
    pub motion: bool,
}

impl Default for InterfaceConfig {
    fn default() -> Self {
        Self { motion: true }
    }
}
```

`Config` gains `/// How the interface draws itself. The daemon ignores it. pub interface: InterfaceConfig,`. In `unknown_keys`, add an arm beside `delegation`:

```rust
            ("interface", toml::Value::Table(table)) => {
                for key in table.keys() {
                    if key != "motion" {
                        unknown.push(format!("interface.{key}"));
                    }
                }
            }
```

Re-export `InterfaceConfig` from `lib.rs`.

`App` gains `motion: bool,` (`true` in `App::new`) and:

```rust
    /// Turns motion on or off: spinners, pulses, easing and transitions.
    pub fn set_motion(&mut self, on: bool) {
        self.motion = on;
    }
```

`main.rs`: read the config before the terminal is taken over, so a bad file is reported readably — just before `install_panic_hook();`:

```rust
    // Only `[interface]` is the client's; the daemon reads the rest. Unknown
    // keys are logged rather than fatal, as the daemon does.
    let config_path =
        dispatch_os::paths::config_file().context("failed to locate the configuration file")?;
    let loaded = dispatch_config::Config::load_reporting(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    if !loaded.unknown.is_empty() {
        tracing::warn!(keys = ?loaded.unknown, path = %config_path.display(), "ignoring unknown configuration keys");
    }
    app.set_motion(loaded.config.interface.motion);
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-config && cargo build --workspace`
Expected: all pass; builds.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-config dispatch
git commit -m "feat(config): a setting to turn the interface's motion off"
```

---

### Task 8: Showing the state — glyphs, spinner, rollups, tabs, the status row

**Files:**
- Modify: `crates/dispatch-tui/src/sidebar.rs`, `crates/dispatch-tui/src/sidebar/tests.rs`
- Modify: `dispatch/src/app.rs`

**Interfaces:**
- Consumes: `PaneStatus::Blocked`, `AppState::is_unseen` (Task 5); `App::motion` (Task 7); `App::now` (Task 6).
- Produces (`dispatch_tui::sidebar`): `UNSEEN: &str = "\u{f058}"`; `SPINNER: [&str; 10]`; `Sidebar::with_spinner(self, frame: Option<usize>) -> Self` (`None` = static play glyph); `enum Rollup { Working, Done, Blocked }` (`Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord`) with `Rollup::of<'p>(state: &AppState, panes: impl IntoIterator<Item = &'p Pane>) -> Option<Rollup>` and `Rollup::glyph(self, spinner: Option<usize>, theme: &Theme) -> (&'static str, Style)`. `App::spinner_frame(&self) -> Option<usize>` and `App::started: Instant` (private).

- [ ] **Step 1: Write the failing tests**

`sidebar/tests.rs`:

```rust
fn render_spinning(state: &AppState, frame: Option<usize>) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, 8);
    let mut buf = Buffer::empty(area);
    Sidebar::new(state).with_spinner(frame).render(area, &mut buf);
    buf
}

#[test]
fn a_working_pane_spins() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state.set_pane_status(pane, PaneStatus::Running).expect("pane exists");

    assert_eq!(state_cell(&render_spinning(&state, Some(0)), TOP + 1).0, SPINNER[0]);
    assert_eq!(state_cell(&render_spinning(&state, Some(3)), TOP + 1).0, SPINNER[3]);
    assert_eq!(
        state_cell(&render_spinning(&state, Some(13)), TOP + 1).0,
        SPINNER[3],
        "the frame wraps"
    );
    assert_eq!(
        state_cell(&render_spinning(&state, None), TOP + 1),
        (RUNNING.to_string(), Color::Green),
        "with motion off, the still play glyph"
    );
}

#[test]
fn an_idle_pane_is_faded_and_one_finished_out_of_sight_is_marked() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state.set_pane_status(pane, PaneStatus::Idle).expect("pane exists");

    assert_eq!(
        state_cell(&render(&state, WIDTH, 6), TOP + 1),
        (IDLE.to_string(), Theme::fallback().faded)
    );

    state.mark_unseen(pane);
    assert_eq!(
        state_cell(&render(&state, WIDTH, 6), TOP + 1),
        (UNSEEN.to_string(), Theme::fallback().accent)
    );
}

#[test]
fn a_folded_project_shows_its_most_urgent_pane() {
    let (mut state, alpha, _) = state();
    let working = spawn(&mut state, alpha, "claude");
    let done = spawn(&mut state, alpha, "codex");
    let blocked = spawn(&mut state, alpha, "opencode");
    state.set_pane_status(working, PaneStatus::Running).expect("exists");
    state.set_pane_status(done, PaneStatus::Idle).expect("exists");
    state.mark_unseen(done);
    state.set_pane_status(blocked, PaneStatus::Blocked).expect("exists");

    state.toggle_project_collapsed(alpha);
    assert_eq!(state_cell(&render(&state, WIDTH, 6), TOP).0, BLOCKED);

    state.set_pane_status(blocked, PaneStatus::Idle).expect("exists");
    assert_eq!(state_cell(&render(&state, WIDTH, 6), TOP).0, UNSEEN);

    state.mark_seen(done);
    assert_eq!(
        state_cell(&render_spinning(&state, Some(2)), TOP).0,
        SPINNER[2]
    );
}

#[test]
fn an_open_project_and_an_idle_one_show_no_rollup() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    state.set_pane_status(pane, PaneStatus::Blocked).expect("exists");

    assert_eq!(state_cell(&render(&state, WIDTH, 6), TOP).0, " ", "open: its panes say it");

    state.set_pane_status(pane, PaneStatus::Idle).expect("exists");
    state.toggle_project_collapsed(alpha);
    assert_eq!(state_cell(&render(&state, WIDTH, 6), TOP).0, " ", "nothing to say");
}
```

`app.rs` tests:

```rust
    #[test]
    fn a_tab_is_prefixed_with_its_most_urgent_state() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 5);
        app.state
            .set_pane_status(panes[1], PaneStatus::Blocked)
            .expect("exists");

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        terminal.draw(|frame| app.draw(frame)).expect("the frame is drawn");
        let text = rendered_text(&terminal);
        let top: String = text
            .lines()
            .next()
            .expect("a row")
            .chars()
            .skip(sidebar::WIDTH as usize)
            .collect();

        assert!(top.contains(&format!("{} 1 ", sidebar::BLOCKED)), "{top:?}");
        assert!(!top.contains(&format!("{} 2 ", sidebar::BLOCKED)), "tab 2 has nothing blocked: {top:?}");
    }

    #[test]
    fn the_status_row_counts_panes_waiting_on_the_user() {
        let (mut app, project, daemon, _sent) = attached_app();
        let panes = spawn_several(&mut app, &daemon, project, 3);
        for pane in &panes[..2] {
            app.state.set_pane_status(*pane, PaneStatus::Blocked).expect("exists");
        }

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30))
            .expect("a test backend can be created");
        terminal.draw(|frame| app.draw(frame)).expect("the frame is drawn");
        let text = rendered_text(&terminal);

        assert!(text.lines().last().expect("a row").contains("2 waiting on you"), "{text}");
    }

    #[test]
    fn the_spinner_follows_the_clock_and_stops_with_motion_off() {
        let (mut app, _, _, _) = attached_app();
        let clock = hand_clock(&mut app);
        app.started = clock.get();

        advance(&clock, Duration::from_millis(350));
        assert_eq!(app.spinner_frame(), Some(3));

        app.set_motion(false);
        assert_eq!(app.spinner_frame(), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui sidebar:: && cargo test -p dispatch -- tab_is_prefixed status_row_counts spinner_follows`
Expected: compile errors — `SPINNER`, `UNSEEN`, `with_spinner`, `spinner_frame`, `started` not found.

- [ ] **Step 3: Implement the sidebar**

Constants after `BLOCKED`:

```rust
/// Finished while the user was looking elsewhere, until they look.
pub const UNSEEN: &str = "\u{f058}";

/// A working pane's glyph, one frame per tenth of a second.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
```

`Sidebar` gains `spinner: Option<usize>` (`None` in `new`) and:

```rust
    /// Draws working panes with spinner frame `frame`; `None` draws the still
    /// play glyph, as with motion off.
    #[must_use]
    pub fn with_spinner(mut self, frame: Option<usize>) -> Self {
        self.spinner = frame;
        self
    }
```

`state_glyph` becomes:

```rust
/// What a pane's state looks like, and the colour it is drawn in.
///
/// A tombstone reports being closed whatever its process did: a closed row
/// with live work beneath it has to look different from one that is merely
/// finished.
fn state_glyph(
    pane: &Pane,
    unseen: bool,
    spinner: Option<usize>,
    theme: &Theme,
) -> (&'static str, Style) {
    if pane.closed {
        return (CLOSED, Style::default().fg(theme.faded));
    }

    match pane.status {
        PaneStatus::Starting => (STARTING, Style::default().fg(Color::Yellow)),
        PaneStatus::Running => Rollup::Working.glyph(spinner, theme),
        PaneStatus::Blocked => Rollup::Blocked.glyph(spinner, theme),
        PaneStatus::Idle if unseen => Rollup::Done.glyph(spinner, theme),
        PaneStatus::Idle => (IDLE, Style::default().fg(theme.faded)),
        // A pane that exited stays listed until it is closed, so it has to be
        // visibly different from one that is still working.
        PaneStatus::Exited(0) => (DONE, Style::default().fg(theme.faded)),
        PaneStatus::Exited(_) => (FAILED, Style::default().fg(Color::Red)),
    }
}

/// The most urgent thing a group of panes is doing, for a row or a tab that
/// stands for them. Ordered by urgency, so the largest wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rollup {
    /// Something is working.
    Working,
    /// Something finished while the user was looking elsewhere.
    Done,
    /// Something is waiting on the user.
    Blocked,
}

impl Rollup {
    /// The most urgent state among `panes`; `None` when they are all idle,
    /// starting, exited or closed — nothing worth drawing for the group.
    #[must_use]
    pub fn of<'p>(state: &AppState, panes: impl IntoIterator<Item = &'p Pane>) -> Option<Rollup> {
        panes
            .into_iter()
            .filter(|pane| !pane.closed)
            .filter_map(|pane| match pane.status {
                PaneStatus::Blocked => Some(Rollup::Blocked),
                PaneStatus::Idle if state.is_unseen(pane.id) => Some(Rollup::Done),
                PaneStatus::Running => Some(Rollup::Working),
                _ => None,
            })
            .max()
    }

    /// Its glyph and colour.
    #[must_use]
    pub fn glyph(self, spinner: Option<usize>, theme: &Theme) -> (&'static str, Style) {
        match self {
            Rollup::Working => (
                spinner.map_or(RUNNING, |frame| SPINNER[frame % SPINNER.len()]),
                Style::default().fg(Color::Green),
            ),
            Rollup::Done => (UNSEEN, Style::default().fg(theme.accent)),
            Rollup::Blocked => (
                BLOCKED,
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
        }
    }
}
```

`render_pane` calls `state_glyph(pane, self.state.is_unseen(pane.id), self.spinner, &self.theme)`. In `render_project`, replace the lines that write the name with:

```rust
        // A folded project stands for its panes, so it carries the most
        // urgent of their states where a pane row carries its own; an open
        // one leaves that to the rows below.
        let rollup = collapsed
            .then(|| Rollup::of(self.state, self.state.panes_for(id)))
            .flatten();
        let state_x = (area.x + area.width).saturating_sub(2);

        let name_x = x + NAME;
        let right = if rollup.is_some() {
            // The name stops a blank short of the glyph, as a pane title does.
            state_x.saturating_sub(1)
        } else {
            area.x + area.width
        };
        let room = right.saturating_sub(name_x) as usize;
        write(buf, area, name_x, y, &truncate(&project.name, room), style);

        if let Some(rollup) = rollup {
            let (glyph, glyph_style) = rollup.glyph(self.spinner, &self.theme);
            write(buf, area, state_x, y, glyph, glyph_style);
        }
```

- [ ] **Step 4: Implement the app side**

`App` gains `started: Instant,` (`Instant::now()` in `App::new`) and:

```rust
    /// Which spinner frame a working pane shows now, from the clock, so every
    /// spinner on screen turns in step; `None` with motion off.
    fn spinner_frame(&self) -> Option<usize> {
        self.motion.then(|| {
            let elapsed = self.now().saturating_duration_since(self.started);
            usize::try_from(elapsed.as_millis() / 100).unwrap_or(0) % sidebar::SPINNER.len()
        })
    }
```

In `draw`, the sidebar gets `.with_spinner(self.spinner_frame())`.

In `draw_tabs`, for each tab compute its panes (`tileable.chunks(PANES_PER_TAB).nth(index)`), their rollup, and prefix the label:

```rust
            let panes = tileable
                .chunks(PANES_PER_TAB)
                .nth(index)
                .unwrap_or_default();
            let rollup = Rollup::of(&self.state, panes.iter().filter_map(|id| self.state.pane(*id)));

            if index > 0 {
                spans.push(Span::raw(" "));
            }
            if let Some(rollup) = rollup {
                let (glyph, glyph_style) = rollup.glyph(self.spinner_frame(), &self.theme);
                spans.push(Span::styled(" ", style));
                spans.push(Span::styled(glyph, style.patch(glyph_style)));
            }
            spans.push(Span::styled(label, style));
```

(`style` is the tab's own style, computed before these lines; keep `label` as today — it already starts with a space, which becomes the gap after the glyph. Import `sidebar::Rollup`.)

In `draw_status`, beside `waiting_reminder`:

```rust
        // A blocked pane on another tab, or in a folded project, still needs
        // to be found; the status row is the one place always on screen.
        let blocked = self
            .state
            .projects()
            .iter()
            .flat_map(|project| self.state.panes_for(project.id))
            .filter(|pane| !pane.closed && pane.status == PaneStatus::Blocked)
            .count();
        let blocked_reminder =
            (blocked > 0 && self.overlay.is_none()).then(|| format!("{blocked} waiting on you"));
```

and append it after the delegation reminder in the final `text` the same way (`format!("{base}  {reminder}")`), both when present.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: all pass. The existing `a_panes_state_is_one_glyph_at_the_end_of_its_row` expects the still `RUNNING` glyph with no spinner set — it still passes, since a bare `Sidebar::new` has `spinner: None`. `every_state_has_a_glyph_of_its_own` should gain `BLOCKED` and `UNSEEN` in its list.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui dispatch
git commit -m "feat(tui): show what each pane is doing, and roll it up onto projects and tabs"
```

---

### Task 9: The motion engine and frame pacing

**Files:**
- Create: `crates/dispatch-tui/src/motion.rs`, `crates/dispatch-tui/src/motion/tests.rs`
- Modify: `crates/dispatch-tui/src/lib.rs`, `crates/dispatch-tui/src/theme.rs`, `crates/dispatch-tui/src/theme/tests.rs`
- Modify: `dispatch/src/app.rs`, `dispatch/src/main.rs`

**Interfaces:**
- Produces (`dispatch_tui::motion`): `Tween { start, duration }` with `new(start: Instant, duration: Duration)`, `linear(&self, now) -> f32`, `progress(&self, now) -> f32` (ease-out cubic), `done(&self, now) -> bool`; `Animations<K: Copy + Eq + Hash>` with `new(enabled: bool)`, `set_enabled(&mut self, bool)`, `enabled(&self) -> bool`, `start(&mut self, key: K, now: Instant, duration: Duration, from: f32)`, `stop(&mut self, key: K)`, `value(&self, key: K, now: Instant) -> Option<f32>` (from eased toward 1.0), `linear(&self, key: K, now: Instant) -> Option<f32>`, `active(&self, now: Instant) -> bool`, `sweep(&mut self, now: Instant) -> Vec<K>`. Constants `TWEEN_FRAME` (33 ms), `SPIN_FRAME` (100 ms). (`dispatch_tui::theme`): `Role { Background, Faded, Tint, Tab, Accent, Pulse }`; `Theme::rgb(&self, role: Role) -> Rgb`; `Theme::blend(&self, from: Rgb, to: Rgb, t: f32) -> Color`. (`dispatch::app`): private `enum Target` and `App::animations: Animations<Target>`; `App::next_frame(&self, now: Instant) -> Option<Duration>`; `set_motion` also enables/disables `animations`.

- [ ] **Step 1: Write the failing tests**

`motion/tests.rs`:

```rust
//! Tests for tweens and the animation store.

use std::time::{Duration, Instant};

use super::*;

const MS: fn(u64) -> Duration = Duration::from_millis;

#[test]
fn a_tween_runs_from_zero_to_one_and_eases_out() {
    let start = Instant::now();
    let tween = Tween::new(start, MS(100));

    assert_eq!(tween.progress(start), 0.0);
    assert_eq!(tween.progress(start + MS(100)), 1.0);
    assert_eq!(tween.progress(start + MS(500)), 1.0, "held at the end");
    assert!(tween.progress(start + MS(50)) > tween.linear(start + MS(50)), "eased out: fast first");
    assert!(!tween.done(start + MS(99)));
    assert!(tween.done(start + MS(100)));
}

#[test]
fn a_zero_length_tween_is_already_done() {
    let start = Instant::now();
    assert!(Tween::new(start, Duration::ZERO).done(start));
}

#[test]
fn a_value_runs_from_where_it_started_to_one() {
    let start = Instant::now();
    let mut animations = Animations::new(true);
    animations.start('a', start, MS(100), 0.5);

    assert_eq!(animations.value('a', start), Some(0.5));
    assert_eq!(animations.value('a', start + MS(100)), Some(1.0));
    assert_eq!(animations.value('b', start), None, "nothing runs on it");
}

#[test]
fn starting_again_replaces_the_running_one() {
    let start = Instant::now();
    let mut animations = Animations::new(true);
    animations.start('a', start, MS(100), 0.0);
    let halfway = animations.value('a', start + MS(50)).expect("running");

    animations.start('a', start + MS(50), MS(100), halfway);
    assert_eq!(
        animations.value('a', start + MS(50)),
        Some(halfway),
        "it continues from where it had got to"
    );
}

#[test]
fn finished_animations_are_swept_up() {
    let start = Instant::now();
    let mut animations = Animations::new(true);
    animations.start('a', start, MS(100), 0.0);
    animations.start('b', start, MS(300), 0.0);

    assert!(animations.active(start + MS(200)));
    assert_eq!(animations.sweep(start + MS(200)), vec!['a']);
    assert_eq!(animations.value('a', start + MS(200)), None);
    assert_eq!(animations.sweep(start + MS(400)), vec!['b']);
    assert!(!animations.active(start + MS(400)));
}

#[test]
fn with_motion_off_nothing_starts() {
    let start = Instant::now();
    let mut animations = Animations::new(false);
    animations.start('a', start, MS(100), 0.0);

    assert_eq!(animations.value('a', start), None);
    assert!(!animations.active(start));
}
```

`theme/tests.rs`:

```rust
#[test]
fn a_blend_runs_between_two_colours_at_the_themes_depth() {
    let theme = Theme::fallback();
    let (from, to) = (theme.rgb(Role::Faded), theme.rgb(Role::Accent));

    assert_eq!(theme.blend(from, to, 0.0), Color::Rgb(from.0, from.1, from.2));
    assert_eq!(theme.blend(from, to, 1.0), theme.accent);
    assert!(matches!(
        Theme::new(Palette::FALLBACK, Depth::Indexed).blend(from, to, 0.5),
        Color::Indexed(_)
    ));
}

#[test]
fn the_pulse_colour_is_the_background_most_of_the_way_to_the_accent() {
    let theme = Theme::fallback();
    let palette = Palette::FALLBACK;

    assert_eq!(theme.rgb(Role::Pulse), palette.background.mix(palette.accent, 0.55));
    assert_eq!(theme.rgb(Role::Background), palette.background);
}
```

`app.rs` tests:

```rust
    #[test]
    fn a_still_interface_asks_for_no_frames() {
        let (mut app, project, daemon, _sent) = attached_app();
        let pane = spawn_several(&mut app, &daemon, project, 1)[0];
        let now = app.now();
        app.animations.sweep(now + Duration::from_secs(10));
        app.state.set_pane_status(pane, PaneStatus::Idle).expect("exists");

        assert_eq!(app.next_frame(now + Duration::from_secs(10)), None);
    }

    #[test]
    fn a_spinner_asks_for_a_frame_a_tenth_of_a_second() {
        let (mut app, project, daemon, _sent) = attached_app();
        let pane = spawn_several(&mut app, &daemon, project, 1)[0];
        let later = app.now() + Duration::from_secs(10);
        app.animations.sweep(later);
        app.state.set_pane_status(pane, PaneStatus::Running).expect("exists");

        assert_eq!(app.next_frame(later), Some(Duration::from_millis(100)));

        app.set_motion(false);
        assert_eq!(app.next_frame(later), None, "a still glyph needs no frames");
    }

    #[test]
    fn a_running_tween_asks_for_thirty_frames_a_second() {
        let (mut app, _, _, _) = attached_app();
        let now = app.now();
        app.animations
            .start(Target::Glide, now, Duration::from_millis(150), 0.0);

        assert_eq!(app.next_frame(now), Some(Duration::from_millis(33)));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui motion:: theme:: && cargo test -p dispatch -- still_interface spinner_asks running_tween`
Expected: compile errors — `Tween`, `Animations`, `Role`, `blend`, `Target`, `next_frame` not found.

- [ ] **Step 3: Implement the engine**

`crates/dispatch-tui/src/motion.rs`:

```rust
//! Time-based motion: a tween, and a store of running ones keyed by what they
//! animate.
//!
//! Everything that moves reads its progress from here at draw time, so one
//! clock drives every animation and nothing keeps state of its own between
//! frames.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// How often to draw while something is tweening: about thirty frames a
/// second, smooth for a 150 ms ease without redrawing at the terminal's
/// full rate.
pub const TWEEN_FRAME: Duration = Duration::from_millis(33);

/// How often to draw while a spinner is on screen: one braille frame each.
pub const SPIN_FRAME: Duration = Duration::from_millis(100);

/// One stretch of time, and how far through it a moment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tween {
    start: Instant,
    duration: Duration,
}

impl Tween {
    /// A tween starting at `start` and lasting `duration`.
    #[must_use]
    pub fn new(start: Instant, duration: Duration) -> Self {
        Self { start, duration }
    }

    /// How far through it `now` is, evenly: 0.0 at the start, 1.0 at and
    /// after the end.
    #[must_use]
    pub fn linear(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(self.start).as_secs_f32();
        (elapsed / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// How far through it `now` is, eased out: quick to start, gentle to
    /// land, which is how something moving under its own momentum reads.
    #[must_use]
    pub fn progress(&self, now: Instant) -> f32 {
        1.0 - (1.0 - self.linear(now)).powi(3)
    }

    /// Whether it has run its course.
    #[must_use]
    pub fn done(&self, now: Instant) -> bool {
        self.linear(now) >= 1.0
    }
}

/// Running tweens, one per key.
///
/// Starting a tween on a key that already has one replaces it; the caller
/// passes the value the old one had reached as the new one's `from`, so a
/// quick succession of changes continues smoothly rather than queuing or
/// snapping back.
#[derive(Debug)]
pub struct Animations<K> {
    enabled: bool,
    running: HashMap<K, (Tween, f32)>,
}

impl<K: Copy + Eq + Hash> Animations<K> {
    /// A store that animates when `enabled`, and otherwise starts nothing, so
    /// every change is shown at once.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            running: HashMap::new(),
        }
    }

    /// Turns motion on or off. Off, whatever runs stops where it is.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.running.clear();
        }
    }

    /// Whether motion is on.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Animates `key` from `from` (0.0–1.0, the value it shows now) to 1.0
    /// over `duration`.
    pub fn start(&mut self, key: K, now: Instant, duration: Duration, from: f32) {
        if self.enabled {
            self.running
                .insert(key, (Tween::new(now, duration), from.clamp(0.0, 1.0)));
        }
    }

    /// Stops animating `key`.
    pub fn stop(&mut self, key: K) {
        self.running.remove(&key);
    }

    /// Where `key` has got to, eased; `None` when nothing runs on it, which
    /// the caller reads as "at rest".
    #[must_use]
    pub fn value(&self, key: K, now: Instant) -> Option<f32> {
        self.running
            .get(&key)
            .map(|(tween, from)| from + (1.0 - from) * tween.progress(now))
    }

    /// How far through `key`'s tween `now` is, evenly, for an animation with
    /// a shape of its own.
    #[must_use]
    pub fn linear(&self, key: K, now: Instant) -> Option<f32> {
        self.running.get(&key).map(|(tween, _)| tween.linear(now))
    }

    /// Whether anything is still moving at `now`.
    #[must_use]
    pub fn active(&self, now: Instant) -> bool {
        self.running.values().any(|(tween, _)| !tween.done(now))
    }

    /// Forgets the tweens that have finished by `now`, returning their keys.
    pub fn sweep(&mut self, now: Instant) -> Vec<K> {
        let finished: Vec<K> = self
            .running
            .iter()
            .filter(|(_, (tween, _))| tween.done(now))
            .map(|(key, _)| *key)
            .collect();
        for key in &finished {
            self.running.remove(key);
        }
        finished
    }
}

#[cfg(test)]
mod tests;
```

(Order of `sweep`'s result: `HashMap` iteration is unordered; the test sweeps one key at a time, so it holds.) Add `pub mod motion;` to `lib.rs`.

`theme.rs`: `Theme` gains two private fields, `palette: Palette` and `depth: Depth`, set in `Theme::new`; and:

```rust
/// A colour the theme is mixed from, for animations that move between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The terminal's background.
    Background,
    /// Secondary text and unfocused borders.
    Faded,
    /// A selected or focused row.
    Tint,
    /// The active tab.
    Tab,
    /// The focused pane's border.
    Accent,
    /// The peak of an attention pulse: the background most of the way to the
    /// accent.
    Pulse,
}

impl Theme {
    /// The 24-bit colour behind `role`, before it is drawn at the theme's
    /// depth.
    #[must_use]
    pub fn rgb(&self, role: Role) -> Rgb {
        let p = self.palette;
        match role {
            Role::Background => p.background,
            Role::Faded => p.foreground.mix(p.background, 0.45),
            Role::Tint => p.background.mix(p.foreground, 0.10),
            Role::Tab => p.background.mix(p.accent, 0.30),
            Role::Accent => p.accent,
            Role::Pulse => p.background.mix(p.accent, 0.55),
        }
    }

    /// `from` moved `t` of the way toward `to`, drawn at this theme's depth —
    /// so a 256-colour terminal steps through the nearest entries rather than
    /// being sent colours it would misread.
    #[must_use]
    pub fn blend(&self, from: Rgb, to: Rgb, t: f32) -> Color {
        let mixed = from.mix(to, t.clamp(0.0, 1.0));
        match self.depth {
            Depth::TrueColor => Color::Rgb(mixed.0, mixed.1, mixed.2),
            Depth::Indexed => Color::Indexed(nearest_indexed(mixed)),
        }
    }
}
```

(At rest, draw with the theme's own role colours rather than `blend(…, 1.0)`: in 256 colours the accent is `Indexed(5)`, which a blend would only approximate.)

- [ ] **Step 4: Implement the app side**

In `app.rs`:

```rust
/// What an animation animates, so a new one on the same thing replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Target {
    /// A pane's border easing toward the accent: it has just been focused.
    Focus(PaneId),
    /// A pane's border easing back to faded: focus has just left it.
    Blur(PaneId),
    /// The sidebar's focus tint moving to the newly focused row.
    Glide,
    /// A pane's sidebar row pulsing for attention.
    Pulse(PaneId),
    /// A new pane's border being drawn in.
    Open(PaneId),
    /// A closed pane's tile retracting.
    Close(PaneId),
    /// The active tab's tint sliding to the newly active tab.
    Tab,
}
```

`App` gains `animations: Animations<Target>,` (`Animations::new(true)` in `App::new`); `set_motion` also calls `self.animations.set_enabled(on);`.

```rust
    /// How long until the next frame something on screen needs: a tween
    /// running needs about thirty a second, a spinner ten, and nothing
    /// moving needs none — a still Dispatch draws only when something
    /// changes.
    #[must_use]
    pub fn next_frame(&self, now: Instant) -> Option<Duration> {
        if self.animations.active(now) {
            return Some(TWEEN_FRAME);
        }

        let spinning = self.motion
            && self
                .state
                .projects()
                .iter()
                .flat_map(|project| self.state.panes_for(project.id))
                .any(|pane| !pane.closed && pane.status == PaneStatus::Running);

        spinning.then_some(SPIN_FRAME)
    }
```

(import `dispatch_tui::motion::{Animations, SPIN_FRAME, TWEEN_FRAME}`). At the top of `draw`, after `let area = frame.area();`: `let now = self.now(); self.animations.sweep(now);` (Task 11 acts on what the sweep returns).

`main.rs`, in `run`, after the `app.poll_daemon()` block:

```rust
        // Something on screen is moving: draw its next frame once it is due,
        // input or not.
        if app
            .next_frame(Instant::now())
            .is_some_and(|every| last_draw.elapsed() >= every)
        {
            needs_draw = true;
        }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui dispatch
git commit -m "feat(tui): tweens, an animation store, and frames only while something moves"
```

---

### Task 10: Attention pulse and focus easing

**Files:**
- Modify: `crates/dispatch-tui/src/sidebar.rs`, `crates/dispatch-tui/src/sidebar/tests.rs`
- Modify: `dispatch/src/app.rs`
- Modify: `docs/superpowers/specs/2026-09-25-live-status-motion-design.md` (glide replacement wording)

**Interfaces:**
- Consumes: `Animations`, `Target`, `Theme::{rgb, blend}`, `Role` (Task 9); `refresh_activity` (Task 6); `Anchor` (sidebar, A).
- Produces (`dispatch_tui::sidebar`): `SidebarMotion { pub pulses: Vec<(PaneId, f32)>, pub glide: Option<Glide> }` (`Debug, Clone, Default`); `Glide { pub from: Anchor, pub t: f32 }` (`Debug, Clone, Copy, PartialEq`); `Sidebar::with_motion(self, motion: &'a SidebarMotion) -> Self`; `pub fn pulse_strength(t: f32) -> f32`. (`app`) constants `EASE` (150 ms), `PULSE` (1.2 s); fields `last_focus: Option<PaneId>`, `last_anchor: Option<Anchor>`, `glide_from: Option<Anchor>`; `fn border_level(&self, id, now) -> f32`; `pane_block(colour: Color, bold: bool) -> Block<'static>`.

- [ ] **Step 1: Write the failing tests**

`sidebar/tests.rs`:

```rust
fn render_moving(state: &AppState, motion: &SidebarMotion, height: u16) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, height);
    let mut buf = Buffer::empty(area);
    Sidebar::new(state).with_motion(motion).render(area, &mut buf);
    buf
}

#[test]
fn a_pulse_rises_and_falls_three_times() {
    assert_eq!(pulse_strength(0.0), 0.0);
    assert!(pulse_strength(1.0 / 6.0) > 0.99, "the first peak");
    assert!(pulse_strength(1.0 / 3.0) < 0.01, "the first trough");
    assert!(pulse_strength(1.0) < 0.01, "and it settles");
}

#[test]
fn a_pulsing_row_is_drawn_toward_the_pulse_colour() {
    let (mut state, alpha, _) = state();
    let pane = spawn(&mut state, alpha, "claude");
    spawn(&mut state, alpha, "codex"); // focus moves off the first
    let theme = Theme::fallback();
    let motion = SidebarMotion {
        pulses: vec![(pane, 1.0)],
        glide: None,
    };

    let buf = render_moving(&state, &motion, 6);

    assert_eq!(
        buf.cell((LEFT + 8, TOP + 1)).expect("cell exists").bg,
        theme.blend(theme.rgb(Role::Background), theme.rgb(Role::Pulse), 1.0)
    );
}

#[test]
fn the_focus_tint_glides_through_the_rows_between() {
    let (mut state, alpha, _) = state();
    let first = spawn(&mut state, alpha, "claude");
    spawn(&mut state, alpha, "codex");
    let last = spawn(&mut state, alpha, "opencode"); // focused
    let tint = Theme::fallback().tint;
    let motion = SidebarMotion {
        pulses: Vec::new(),
        glide: Some(Glide {
            from: Anchor::Pane(first),
            t: 0.5,
        }),
    };

    let buf = render_moving(&state, &motion, 8);

    // First at TOP+1, last at TOP+3: halfway is TOP+2.
    assert_eq!(buf.cell((LEFT + 8, TOP + 2)).expect("cell").bg, tint);
    assert_ne!(buf.cell((LEFT + 8, TOP + 3)).expect("cell").bg, tint, "not arrived yet");
    let _ = last;
}

#[test]
fn a_glide_between_sections_fades_instead() {
    let (mut state, laptop, tower) = fleet();
    let on_laptop = state.projects().iter().find(|p| p.device == laptop).expect("one").id;
    let on_tower = state.projects().iter().find(|p| p.device == tower).expect("one").id;
    let from = spawn(&mut state, on_laptop, "claude");
    let _ = state.select_project(on_tower);
    spawn(&mut state, on_tower, "codex"); // focused
    let theme = Theme::fallback();
    let motion = SidebarMotion {
        pulses: Vec::new(),
        glide: Some(Glide {
            from: Anchor::Pane(from),
            t: 0.5,
        }),
    };

    let buf = render_moving(&state, &motion, 14);
    let halfway = theme.blend(theme.rgb(Role::Background), theme.rgb(Role::Tint), 0.5);
    let row_of = |text: &str| {
        (0..buf.area.height)
            .find(|y| row_text(&buf, *y).contains(text))
            .unwrap_or_else(|| panic!("{text:?} is drawn"))
    };

    assert_eq!(buf.cell((LEFT + 8, row_of("claude"))).expect("cell").bg, halfway);
    assert_eq!(buf.cell((LEFT + 8, row_of("codex"))).expect("cell").bg, halfway);
}
```

`app.rs` tests:

```rust
    /// The colour of the top-left corner of `pane`'s tile, as last drawn.
    fn corner(
        app: &App,
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        pane: PaneId,
    ) -> Color {
        let (_, rect) = app
            .frames
            .iter()
            .find(|(id, _)| *id == pane)
            .expect("the pane is tiled");
        terminal
            .backend()
            .buffer()
            .cell((rect.x, rect.y))
            .expect("the corner is on screen")
            .fg
    }

    fn drawn(app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>) {
        terminal.draw(|frame| app.draw(frame)).expect("the frame is drawn");
    }

    #[test]
    fn focus_eases_the_border_from_faded_to_accent() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        let theme = Theme::fallback();
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_secs(1));
        drawn(&mut app, &mut terminal);

        app.focus_pane(panes[0]);
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_millis(60));
        drawn(&mut app, &mut terminal);

        let (new, old) = (corner(&app, &terminal, panes[0]), corner(&app, &terminal, panes[1]));
        for colour in [new, old] {
            assert_ne!(colour, theme.faded, "mid-ease");
            assert_ne!(colour, theme.accent, "mid-ease");
        }

        advance(&clock, Duration::from_millis(200));
        drawn(&mut app, &mut terminal);
        assert_eq!(corner(&app, &terminal, panes[0]), theme.accent);
        assert_eq!(corner(&app, &terminal, panes[1]), theme.faded);
    }

    #[test]
    fn rapid_focus_changes_continue_rather_than_queue() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 3);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_secs(1));

        for pane in [panes[0], panes[1], panes[0], panes[2]] {
            app.focus_pane(pane);
            drawn(&mut app, &mut terminal);
            advance(&clock, Duration::from_millis(20));
        }

        advance(&clock, Duration::from_millis(200));
        drawn(&mut app, &mut terminal);
        let theme = Theme::fallback();
        assert_eq!(corner(&app, &terminal, panes[2]), theme.accent);
        assert_eq!(corner(&app, &terminal, panes[0]), theme.faded);
        assert_eq!(corner(&app, &terminal, panes[1]), theme.faded);
        assert!(!app.animations.active(app.now()), "nothing left running");
    }

    #[test]
    fn with_motion_off_focus_changes_at_once() {
        let (mut app, project, daemon, _sent) = attached_app();
        app.set_motion(false);
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        drawn(&mut app, &mut terminal);

        app.focus_pane(panes[0]);
        drawn(&mut app, &mut terminal);

        assert_eq!(corner(&app, &terminal, panes[0]), Theme::fallback().accent);
    }

    #[test]
    fn a_pane_turning_blocked_out_of_focus_pulses() {
        let def = dispatch_config::HarnessDef {
            id: "shell".to_string(),
            display_name: "Shell".to_string(),
            status: Some(
                toml::from_str(
                    r#"
                    [[rules]]
                    state = "blocked"
                    region = "screen"
                    contains = ["proceed?"]
                    "#,
                )
                .expect("the rules parse"),
            ),
            ..dispatch_config::HarnessDef::default()
        };
        let (client, daemon, _sent) = Client::for_test();
        let mut app = App::attached([def].into_iter().collect(), client);
        let project = Project::new("/tmp/pulse", ProjectSource::LocalDir);
        let project_id = project.id;
        daemon.send(ServerMessage::ProjectOpened { project }).expect("listening");
        app.poll_daemon();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project_id, 2);

        advance(&clock, Duration::from_secs(4));
        print(&mut app, &daemon, panes[0], b"Do you want to proceed?\r\n");

        assert!(app.animations.linear(Target::Pulse(panes[0]), app.now()).is_some());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui sidebar:: && cargo test -p dispatch -- focus_eases rapid_focus motion_off_focus turning_blocked`
Expected: compile errors — `SidebarMotion`, `Glide`, `pulse_strength`, `with_motion` not found.

- [ ] **Step 3: Implement the sidebar**

```rust
/// What moves in the sidebar this frame.
#[derive(Debug, Clone, Default)]
pub struct SidebarMotion {
    /// Rows pulsing for attention, with how strongly each shows now
    /// (0.0–1.0) — the caller turns a pulse's progress into this with
    /// [`pulse_strength`].
    pub pulses: Vec<(PaneId, f32)>,
    /// The focus tint on its way to the focused row.
    pub glide: Option<Glide>,
}

/// The focus tint moving from one row to the focused one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glide {
    /// Where it started.
    pub from: Anchor,
    /// How far it has got, eased, 0.0–1.0.
    pub t: f32,
}

/// How strongly a pulse shows `t` of the way through it: three rises and
/// falls, from nothing and back to nothing.
#[must_use]
pub fn pulse_strength(t: f32) -> f32 {
    (std::f32::consts::PI * 3.0 * t.clamp(0.0, 1.0)).sin().powi(2)
}
```

`Sidebar` gains `motion: Option<&'a SidebarMotion>` (`None` in `new`) and `with_motion`. In `render`, before the per-section row loop, the glide pre-pass:

```rust
        // The focus tint, while it moves, is drawn here rather than by the
        // row it belongs to: gliding, it is on a row that is not the focused
        // one; fading, it is on two at once.
        let glide = self.motion.and_then(|motion| motion.glide);
        if let Some(glide) = glide {
            self.render_glide(buf, &sections, glide);
        }
```

with:

```rust
    /// Draws the moving focus tint: gliding row by row when both ends are in
    /// one section's view, fading out of one and into the other otherwise.
    fn render_glide(&self, buf: &mut Buffer, sections: &[Section<'_>], glide: Glide) {
        let to = self
            .state
            .focused_pane()
            .map(Anchor::Pane)
            .or_else(|| self.state.selected_project().map(Anchor::Project));
        let Some(to) = to else {
            return;
        };

        let find = |anchor: Anchor| {
            sections.iter().enumerate().find_map(|(index, section)| {
                section
                    .visible()
                    .find(|(_, row)| row.is(anchor))
                    .map(|(y, _)| (index, y, section.body))
            })
        };
        let text = self.theme.text;

        match (find(glide.from), find(to)) {
            (Some((a, from_y, body)), Some((b, to_y, _))) if a == b => {
                let span = f32::from(to_y) - f32::from(from_y);
                let y = (f32::from(from_y) + span * glide.t).round() as u16;
                fill(buf, body, body.x, y, self.tinted());
            }
            (from, to) => {
                let (background, tint) = (self.theme.rgb(Role::Background), self.theme.rgb(Role::Tint));
                if let Some((_, y, body)) = from {
                    let colour = self.theme.blend(background, tint, 1.0 - glide.t);
                    fill(buf, body, body.x, y, Style::default().bg(colour).fg(text));
                }
                if let Some((_, y, body)) = to {
                    let colour = self.theme.blend(background, tint, glide.t);
                    fill(buf, body, body.x, y, Style::default().bg(colour).fg(text));
                }
            }
        }
    }
```

(`Row::is` exists from A; import `Role`.) While a glide runs, the glide pass draws the moving tint, so the row it stands for must not also paint its own: in `render_pane`, the focused row's `fill` becomes `if is_focused && !gliding { … }`; in `render_project`, the selected row's `fill` becomes `if is_selected && !(gliding && self.state.focused_pane().is_none()) { … }` — a project row is the glide's anchor only when no pane is focused, and otherwise keeps its tint as it does at rest. `gliding` is `self.motion.is_some_and(|motion| motion.glide.is_some())`.

In `render_pane`, after that fill and before writing text, the pulse — the pair's value is already a strength:

```rust
        if let Some(strength) = self
            .motion
            .and_then(|motion| motion.pulses.iter().find(|(id, _)| *id == pane.id))
            .map(|(_, strength)| *strength)
        {
            let colour = self.theme.blend(
                self.theme.rgb(Role::Background),
                self.theme.rgb(Role::Pulse),
                strength,
            );
            fill(buf, area, x, y, Style::default().bg(colour).fg(self.theme.text));
        }
```

- [ ] **Step 4: Implement the app side**

Constants: `const EASE: Duration = Duration::from_millis(150);` `const PULSE: Duration = Duration::from_millis(1200);`.

`App` gains `last_focus: Option<PaneId>`, `last_anchor: Option<sidebar::Anchor>`, `glide_from: Option<sidebar::Anchor>` (all `None` in `new`).

`pane_block` becomes:

```rust
/// The border drawn around one pane, in `colour`, its title bold when
/// `focused`.
///
/// Square, like every other edge in the interface. The colour is worked out
/// by the caller, which knows whether the border is easing between faded and
/// the accent.
fn pane_block(colour: Color, focused: bool) -> Block<'static> {
    let title = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    Block::bordered()
        .border_style(Style::default().fg(colour))
        .title_style(title)
}
```

```rust
    /// How far toward the accent `id`'s border is at `now`: 1.0 focused and
    /// at rest, 0.0 unfocused and at rest, between while easing.
    fn border_level(&self, id: PaneId, now: Instant) -> f32 {
        if let Some(value) = self.animations.value(Target::Focus(id), now) {
            return value;
        }
        if let Some(value) = self.animations.value(Target::Blur(id), now) {
            return 1.0 - value;
        }
        if self.state.focused_pane() == Some(id) { 1.0 } else { 0.0 }
    }

    /// Starts the easing a change of focus sets off: the new pane's border
    /// toward the accent, the old one's back to faded, and the sidebar tint
    /// from the old row to the new — each from wherever it had got to.
    fn notice_focus(&mut self, now: Instant) {
        let focus = self.state.focused_pane();
        let anchor = focus
            .map(sidebar::Anchor::Pane)
            .or_else(|| self.state.selected_project().map(sidebar::Anchor::Project));

        if focus != self.last_focus {
            if let Some(new) = focus {
                let from = self.border_level(new, now);
                self.animations.stop(Target::Blur(new));
                self.animations.start(Target::Focus(new), now, EASE, from);
            }
            if let Some(old) = self.last_focus {
                let from = 1.0 - self.border_level(old, now);
                self.animations.stop(Target::Focus(old));
                self.animations.start(Target::Blur(old), now, EASE, from);
            }
        }

        if anchor != self.last_anchor {
            // A glide replaced mid-way starts from the row the old one was
            // heading to: within 150 ms that reads as continuous.
            self.glide_from = self.last_anchor;
            if self.glide_from.is_some() {
                self.animations.start(Target::Glide, now, EASE, 0.0);
            }
        }

        self.last_focus = focus;
        self.last_anchor = anchor;
    }
```

In `draw`, right after `self.animations.sweep(now);`: `self.notice_focus(now);`. Build the sidebar motion and pass it:

```rust
        let sidebar_motion = sidebar::SidebarMotion {
            pulses: self
                .state
                .projects()
                .iter()
                .flat_map(|project| self.state.panes_for(project.id))
                .filter_map(|pane| {
                    self.animations
                        .linear(Target::Pulse(pane.id), now)
                        .map(|t| (pane.id, sidebar::pulse_strength(t)))
                })
                .collect(),
            glide: self
                .glide_from
                .zip(self.animations.value(Target::Glide, now))
                .map(|(from, t)| sidebar::Glide { from, t }),
        };
```

(`frame.render_widget(Sidebar::new(…)…​.with_motion(&sidebar_motion), sidebar_area)`.)

In `draw_panes`, the border colour:

```rust
            let level = self.border_level(*id, now);
            let colour = if self.animations.value(Target::Focus(*id), now).is_some()
                || self.animations.value(Target::Blur(*id), now).is_some()
            {
                self.theme
                    .blend(self.theme.rgb(Role::Faded), self.theme.rgb(Role::Accent), level)
            } else if is_focused {
                self.theme.accent
            } else {
                self.theme.faded
            };
            frame.render_widget(pane_block(colour, is_focused).title(pane_title(&self.state, *id)), *outer);
```

(`draw_panes` takes `now: Instant`; `draw` passes it.)

Pulse start, in `refresh_activity`: where a status changes, after the unseen marking:

```rust
            // A pane that now wants the user — blocked, or finished out of
            // sight — pulses its row so the eye finds it.
            if (status == PaneStatus::Blocked && focused != Some(id))
                || self.state.is_unseen(id) && !was_unseen
            {
                self.animations.start(Target::Pulse(id), now, PULSE, 0.0);
            }
```

(record `let was_unseen = self.state.is_unseen(id);` before the marking). Likewise in `take_output`, when a bell marks a pane unseen, start the same pulse.

Spec edit, "The animations" table's focus row: append "A glide replaced mid-way starts from the row the previous one was heading to."

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: all pass. Tests from Task 8 and A that check a focused pane's border colour at rest still pass (at rest the border is exactly `accent`/`faded`).

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui dispatch docs/superpowers/specs/2026-09-25-live-status-motion-design.md
git commit -m "feat(dispatch): pulse a pane that wants you, and ease focus as it moves"
```

---

### Task 11: Panes opening and closing, and tabs sliding

**Files:**
- Modify: `dispatch/src/app.rs`

**Interfaces:**
- Consumes: `Animations`, `Target::{Open, Close, Tab}` (Task 9); `pane_block` (Task 10).
- Produces: constants `OPEN` (200 ms), `CLOSE` (150 ms), `SLIDE` (150 ms); fields `held: Option<(Rect, Vec<(PaneId, Rect)>)>`, `closing: Vec<(PaneId, Rect)>`, `last_tab: usize`, `tab_from: usize`; `fn perimeter(rect: Rect) -> Vec<(u16, u16)>` (clockwise from the top-left corner); `fn mask_border(buf: &mut Buffer, rect: Rect, shown: f32)`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn the_perimeter_runs_clockwise_from_the_top_left() {
        let path = perimeter(Rect::new(0, 0, 3, 3));

        assert_eq!(
            path,
            vec![(0, 0), (1, 0), (2, 0), (2, 1), (2, 2), (1, 2), (0, 2), (0, 1)]
        );
    }

    #[test]
    fn a_new_panes_border_is_drawn_in() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let pane = spawn_several(&mut app, &daemon, project, 1)[0];
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");

        // 40 ms of 200 is a fifth of the way; eased out, a little under half
        // of the border is drawn — well short of the bottom-left corner,
        // which is about five-sixths of the way round.
        advance(&clock, Duration::from_millis(40));
        drawn(&mut app, &mut terminal);
        let (_, rect) = *app.frames.iter().find(|(id, _)| *id == pane).expect("tiled");
        let buf = terminal.backend().buffer();

        assert_eq!(buf.cell((rect.x, rect.y)).expect("cell").symbol(), "┌", "the sweep starts at the corner");
        assert_eq!(
            buf.cell((rect.x, rect.y + rect.height - 1)).expect("cell").symbol(),
            " ",
            "the bottom-left corner is the last to be drawn"
        );

        advance(&clock, Duration::from_millis(200));
        drawn(&mut app, &mut terminal);
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((rect.x, rect.y + rect.height - 1)).expect("cell").symbol(), "└");
    }

    #[test]
    fn a_closed_panes_tile_holds_the_grid_until_it_has_retracted() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_secs(1));
        drawn(&mut app, &mut terminal);
        let before: Vec<(PaneId, Rect)> = app.frames.clone();

        app.close_focused(); // panes[1]
        drawn(&mut app, &mut terminal);
        let kept = app.frames.iter().find(|(id, _)| *id == panes[0]).expect("tiled").1;
        assert_eq!(
            kept,
            before.iter().find(|(id, _)| *id == panes[0]).expect("was tiled").1,
            "the other pane keeps its tile while the closed one retracts"
        );
        assert!(!app.frames.iter().any(|(id, _)| *id == panes[1]), "the closed pane takes no input");

        advance(&clock, Duration::from_millis(200));
        drawn(&mut app, &mut terminal);
        let reflowed = app.frames.iter().find(|(id, _)| *id == panes[0]).expect("tiled").1;
        assert!(reflowed.width > kept.width, "then the grid reflows");
    }

    #[test]
    fn closing_two_panes_in_quick_succession_reflows_once() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 3);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_secs(1));
        drawn(&mut app, &mut terminal);
        let before = app.frames.iter().find(|(id, _)| *id == panes[0]).expect("tiled").1;

        app.close_focused();
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_millis(50));
        let _ = app.state.focus(panes[1]);
        app.close_focused();
        drawn(&mut app, &mut terminal);
        assert_eq!(
            app.frames.iter().find(|(id, _)| *id == panes[0]).expect("tiled").1,
            before,
            "still held while either tile retracts"
        );

        advance(&clock, Duration::from_millis(300));
        drawn(&mut app, &mut terminal);
        let _ = app.state.focus(panes[0]);
        app.close_focused();
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_millis(300));
        drawn(&mut app, &mut terminal);
        assert!(app.frames.is_empty(), "an empty grid is fine");
    }

    #[test]
    fn a_resize_while_a_tile_retracts_lays_out_for_the_new_size_at_once() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        spawn_several(&mut app, &daemon, project, 2);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_secs(1));
        drawn(&mut app, &mut terminal);

        app.close_focused();
        drawn(&mut app, &mut terminal);
        let mut smaller = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 20))
            .expect("a test backend can be created");
        drawn(&mut app, &mut smaller);

        for (_, rect) in &app.frames {
            assert!(rect.x + rect.width <= 60 && rect.y + rect.height <= 20, "{rect:?}");
        }
    }

    #[test]
    fn the_active_tab_tint_slides_to_the_new_tab() {
        let (mut app, project, daemon, _sent) = attached_app();
        let clock = hand_clock(&mut app);
        let panes = spawn_several(&mut app, &daemon, project, 5); // tab 2 active
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30))
            .expect("a test backend can be created");
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_secs(1));
        drawn(&mut app, &mut terminal);
        let tab = Theme::fallback().tab;
        let tinted = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| -> Vec<u16> {
            let buf = terminal.backend().buffer();
            (sidebar::WIDTH..buf.area.width)
                .filter(|x| buf.cell((*x, 0)).is_some_and(|cell| cell.bg == tab))
                .collect()
        };
        let at_rest_on_two = tinted(&terminal);

        let _ = app.state.focus(panes[0]); // tab 1
        drawn(&mut app, &mut terminal);
        advance(&clock, Duration::from_millis(40));
        drawn(&mut app, &mut terminal);
        let sliding = tinted(&terminal);

        advance(&clock, Duration::from_millis(300));
        drawn(&mut app, &mut terminal);
        let at_rest_on_one = tinted(&terminal);

        assert!(sliding.first() > at_rest_on_one.first(), "not arrived yet: {sliding:?}");
        assert!(sliding.first() < at_rest_on_two.first(), "but on its way: {sliding:?}");
    }

    #[test]
    fn with_motion_off_a_close_reflows_at_once() {
        let (mut app, project, daemon, _sent) = attached_app();
        app.set_motion(false);
        let panes = spawn_several(&mut app, &daemon, project, 2);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        drawn(&mut app, &mut terminal);
        let before = app.frames.iter().find(|(id, _)| *id == panes[0]).expect("tiled").1;

        app.close_focused();
        drawn(&mut app, &mut terminal);

        assert!(app.frames.iter().find(|(id, _)| *id == panes[0]).expect("tiled").1.width > before.width);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch -- perimeter drawn_in retracted quick_succession resize_while tab_tint_slides motion_off_a_close`
Expected: compile errors — `perimeter` not found; then behaviour failures.

- [ ] **Step 3: Implement**

```rust
/// How long a new pane's border takes to draw in.
const OPEN: Duration = Duration::from_millis(200);
/// How long a closed pane's tile takes to retract, holding the grid.
const CLOSE: Duration = Duration::from_millis(150);
/// How long the active tab's tint takes to slide.
const SLIDE: Duration = Duration::from_millis(150);

/// The cells around `rect`'s edge, clockwise from its top-left corner.
fn perimeter(rect: Rect) -> Vec<(u16, u16)> {
    if rect.width == 0 || rect.height == 0 {
        return Vec::new();
    }
    let (left, top) = (rect.x, rect.y);
    let (right, bottom) = (rect.x + rect.width - 1, rect.y + rect.height - 1);

    let mut path: Vec<(u16, u16)> = (left..=right).map(|x| (x, top)).collect();
    path.extend((top + 1..=bottom).map(|y| (right, y)));
    if bottom > top {
        path.extend((left..right).rev().map(|x| (x, bottom)));
    }
    if right > left {
        path.extend((top + 1..bottom).rev().map(|y| (left, y)));
    }
    path
}

/// Blanks the part of `rect`'s border past `shown` (0.0–1.0) of the way
/// round, so a border can be drawn in or retract.
fn mask_border(buf: &mut Buffer, rect: Rect, shown: f32) {
    let path = perimeter(rect);
    let kept = (path.len() as f32 * shown.clamp(0.0, 1.0)).ceil() as usize;
    for &(x, y) in path.iter().skip(kept) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.reset();
        }
    }
}
```

(`use ratatui::buffer::Buffer;`.) In `adopt`, after inserting the pane: `self.animations.start(Target::Open(id), now, OPEN, 0.0);`.

`App` gains `held: Option<(Rect, Vec<(PaneId, Rect)>)>` (the area the held frames were for, and the frames), `closing: Vec<(PaneId, Rect)>`, `last_tab: usize`, `tab_from: usize` — `None`, empty, `0`, `0` in `new`.

In `draw`, replace `self.frames = self.compute_frames(panes_area);` with:

```rust
        self.frames = self.lay_out(panes_area, now);
```

and:

```rust
    /// Where each pane goes this frame.
    ///
    /// A pane leaving the grid — closed, exited, gone — leaves its tile in
    /// place for a moment while its border retracts, and the rest of the grid
    /// keeps its shape for drawing and input alike until the last such tile
    /// is gone; then everything reflows once. A resize ends that at once:
    /// held tiles belong to a screen that no longer exists.
    fn lay_out(&mut self, area: Rect, now: Instant) -> Vec<(PaneId, Rect)> {
        let tileable = self.tileable();

        let finished = self
            .closing
            .iter()
            .filter(|(id, _)| self.animations.value(Target::Close(*id), now).is_none())
            .count();
        if finished > 0 {
            self.closing
                .retain(|(id, _)| self.animations.value(Target::Close(*id), now).is_some());
        }

        let leaving: Vec<(PaneId, Rect)> = self
            .frames
            .iter()
            .filter(|(id, _)| !tileable.contains(id))
            .copied()
            .collect();
        if self.animations.enabled() {
            for (id, rect) in leaving {
                self.animations.start(Target::Close(id), now, CLOSE, 0.0);
                self.closing.push((id, rect));
                if self.held.is_none() {
                    self.held = Some((area, self.frames.clone()));
                }
            }
        }

        if self.closing.is_empty() || self.held.as_ref().is_some_and(|(held, _)| *held != area) {
            self.held = None;
            self.closing.clear();
        }

        match &self.held {
            Some((_, frames)) => frames
                .iter()
                .filter(|(id, _)| tileable.contains(id))
                .copied()
                .collect(),
            None => self.compute_frames(area),
        }
    }
```

In `draw_panes`, after the loop over live panes, draw the closing tiles:

```rust
        for (id, rect) in &self.closing {
            let t = self.animations.value(Target::Close(*id), now).unwrap_or(1.0);
            Clear.render(*rect, frame.buffer_mut());
            frame.render_widget(pane_block(self.theme.faded, false), *rect);
            mask_border(frame.buffer_mut(), *rect, 1.0 - t);
        }
```

and, for a live pane with an `Open` tween, right after its block is rendered and before its contents:

```rust
            if let Some(t) = self.animations.value(Target::Open(*id), now) {
                mask_border(frame.buffer_mut(), *outer, t);
            }
```

Tab slide: in `draw`, before `draw_tabs`: 

```rust
        let tab = self.current_tab();
        if tab != self.last_tab {
            self.tab_from = self.last_tab;
            self.animations.start(Target::Tab, now, SLIDE, 0.0);
            self.last_tab = tab;
        }
```

`draw_tabs` becomes `draw_tabs(&self, frame, area, now)`. While a slide runs the active tab's own style leaves its background off — `let sliding = self.animations.value(Target::Tab, now).is_some();` and, for the active tab, `if !sliding { style = style.bg(self.theme.tab); }` with `fg(text)` and bold kept either way. Each tab's extent is recorded as its spans are pushed:

```rust
        // Where each tab sits in the row, for the sliding tint.
        let mut extents: Vec<(u16, u16)> = Vec::new();
        let mut column = area.x;
        for index in 0..self.tab_count() {
            // …the gap, the rollup glyph and the label, pushed as in Task 8…
            let start = column;
            // after pushing this tab's spans:
            let width: u16 = spans[first_span_of_this_tab..]
                .iter()
                .map(|span| u16::try_from(span.width()).unwrap_or(u16::MAX))
                .sum();
            column = column.saturating_add(width);
            extents.push((start, width));
        }
```

— concretely: remember `let first = spans.len();` after pushing the `" "` gap (the gap belongs to no tab) and before pushing the tab's own spans, then after them sum `spans[first..]`'s widths into `width` and push `(start, width)` with `start` the column at `first`. After rendering the paragraph:

```rust
        if let Some(t) = self.animations.value(Target::Tab, now) {
            let (from_x, from_w) = extents.get(self.tab_from).copied().unwrap_or((area.x, 0));
            let (to_x, to_w) = extents.get(current).copied().unwrap_or((area.x, 0));
            let lerp = |a: u16, b: u16| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u16;
            let (x, width) = (lerp(from_x, to_x), lerp(from_w, to_w));
            for column in x..(x + width).min(area.x + area.width) {
                if let Some(cell) = frame.buffer_mut().cell_mut((column, area.y)) {
                    cell.set_bg(self.theme.tab);
                }
            }
        }
```

(`extents: Vec<(u16, u16)>` indexed by tab; widths measured with `unicode_width` via the spans' `width()`.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: all pass. A–era tests that close a pane and immediately check the reflowed grid now see the held grid for 150 ms: give each such test `app.set_motion(false)` (the spec's motion-off path), and say which in the report.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add dispatch
git commit -m "feat(dispatch): draw new panes in, retract closed ones, slide the active tab"
```

---

### Task 12: Verify the whole branch

**Files:** none new; fixes only where a check fails.

- [ ] **Step 1: Everything CI runs, here**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --no-fail-fast`
Expected: clean; report the pass count.

- [ ] **Step 2: The other targets**

Run: `cargo clippy -p dispatch-tui -p dispatch-config -p dispatch-pty --all-targets --target aarch64-apple-darwin -- -D warnings`
Expected: clean. (Windows-gnu: confirm nothing new beyond the known `dispatch-os` errors.)

- [ ] **Step 3: Look at it**

From the repo root: `cargo build -p dispatch && (sleep 3; printf '\x01q') | timeout 8 script -qfc "stty cols 120 rows 30; ./target/debug/dispatch" /dev/null > "$SCRATCH/look.txt"` (with `SCRATCH` the session's scratchpad), then strip escapes as in the A slice's Task 13 and confirm the interface still draws (`D I S P A T C H`, `Projects`). No `dispatch` process may be left running (`pgrep -af target/debug/dispatch`).

- [ ] **Step 4: The spec still describes what was built**

Read the spec against the code; fix whichever is wrong. The refinements made on the way (Task 4's `input`/`output`, Task 10's glide replacement) are already in it.

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix(dispatch): what verifying live status and motion turned up"
```

(Skip if nothing changed.)
