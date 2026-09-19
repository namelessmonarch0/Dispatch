# Orchestrator Delegation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An agent in a Dispatch pane can run `dispatch delegate "<task>"` to have a second agent started on that task, approved by hand, and read its output back.

**Architecture:** The shim is an ordinary client of `dispatchd` with a new `Role::Delegate` on its `Hello`. The daemon holds pending requests, enforces caps, spawns the subagent from the harness's non-interactive `[task]` form, and answers the caller with the subagent's exit code and the last 8 KiB of its output — a slice of the pane history the daemon already keeps for reattach. Interface clients draw the subagent nested under its parent and answer the approval prompt.

**Tech Stack:** Rust 2024 (rust-version 1.85), `clap` 4 derive, `serde`/`toml`, `ratatui` 0.29, MessagePack framing via `rmp_serde`, `insta` for snapshot tests where a crate already uses it.

**Spec:** `docs/superpowers/specs/2026-09-19-orchestrator-delegation-design.md`

## Global Constraints

- Every new protocol field carries `#[serde(default)]`; every new message is a variant an older peer can ignore. A major version bump is not permitted in this plan.
- `{task}` is substituted as exactly one argv element. It is never interpolated into a shell string.
- Caps, from `config.toml` `[delegation]`: `max_depth = 1`, `max_live_per_parent = 4`, `request_timeout_secs = 600`.
- Approval is never automatic. The only path that skips a prompt is a blanket approval the user gave with `A` for that specific pane, held in memory for the daemon's lifetime only, never written to disk.
- Exit codes from the shim: subagent's own code when it ran; `69` no daemon, `75` timed out, `77` denied, `78` refused.
- `DISPATCH_PANE` is attribution, not a security boundary. Do not add a check that pretends otherwise.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets` must be clean at every commit. Workspace lints deny `unsafe_op_in_unsafe_fn` and warn on undocumented unsafe blocks.
- Doc comments on every new public item, in the style of the surrounding code: say why, not what.

---

## File Structure

**Created**

| File | Responsibility |
|---|---|
| `crates/dispatch-config/src/config.rs` | `Config` and `DelegationLimits`: the first top-level `config.toml` loader |
| `crates/dispatch-config/src/config/tests.rs` | Tests for the above |
| `crates/dispatch-daemon/src/delegation.rs` | `Pending`, cap checks, the request state machine |
| `crates/dispatch-daemon/src/delegation/tests.rs` | Tests for cap checks in isolation |
| `dispatch/src/delegate.rs` | The `dispatch delegate` subcommand: the shim |
| `dispatch/src/approval.rs` | The approval overlay widget and its key handling |

**Modified**

| File | Change |
|---|---|
| `crates/dispatch-config/src/harness.rs` | `TaskLaunch`, `HarnessDef::task`, `HarnessDef::task_launch` |
| `crates/dispatch-config/src/defaults.rs` | `[task]` forms for the `claude` and `codex` built-ins |
| `crates/dispatch-config/src/lib.rs` | Export `Config`, `DelegationLimits`, `TaskLaunch` |
| `crates/dispatch-core/src/id.rs` | `RequestId` |
| `crates/dispatch-core/src/pane.rs` | `Pane::parent`, `Pane::durable`, `Pane::closed` |
| `crates/dispatch-core/src/state.rs` | `children_of`, `live_children`, tombstone semantics in `close_pane` |
| `crates/dispatch-proto/src/message.rs` | `Role`, `DelegateOutcome`, four new messages |
| `crates/dispatch-daemon/src/pane.rs` | `DaemonPane::parent`, `durable`, `request` |
| `crates/dispatch-daemon/src/session.rs` | Role on clients, pending requests, decisions, env injection, lifetime rules |
| `crates/dispatch-client/src/lib.rs` | `Client::attach_as(role, …)` |
| `dispatch/src/main.rs` | `delegate` subcommand dispatch; pass `Config` into the app |
| `dispatch/src/app.rs` | Pending-request state, approval overlay wiring, children excluded from the grid |
| `dispatch/src/backend.rs` | Nothing functional; `RemotePane` gains the pane's parent for display |
| `crates/dispatch-tui/src/sidebar.rs` | One level of nesting, outcome markers, tombstone rows |
| `crates/dispatch-tui/src/input.rs` | `Action::Approvals` for `^a p` |
| `dispatch/tests/end_to_end.rs` | Approve and deny paths through the real binaries |
| `README.md` | A delegation section |

---

### Task 1: The `[task]` form on a harness

**Files:**
- Modify: `crates/dispatch-config/src/harness.rs`
- Modify: `crates/dispatch-config/src/defaults.rs`
- Modify: `crates/dispatch-config/src/lib.rs`
- Test: `crates/dispatch-config/src/tests.rs`

**Interfaces:**
- Consumes: `Launch`, `HarnessDef`, `HarnessDef::launch_for` (existing, `crates/dispatch-config/src/harness.rs`).
- Produces: `pub struct TaskLaunch { pub args: Vec<String> }`; `HarnessDef::task: Option<TaskLaunch>`; `HarnessDef::task_launch(&self, task: &str) -> Option<Launch>` — the platform launch with `{task}` replaced, `None` when the harness declares no task form.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-config/src/tests.rs`:

```rust
#[test]
fn a_harness_can_declare_a_one_shot_task_form() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "claude"
display_name = "Claude Code"
command = "claude"

[task]
args = ["-p", "{task}"]
"#,
    )
    .expect("the definition parses");

    let launch = def
        .task_launch("write the tests")
        .expect("the harness declares a task form");

    assert_eq!(launch.command, "claude");
    assert_eq!(launch.args, vec!["-p", "write the tests"]);
}

#[test]
fn a_harness_without_a_task_form_cannot_be_delegated_to() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "agy"
display_name = "agy"
command = "agy"
"#,
    )
    .expect("the definition parses");

    assert!(
        def.task_launch("anything").is_none(),
        "a harness with no [task] form has no non-interactive shape to run"
    );
}

#[test]
fn a_task_is_one_argument_however_it_is_written() {
    // Substituted as an argv element, never interpolated into a shell string:
    // quotes, newlines and command substitution have to arrive as text.
    let def: HarnessDef = toml::from_str(
        r#"
id = "shell"
display_name = "Shell"
command = "sh"

[task]
args = ["-c", "{task}"]
"#,
    )
    .expect("the definition parses");

    let hostile = "say \"hi\"\nthen $(rm -rf /)";
    let launch = def.task_launch(hostile).expect("a task form exists");

    assert_eq!(launch.args.len(), 2);
    assert_eq!(launch.args[1], hostile);
}

#[test]
fn a_task_placeholder_inside_a_longer_argument_is_substituted() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "codex"
display_name = "Codex"
command = "codex"

[task]
args = ["exec", "--prompt={task}"]
"#,
    )
    .expect("the definition parses");

    let launch = def.task_launch("build it").expect("a task form exists");
    assert_eq!(launch.args, vec!["exec", "--prompt=build it"]);
}

#[test]
fn the_built_in_agents_that_can_be_delegated_to_say_so() {
    // claude and codex have documented non-interactive forms. agy and opencode
    // do not ship one: a guess at their flags would run a process with flags
    // that mean something else.
    let dir = temp_dir("built-in-task-forms");
    write_missing_built_ins(&dir).expect("the built-ins are written");
    let registry = HarnessRegistry::load_from_dir(&dir).expect("they load");

    for id in ["claude", "codex"] {
        assert!(
            registry
                .get(id)
                .expect("the built-in exists")
                .task_launch("x")
                .is_some(),
            "{id} should declare a [task] form"
        );
    }

    for id in ["agy", "opencode"] {
        assert!(
            registry
                .get(id)
                .expect("the built-in exists")
                .task_launch("x")
                .is_none(),
            "{id} should not guess at a [task] form"
        );
    }
}
```

`temp_dir` is the helper already used in `crates/dispatch-config/src/tests.rs`. If its name there differs, use the existing one rather than adding another.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-config 2>&1 | tail -20`

Expected: FAIL — `no method named task_launch found for struct HarnessDef`.

- [ ] **Step 3: Add the type and the accessor**

In `crates/dispatch-config/src/harness.rs`, after the `Launch` struct:

```rust
/// How to run a harness once, on one task, without a person at the keyboard.
///
/// Delegation needs a form that finishes: an interactive agent waits for input
/// forever, so a caller blocking on one would never be answered. A harness
/// without this cannot be delegated to, and Dispatch says so rather than
/// guessing at flags that may mean something else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLaunch {
    /// Arguments for the one-shot form. Exactly one `{task}` placeholder is
    /// expected; it is replaced as a single argument.
    #[serde(default)]
    pub args: Vec<String>,
}
```

Add the field to `HarnessDef`, after `platform`:

```rust
    /// The non-interactive form used when another agent delegates to this one.
    #[serde(default)]
    pub task: Option<TaskLaunch>,
```

And the accessor in `impl HarnessDef`:

```rust
    /// The launch for running `task` once, or `None` when the harness has no
    /// non-interactive form.
    ///
    /// The task replaces `{task}` inside each argument, which keeps it one
    /// argv element however the harness spells the flag — `"{task}"` or
    /// `"--prompt={task}"`. It is never passed through a shell, so quotes,
    /// newlines and `$(…)` in a task are inert.
    #[must_use]
    pub fn task_launch(&self, task: &str) -> Option<Launch> {
        let form = self.task.as_ref()?;
        let mut launch = self.launch_for_current_platform();

        launch.args = form
            .args
            .iter()
            .map(|arg| arg.replace("{task}", task))
            .collect();

        Some(launch)
    }
```

- [ ] **Step 4: Give the built-ins their task forms**

In `crates/dispatch-config/src/defaults.rs`, add to the `claude` body:

```toml

[task]
args = ["-p", "{task}"]
```

and to the `codex` body:

```toml

[task]
args = ["exec", "{task}"]
```

Leave `agy` and `opencode` untouched.

- [ ] **Step 5: Export the new type**

In `crates/dispatch-config/src/lib.rs`, add `TaskLaunch` to the existing `pub use harness::{…}` list.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p dispatch-config 2>&1 | tail -20`

Expected: PASS, all tests in the crate.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p dispatch-config --all-targets
git add crates/dispatch-config
git commit -m "feat(config): let a harness declare how to run one task

Delegation needs a form of a harness that finishes. An interactive agent waits
for input forever, so a caller blocking on one would never be answered.

claude and codex have documented non-interactive forms and ship with one. agy and
opencode do not: their flags would be a guess, and a wrong guess runs a process
with flags that mean something else."
```

---

### Task 2: `config.toml` and the delegation caps

**Files:**
- Create: `crates/dispatch-config/src/config.rs`
- Create: `crates/dispatch-config/src/config/tests.rs`
- Modify: `crates/dispatch-config/src/lib.rs`

**Interfaces:**
- Consumes: `ConfigError` (existing, `crates/dispatch-config/src/lib.rs`).
- Produces: `pub struct Config { pub delegation: DelegationLimits }`; `pub struct DelegationLimits { pub max_depth: u8, pub max_live_per_parent: usize, pub request_timeout_secs: u64 }`; `Config::load(path: &Path) -> Result<Config, ConfigError>`; `Config::default()`.

- [ ] **Step 1: Write the failing tests**

Create `crates/dispatch-config/src/config/tests.rs`:

```rust
//! Tests for the top-level configuration file.

use super::*;

/// A temporary directory that cleans itself up.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let path = std::env::temp_dir().join(format!(
            "dispatch-config-{}-{label}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir is writable");
        Self(path)
    }

    fn file(&self, contents: &str) -> std::path::PathBuf {
        let path = self.0.join("config.toml");
        std::fs::write(&path, contents).expect("temp dir is writable");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_missing_file_means_the_defaults() {
    // A fresh install has no config.toml, and must behave like a configured one
    // that changed nothing.
    let dir = TempDir::new("missing");
    let config = Config::load(&dir.0.join("config.toml")).expect("an absent file is not an error");

    assert_eq!(config, Config::default());
    assert_eq!(config.delegation.max_depth, 1);
    assert_eq!(config.delegation.max_live_per_parent, 4);
    assert_eq!(config.delegation.request_timeout_secs, 600);
}

#[test]
fn the_caps_can_be_raised_deliberately() {
    let dir = TempDir::new("raised");
    let path = dir.file(
        r#"
[delegation]
max_depth = 2
max_live_per_parent = 8
request_timeout_secs = 60
"#,
    );

    let config = Config::load(&path).expect("the file parses");
    assert_eq!(config.delegation.max_depth, 2);
    assert_eq!(config.delegation.max_live_per_parent, 8);
    assert_eq!(config.delegation.request_timeout_secs, 60);
}

#[test]
fn a_partly_written_section_keeps_the_other_defaults() {
    let dir = TempDir::new("partial");
    let path = dir.file("[delegation]\nmax_depth = 2\n");

    let config = Config::load(&path).expect("the file parses");
    assert_eq!(config.delegation.max_depth, 2);
    assert_eq!(
        config.delegation.max_live_per_parent, 4,
        "an unmentioned cap keeps its default"
    );
}

#[test]
fn a_key_this_build_does_not_know_is_kept_and_reported() {
    // A newer daemon's key must not stop an older one starting, and a typo must
    // not be silent.
    let dir = TempDir::new("unknown");
    let path = dir.file(
        r#"
[delegation]
max_depth = 1
max_liv_per_parent = 9
"#,
    );

    let loaded = Config::load_reporting(&path).expect("the file parses");
    assert_eq!(loaded.config.delegation.max_live_per_parent, 4);
    assert_eq!(
        loaded.unknown,
        vec!["delegation.max_liv_per_parent".to_string()],
        "the key is named so a typo can be found"
    );
}

#[test]
fn a_broken_file_names_itself() {
    let dir = TempDir::new("broken");
    let path = dir.file("[delegation\nmax_depth = 1\n");

    let error = Config::load(&path).expect_err("invalid TOML is an error");
    assert!(
        error.to_string().contains("config.toml"),
        "the user has to be told which file to fix, got {error}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-config config:: 2>&1 | tail -20`

Expected: FAIL — `config.rs` does not exist; `unresolved module`.

- [ ] **Step 3: Write the loader**

Create `crates/dispatch-config/src/config.rs`:

```rust
//! Dispatch's own configuration, as opposed to a harness's.
//!
//! Absent means default: a fresh install has no `config.toml`, and must behave
//! exactly like a configured one that changed nothing. Every field therefore
//! has a default, and a file may mention only what it changes.
//!
//! An unknown key is kept rather than rejected. A newer Dispatch's key must not
//! stop an older one starting — two versions share a configuration directory
//! whenever a machine is mid-upgrade — but a typo must not be silent either, so
//! unknown keys are reported by name for the caller to log.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Limits on delegation, which starts processes on this machine.
///
/// These refuse rather than prompt. A prompt for the five-hundredth request is
/// not a safeguard; it is a way to make someone hold down `d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationLimits {
    /// How deep delegation may nest. At `1`, a subagent cannot delegate.
    pub max_depth: u8,
    /// How many subagents one pane may have running at once.
    pub max_live_per_parent: usize,
    /// How long a request waits for an answer before it is refused.
    pub request_timeout_secs: u64,
}

impl Default for DelegationLimits {
    fn default() -> Self {
        Self {
            max_depth: 1,
            max_live_per_parent: 4,
            request_timeout_secs: 600,
        }
    }
}

/// Everything `config.toml` can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Limits on delegation.
    pub delegation: DelegationLimits,
}

/// A loaded configuration, plus the keys this build did not understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    /// The configuration, with defaults for anything unmentioned.
    pub config: Config,
    /// Dotted paths of keys this build ignored, for the caller to log.
    pub unknown: Vec<String>,
}

impl Config {
    /// Loads `path`, or the defaults when it does not exist.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Ok(Self::load_reporting(path)?.config)
    }

    /// Loads `path`, also reporting keys this build ignored.
    pub fn load_reporting(path: &Path) -> Result<LoadedConfig, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadedConfig {
                    config: Self::default(),
                    unknown: Vec::new(),
                });
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Toml {
            path: path.to_path_buf(),
            source,
        })?;

        // Parsed a second time as plain tables to find keys the typed parse
        // silently dropped. Cheap: this file is a handful of lines, read once
        // per process.
        let raw: toml::Table = toml::from_str(&text).map_err(|source| ConfigError::Toml {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(LoadedConfig {
            config,
            unknown: unknown_keys(&raw),
        })
    }
}

/// Dotted paths of keys Dispatch does not know.
fn unknown_keys(raw: &toml::Table) -> Vec<String> {
    const DELEGATION: [&str; 3] = ["max_depth", "max_live_per_parent", "request_timeout_secs"];

    let mut unknown = Vec::new();

    for (section, value) in raw {
        match (section.as_str(), value) {
            ("delegation", toml::Value::Table(table)) => {
                for key in table.keys() {
                    if !DELEGATION.contains(&key.as_str()) {
                        unknown.push(format!("delegation.{key}"));
                    }
                }
            }
            _ => unknown.push(section.clone()),
        }
    }

    unknown.sort();
    unknown
}

#[cfg(test)]
mod tests;
```

In `crates/dispatch-config/src/lib.rs`, add the module and exports:

```rust
pub mod config;
pub use config::{Config, DelegationLimits, LoadedConfig};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-config 2>&1 | tail -20`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p dispatch-config --all-targets
git add crates/dispatch-config
git commit -m "feat(config): read config.toml, starting with the delegation caps

Dispatch has had a config_file() path and nothing reading it. Delegation needs
caps a user can raise deliberately, so this is the loader: absent file means
defaults, a file may mention only what it changes.

An unknown key is kept and reported by name rather than rejected. Two versions
share a configuration directory whenever a machine is mid-upgrade, so a newer
key must not stop an older build; a typo must not be silent either."
```

---

### Task 3: Parents, tombstones and `RequestId` in core

**Files:**
- Modify: `crates/dispatch-core/src/id.rs`
- Modify: `crates/dispatch-core/src/pane.rs`
- Modify: `crates/dispatch-core/src/state.rs`
- Modify: `crates/dispatch-core/src/lib.rs`

**Interfaces:**
- Consumes: `Pane`, `PaneId`, `AppState::adopt_pane`, `AppState::close_pane` (existing).
- Produces: `RequestId`; `Pane::parent: Option<PaneId>`, `Pane::durable: bool`, `Pane::closed: bool`; `AppState::children_of(&self, PaneId) -> Vec<&Pane>`; `AppState::live_children(&self, PaneId) -> usize`; `AppState::close_pane` keeping a parent as a tombstone while it has live durable children.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` in `crates/dispatch-core/src/state.rs`:

```rust
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
```

Add to the `mod tests` in `crates/dispatch-core/src/pane.rs`:

```rust
    #[test]
    fn a_new_pane_has_no_parent_and_is_not_a_tombstone() {
        let pane = Pane::new(ProjectId::new(), HarnessId::new("claude"));

        assert_eq!(pane.parent, None);
        assert!(!pane.durable);
        assert!(!pane.closed);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-core 2>&1 | tail -20`

Expected: FAIL — `no field parent on type Pane`.

- [ ] **Step 3: Add the id, and make ids readable back**

The shim reads `DISPATCH_PANE` out of the environment and has to turn it back into
a `PaneId`, which the macro does not currently allow. Add to the test module in
`crates/dispatch-core/src/id.rs`:

```rust
    #[test]
    fn an_id_survives_being_written_out_and_read_back() {
        // A pane's id reaches a subagent through the environment, as text.
        let id = PaneId::new();
        let parsed: PaneId = id.to_string().parse().expect("its own output parses");

        assert_eq!(parsed, id);
    }

    #[test]
    fn text_that_is_not_an_id_is_refused() {
        assert!("not-a-uuid".parse::<PaneId>().is_err());
        assert!("".parse::<PaneId>().is_err());
    }
```

and inside the `id_type!` macro, beside the `Display` implementation:

```rust
        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Ok(Self(text.parse()?))
            }
        }
```

Then, after the `PaneId` block:

```rust
id_type! {
    /// Identifies one delegation request.
    ///
    /// Separate from [`PaneId`] because a request has a life before a pane
    /// does: it can be refused or denied and never become one.
    RequestId
}
```

Add `RequestId` to the `pub use id::{…}` list in `crates/dispatch-core/src/lib.rs`.

- [ ] **Step 4: Add the pane fields**

In `crates/dispatch-core/src/pane.rs`, add to `Pane`:

```rust
    /// The pane that delegated this one's work, when it was delegated.
    #[serde(default)]
    pub parent: Option<PaneId>,
    /// Whether this pane outlives the caller that asked for it.
    ///
    /// Set when the user approved it for the whole parent pane rather than
    /// once: that is how they say "let this pane's work run".
    #[serde(default)]
    pub durable: bool,
    /// Whether this pane is closed but kept as a row for live children.
    #[serde(default)]
    pub closed: bool,
```

and initialise them in `Pane::new`:

```rust
            parent: None,
            durable: false,
            closed: false,
```

- [ ] **Step 5: Add the state operations**

In `crates/dispatch-core/src/state.rs`, add to `impl AppState`:

```rust
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
```

Replace the body of `close_pane` with one that understands children. Keep the existing focus and zoom repair; this only changes which panes are removed:

```rust
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
            self.remove_pane(id);

            // A tombstone exists only for its children. Closing the last one
            // takes the row with it.
            if let Some(parent) = self.parent_tombstone(id) {
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
```

`parent_tombstone` has to be read before the child is removed, so call it first. Adjust the order in `close_pane` to compute `let tombstone = self.parent_tombstone(id);` before `self.remove_pane(id);` and use that value afterwards.

Add the private helper that holds the existing removal logic, so both paths share it:

```rust
    /// Removes one pane and repairs focus and zoom around it.
    fn remove_pane(&mut self, id: PaneId) {
        self.panes.retain(|p| p.id != id);

        if self.focused_pane == Some(id) {
            self.focused_pane = self
                .visible_panes()
                .first()
                .map(|p| p.id)
                .or_else(|| self.panes.first().map(|p| p.id));
        }
        if self.zoomed_pane == Some(id) {
            self.zoomed_pane = None;
        }
    }
```

Finally, exclude tombstones from `visible_panes`, since a closed pane has no process to draw:

```rust
        self.panes
            .iter()
            .filter(|p| !p.closed && Some(p.project) == self.selected_project)
            .collect()
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p dispatch-core 2>&1 | tail -20`

Expected: PASS, including the pre-existing focus and zoom tests. If a pre-existing test fails, the focus repair in `remove_pane` differs from what `close_pane` used to do — match the old behaviour exactly rather than changing the test.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
cargo clippy -p dispatch-core --all-targets
git add crates/dispatch-core
git commit -m "feat(core): give panes parents, and parents tombstones

A delegated pane belongs under the pane that asked for it, and a blanket-approved
one outlives the caller that asked. That leaves a pane that is closed but still
has work running under it, so closing such a parent marks it closed and keeps the
row; the row goes when its last live child does.

Children that were one-off, or that have already finished, go with their parent.
Their transcripts were reachable through the pane just closed, and rows for
finished work under a pane that no longer exists are debris."
```

---

### Task 4: The protocol

**Files:**
- Modify: `crates/dispatch-proto/src/message.rs`
- Modify: `crates/dispatch-proto/src/lib.rs`
- Test: `crates/dispatch-proto/src/message/tests.rs`

**Interfaces:**
- Consumes: `RequestId` (Task 3), existing `ClientMessage`/`ServerMessage`.
- Produces: `Role`, `DelegateOutcome`, `ClientMessage::{DelegateRequest, DelegateDecision}`, `ServerMessage::{DelegatePending, DelegateResolved, DelegateFinished}`, `Hello.role`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/dispatch-proto/src/message/tests.rs`:

```rust
#[test]
fn the_delegation_messages_round_trip() {
    let request = RequestId::new();
    let messages = vec![
        ClientMessage::DelegateRequest {
            parent: PaneId::new(),
            harness: "claude".into(),
            task: "write the tests".into(),
            size: (80, 24),
        },
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    ];
    for message in messages {
        assert_eq!(round_trip(&message), message);
    }

    let replies = vec![
        ServerMessage::DelegatePending {
            request,
            parent: PaneId::new(),
            project: ProjectId::new(),
            harness: "claude".into(),
            task: "write the tests".into(),
            depth: 0,
        },
        ServerMessage::DelegateResolved {
            request,
            outcome: DelegateOutcome::Approved { pane: PaneId::new() },
        },
        ServerMessage::DelegateResolved {
            request,
            outcome: DelegateOutcome::Refused {
                reason: "harness \"agy\" has no [task] form".into(),
            },
        },
        ServerMessage::DelegateFinished {
            request,
            exit: 0,
            tail: b"done\r\n".to_vec(),
        },
    ];
    for message in replies {
        assert_eq!(round_trip(&message), message);
    }
}

#[test]
fn an_older_peer_is_an_interface_client() {
    // A client built before delegation existed sends no role, and is exactly
    // what Interface means: it draws panes.
    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OldClientMessage {
        Hello { version: Version, client: String },
    }

    let old = OldClientMessage::Hello {
        version: crate::VERSION,
        client: "old dispatch".into(),
    };

    let mut buf = Vec::new();
    Frame::write(&mut buf, &old).expect("writing succeeds");
    let read: ClientMessage = Frame::read(&mut buf.as_slice()).expect("reading succeeds");

    assert_eq!(
        read,
        ClientMessage::Hello {
            version: crate::VERSION,
            client: "old dispatch".into(),
            role: Role::Interface,
        }
    );
}

#[test]
fn a_delegate_callers_tail_is_binary_not_a_list_of_numbers() {
    // Same reason pane output is: this is the bulk of what the message carries.
    let message = ServerMessage::DelegateFinished {
        request: RequestId::new(),
        exit: 0,
        tail: vec![0u8; 1024],
    };

    let mut buf = Vec::new();
    Frame::write(&mut buf, &message).expect("writing succeeds");
    assert!(
        buf.len() < 1024 * 2,
        "1 KiB of output should not cost {} bytes",
        buf.len()
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-proto 2>&1 | tail -20`

Expected: FAIL — `cannot find type Role in this scope`.

- [ ] **Step 3: Add the types and messages**

In `crates/dispatch-proto/src/message.rs`, import `RequestId` alongside the existing core imports, then add:

```rust
/// What a connection is for.
///
/// The two audiences want different traffic. An interface draws panes and wants
/// every byte they produce; a delegate caller wants the fate of its own request
/// and nothing else, so sending it pane output would be a firehose it never
/// reads — and would slow the call down on a busy fleet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A client that draws the fleet. The default, because a peer built before
    /// roles existed is one of these.
    #[default]
    Interface,
    /// A `dispatch delegate` call waiting on one request.
    Delegate,
}

/// How a delegation request ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DelegateOutcome {
    /// The user approved it, and this pane is running the task.
    Approved {
        /// The subagent's pane.
        pane: PaneId,
    },
    /// The user denied it.
    Denied,
    /// The daemon refused it without asking: a cap, a missing task form, or a
    /// deadline that passed.
    Refused {
        /// Why, in words, because an agent reads this and should be able to act
        /// on it.
        reason: String,
    },
}
```

Add `role` to `ClientMessage::Hello`:

```rust
        /// What the connection is for.
        #[serde(default)]
        role: Role,
```

Add the two client messages:

```rust
    /// Asks for a subagent to be started on a task.
    ///
    /// Sent by `dispatch delegate` from inside a pane. The daemon decides
    /// whether it is allowed, and the user whether it happens.
    DelegateRequest {
        /// The pane asking, from `DISPATCH_PANE` in its environment.
        parent: PaneId,
        /// Which harness should run the task.
        harness: String,
        /// What to do, verbatim.
        task: String,
        /// Initial size in cells.
        size: (u16, u16),
    },

    /// Answers a [`ServerMessage::DelegatePending`].
    DelegateDecision {
        /// Which request.
        request: RequestId,
        /// Whether it may run.
        approve: bool,
        /// Whether every later request from the same pane is approved too, for
        /// as long as this daemon runs.
        #[serde(default)]
        blanket: bool,
    },
```

And the three server messages:

```rust
    /// A pane is asking to delegate, and a user has to decide.
    ///
    /// Sent to subscribed interface clients only.
    DelegatePending {
        /// Which request.
        request: RequestId,
        /// The pane asking.
        parent: PaneId,
        /// Its project.
        project: ProjectId,
        /// Which harness would run.
        harness: String,
        /// What it would be asked to do, in full: approving something you
        /// cannot read is not approval.
        task: String,
        /// How deep the parent already is, for display.
        #[serde(default)]
        depth: u8,
    },

    /// A request will not be asked about again.
    DelegateResolved {
        /// Which request.
        request: RequestId,
        /// What happened.
        outcome: DelegateOutcome,
    },

    /// A subagent has exited, and its caller can stop waiting.
    DelegateFinished {
        /// Which request.
        request: RequestId,
        /// The subagent's exit code.
        exit: i32,
        /// The tail of what it printed, for the caller to hand to its agent.
        #[serde(with = "serde_bytes_compat")]
        tail: Vec<u8>,
    },
```

Add the parent to the existing spawn announcement, so a client can place a pane
in the tree the moment it hears about it — including a client that attaches to a
daemon whose subagents are already running:

```rust
    /// A pane was started.
    PaneSpawned {
        /// The new pane.
        pane: PaneId,
        /// Its project.
        project: ProjectId,
        /// Which harness is running.
        harness: String,
        /// The pane that delegated this one's work, when it was delegated.
        #[serde(default)]
        parent: Option<PaneId>,
    },
```

Update the two construction sites in `crates/dispatch-daemon/src/session.rs`
(spawn and the subscribe catch-up) to pass `parent: None` for now; Task 5 fills it
in. Add a round-trip case for the new field to the existing
`every_server_message_round_trips` test.

Export `Role` and `DelegateOutcome` from `crates/dispatch-proto/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-proto 2>&1 | tail -20`

Expected: PASS. Existing `Hello` constructions across the workspace will not compile yet; that is Task 5's problem, and `cargo test -p dispatch-proto` does not build them.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p dispatch-proto --all-targets
git add crates/dispatch-proto
git commit -m "feat(proto): add the delegation messages and a connection role

A request has a life before a pane does, so it has an id of its own and three
messages: pending, resolved, finished.

Hello gains a role, defaulting to Interface, which is what a peer built before
delegation existed already is. The split matters because a delegate caller wants
the fate of one request and not the pane-output firehose it would never read."
```

---

### Task 5: Environment for panes, and the request state machine

**Files:**
- Create: `crates/dispatch-daemon/src/delegation.rs`
- Create: `crates/dispatch-daemon/src/delegation/tests.rs`
- Modify: `crates/dispatch-daemon/src/pane.rs`
- Modify: `crates/dispatch-daemon/src/session.rs`
- Modify: `crates/dispatch-daemon/src/lib.rs`
- Modify: `crates/dispatch-daemon/Cargo.toml` (add `dispatch-config` is already there; no change expected)
- Test: `crates/dispatch-daemon/src/session/tests.rs`

**Interfaces:**
- Consumes: `Config`/`DelegationLimits` (Task 2), `HarnessDef::task_launch` (Task 1), `RequestId` (Task 3), protocol messages (Task 4), `Pty` (existing).
- Produces: `Daemon::with_limits(harnesses, device, limits)`; `DaemonPane::{parent, durable, request}`; `delegation::Pending`; `delegation::refusal(depth, live, limits, has_task_form, harness) -> Option<String>`.

- [ ] **Step 1: Write the failing cap tests**

Create `crates/dispatch-daemon/src/delegation/tests.rs`:

```rust
//! Tests for the rules that refuse a request without asking anyone.

use super::*;

fn limits() -> DelegationLimits {
    DelegationLimits::default()
}

#[test]
fn a_request_within_the_caps_is_not_refused() {
    assert_eq!(refusal(0, 0, limits(), true, "claude"), None);
}

#[test]
fn a_subagent_cannot_delegate_at_the_default_depth() {
    let reason = refusal(1, 0, limits(), true, "claude").expect("depth 1 is the cap");
    assert!(
        reason.contains("depth"),
        "the reason should say what stopped it, got {reason:?}"
    );
}

#[test]
fn a_parent_is_capped_on_how_many_run_at_once() {
    let reason = refusal(0, 4, limits(), true, "claude").expect("four live is the cap");
    assert!(
        reason.contains('4'),
        "the reason should name the cap, got {reason:?}"
    );

    assert_eq!(refusal(0, 3, limits(), true, "claude"), None);
}

#[test]
fn a_harness_with_no_task_form_is_refused_by_name() {
    let reason = refusal(0, 0, limits(), false, "agy").expect("no task form");
    assert!(
        reason.contains("agy") && reason.contains("[task]"),
        "the reason should name the harness and what it lacks, got {reason:?}"
    );
}

#[test]
fn raised_caps_are_honoured() {
    let generous = DelegationLimits {
        max_depth: 2,
        max_live_per_parent: 8,
        request_timeout_secs: 60,
    };

    assert_eq!(refusal(1, 7, generous, true, "claude"), None);
    assert!(refusal(2, 0, generous, true, "claude").is_some());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dispatch-daemon 2>&1 | tail -20`

Expected: FAIL — `file not found for module delegation`.

- [ ] **Step 3: Write the rules**

Create `crates/dispatch-daemon/src/delegation.rs`:

```rust
//! Deciding whether a delegation request may be asked about at all.
//!
//! These rules refuse without prompting. A prompt for the five-hundredth
//! request is not a safeguard; it is a way to make someone hold down `d`. The
//! user's judgement is for requests that are plausible.

use std::time::Instant;

use dispatch_config::DelegationLimits;
use dispatch_core::{PaneId, RequestId};
use dispatch_proto::ServerMessage;

/// A request that has been asked about and not yet answered.
pub struct Pending {
    /// The request.
    pub id: RequestId,
    /// The pane that asked.
    pub parent: PaneId,
    /// Which harness would run.
    pub harness: String,
    /// What it would be asked to do.
    pub task: String,
    /// Size to start the subagent at.
    pub size: (u16, u16),
    /// Which client is waiting for the answer.
    pub caller: u64,
    /// When it was asked, for the deadline.
    pub asked: Instant,
    /// What the interface clients were told, so a late subscriber can be sent
    /// the same thing without rebuilding it.
    pub announcement: ServerMessage,
}

/// Why a request cannot be asked about, if it cannot.
///
/// `depth` is how many parents the asking pane already has, `live` how many of
/// its children are running, and `has_task_form` whether the harness has a
/// non-interactive shape to run at all.
#[must_use]
pub fn refusal(
    depth: u8,
    live: usize,
    limits: DelegationLimits,
    has_task_form: bool,
    harness: &str,
) -> Option<String> {
    if !has_task_form {
        return Some(format!(
            "harness {harness:?} has no [task] form, so it cannot be run on one task; \
             add one or delegate to a harness that has one"
        ));
    }

    if depth >= limits.max_depth {
        return Some(format!(
            "delegation is capped at depth {}; this pane is already a subagent",
            limits.max_depth
        ));
    }

    if live >= limits.max_live_per_parent {
        return Some(format!(
            "this pane already has {live} subagents running, and the cap is {}",
            limits.max_live_per_parent
        ));
    }

    None
}

#[cfg(test)]
mod tests;
```

Add `mod delegation;` to `crates/dispatch-daemon/src/lib.rs`.

- [ ] **Step 4: Run to verify the cap tests pass**

Run: `cargo test -p dispatch-daemon delegation 2>&1 | tail -20`

Expected: PASS, five tests.

- [ ] **Step 5: Commit the rules**

```bash
cargo fmt --all
git add crates/dispatch-daemon
git commit -m "feat(daemon): decide which delegation requests are worth asking about

Depth, a per-parent count of running subagents, and whether the harness has a
non-interactive form at all. Each refusal says what stopped it, because an agent
reads the reason and should be able to act on it."
```

- [ ] **Step 6: Write the failing daemon tests**

Add to `crates/dispatch-daemon/src/session/tests.rs`. These need a harness with a task form, so extend the existing `harnesses` helper body to include one — `[task]\nargs = ["-c", "{task}"]` for `sh`, `["/c", "{task}"]` for `cmd.exe` — and keep the rest of that helper as it is.

```rust
/// Attaches a delegate caller and asks for a subagent.
fn ask(daemon: &mut Daemon, parent: PaneId, task: &str) -> Receiver<ServerMessage> {
    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent,
            harness: "shell".into(),
            task: task.into(),
            size: (80, 24),
        },
    );
    caller
}

/// The first pending request an interface client was told about.
fn pending(messages: &[ServerMessage]) -> Option<dispatch_core::RequestId> {
    messages.iter().find_map(|m| match m {
        ServerMessage::DelegatePending { request, .. } => Some(*request),
        _ => None,
    })
}

#[test]
fn a_delegation_request_is_put_to_the_user() {
    let (mut daemon, project, _dir) = daemon("delegate-ask");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let _caller = ask(&mut daemon, parent, "echo delegated");

    let seen = drain(&ui);
    let request = pending(&seen).expect("the interface is asked");
    assert!(
        matches!(
            seen.iter().find(|m| matches!(m, ServerMessage::DelegatePending { .. })),
            Some(ServerMessage::DelegatePending { task, .. }) if task == "echo delegated"
        ),
        "the whole task travels, got {seen:#?}"
    );
    assert_eq!(daemon.pane_count(), 1, "nothing runs before an answer");
    let _ = request;
}

#[test]
fn approving_a_request_starts_a_subagent_under_its_parent() {
    let (mut daemon, project, _dir) = daemon("delegate-approve");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo delegated-42");
    let request = pending(&drain(&ui)).expect("the interface is asked");

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );

    let seen = wait_for(&mut daemon, &caller, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });

    let finished = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::DelegateFinished { exit, tail, .. } => Some((*exit, tail.clone())),
            _ => None,
        })
        .expect("the caller is answered");
    assert_eq!(finished.0, 0, "the subagent's own exit code");
    assert!(
        String::from_utf8_lossy(&finished.1).contains("delegated-42"),
        "the tail carries what the subagent printed, got {:?}",
        String::from_utf8_lossy(&finished.1)
    );
    assert_eq!(daemon.pane_count(), 2, "the subagent's pane is kept");
}

#[test]
fn denying_a_request_starts_nothing() {
    let (mut daemon, project, _dir) = daemon("delegate-deny");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo never");
    let request = pending(&drain(&ui)).expect("the interface is asked");

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: false,
            blanket: false,
        },
    );

    let seen = drain(&caller);
    assert!(
        seen.iter().any(|m| matches!(
            m,
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Denied,
                ..
            }
        )),
        "the caller is told, got {seen:#?}"
    );
    assert_eq!(daemon.pane_count(), 1);
}

#[test]
fn a_blanket_approval_stops_the_asking() {
    let (mut daemon, project, _dir) = daemon("delegate-blanket");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let first = ask(&mut daemon, parent, "echo one");
    let request = pending(&drain(&ui)).expect("the first is asked about");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: true,
        },
    );
    wait_for(&mut daemon, &first, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });
    let _ = drain(&ui);

    // The second request from the same pane is not put to anyone.
    let second = ask(&mut daemon, parent, "echo two");
    wait_for(&mut daemon, &second, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });

    assert!(
        pending(&drain(&ui)).is_none(),
        "a pane approved with [A] is not asked about again"
    );
}

#[test]
fn a_subagent_dies_with_the_caller_that_asked_for_it() {
    let (mut daemon, project, _dir) = daemon("delegate-orphan");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _caller = ask(&mut daemon, parent, "sleep 30");
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );
    wait_for(&mut daemon, &ui, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }) && m_is_child(m))
    });

    // The agent hits Ctrl-C, or its pane is killed: either way the socket goes.
    daemon.detach_for_test(9);
    daemon.tick();

    assert_eq!(
        daemon.pane_count(),
        1,
        "a one-off subagent has nobody left to answer"
    );
}

#[test]
fn a_blanket_approved_subagent_survives_its_caller() {
    let (mut daemon, project, _dir) = daemon("delegate-durable");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _caller = ask(&mut daemon, parent, "sleep 30");
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: true,
        },
    );
    wait_for(&mut daemon, &ui, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }) && m_is_child(m))
    });

    daemon.detach_for_test(9);
    daemon.tick();

    assert_eq!(
        daemon.pane_count(),
        2,
        "[A] is how the user says to let this pane's work run"
    );
}

#[test]
fn a_request_nobody_answers_is_refused_when_its_time_is_up() {
    let (mut daemon, project, _dir) = daemon_with_limits(
        "delegate-timeout",
        DelegationLimits {
            request_timeout_secs: 0,
            ..DelegationLimits::default()
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, parent, "echo never");

    daemon.tick();

    let seen = drain(&caller);
    assert!(
        seen.iter().any(|m| matches!(
            m,
            ServerMessage::DelegateResolved {
                outcome: dispatch_proto::DelegateOutcome::Refused { .. },
                ..
            }
        )),
        "a caller must not wait on an unattended daemon forever, got {seen:#?}"
    );
    assert_eq!(daemon.pane_count(), 1, "and a late approval spawns nothing");
}

#[test]
fn a_pane_the_daemon_does_not_own_cannot_delegate() {
    let (mut daemon, _project, _dir) = daemon("delegate-stranger");
    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );

    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent: PaneId::new(),
            harness: "shell".into(),
            task: "echo hello".into(),
            size: (80, 24),
        },
    );

    assert!(
        matches!(
            drain(&caller).first(),
            Some(ServerMessage::Error {
                error: ProtocolError::NoSuchPane(_)
            })
        ),
        "an unknown parent is not a pane this daemon can attribute work to"
    );
}

#[test]
fn a_delegate_caller_is_not_sent_pane_output() {
    // It waits on one request; the fleet's output is a firehose it never reads.
    let (mut daemon, project, _dir) = daemon("delegate-quiet");
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let caller = ask(&mut daemon, parent, "echo quiet");
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: parent,
            bytes: b"echo noisy\r".to_vec(),
        },
    );
    wait_for(&mut daemon, &ui, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. }))
    });

    assert!(
        !drain(&caller)
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. })),
        "a delegate caller hears about its own request only"
    );
}
```

Two helpers these tests use have to be added to the same file:

```rust
/// A daemon with one project and non-default limits.
fn daemon_with_limits(label: &str, limits: DelegationLimits) -> (Daemon, ProjectId, TempDir) {
    let dir = TempDir::new(label);
    let registry = harnesses(&dir.0.join("harnesses"));

    let mut daemon = Daemon::with_limits(registry, "test-device", limits);
    let root = dir.0.canonicalize().expect("the temp dir resolves");
    let project = daemon.open_project(root);

    (daemon, project, dir)
}

/// Spawns a pane the ordinary way and returns its id.
fn spawn_pane_for_test(
    daemon: &mut Daemon,
    inbox: &Receiver<ServerMessage>,
    project: ProjectId,
) -> PaneId {
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(daemon, inbox, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    seen.iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("a pane was spawned")
}

/// Whether a spawn announcement is for a delegated pane.
fn m_is_child(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::PaneSpawned {
            parent: Some(_),
            ..
        }
    )
}
```

`m_is_child` requires `ServerMessage::PaneSpawned` to carry the parent. Add that field in this task, as `#[serde(default)] parent: Option<PaneId>`, and update the existing construction sites.

- [ ] **Step 7: Run to verify they fail**

Run: `cargo test -p dispatch-daemon 2>&1 | tail -30`

Expected: FAIL to compile — `Daemon::with_limits` and the delegation arms do not exist.

- [ ] **Step 8: Implement the daemon side**

In `crates/dispatch-daemon/src/pane.rs`, add to `DaemonPane`:

```rust
    /// The pane that delegated this one's work.
    pub parent: Option<PaneId>,
    /// Whether it outlives the caller that asked for it.
    pub durable: bool,
    /// The request it answers, while one is waiting.
    pub request: Option<dispatch_core::RequestId>,
    /// Which client is waiting, so its disappearance can end a one-off pane.
    pub caller: Option<u64>,
```

In `crates/dispatch-daemon/src/session.rs`:

1. `Client` gains `role: Role`, set from `Hello`. `broadcast` sends to subscribed clients whose role is `Interface`.
2. `Daemon` gains `limits: DelegationLimits`, `pending: HashMap<RequestId, Pending>`, `blanket: HashSet<PaneId>`. `Daemon::new` keeps its signature and uses `DelegationLimits::default()`; `Daemon::with_limits` takes them explicitly.
3. Pane spawning injects the environment, for every pane rather than only delegated ones:

```rust
    /// The environment a pane needs to talk back to this daemon.
    ///
    /// `DISPATCH_PANE` is attribution, not a permission: the socket is
    /// owner-only, and anything that can connect can already spawn panes. It
    /// decides which pane a request is attributed to, and protects nothing.
    ///
    /// `PATH` gains the directory holding the `dispatch` binary — a sibling of
    /// this executable — so `dispatch delegate` is runnable from inside a pane.
    /// Where there is no sibling, `PATH` is left alone: "command not found" is
    /// honest, and a daemon pretending otherwise is not.
    fn pane_env(&self, pane: PaneId) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("DISPATCH_PANE".to_string(), pane.to_string());

        if let Ok(dir) = dispatch_os::paths::config_dir() {
            env.insert(
                dispatch_os::paths::CONFIG_DIR_ENV.to_string(),
                dir.display().to_string(),
            );
        }

        if let Some(bin) = client_binary_dir() {
            let existing = std::env::var("PATH").unwrap_or_default();
            let separator = if cfg!(windows) { ";" } else { ":" };
            env.insert(
                "PATH".to_string(),
                format!("{}{separator}{existing}", bin.display()),
            );
        }

        env
    }
```

with

```rust
/// The directory holding the `dispatch` client binary, when it sits beside this
/// one.
fn client_binary_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "dispatch.exe"
    } else {
        "dispatch"
    };

    let candidate = exe.with_file_name(name);
    candidate.is_file().then(|| exe.parent()?.to_path_buf())
}
```

Merge `pane_env` into the `Launch`'s `env` before `Pty::spawn`, with the harness's own entries winning where they collide — a harness that sets `PATH` deliberately means it.

4. The request arm, in the order the spec fixes:

```rust
            ClientMessage::DelegateRequest {
                parent,
                harness,
                task,
                size,
            } => self.delegate_request(id, parent, harness, task, size),
```

```rust
    /// Refuses, approves, or asks about a request to delegate.
    fn delegate_request(
        &mut self,
        caller: ClientId,
        parent: PaneId,
        harness: String,
        task: String,
        size: (u16, u16),
    ) {
        let Some(asking) = self.panes.get(&parent) else {
            self.send(
                caller,
                ServerMessage::Error {
                    error: ProtocolError::NoSuchPane(parent),
                },
            );
            return;
        };

        let project = asking.project;
        // An empty harness means "whatever the asking pane is running": an agent
        // delegating to another of itself is the common case.
        let harness = if harness.is_empty() {
            asking.harness.clone()
        } else {
            harness
        };

        let depth = self.depth_of(parent);
        let live = self.live_children(parent);
        let has_task_form = self
            .harnesses
            .get(&harness)
            .is_some_and(|def| def.task.is_some());

        if let Some(reason) = crate::delegation::refusal(
            depth,
            live,
            self.limits,
            has_task_form,
            &harness,
        ) {
            tracing::info!(%parent, %harness, %reason, "refused a delegation");
            self.send(
                caller,
                ServerMessage::DelegateResolved {
                    request: RequestId::new(),
                    outcome: DelegateOutcome::Refused { reason },
                },
            );
            return;
        }

        let request = RequestId::new();

        // A pane the user has already approved for everything does not ask
        // again, for as long as this daemon runs.
        if self.blanket.contains(&parent) {
            self.approve(request, parent, &harness, &task, size, caller, true);
            return;
        }

        let announcement = ServerMessage::DelegatePending {
            request,
            parent,
            project,
            harness: harness.clone(),
            task: task.clone(),
            depth,
        };

        self.pending.insert(
            request,
            Pending {
                id: request,
                parent,
                harness,
                task,
                size,
                caller,
                asked: Instant::now(),
                announcement: announcement.clone(),
            },
        );

        self.broadcast(announcement);
    }

    /// How many parents the pane already has above it.
    fn depth_of(&self, pane: PaneId) -> u8 {
        let mut depth = 0;
        let mut current = self.panes.get(&pane).and_then(|p| p.parent);

        while let Some(id) = current {
            depth = depth.saturating_add(1);
            current = self.panes.get(&id).and_then(|p| p.parent);
        }

        depth
    }

    /// How many of a pane's subagents are still running.
    fn live_children(&self, parent: PaneId) -> usize {
        self.panes
            .values()
            .filter(|p| p.parent == Some(parent))
            .filter(|p| matches!(p.session.state(), RunState::Running))
            .count()
    }
```

5. The decision arm and the spawn it leads to:

```rust
            ClientMessage::DelegateDecision {
                request,
                approve,
                blanket,
            } => {
                let Some(waiting) = self.pending.remove(&request) else {
                    // Already answered, by another client or by the deadline.
                    return;
                };

                if !approve {
                    self.send(
                        waiting.caller,
                        ServerMessage::DelegateResolved {
                            request,
                            outcome: DelegateOutcome::Denied,
                        },
                    );
                    return;
                }

                if blanket {
                    self.blanket.insert(waiting.parent);
                }

                self.approve(
                    request,
                    waiting.parent,
                    &waiting.harness,
                    &waiting.task,
                    waiting.size,
                    waiting.caller,
                    blanket,
                );
            }
```

```rust
    /// Starts an approved subagent and tells everyone.
    #[allow(clippy::too_many_arguments)]
    fn approve(
        &mut self,
        request: RequestId,
        parent: PaneId,
        harness: &str,
        task: &str,
        size: (u16, u16),
        caller: ClientId,
        durable: bool,
    ) {
        let Some(asking) = self.panes.get(&parent) else {
            return;
        };
        let project = asking.project;

        let Some(root) = self.projects.get(&project).map(|p| p.root.clone()) else {
            return;
        };

        let Some(launch) = self
            .harnesses
            .get(harness)
            .and_then(|def| def.task_launch(task))
        else {
            self.send(
                caller,
                ServerMessage::DelegateResolved {
                    request,
                    outcome: DelegateOutcome::Refused {
                        reason: format!("harness {harness:?} has no [task] form"),
                    },
                },
            );
            return;
        };

        let id = PaneId::new();
        let mut launch = launch;
        for (key, value) in self.pane_env(id) {
            launch.env.entry(key).or_insert(value);
        }

        let session = match Pty::spawn(&launch, &root, Size::new(size.0, size.1)) {
            Ok(session) => session,
            Err(error) => {
                self.send(
                    caller,
                    ServerMessage::DelegateResolved {
                        request,
                        outcome: DelegateOutcome::Refused {
                            reason: format!("failed to start {harness}: {error}"),
                        },
                    },
                );
                return;
            }
        };

        self.panes.insert(
            id,
            DaemonPane {
                id,
                session,
                harness: harness.to_string(),
                project,
                history: Vec::new(),
                status: PaneStatus::Starting,
                parent: Some(parent),
                durable,
                request: Some(request),
                caller: Some(caller),
            },
        );

        self.send(
            caller,
            ServerMessage::DelegateResolved {
                request,
                outcome: DelegateOutcome::Approved { pane: id },
            },
        );

        self.broadcast(ServerMessage::PaneSpawned {
            pane: id,
            project,
            harness: harness.to_string(),
            parent: Some(parent),
        });
    }
```

6. Answering the caller when the subagent exits, inside `pump_panes` where the
   exit is already noticed:

```rust
        // A subagent's caller is waiting on exactly this.
        let mut answers = Vec::new();
        for (id, code) in &exited {
            let Some(pane) = self.panes.get_mut(id) else {
                continue;
            };

            if let (Some(request), Some(caller)) = (pane.request.take(), pane.caller) {
                let start = pane.history.len().saturating_sub(TAIL_BYTES);
                answers.push((
                    caller,
                    ServerMessage::DelegateFinished {
                        request,
                        exit: *code,
                        tail: pane.history[start..].to_vec(),
                    },
                ));
            }
        }

        for (caller, answer) in answers {
            self.send(caller, answer);
        }
```

with

```rust
/// How much of a subagent's output its caller is given.
///
/// Enough for an agent to act on, far short of a session: the pane keeps the
/// rest, and a person can open it.
const TAIL_BYTES: usize = 8 * 1024;
```

7. A caller that disappears, in the `Event::Detached` arm:

```rust
            Event::Detached(id) => {
                self.clients.remove(&id);
                self.abandon(id);
                tracing::info!(client = id, "client detached");
            }
```

```rust
    /// Drops what a departed client was waiting on.
    ///
    /// A one-off subagent exists to answer a caller. No caller, no reason to keep
    /// spending, so it goes. A blanket-approved one keeps running: that is what
    /// the user said when they approved the pane rather than the request.
    fn abandon(&mut self, caller: ClientId) {
        self.pending.retain(|_, waiting| waiting.caller != caller);

        let orphaned: Vec<PaneId> = self
            .panes
            .values()
            .filter(|pane| pane.caller == Some(caller) && !pane.durable)
            .map(|pane| pane.id)
            .collect();

        for id in orphaned {
            if let Some(mut pane) = self.panes.remove(&id) {
                tracing::info!(pane = %id, "the caller of a subagent is gone");
                pane.session.terminate();
                self.broadcast(ServerMessage::PaneClosed { pane: id });
            }
        }
    }
```

8. Expiring requests nobody answers, called from `pump_panes` so both `run` and
   `tick` reach it:

```rust
    /// Refuses requests whose time is up.
    ///
    /// The daemon owns this deadline. Without it an agent on an unattended daemon
    /// waits for a person who is not there, and a late approval would start a
    /// subagent nobody is waiting for.
    fn expire_requests(&mut self) {
        let limit = Duration::from_secs(self.limits.request_timeout_secs);

        let expired: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, waiting)| waiting.asked.elapsed() >= limit)
            .map(|(id, _)| *id)
            .collect();

        for request in expired {
            let Some(waiting) = self.pending.remove(&request) else {
                continue;
            };

            tracing::info!(%request, "a delegation request went unanswered");
            self.send(
                waiting.caller,
                ServerMessage::DelegateResolved {
                    request,
                    outcome: DelegateOutcome::Refused {
                        reason: format!(
                            "nobody answered within {} seconds",
                            self.limits.request_timeout_secs
                        ),
                    },
                },
            );
        }
    }
```

9. A closed pane can ask for nothing more, so its blanket approval goes with it —
   in the `ClosePane` arm, beside the existing `terminate`:

```rust
                    self.blanket.remove(&pane);
```

Closing a pane also applies the lifetime split to its children: one-off children
are terminated with it, durable ones are left running. Reuse `abandon`'s body by
extracting a helper that takes the ids to drop, rather than writing the loop
twice.

- [ ] **Step 9: Run to verify they pass**

Run: `cargo test -p dispatch-daemon 2>&1 | tail -30`

Expected: PASS, all daemon tests including the pre-existing ones.

- [ ] **Step 10: Commit**

```bash
cargo fmt --all
cargo clippy -p dispatch-daemon --all-targets
git add crates/dispatch-daemon
git commit -m "feat(daemon): hold delegation requests and own the subagents

A request arrives from a pane, is refused outright or put to the user, and on
approval becomes a pane the daemon owns with its parent recorded. The caller is
answered with the subagent's exit code and the last 8 KiB of its output, which is
a slice of the history replay already keeps rather than new storage.

Lifetime splits on how it was approved, as designed: a one-off subagent dies with
the caller that asked for it, and a blanket-approved one keeps running. A request
nobody answers is refused when its time is up, so an agent on an unattended daemon
fails cleanly instead of hanging.

Every pane now gets DISPATCH_PANE, the configuration directory, and the client
binary's directory on PATH, which is what makes the shim runnable from inside one."
```

---

### Task 6: The shim

**Files:**
- Create: `dispatch/src/delegate.rs`
- Modify: `dispatch/src/main.rs`
- Modify: `crates/dispatch-client/src/lib.rs`
- Test: `dispatchd/tests/serves_clients.rs`

**Interfaces:**
- Consumes: `Client` (existing), protocol messages (Task 4), daemon behaviour (Task 5).
- Produces: `Client::attach_as(role: Role, name: &str) -> Result<Client, ClientError>`; `dispatch delegate` subcommand; `delegate::run(args) -> anyhow::Result<std::process::ExitCode>`.

- [ ] **Step 1: Write the failing integration test**

Add to `dispatchd/tests/serves_clients.rs`, reusing its `attach`/`wait_for` helpers, and its shell harness — extended with a `[task]` form as in Task 5:

```rust
#[test]
fn a_delegate_caller_and_an_interface_client_share_one_daemon() {
    // The shim's path through the real binary: one connection asks, another
    // approves, and the asker is answered with the subagent's output.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let endpoint = Endpoint::new("delegate");

    let project_dir = endpoint.dir.join("project");
    std::fs::create_dir_all(&project_dir).expect("temp dir is writable");
    let _daemon = RunningDaemon::start(&endpoint.dir, &project_dir);

    // The interface client, which will approve.
    let (ui, mut ui_writer) = attach();
    let announced = wait_for(&ui, "the project", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::ProjectOpened { .. }))
    });
    let project = announced
        .iter()
        .find_map(|m| match m {
            ServerMessage::ProjectOpened { project } => Some(project.id),
            _ => None,
        })
        .expect("checked by wait_for");

    Frame::write(
        &mut ui_writer,
        &ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");
    let spawned = wait_for(&ui, "the parent pane", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });
    let parent = spawned
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
            _ => None,
        })
        .expect("checked by wait_for");

    // The delegate caller.
    let (caller, mut caller_writer) = attach_as(dispatch_proto::Role::Delegate);
    Frame::write(
        &mut caller_writer,
        &ClientMessage::DelegateRequest {
            parent,
            harness: "shell".into(),
            task: "echo delegated-$((6*7))".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");

    let asked = wait_for(&ui, "the pending request", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegatePending { .. }))
    });
    let request = asked
        .iter()
        .find_map(|m| match m {
            ServerMessage::DelegatePending { request, .. } => Some(*request),
            _ => None,
        })
        .expect("checked by wait_for");

    Frame::write(
        &mut ui_writer,
        &ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    )
    .expect("writing succeeds");

    let finished = wait_for(&caller, "the subagent's result", |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });
    let (exit, tail) = finished
        .iter()
        .find_map(|m| match m {
            ServerMessage::DelegateFinished { exit, tail, .. } => Some((*exit, tail.clone())),
            _ => None,
        })
        .expect("checked by wait_for");

    assert_eq!(exit, 0);
    assert!(
        String::from_utf8_lossy(&tail).contains("delegated-42"),
        "the arithmetic proves the subagent ran, got {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(
        !finished
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneOutput { .. })),
        "a delegate caller is spared the fleet's output"
    );
}
```

`attach_as(role)` is `attach()` with the role set; refactor `attach()` to call it with `Role::Interface`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dispatchd 2>&1 | tail -20`

Expected: FAIL to compile — `attach_as` is not defined.

- [ ] **Step 3: Add the role to the client, then write the shim**

In `crates/dispatch-client/src/lib.rs`, add `Client::attach_as(role, name)` and `attach_with_as(role, name, liveness)`, keeping `attach` and `attach_with` as `Role::Interface` wrappers so no existing caller changes. `Wire` carries the role so a reconnection re-announces it.

Create `dispatch/src/delegate.rs`:

```rust
//! `dispatch delegate`: asking Dispatch to run one task in a second agent.
//!
//! Runs inside a pane, as a command the agent there executes. It blocks until
//! the subagent has finished, prints what that subagent printed, and exits with
//! its code — the shape of every other tool an agent runs.
//!
//! Status lines go to stderr and the subagent's output to stdout, so
//! `dispatch delegate "…" > result.md` captures the work and nothing else while
//! a human watching the pane still sees progress.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dispatch_client::{Client, ClientError};
use dispatch_core::PaneId;
use dispatch_proto::{ClientMessage, DelegateOutcome, Role, ServerMessage};

/// Exit codes, following `sysexits(3)` so an agent can branch without parsing
/// prose. Documented in `--help` because an agent reads that far more often
/// than a README.
mod exit {
    /// No daemon is listening.
    pub const UNAVAILABLE: u8 = 69;
    /// The request was never answered.
    pub const TEMPFAIL: u8 = 75;
    /// The user said no.
    pub const NOPERM: u8 = 77;
    /// A cap, or a harness with no task form.
    pub const CONFIG: u8 = 78;
}

/// Runs one delegation to completion.
pub fn run(harness: Option<String>, size: (u16, u16), task: &str) -> Result<ExitCode> {
    let parent: PaneId = std::env::var("DISPATCH_PANE")
        .context("DISPATCH_PANE is not set: `dispatch delegate` runs inside a Dispatch pane")?
        .parse()
        .context("DISPATCH_PANE does not name a pane")?;

    let client = match Client::attach_as(Role::Delegate, "dispatch delegate") {
        Ok(client) => client,
        Err(ClientError::NotRunning(endpoint)) => {
            eprintln!("[dispatch] no daemon is listening on {endpoint}");
            return Ok(ExitCode::from(exit::UNAVAILABLE));
        }
        Err(error) => return Err(error).context("failed to reach the daemon"),
    };

    // The parent's own harness when none was named: the common case is an agent
    // delegating to another of itself.
    let harness = harness.unwrap_or_default();

    client.send(ClientMessage::DelegateRequest {
        parent,
        harness,
        task: task.to_string(),
        size,
    });
    eprintln!("[dispatch] waiting for approval (pane {parent})");

    // The daemon owns the approval deadline and refuses a request whose time is
    // up. This one is only a backstop for a daemon that dies without closing its
    // socket cleanly, so it is deliberately longer than any the daemon enforces:
    // the two must never race to answer the same request.
    let backstop = Instant::now() + Duration::from_secs(24 * 60 * 60);

    loop {
        for message in client.poll() {
            match message {
                ServerMessage::DelegateResolved { outcome, .. } => match outcome {
                    DelegateOutcome::Approved { pane } => {
                        eprintln!("[dispatch] approved; subagent pane {pane}");
                    }
                    DelegateOutcome::Denied => {
                        eprintln!("[dispatch] denied");
                        return Ok(ExitCode::from(exit::NOPERM));
                    }
                    DelegateOutcome::Refused { reason } => {
                        eprintln!("[dispatch] refused: {reason}");
                        return Ok(ExitCode::from(exit::CONFIG));
                    }
                },

                ServerMessage::DelegateFinished { exit, tail, .. } => {
                    use std::io::Write;
                    std::io::stdout()
                        .write_all(&tail)
                        .context("failed to write the subagent's output")?;
                    std::io::stdout().flush().ok();

                    eprintln!("[dispatch] subagent exited {exit}");
                    return Ok(ExitCode::from(u8::try_from(exit).unwrap_or(1)));
                }

                ServerMessage::Error { error } => {
                    eprintln!("[dispatch] {error}");
                    return Ok(ExitCode::from(exit::CONFIG));
                }

                _ => {}
            }
        }

        if !client.is_connected() {
            eprintln!("[dispatch] the daemon stopped answering");
            return Ok(ExitCode::from(exit::TEMPFAIL));
        }

        if Instant::now() >= backstop {
            eprintln!("[dispatch] gave up waiting for the daemon");
            return Ok(ExitCode::from(exit::TEMPFAIL));
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}
```

In `dispatch/src/main.rs`, add a subcommand. `Args` gains:

```rust
    /// Subcommands. Absent means run the interface.
    #[command(subcommand)]
    command: Option<Command>,
```

```rust
/// What to do instead of drawing an interface.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Ask Dispatch to run one task in a second agent, and wait for it.
    ///
    /// Runs inside a Dispatch pane. Prints the subagent's output on stdout and
    /// progress on stderr, and exits with the subagent's own status: 69 when no
    /// daemon is listening, 75 when the request went unanswered, 77 when it was
    /// denied, 78 when it was refused.
    Delegate {
        /// Which harness to run. Defaults to this pane's own.
        #[arg(long)]
        harness: Option<String>,

        /// Size to start the subagent at, as COLSxROWS.
        #[arg(long, default_value = "80x24", value_parser = parse_size)]
        size: (u16, u16),

        /// What the subagent should do.
        task: String,
    },
}

/// Parses `COLSxROWS`.
fn parse_size(text: &str) -> Result<(u16, u16), String> {
    let (cols, rows) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected COLSxROWS, got {text:?}"))?;

    Ok((
        cols.parse().map_err(|_| format!("bad width {cols:?}"))?,
        rows.parse().map_err(|_| format!("bad height {rows:?}"))?,
    ))
}
```

and `main` branches before touching the terminal:

```rust
    if let Some(Command::Delegate { harness, size, task }) = args.command {
        return delegate::run(harness, size, &task).map(std::process::ExitCode::from);
    }
```

`main`'s return type becomes `Result<ExitCode>`, with the interface path returning `ExitCode::SUCCESS`. Logging for the shim goes to the daemon-adjacent client log as it already does; the shim must not write to stdout for anything but the tail.

An empty `harness` string means "the parent's own", which the daemon resolves; document that where `harness` is built.

- [ ] **Step 4: Run to verify the test passes**

Run: `cargo test -p dispatchd 2>&1 | tail -20`

Expected: PASS, three tests in that file.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
git add crates/dispatch-client dispatch dispatchd
git commit -m "feat(dispatch): add the delegate subcommand

An agent asks for a subagent by running a command, which is the shape of every
other tool it uses: run, read the output, check the status. It blocks until the
subagent exits, prints what it printed on stdout and progress on stderr, and
exits with the subagent's own code — 69, 75, 77 and 78 for no daemon, no answer,
denied and refused, following sysexits so an agent can branch without parsing
prose.

The client can now attach as a delegate caller rather than an interface, which is
what keeps the fleet's output away from a call that only waits on one request."
```

---

### Task 7: The sidebar tree

**Files:**
- Modify: `crates/dispatch-tui/src/sidebar.rs`
- Modify: `dispatch/src/app.rs`
- Test: `crates/dispatch-tui/src/sidebar/tests.rs`

**Interfaces:**
- Consumes: `AppState::children_of`, `Pane::{parent, closed, status}` (Task 3).
- Produces: nested rows with outcome markers; `App` filtering unopened children out of the grid.

- [ ] **Step 1: Write the failing tests**

Add to `crates/dispatch-tui/src/sidebar/tests.rs`, following the rendering-assertion style already there:

```rust
#[test]
fn a_subagent_is_listed_under_the_pane_that_asked_for_it() {
    let mut state = AppState::new();
    let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
    let parent = state
        .spawn_pane(project, HarnessId::new("claude"))
        .expect("the project exists");
    state.set_pane_title(parent, "Claude Code").expect("it exists");

    let mut child = Pane::new(project, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.title = "tests".into();
    state.adopt_pane(child).expect("the project exists");

    let lines = render(&state, 28, 6);

    let parent_row = lines
        .iter()
        .position(|l| l.contains("Claude Code"))
        .expect("the parent is listed");
    let child_row = lines
        .iter()
        .position(|l| l.contains("tests"))
        .expect("the child is listed");

    assert!(child_row > parent_row, "a child comes after its parent");
    assert!(
        indent(&lines[child_row]) > indent(&lines[parent_row]),
        "and is indented under it: {:?}",
        lines[child_row]
    );
}

#[test]
fn a_finished_subagent_shows_how_it_ended() {
    let mut state = AppState::new();
    let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
    let parent = state
        .spawn_pane(project, HarnessId::new("claude"))
        .expect("the project exists");

    let mut clean = Pane::new(project, HarnessId::new("claude"));
    clean.parent = Some(parent);
    clean.title = "tests".into();
    clean.status = PaneStatus::Exited(0);
    state.adopt_pane(clean).expect("the project exists");

    let mut failed = Pane::new(project, HarnessId::new("claude"));
    failed.parent = Some(parent);
    failed.title = "docs".into();
    failed.status = PaneStatus::Exited(1);
    state.adopt_pane(failed).expect("the project exists");

    let lines = render(&state, 28, 6);

    assert!(
        lines.iter().any(|l| l.contains("tests") && l.contains('✓')),
        "a clean exit is marked: {lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("docs") && l.contains('!')),
        "a failure is marked differently: {lines:#?}"
    );
}

#[test]
fn a_tombstone_says_it_is_closed_and_still_shows_its_children() {
    let mut state = AppState::new();
    let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
    let parent = state
        .spawn_pane(project, HarnessId::new("claude"))
        .expect("the project exists");
    state.set_pane_title(parent, "Claude Code").expect("it exists");

    let mut child = Pane::new(project, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.durable = true;
    child.title = "bench".into();
    state.adopt_pane(child).expect("the project exists");

    state.close_pane(parent).expect("the pane exists");
    let lines = render(&state, 28, 6);

    assert!(
        lines.iter().any(|l| l.contains("Claude Code") && l.contains('⊘')),
        "the closed parent is marked as such: {lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("bench")),
        "its surviving child is still reachable: {lines:#?}"
    );
}
```

`render` and `indent` are helpers: `render` draws the sidebar into a `Buffer` and returns trimmed lines — copy the existing test helper in that file rather than writing a second one — and `indent` counts leading spaces.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p dispatch-tui 2>&1 | tail -20`

Expected: FAIL — children render at the same indent, with no markers.

- [ ] **Step 3: Render the tree**

In `crates/dispatch-tui/src/sidebar.rs`, the pane loop walks top-level panes and
then each one's children, two columns further in:

```rust
            for pane in self.state.visible_panes() {
                if pane.parent.is_some() {
                    // Drawn under its parent, below, not in its own right.
                    continue;
                }

                y = self.render_pane(buf, area, y, pane, focused, 2);

                for child in self.state.children_of(pane.id) {
                    y = self.render_pane(buf, area, y, child, focused, 4);
                }
            }
```

`visible_panes` excludes tombstones, so a closed parent holding live children needs
its own pass — a row that exists only to keep its children reachable:

```rust
            for pane in self.state.tombstones_of(self.state.selected_project()) {
                y = self.render_pane(buf, area, y, pane, focused, 2);

                for child in self.state.children_of(pane.id) {
                    y = self.render_pane(buf, area, y, child, focused, 4);
                }
            }
```

`render_pane` is the existing row-drawing code, lifted into a method that takes an
indent and appends the outcome glyph. `tombstones_of` is a two-line addition to
`AppState` returning the closed panes of a project; add it in this task with a test
beside the ones from Task 3.

Each row ends with a marker:

```rust
/// What a pane's outcome looks like in one glyph.
fn outcome(pane: &Pane) -> (&'static str, Style) {
    if pane.closed {
        return ("⊘", Style::default().fg(Color::DarkGray));
    }

    match pane.status {
        PaneStatus::Exited(0) => ("✓", Style::default().fg(Color::Green)),
        PaneStatus::Exited(_) => ("!", Style::default().fg(Color::Red)),
        _ => ("⋯", Style::default().fg(Color::DarkGray)),
    }
}
```

A tombstone row is drawn dim and is never the focus marker's target.

- [ ] **Step 4: Keep children out of the grid**

In `dispatch/src/app.rs`, add `expanded: HashSet<PaneId>` to `App`, and filter before tiling:

```rust
    /// The panes to tile this frame.
    ///
    /// Children are left out unless the user has opened them: ten subagents
    /// would otherwise shrink every pane to nothing. Which rows are open is this
    /// client's business, so the daemon is never told.
    fn tileable(&self) -> Vec<PaneId> {
        self.state
            .visible_panes()
            .iter()
            .filter(|pane| pane.parent.is_none() || self.expanded.contains(&pane.id))
            .map(|pane| pane.id)
            .collect()
    }
```

`compute_layout` uses `tileable()` in place of its current `visible_panes` mapping. Opening a child is Task 8's key handling; for now `expanded` starts empty.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test -p dispatch-tui -p dispatch 2>&1 | tail -20`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p dispatch-tui -p dispatch --all-targets
git add crates/dispatch-tui dispatch/src/app.rs
git commit -m "feat(tui): show subagents under the pane that asked for them

One level of nesting, with a glyph for how each ended: running, clean, failed, or
a closed parent kept only to hold live children.

Children are not tiled unless the user opens one. Ten subagents would otherwise
shrink every pane to nothing, and which rows are open is this client's business —
two clients on one fleet can disagree about it, so the daemon is never told."
```

---

### Task 8: The approval prompt

**Files:**
- Create: `dispatch/src/approval.rs`
- Modify: `dispatch/src/app.rs`
- Modify: `crates/dispatch-tui/src/input.rs`
- Test: `dispatch/tests/end_to_end.rs`

**Interfaces:**
- Consumes: `ServerMessage::{DelegatePending, DelegateResolved}` (Task 4), `Overlay` (existing in `dispatch/src/app.rs`).
- Produces: `approval::Approval` widget; `Action::Approvals`; `App` handling of pending requests.

- [ ] **Step 1: Write the failing end-to-end test**

Add to `dispatch/tests/end_to_end.rs`. It needs the test shell harness to have a `[task]` form; add it to `SHELL_HARNESS`.

```rust
#[test]
fn a_delegation_is_approved_by_hand_and_its_output_comes_back() {
    // The whole product promise in one test: an agent asks, a person says yes,
    // a second agent runs, and the first one reads the result.
    let fixture = Fixture::new("d7");
    let _daemon = Daemon::start(&fixture);
    let mut dispatch = Harness::attached(&fixture, Size::new(100, 30));

    assert!(dispatch.wait_for(|lines| contains(lines, "project")));
    dispatch.spawn_shell();
    assert!(
        dispatch.wait_for(|lines| contains(lines, "$")),
        "the parent shell should be ready"
    );

    dispatch.send(b"dispatch delegate \"echo delegated-$((6*7))\"\r");

    assert!(
        dispatch.wait_for(|lines| contains(lines, "wants to delegate")),
        "the approval prompt should open"
    );
    dispatch.send(b"a");

    assert!(
        dispatch.wait_for(|lines| sidebar_panes(lines) == 2),
        "the subagent should be listed under its parent"
    );
    assert!(
        dispatch.wait_for(|lines| contains(lines, "delegated-42")),
        "the parent pane should receive the subagent's output"
    );
}

#[test]
fn a_denied_delegation_runs_nothing_and_says_so() {
    let fixture = Fixture::new("d8");
    let _daemon = Daemon::start(&fixture);
    let mut dispatch = Harness::attached(&fixture, Size::new(100, 30));

    assert!(dispatch.wait_for(|lines| contains(lines, "project")));
    dispatch.spawn_shell();
    assert!(dispatch.wait_for(|lines| contains(lines, "$")));

    dispatch.send(b"dispatch delegate \"echo never-run\"\r");
    assert!(dispatch.wait_for(|lines| contains(lines, "wants to delegate")));

    dispatch.send(b"d");

    assert!(
        dispatch.wait_for(|lines| contains(lines, "denied")),
        "the agent should be told, in its own pane"
    );
    assert_eq!(
        sidebar_panes(&dispatch.lines()),
        1,
        "and nothing should have been started"
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p dispatch --test end_to_end delegation 2>&1 | tail -30`

Expected: FAIL — no prompt appears; `wants to delegate` never shows.

- [ ] **Step 3: Write the overlay**

Create `dispatch/src/approval.rs`:

```rust
//! The prompt that asks whether a pane may delegate.
//!
//! Shows the whole task. Approving something you cannot read is not approval, so
//! a long task wraps and scrolls rather than being cut to fit.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

/// One request, as the user needs to see it.
pub struct Approval<'a> {
    /// Title of the pane that asked.
    pub asking: &'a str,
    /// Which harness would run.
    pub harness: &'a str,
    /// Which project it would run in.
    pub project: &'a str,
    /// How deep the asking pane already is.
    pub depth: u8,
    /// What it would be asked to do.
    pub task: &'a str,
    /// How many further requests are queued behind this one.
    pub waiting: usize,
    /// First line of the task to show, for scrolling a long one.
    pub scroll: u16,
}

impl Widget for Approval<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} wants to delegate ", self.asking));
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines = vec![
            Line::from(vec![
                Span::styled("harness  ", Style::default().fg(Color::DarkGray)),
                Span::raw(self.harness),
                Span::styled("   project  ", Style::default().fg(Color::DarkGray)),
                Span::raw(self.project),
                Span::styled("   depth  ", Style::default().fg(Color::DarkGray)),
                Span::raw(self.depth.to_string()),
            ]),
            Line::from(""),
        ];

        for line in self.task.lines() {
            lines.push(Line::from(line.to_string()));
        }

        lines.push(Line::from(""));
        if self.waiting > 0 {
            lines.push(Line::styled(
                format!("{} more waiting", self.waiting),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(vec![
            Span::styled("a", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" approve   "),
            Span::styled("d", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" deny   "),
            Span::styled("A", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" approve all from this pane   "),
            Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" later"),
        ]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .render(inner, buf);
    }
}
```

The task text is never truncated: `Wrap` folds it, and `scroll` moves through a
long one with `↑`/`↓` handled beside the approval keys.

- [ ] **Step 4: Wire it into the app**

In `dispatch/src/app.rs`:

1. `pending: VecDeque<PendingRequest>` on `App`, filled from `ServerMessage::DelegatePending` and drained by `DelegateResolved` — a request another client answered disappears from this one's queue.
2. `Overlay::Approval` added to the existing overlay enum, opened when a request arrives and the overlay is free, so a keystroke meant for an agent can never land on it.
3. `handle_overlay` gains the approval keys: `a` sends `DelegateDecision { approve: true, blanket: false }`, `d` sends `approve: false`, `A` sends `approve: true, blanket: true`, `Esc` closes the overlay and leaves the request queued.
4. When the overlay closes with requests still queued, the status line reads `N delegation(s) waiting — ^a p`.
5. `Action::Approvals` reopens it. Add that variant in `crates/dispatch-tui/src/input.rs` and bind `p` after the prefix, beside the existing `n`, `x`, `z` bindings.
6. Enter on a child row in the sidebar adds it to `expanded` — the sidebar is not focusable today, so bind this to the existing pane-focus path: focusing a child pane adds it to `expanded`, and closing it removes it.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test -p dispatch --test end_to_end 2>&1 | tail -20`

Expected: PASS, every end-to-end test.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
git add dispatch crates/dispatch-tui
git commit -m "feat(dispatch): ask before running a subagent

The prompt takes the keyboard like the pickers do, so a keystroke meant for an
agent can never land on an approval. It shows the whole task text, because
approving something you cannot read is not approval.

Esc defers rather than denying: a mistaken deny throws away work the agent has
already reasoned about. The status line then says how many are waiting and which
key reopens them, so nothing is ever decided by inaction."
```

---

### Task 9: Documentation, and the manual check

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-09-19-orchestrator-delegation-design.md`

- [ ] **Step 1: Write the README section**

Add after the daemon section, in the README's voice:

```markdown
## Delegation

An agent in a pane can ask for a second agent to work on something:

```sh
dispatch delegate "write the tests for the http client"
```

Dispatch asks you first, every time. The subagent runs as a pane under the one
that asked, and the caller gets its output and exit code when it finishes.

Delegation needs two things. The daemon must own the panes (`--attach`), because
it is what starts the subagent; and the harness must declare a non-interactive
form, since an interactive agent never exits:

```toml
# ~/.config/dispatch/harnesses/claude.toml
[task]
args = ["-p", "{task}"]
```

`claude` and `codex` ship with one. Caps live in `config.toml`, and refuse rather
than prompt:

```toml
[delegation]
max_depth = 1              # a subagent cannot delegate
max_live_per_parent = 4
request_timeout_secs = 600
```
```

- [ ] **Step 2: Record the manual check in the spec**

Append to the spec's testing section:

```markdown
### Manual check, performed once per release

With a real `claude` harness and credentials:

1. `dispatchd <project>` and `dispatch --attach`.
2. Spawn a `claude` pane. Ask it to run `dispatch delegate "summarise this repo"`.
3. The prompt appears; approve with `a`.
4. The subagent appears nested, runs, and exits; its summary arrives in the
   parent pane, and the parent agent can quote it back.
```

- [ ] **Step 3: Full verification**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Expected: no formatting diff, no warnings, every test passing.

- [ ] **Step 4: Commit**

```bash
git add README.md docs
git commit -m "docs: document delegation

What it does, the two things it needs — a daemon that owns the panes and a
harness with a non-interactive form — and the caps that refuse rather than
prompt. Plus the one check that cannot be automated, because it needs a model."
```

---

## Notes for whoever executes this

- **Run the workspace, not one package.** `cargo test -p dispatch` does not rebuild `dispatchd`, so the end-to-end tests will happily exercise a stale daemon binary. This has already cost one confusing debugging session; `cargo test --workspace` is the honest command.
- **The test harness with `[task] args = ["-c", "{task}"]` is the whole testing strategy.** It makes a subagent any shell command, so every layer is proved with real processes and no model. If a test seems to need mocking, reach for that harness instead.
- **Keep `DISPATCH_PANE` honest.** It is attribution. The socket is owner-only and anything that can connect can already spawn panes; a check that pretends the variable is a permission would be security theatre.
- **`DISPATCH_E2E_KEEP=1`** leaves the end-to-end fixtures behind, logs included. These tests drive whole processes, and the daemon's log is usually the only record of why one did the wrong thing.
