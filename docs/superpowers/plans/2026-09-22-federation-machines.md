# Federation F2b — Machines Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Dispatch remembers the machines a user reaches over ssh, dials every one of them in the background so a machine asleep at startup joins when it wakes, and lets the user open a project on a remote machine by typing its path.

**Architecture:** A `machines.toml` registry in `dispatch-config`, edited by `dispatch machine add/list/remove` and an in-TUI `^a m` overlay. `Client::dial` makes a client that starts disconnected at generation 0 and lets the existing supervisor make the first dial, so a first connection is just a generation change the App already handles. Kept projects become per machine, and the daemon answers a root it cannot open with `ServerMessage::ProjectRefused`, which the client uses to forget it.

**Tech Stack:** Rust 2024 (1.85+), serde + toml, clap, ratatui/crossterm, std threads and `mpsc`. `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt`.

**Spec:** `docs/superpowers/specs/2026-09-22-federation-machines-design.md`

## Global Constraints

- Rust edition 2024, rust-version as pinned in the workspace `Cargo.toml`. Do not raise it.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets` (zero warnings) and `cargo test --workspace` must pass before every commit. Run the workspace, not one package: `dispatch`'s end-to-end tests use the `dispatchd` binary the last workspace build left beside it.
- TDD: write the failing test, RUN it, and record the failure before implementing.
- No new third-party dependencies. `directories`, `serde`, `toml`, `clap` and `thiserror` are already in the workspace.
- `ProtocolError` gains no variant: it is externally tagged with no `Unknown`, so a new variant fails an older peer's whole frame. New reasons go in `ServerMessage`, which has `#[serde(other)] Unknown`. `dispatch_proto::VERSION` stays `1.1`.
- The default machine command is exactly `ssh -T -o BatchMode=yes -o ConnectTimeout=10 <target> dispatchd --stdio`.
- Backoff: an `Endpoint` dial keeps 100ms first retry and 2s ceiling; a `Command` dial starts at 1s and stops doubling at 30s.
- A machine name is one or more of `A-Z a-z 0-9 - _`.
- `^a m` is refused while Dispatch runs standalone. Attaching would tear down every standalone pane.
- Doc comments on every public item, and on private items where the file already does so, in the house style: say WHY, not what. Match the prose of the file being edited.
- Tests that need a POSIX helper (`sh`, `true`) are gated `#[cfg(unix)]`, or `#[cfg_attr(windows, ignore = "...")]` in `dispatch/tests/`, matching the existing files.
- Commit messages: Conventional Commits, ending with exactly this line and nothing after it:
  `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`

## Review Focus

1. **`^a m` pressed while standalone.** A user who never passed `--attach` expects a message, not every running agent killed. Pinned in Task 10 (`adding_a_machine_is_refused_while_standalone`).
2. **Enter on an empty or whitespace-only prompt.** Expect nothing to happen: no `OpenProject` for `""`, no machine named `""`. Pinned in Task 8 (`an_answer_is_what_was_typed_without_its_edges`), Task 9 (`enter_on_an_empty_path_does_nothing`) and Task 10 (`enter_on_an_empty_target_does_nothing`).
3. **Esc while a machine check is in flight.** Expect nothing saved and no row added when the check later succeeds. Pinned in Task 10 (`closing_the_overlay_mid_check_saves_nothing`).
4. **Targets that are not a bare hostname** — `me@tower.lan`, `ssh://me@tower:2222`, `192.168.1.5`. Expect a usable default name each time, never an invalid one. Pinned in Task 2 (`a_default_name_comes_from_the_targets_host`).
5. **An empty `machines.toml`** (a user who created the file and deleted every entry). Expect no machines and plain standalone start, not a parse error. Pinned in Task 2 (`an_empty_file_is_no_machines`).

## File structure

| File | Responsibility |
|---|---|
| `crates/dispatch-proto/src/message.rs` | `ServerMessage::ProjectRefused`. |
| `crates/dispatch-os/src/paths.rs` | `expand_home`. |
| `crates/dispatch-daemon/src/session.rs` | `open_project_for` expands `~` and answers `ProjectRefused`. |
| `crates/dispatch-config/src/machines.rs` (new) + `machines/tests.rs` | The registry: `Machine`, `Command`, names, `load/save/check/add/remove`. |
| `crates/dispatch-config/src/projects.rs` | Per-machine kept roots: `load_on`, `remember_on`, `forget_on`, `forget_machine`. |
| `crates/dispatch-config/src/lib.rs` | `pub mod machines;`, `ConfigError::Machine`. |
| `crates/dispatch-client/src/lib.rs` | `Client::dial`, per-dial backoff, `last_error`, `dialled`, the in-flight-dial fix, test helpers. |
| `crates/dispatch-core/src/device.rs` | `Device::pending`. |
| `crates/dispatch-tui/src/prompt.rs` (new) + `prompt/tests.rs` | One-line text input overlay. |
| `crates/dispatch-tui/src/picker.rs` | `centred` and `write` become `pub(crate)` so the prompt shares them. |
| `crates/dispatch-tui/src/input.rs` | `Action::AddMachine` on `m`. |
| `dispatch/src/app.rs` | Labelled/remote attachments, first-connect and outage status, per-machine keeping, `ProjectRefused`, the `^a o` machine step, the `^a m` overlay's wiring. |
| `dispatch/src/add_machine.rs` (new) | The `^a m` overlay's state machine and its background check. |
| `dispatch/src/machine.rs` (new) | `dispatch machine add/list/remove`. |
| `dispatch/src/main.rs` | The registry implies attach; every machine and `--daemon`/`--daemon-command` dialled with `Client::dial`; the `machine` subcommand. |
| `dispatch/tests/end_to_end.rs` | A machine asleep at startup joins when it wakes. |
| `dispatch/tests/machine_verbs.rs` (new) | The CLI verbs against real processes. |
| `README.md`, `docs/superpowers/federation-handoff.md` | What it is for; where federation stands now. |

---

### Task 1: A root the daemon cannot open is refused by name

**Files:**
- Modify: `crates/dispatch-proto/src/message.rs` (the `ServerMessage` enum, after `ProjectClosed`)
- Modify: `crates/dispatch-proto/src/message/tests.rs`
- Modify: `crates/dispatch-os/src/paths.rs` (new fn after `resolve`, test in the inline `mod tests`)
- Modify: `crates/dispatch-daemon/src/session.rs:516-548` (`open_project_for`)
- Modify: `crates/dispatch-daemon/src/session/tests.rs:717-755` (`opening_a_path_that_is_not_a_directory_is_reported`)
- Modify: `dispatch/src/app.rs:1251-1254` (the ignored arm of `apply_from`)

**Interfaces:**
- Produces: `ServerMessage::ProjectRefused { root: PathBuf, reason: String }`; `dispatch_os::paths::expand_home(path: &Path) -> PathBuf`.

- [ ] **Step 1: Write the failing tests**

In `crates/dispatch-proto/src/message/tests.rs`, add:

```rust
#[test]
fn a_refused_root_round_trips_and_an_older_peer_skips_it() {
    // The root travels back exactly as it was sent, so the client can find
    // it in its own kept list without resolving anything itself.
    let refused = ServerMessage::ProjectRefused {
        root: PathBuf::from("~/code/typo"),
        reason: "No such file or directory".into(),
    };
    assert_eq!(round_trip(&refused), refused);

    // An older client has no `project_refused`. It must land in `Unknown`
    // rather than failing the frame: a fleet is exactly where an older
    // client meets a newer daemon.
    #[derive(Serialize)]
    struct FromNewer {
        #[serde(rename = "type")]
        kind: &'static str,
        root: &'static str,
        reason: &'static str,
    }
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &FromNewer {
            kind: "some_message_from_the_future",
            root: "~/x",
            reason: "no",
        },
    )
    .expect("writing succeeds");
    let read: ServerMessage = Frame::read(&mut buf.as_slice()).expect("reading succeeds");
    assert_eq!(read, ServerMessage::Unknown);
}
```

In the inline `mod tests` of `crates/dispatch-os/src/paths.rs`, add:

```rust
    #[test]
    fn a_leading_tilde_is_the_home_directory() {
        // A path typed by hand for another machine starts with `~` more
        // often than not, and no shell stands between the client and the
        // daemon to expand it.
        let home = directories::BaseDirs::new()
            .expect("a home directory exists in the test environment")
            .home_dir()
            .to_path_buf();

        assert_eq!(expand_home(Path::new("~/code/app")), home.join("code/app"));
        assert_eq!(expand_home(Path::new("~")), home.join(""));
        assert_eq!(
            expand_home(Path::new("/srv/app")),
            PathBuf::from("/srv/app"),
            "an absolute path is left alone"
        );
        assert_eq!(
            expand_home(Path::new("~someone/app")),
            PathBuf::from("~someone/app"),
            "another user's home is not ours to guess"
        );
    }
```

In `crates/dispatch-daemon/src/session/tests.rs`, replace the body of `opening_a_path_that_is_not_a_directory_is_reported` and add a second test after it:

```rust
#[test]
fn opening_a_path_that_is_not_a_directory_is_reported() {
    let (mut daemon, _, dir) = daemon("open-bad");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    let _ = drain(&inbox);

    let file = dir.0.join("not-a-directory");
    std::fs::write(&file, b"contents").expect("temp dir is writable");

    daemon.request_for_test(1, ClientMessage::OpenProject { root: file.clone() });
    assert!(
        matches!(
            drain(&inbox).first(),
            Some(ServerMessage::ProjectRefused { root, .. }) if *root == file
        ),
        "a file is not a project, and the refusal names it as it was sent"
    );

    let missing = dir.0.join("missing");
    daemon.request_for_test(1, ClientMessage::OpenProject { root: missing.clone() });
    assert!(
        matches!(
            drain(&inbox).first(),
            Some(ServerMessage::ProjectRefused { root, .. }) if *root == missing
        ),
        "a path that does not exist is not a project"
    );

    assert_eq!(daemon.projects().len(), 1, "neither was registered");
}

#[test]
fn a_root_under_home_is_opened_from_the_daemons_own_home() {
    // The daemon is on the machine the directory is on, so its home is the
    // one `~` means. `~` itself always exists, so this needs no scratch
    // directory under the real home.
    let (mut daemon, _, _dir) = daemon("open-home");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::OpenProject {
            root: PathBuf::from("~"),
        },
    );

    // Through `expand_home` rather than `directories`, which this crate does
    // not depend on: what is under test is that the daemon expands at all.
    let home = dispatch_os::paths::expand_home(Path::new("~"));
    let expected = dispatch_os::paths::resolve(&home).expect("home resolves");

    assert!(
        drain(&inbox).iter().any(|message| matches!(
            message,
            ServerMessage::ProjectOpened { project } if project.root == expected
        )),
        "`~` should open the daemon's home"
    );
}
```

Import `std::path::Path` in the test module if it is not already in scope.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-proto a_refused_root; cargo test -p dispatch-os a_leading_tilde; cargo test -p dispatch-daemon opening_a_path a_root_under_home`
Expected: compile errors — `ProjectRefused` and `expand_home` do not exist.

- [ ] **Step 3: Implement**

In `crates/dispatch-proto/src/message.rs`, add to `ServerMessage` after `ProjectClosed`:

```rust
    /// A root asked for in [`ClientMessage::OpenProject`] could not be opened.
    ///
    /// Sent only to the client that asked. Its own message rather than an
    /// [`ProtocolError`]: that enum has no `Unknown` to land in, so a variant
    /// added there would fail an older client's whole frame — and a fleet is
    /// where an older client meets a newer daemon. An older client skips this
    /// and loses only the status line.
    ProjectRefused {
        /// The root exactly as the client sent it, so the client can find it
        /// in what it keeps without resolving anything itself.
        root: PathBuf,
        /// Why: not a directory, no such file, permission denied.
        reason: String,
    },
```

In `crates/dispatch-os/src/paths.rs`, after `resolve`:

```rust
/// Expands a leading `~` against the home directory of the user running this
/// process.
///
/// A path typed by hand for another machine almost always starts with one,
/// and no shell stands between the client and the daemon to expand it:
/// without this the daemon looks for a directory literally named `~` in
/// whatever directory it was started in. Only `~` and `~/…` are expanded;
/// `~user` is left as typed, because another user's home is not this
/// process's to guess.
pub fn expand_home(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };

    match directories::BaseDirs::new() {
        Some(dirs) => dirs.home_dir().join(rest),
        None => path.to_path_buf(),
    }
}
```

In `crates/dispatch-daemon/src/session.rs`, replace the `resolved` match in `open_project_for`:

```rust
        // `~` first: a root typed for this machine from another one arrives
        // with no shell having expanded it.
        let expanded = dispatch_os::paths::expand_home(&root);

        let reason = match dispatch_os::paths::resolve(&expanded) {
            Ok(resolved) if resolved.is_dir() => {
                self.opened_for(resolved);
                return;
            }
            Ok(resolved) => format!("not a directory: {}", resolved.display()),
            Err(error) => error.to_string(),
        };

        // Named by the root as it was sent, not as it resolved: the client
        // keeps what it typed, and has to be able to find it to forget it.
        self.send(client, ServerMessage::ProjectRefused { root, reason });
    }
```

and move the rest of the old function body (from `let id = self.open_project(resolved);` to the end) into a new private method directly below it:

```rust
    /// Registers a root that has been resolved and checked, and tells every
    /// subscriber.
    fn opened_for(&mut self, resolved: PathBuf) {
        let id = self.open_project(resolved);
        // ... the remainder of the old `open_project_for` body, unchanged ...
    }
```

(Copy the remaining lines verbatim; they reference only `id`, `project` and `self`.)

In `dispatch/src/app.rs`, add `ServerMessage::ProjectRefused { .. }` to the ignored arm so the workspace compiles; Task 5 gives it behaviour:

```rust
            ServerMessage::Welcome { .. }
            | ServerMessage::Pong { .. }
            | ServerMessage::DelegateFinished { .. }
            | ServerMessage::ProjectRefused { .. }
            | ServerMessage::Unknown => false,
```

If `cargo build --workspace` reports another exhaustive `match` on `ServerMessage` (for example in `dispatch/src/delegate.rs`), add `ProjectRefused { .. }` to that match's ignored arm the same way.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dispatch-proto crates/dispatch-os/src/paths.rs crates/dispatch-daemon/src/session.rs crates/dispatch-daemon/src/session/tests.rs dispatch/src/app.rs
git commit -m "feat(dispatchd): refuse a root by name, and read ~ as this machine's home

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: The machine registry

**Files:**
- Create: `crates/dispatch-config/src/machines.rs`
- Create: `crates/dispatch-config/src/machines/tests.rs`
- Modify: `crates/dispatch-config/src/lib.rs` (`pub mod machines;` and a `ConfigError::Machine` variant)

**Interfaces:**
- Produces:
  - `pub struct Machine { pub name: String, pub target: String, pub command: Option<Command> }` (`Debug, Clone, PartialEq, Eq, Serialize, Deserialize`)
  - `pub struct Command { pub program: String, pub args: Vec<String> }`
  - `Machine::new(name: impl Into<String>, target: impl Into<String>) -> Machine`
  - `Machine::dial(&self) -> (OsString, Vec<OsString>)`
  - `Machine::describe(&self) -> String` — the command as one line, for messages
  - `pub fn valid_name(name: &str) -> bool`
  - `pub fn default_name(target: &str) -> Option<String>`
  - `pub fn load(dir: &Path) -> Result<Vec<Machine>, ConfigError>`
  - `pub fn save(dir: &Path, machines: &[Machine]) -> Result<(), ConfigError>`
  - `pub fn check(dir: &Path, name: &str, this_host: &str) -> Result<(), ConfigError>`
  - `pub fn add(dir: &Path, machine: Machine, this_host: &str) -> Result<(), ConfigError>`
  - `pub fn remove(dir: &Path, name: &str) -> Result<bool, ConfigError>`
  - `ConfigError::Machine { path: PathBuf, reason: String }`, displayed as `{path}: {reason}`

- [ ] **Step 1: Write the failing tests**

Create `crates/dispatch-config/src/machines/tests.rs`:

```rust
//! Tests for the machine registry.

use super::*;

use crate::testing::TempDir;

#[test]
fn nothing_is_registered_before_anything_is_added() {
    let dir = TempDir::new("machines-absent");
    assert!(load(dir.path()).expect("an absent file is not an error").is_empty());
}

#[test]
fn an_empty_file_is_no_machines() {
    // A user who created the file and then deleted every entry: that is no
    // machines, not a broken configuration.
    let dir = TempDir::new("machines-empty");
    dir.write("machines.toml", "");
    assert!(load(dir.path()).expect("an empty file parses").is_empty());
}

#[test]
fn a_machine_survives_a_restart_with_and_without_a_command() {
    let dir = TempDir::new("machines-roundtrip");
    let plain = Machine::new("tower", "me@tower");
    let custom = Machine {
        name: "gpu".into(),
        target: "gpu-box".into(),
        command: Some(Command {
            program: "/Applications/My Tools/tunnel".into(),
            args: vec!["gpu-box".into(), "dispatchd".into(), "--stdio".into()],
        }),
    };

    save(dir.path(), &[plain.clone(), custom.clone()]).expect("the directory is writable");

    assert_eq!(load(dir.path()).expect("it reads back"), [plain, custom]);
}

#[test]
fn the_default_command_is_ssh_that_never_prompts() {
    // `BatchMode` is what keeps ssh from asking for a password on the
    // terminal the interface is drawn on.
    let (program, args) = Machine::new("tower", "me@tower").dial();

    assert_eq!(program, "ssh");
    assert_eq!(
        args,
        [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "me@tower",
            "dispatchd",
            "--stdio"
        ]
        .map(OsString::from)
    );
}

#[test]
fn a_command_replaces_the_default_whole() {
    let machine = Machine {
        name: "gpu".into(),
        target: "gpu-box".into(),
        command: Some(Command {
            program: "/Applications/My Tools/tunnel".into(),
            args: vec!["--stdio".into()],
        }),
    };

    let (program, args) = machine.dial();

    assert_eq!(program, "/Applications/My Tools/tunnel", "one program, space and all");
    assert_eq!(args, [OsString::from("--stdio")]);
}

#[test]
fn a_default_name_comes_from_the_targets_host() {
    assert_eq!(default_name("tower").as_deref(), Some("tower"));
    assert_eq!(default_name("me@tower.lan").as_deref(), Some("tower"));
    assert_eq!(default_name("ssh://me@tower:2222").as_deref(), Some("tower"));
    assert_eq!(default_name("gpu-box").as_deref(), Some("gpu-box"));
    assert_eq!(
        default_name("192.168.1.5").as_deref(),
        Some("192-168-1-5"),
        "an address keeps all four parts rather than becoming `192`"
    );
    assert_eq!(default_name("me@").as_deref(), None, "nothing to name it after");
}

#[test]
fn a_name_is_letters_digits_dashes_and_underscores() {
    assert!(valid_name("tower"));
    assert!(valid_name("gpu_box-2"));
    assert!(!valid_name(""));
    assert!(!valid_name("my tower"));
    assert!(!valid_name("tower.lan"));
}

#[test]
fn adding_refuses_a_name_already_taken() {
    let dir = TempDir::new("machines-duplicate");
    add(dir.path(), Machine::new("tower", "me@tower"), "laptop").expect("the first is added");

    let error = add(dir.path(), Machine::new("tower", "other@tower"), "laptop")
        .expect_err("the name is taken");

    assert!(error.to_string().contains("already registered"), "{error}");
    assert_eq!(load(dir.path()).expect("it reads back").len(), 1);
}

#[test]
fn adding_refuses_this_machines_own_name() {
    // The local daemon's row already carries the hostname.
    let dir = TempDir::new("machines-self");

    let error = add(dir.path(), Machine::new("laptop", "laptop"), "Laptop.local")
        .expect_err("that is this machine");

    assert!(error.to_string().contains("this machine"), "{error}");
}

#[test]
fn adding_refuses_a_name_that_is_not_one() {
    let dir = TempDir::new("machines-invalid");

    let error = add(dir.path(), Machine::new("my tower", "tower"), "laptop")
        .expect_err("a space is not allowed");

    assert!(error.to_string().contains("not a machine name"), "{error}");
}

#[test]
fn removing_answers_whether_it_was_there() {
    let dir = TempDir::new("machines-remove");
    add(dir.path(), Machine::new("tower", "me@tower"), "laptop").expect("added");

    assert!(remove(dir.path(), "tower").expect("written"));
    assert!(!remove(dir.path(), "tower").expect("nothing to write"));
    assert!(load(dir.path()).expect("it reads back").is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-config machines`
Expected: compile error — module `machines` not found.

- [ ] **Step 3: Implement**

In `crates/dispatch-config/src/lib.rs`, add `pub mod machines;` beside `pub mod projects;`, and add to `ConfigError`:

```rust
    /// A machine could not be registered under the name it was given.
    #[error("{path}: {reason}")]
    Machine {
        /// The registry file.
        path: PathBuf,
        /// What is wrong with the name.
        reason: String,
    },
```

Create `crates/dispatch-config/src/machines.rs`:

```rust
//! The machines a user has registered.
//!
//! A machine is somewhere Dispatch reaches a daemon by running a command —
//! `ssh <target> dispatchd --stdio` unless told otherwise. The list is the
//! user's: nothing here dials, and nothing is on it that `dispatch machine
//! add` or the add overlay did not put there.

use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// The file the list lives in, inside the configuration directory.
const FILE: &str = "machines.toml";

/// What ssh is told before the target.
///
/// `-T` because the bytes are frames, not a session. `BatchMode` because
/// without it ssh asks for a password or a host key on `/dev/tty` — the very
/// terminal the interface is drawn on — and the dial hangs behind the prompt.
/// With it ssh fails at once, saying why on stderr, which the client already
/// surfaces. `ConnectTimeout` so an asleep host fails a dial in seconds rather
/// than the kernel's TCP timeout.
const SSH_OPTIONS: &[&str] = &["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"];

/// What ssh runs on the far side.
const REMOTE: &[&str] = &["dispatchd", "--stdio"];

/// A machine Dispatch can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// What the sidebar and the command line call it.
    pub name: String,
    /// Where ssh connects: a host, `user@host`, or an alias from the user's
    /// ssh configuration.
    pub target: String,
    /// A command that replaces the default ssh one entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Command>,
}

/// A program and its arguments, kept apart.
///
/// Apart so a local program whose path holds a space can be named, which a
/// whitespace-split string cannot do. Strings rather than `OsString`s: serde
/// writes an `OsString` into TOML as a platform-tagged byte array nobody could
/// edit by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// The program to run.
    pub program: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// The file's shape.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    /// Machines in the order they were added. `[[machine]]` in the file,
    /// because each entry is one machine.
    #[serde(default, rename = "machine")]
    machines: Vec<Machine>,
}

impl Machine {
    /// A machine reached by the default ssh command.
    #[must_use]
    pub fn new(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            target: target.into(),
            command: None,
        }
    }

    /// The program and arguments that reach this machine's daemon.
    #[must_use]
    pub fn dial(&self) -> (OsString, Vec<OsString>) {
        if let Some(command) = &self.command {
            return (
                OsString::from(&command.program),
                command.args.iter().map(OsString::from).collect(),
            );
        }

        let mut args: Vec<OsString> = SSH_OPTIONS.iter().map(OsString::from).collect();
        args.push(OsString::from(&self.target));
        args.extend(REMOTE.iter().map(OsString::from));
        (OsString::from("ssh"), args)
    }

    /// The command as one line, for a message that has to say what was run.
    #[must_use]
    pub fn describe(&self) -> String {
        let (program, args) = self.dial();
        let mut line = program.to_string_lossy().into_owned();
        for arg in args {
            line.push(' ');
            line.push_str(&arg.to_string_lossy());
        }
        line
    }
}

/// Whether `name` can name a machine.
///
/// It is a TOML table key in `projects.toml` and a word typed on the command
/// line, so nothing that would need quoting in either.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A name for the machine at `target`, when one can be made from it.
///
/// The host, cut at its first dot: `tower` from `me@tower.lan`. An IPv4
/// address keeps all four parts with dashes, because its first part alone
/// would name every machine on the subnet the same.
#[must_use]
pub fn default_name(target: &str) -> Option<String> {
    let host = target.strip_prefix("ssh://").unwrap_or(target);
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);

    let name = if host.parse::<std::net::Ipv4Addr>().is_ok() {
        host.replace('.', "-")
    } else {
        host.split('.').next().unwrap_or_default().to_string()
    };

    valid_name(&name).then_some(name)
}

/// The registered machines, oldest first.
///
/// No file means nothing is registered, which is what a first run looks like
/// rather than a failure.
pub fn load(dir: &Path) -> Result<Vec<Machine>, ConfigError> {
    let path = dir.join(FILE);

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ConfigError::Io { path, source }),
    };

    let saved: Saved = toml::from_str(&text).map_err(|source| ConfigError::Toml {
        path: path.clone(),
        source,
    })?;

    Ok(saved.machines)
}

/// Writes the list, replacing whatever was there.
pub fn save(dir: &Path, machines: &[Machine]) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let path = dir.join(FILE);
    let text = toml::to_string_pretty(&Saved {
        machines: machines.to_vec(),
    })
    .expect("a list of machines serialises");

    std::fs::write(&path, text).map_err(|source| ConfigError::Io { path, source })
}

/// Whether `name` could be registered now.
///
/// Its own step so a caller can ask before spending thirty seconds proving the
/// machine answers, only to be told the name was taken.
pub fn check(dir: &Path, name: &str, this_host: &str) -> Result<(), ConfigError> {
    let refuse = |reason: String| ConfigError::Machine {
        path: dir.join(FILE),
        reason,
    };

    if !valid_name(name) {
        return Err(refuse(format!(
            "{name:?} is not a machine name; use letters, digits, - and _"
        )));
    }

    // Compared with the host's first label too: `laptop` is this machine
    // whether the operating system says `laptop` or `Laptop.local`.
    let short = this_host.split('.').next().unwrap_or(this_host);
    if name.eq_ignore_ascii_case(this_host) || name.eq_ignore_ascii_case(short) {
        return Err(refuse(format!("{name} is this machine's own name")));
    }

    if load(dir)?.iter().any(|machine| machine.name == name) {
        return Err(refuse(format!("{name} is already registered")));
    }

    Ok(())
}

/// Registers `machine`, refusing a name [`check`] would refuse.
pub fn add(dir: &Path, machine: Machine, this_host: &str) -> Result<(), ConfigError> {
    check(dir, &machine.name, this_host)?;

    let mut machines = load(dir)?;
    machines.push(machine);
    save(dir, &machines)
}

/// Takes the machine called `name` off the list.
///
/// Answers whether it was there to take off.
pub fn remove(dir: &Path, name: &str) -> Result<bool, ConfigError> {
    let mut machines = load(dir)?;
    let before = machines.len();

    machines.retain(|machine| machine.name != name);
    if machines.len() == before {
        return Ok(false);
    }

    save(dir, &machines)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-config && cargo clippy -p dispatch-config --all-targets`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/dispatch-config
git commit -m "feat(config): keep a registry of machines

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Kept projects, per machine

**Files:**
- Modify: `crates/dispatch-config/src/projects.rs`
- Modify: `crates/dispatch-config/src/projects/tests.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces (existing `load`, `save`, `remember`, `forget` keep their signatures and mean this machine):
  - `pub fn load_on(dir: &Path, machine: &str) -> Result<Vec<PathBuf>, ConfigError>`
  - `pub fn remember_on(dir: &Path, machine: &str, root: &Path) -> Result<bool, ConfigError>`
  - `pub fn forget_on(dir: &Path, machine: &str, root: &Path) -> Result<bool, ConfigError>`
  - `pub fn forget_machine(dir: &Path, machine: &str) -> Result<bool, ConfigError>`

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-config/src/projects/tests.rs`:

```rust
#[test]
fn a_remote_machines_projects_are_kept_apart_from_this_ones() {
    let dir = TempDir::new("projects-remote");

    remember(dir.path(), Path::new("/Users/me/code/thing")).expect("written");
    assert!(remember_on(dir.path(), "tower", Path::new("~/code/server")).expect("written"));

    assert_eq!(
        load(dir.path()).expect("it reads back"),
        [PathBuf::from("/Users/me/code/thing")],
        "this machine's list is untouched"
    );
    assert_eq!(
        load_on(dir.path(), "tower").expect("it reads back"),
        [PathBuf::from("~/code/server")]
    );
    assert!(
        load_on(dir.path(), "gpu").expect("an unknown machine is not an error").is_empty()
    );
}

#[test]
fn saving_this_machines_list_keeps_the_others() {
    // `save` replaces this machine's roots; it must not take every remote
    // machine's kept projects with it.
    let dir = TempDir::new("projects-save-keeps");
    remember_on(dir.path(), "tower", Path::new("~/a")).expect("written");

    save(dir.path(), &[PathBuf::from("/tmp/here")]).expect("written");

    assert_eq!(load_on(dir.path(), "tower").expect("it reads back"), [PathBuf::from("~/a")]);
}

#[test]
fn a_remote_project_can_be_forgotten() {
    let dir = TempDir::new("projects-remote-forget");
    remember_on(dir.path(), "tower", Path::new("~/a")).expect("written");
    remember_on(dir.path(), "tower", Path::new("~/b")).expect("written");

    assert!(forget_on(dir.path(), "tower", Path::new("~/a")).expect("written"));
    assert!(!forget_on(dir.path(), "tower", Path::new("~/a")).expect("nothing to do"));
    assert_eq!(load_on(dir.path(), "tower").expect("it reads back"), [PathBuf::from("~/b")]);
}

#[test]
fn forgetting_a_machine_drops_its_whole_list() {
    let dir = TempDir::new("projects-forget-machine");
    remember(dir.path(), Path::new("/tmp/here")).expect("written");
    remember_on(dir.path(), "tower", Path::new("~/a")).expect("written");

    assert!(forget_machine(dir.path(), "tower").expect("written"));
    assert!(!forget_machine(dir.path(), "tower").expect("nothing to do"));
    assert!(load_on(dir.path(), "tower").expect("it reads back").is_empty());
    assert_eq!(load(dir.path()).expect("it reads back"), [PathBuf::from("/tmp/here")]);
}

#[test]
fn a_file_from_before_machines_still_loads() {
    let dir = TempDir::new("projects-old-shape");
    dir.write("projects.toml", "roots = [\"/tmp/old\"]\n");

    assert_eq!(load(dir.path()).expect("it reads"), [PathBuf::from("/tmp/old")]);
    assert!(load_on(dir.path(), "tower").expect("it reads").is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-config projects`
Expected: compile errors — `remember_on`, `load_on`, `forget_on`, `forget_machine` do not exist.

- [ ] **Step 3: Implement**

Rewrite `crates/dispatch-config/src/projects.rs` below the module doc and imports (add `use std::collections::BTreeMap;`):

```rust
/// The file the list lives in, inside the configuration directory.
const FILE: &str = "projects.toml";

/// The file's shape.
///
/// This machine's roots stay at the top level, where every file written
/// before machines existed put them. Each remote machine gets a table of its
/// own; an older build reading this file ignores those tables.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    /// This machine's project roots, in the order they were first opened.
    #[serde(default)]
    roots: Vec<PathBuf>,
    /// Each remote machine's, keyed by its registry name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    machines: BTreeMap<String, Kept>,
}

/// One remote machine's kept roots.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    #[serde(default)]
    roots: Vec<PathBuf>,
}

impl Saved {
    /// The list for `machine`, or this machine's for `None`.
    fn list_mut(&mut self, machine: Option<&str>) -> &mut Vec<PathBuf> {
        match machine {
            None => &mut self.roots,
            Some(name) => &mut self.machines.entry(name.to_string()).or_default().roots,
        }
    }
}

/// Reads the whole file. No file is an empty one.
fn read(dir: &Path) -> Result<Saved, ConfigError> {
    let path = dir.join(FILE);

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Saved::default()),
        Err(source) => return Err(ConfigError::Io { path, source }),
    };

    toml::from_str(&text).map_err(|source| ConfigError::Toml { path, source })
}

/// Writes the whole file.
fn write(dir: &Path, saved: &Saved) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let path = dir.join(FILE);
    let text = toml::to_string_pretty(saved).expect("lists of paths serialise");
    std::fs::write(&path, text).map_err(|source| ConfigError::Io { path, source })
}

/// This machine's remembered project roots, oldest first.
///
/// No file yet means nothing has been kept, which is what a first run looks
/// like rather than a failure.
pub fn load(dir: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    Ok(read(dir)?.roots)
}

/// A remote machine's remembered project roots, oldest first, exactly as they
/// were typed.
///
/// Unresolved on purpose: they are paths on another machine, and only its
/// daemon can resolve them.
pub fn load_on(dir: &Path, machine: &str) -> Result<Vec<PathBuf>, ConfigError> {
    Ok(read(dir)?
        .machines
        .remove(machine)
        .map(|kept| kept.roots)
        .unwrap_or_default())
}

/// Writes this machine's list, replacing whatever was there — and leaving
/// every remote machine's alone.
pub fn save(dir: &Path, roots: &[PathBuf]) -> Result<(), ConfigError> {
    let mut saved = read(dir)?;
    saved.roots = roots.to_vec();
    write(dir, &saved)
}

/// Adds `root` to this machine's list, if it is not already on it.
///
/// Answers whether the file was written: every start opens what is kept, and
/// rewriting the file each time would churn it for nothing.
pub fn remember(dir: &Path, root: &Path) -> Result<bool, ConfigError> {
    remember_in(dir, None, root)
}

/// Adds `root` to a remote machine's list, if it is not already on it.
pub fn remember_on(dir: &Path, machine: &str, root: &Path) -> Result<bool, ConfigError> {
    remember_in(dir, Some(machine), root)
}

/// Takes `root` off this machine's list.
///
/// Answers whether it was there to take off.
pub fn forget(dir: &Path, root: &Path) -> Result<bool, ConfigError> {
    forget_in(dir, None, root)
}

/// Takes `root` off a remote machine's list.
pub fn forget_on(dir: &Path, machine: &str, root: &Path) -> Result<bool, ConfigError> {
    forget_in(dir, Some(machine), root)
}

/// Drops a remote machine's whole list, for a machine that has been removed.
///
/// Answers whether it had one.
pub fn forget_machine(dir: &Path, machine: &str) -> Result<bool, ConfigError> {
    let mut saved = read(dir)?;
    if saved.machines.remove(machine).is_none() {
        return Ok(false);
    }
    write(dir, &saved)?;
    Ok(true)
}

fn remember_in(dir: &Path, machine: Option<&str>, root: &Path) -> Result<bool, ConfigError> {
    let mut saved = read(dir)?;
    let list = saved.list_mut(machine);

    if list.iter().any(|kept| kept == root) {
        return Ok(false);
    }

    list.push(root.to_path_buf());
    write(dir, &saved)?;
    Ok(true)
}

fn forget_in(dir: &Path, machine: Option<&str>, root: &Path) -> Result<bool, ConfigError> {
    let mut saved = read(dir)?;
    let list = saved.list_mut(machine);
    let before = list.len();

    list.retain(|kept| kept != root);
    if list.len() == before {
        return Ok(false);
    }

    write(dir, &saved)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS — the existing `projects` tests and the app tests that read `projects::load` are unchanged in behaviour.

- [ ] **Step 5: Commit**

```bash
git add crates/dispatch-config/src/projects.rs crates/dispatch-config/src/projects/tests.rs
git commit -m "feat(config): keep each remote machine's projects apart

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: A client that dials in the background

**Files:**
- Modify: `crates/dispatch-client/src/lib.rs`
- Modify: `crates/dispatch-client/src/tests.rs`

**Interfaces:**
- Consumes: the existing `Dial`, `connect_within`, `read_from`, `write_to`, `supervise`, `DialledChild`, `reap_dialled`.
- Produces:
  - `Client::dial(role: Role, name: &str, liveness: Liveness, dial: Dial) -> Client` — returns at once; `generation() == 0`, `is_connected() == false`, `device() == ""` until the first connection.
  - `Client::last_error(&self) -> Option<String>` — the most recent failed dial, `None` once connected.
  - `Client::dialled(&self) -> String` — the dial as one line (`Dial`'s `Display`).
  - `Client::pending_for_test() -> (Client, Sender<ServerMessage>, Receiver<ClientMessage>)` (`#[doc(hidden)]`) — like `for_test` but disconnected at generation 0, device `""`, dialled as `test-dial`.
  - `Handle::connect_for_test(&self, device: &str)` (`#[doc(hidden)]`) — as if a dial just connected: renames, bumps the generation, marks connected, clears the last error.
  - `Handle::fail_for_test(&self, error: &str)` (`#[doc(hidden)]`) — as if a dial just failed: marks disconnected, records the error.
  - private: `retry_for(&Dial) -> (Duration, Duration)`, `next_backoff(Duration, &Dial) -> Duration`, `COMMAND_FIRST_RETRY = 1s`, `COMMAND_MAX_RETRY = 30s`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-client/src/tests.rs`:

```rust
#[test]
fn a_dialled_client_starts_down_and_connects_once_a_daemon_answers() {
    // A machine asleep when Dispatch starts must still join when it wakes.
    // Before `Client::dial` there was no client to keep trying.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("dial-later");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Endpoint(endpoint),
    );

    assert_eq!(client.generation(), 0, "nothing has connected yet");
    assert!(!client.is_connected());
    assert_eq!(client.device(), "");
    assert!(
        wait_until(PATIENCE, || client.last_error().is_some()),
        "a failed first dial is remembered for the interface to show"
    );

    client.subscribe();
    let server = serve_one(welcome(), |_| {}, After::Answer);

    assert!(
        wait_until(PATIENCE, || client.generation() >= 1),
        "the supervisor keeps dialling until a daemon answers"
    );
    assert_eq!(client.device(), "desktop");
    assert!(client.last_error().is_none(), "connected, so nothing is wrong");
    assert!(
        wait_until(PATIENCE, || server.contains(&ClientMessage::Subscribe)),
        "a subscription asked for before the first connection goes out on it"
    );

    drop(client);
    drop(server);
}

#[test]
fn backoff_is_measured_against_what_is_being_dialled() {
    let socket = Dial::Endpoint(PathBuf::from("/tmp/dispatchd.sock"));
    let command = Dial::Command {
        program: "ssh".into(),
        args: vec!["tower".into()],
    };

    assert_eq!(retry_for(&socket), (FIRST_RETRY, MAX_RETRY), "a local daemon is unchanged");
    assert_eq!(
        retry_for(&command),
        (Duration::from_secs(1), Duration::from_secs(30)),
        "every attempt at a command is a new ssh"
    );

    let mut gap = retry_for(&command).0;
    for _ in 0..10 {
        gap = next_backoff(gap, &command);
    }
    assert_eq!(gap, Duration::from_secs(30), "doubling stops at the ceiling");
    assert_eq!(next_backoff(MAX_RETRY, &socket), MAX_RETRY);
}

#[test]
#[cfg(unix)]
fn a_command_that_keeps_failing_is_not_respawned_every_moment() {
    // A machine asleep for an hour must not cost an ssh every two seconds.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("backoff");
    let counter = dir.join("runs");

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Command {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from(format!("echo ran >> {}; exit 1", counter.display())),
            ],
        },
    );

    std::thread::sleep(Duration::from_millis(1500));
    let runs = std::fs::read_to_string(&counter)
        .unwrap_or_default()
        .lines()
        .count();

    assert!(
        (1..=2).contains(&runs),
        "one attempt at once and one a second later, not {runs}"
    );

    drop(client);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn a_command_failure_is_remembered_in_its_own_words() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Command {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from("echo 'Permission denied (publickey).' 1>&2; exit 255"),
            ],
        },
    );

    assert!(
        wait_until(PATIENCE, || client
            .last_error()
            .is_some_and(|error| error.contains("Permission denied"))),
        "the command's own words are what the user needs to see: {:?}",
        client.last_error()
    );
}

#[test]
#[cfg(unix)]
fn a_client_dropped_mid_dial_leaves_nothing_behind() {
    // The supervisor checks `closed` at the top of its loop. A dial that
    // completes after the client has gone recorded a pid nobody would ever
    // take. The grandchild outlives `PATIENCE` on its own, so only a reap
    // can end it in time.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("drop-mid-dial");
    let pid_file = dir.join("grandchild.pid");

    let mut encoded = Vec::new();
    Frame::write(&mut encoded, &welcome()).expect("writing succeeds");
    let escaped: String = encoded.iter().map(|b| format!("\\x{b:02x}")).collect();

    let client = Client::dial(
        Role::Interface,
        "test",
        Liveness::default(),
        Dial::Command {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from(format!(
                    "sleep 30 & echo $! >> {}; sleep 0.3; printf '{escaped}'; sleep 30",
                    pid_file.display()
                )),
            ],
        },
    );

    let grandchild = first_recorded_pid(&pid_file);
    drop(client);

    let deadline = Instant::now() + PATIENCE;
    while pid_is_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        !pid_is_alive(grandchild),
        "a dial that finished after its client was dropped left its command running"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
```

`Server` already has `contains(&self, &ClientMessage) -> bool` in this file; if the name differs, use whatever method `Server` exposes over `Heard::contains`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-client dial backoff mid_dial own_words`
Expected: compile errors — `Client::dial`, `last_error`, `retry_for`, `next_backoff` do not exist.

- [ ] **Step 3: Implement**

In `crates/dispatch-client/src/lib.rs`:

1. After `MAX_RETRY`, add:

```rust
/// How long to wait before redialling a command the first time.
///
/// A second rather than [`FIRST_RETRY`]: every attempt is a new `ssh`, with a
/// TCP handshake and an authentication behind it, and a host that refused one
/// a tenth of a second ago will refuse the next.
const COMMAND_FIRST_RETRY: Duration = Duration::from_secs(1);

/// The longest gap between attempts to dial a command.
///
/// [`MAX_RETRY`] would respawn `ssh` against an asleep host thirty times a
/// minute for as long as it sleeps. With `ConnectTimeout=10` in front of it,
/// thirty seconds still has a machine that wakes joining within about forty.
const COMMAND_MAX_RETRY: Duration = Duration::from_secs(30);

/// The first gap between attempts and the longest, for this dial.
fn retry_for(dial: &Dial) -> (Duration, Duration) {
    match dial {
        Dial::Endpoint(_) => (FIRST_RETRY, MAX_RETRY),
        Dial::Command { .. } => (COMMAND_FIRST_RETRY, COMMAND_MAX_RETRY),
    }
}

/// The gap after `current`: doubled, up to this dial's ceiling.
fn next_backoff(current: Duration, dial: &Dial) -> Duration {
    (current * 2).min(retry_for(dial).1)
}
```

2. Add a field to `Wire`, after `child`:

```rust
    /// Why the last dial failed, until one succeeds.
    ///
    /// Kept for the interface rather than only logged: a machine that stays
    /// down has to be able to say `Permission denied (publickey)` somewhere
    /// the user is looking.
    last_error: Mutex<Option<String>>,
```

3. Add a constructor to `impl Wire`:

```rust
    /// A wire with nothing on it yet: down, at generation 0, and nameless.
    fn new(role: Role, name: &str, liveness: Liveness, dial: Dial) -> Self {
        Self {
            writer: Mutex::new(None),
            connected: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            subscribed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            device: Mutex::new(String::new()),
            name: name.to_string(),
            role,
            dial,
            last_heard: Mutex::new(Instant::now()),
            last_asked: Mutex::new(Instant::now()),
            liveness,
            child: DialledChild::default(),
            last_error: Mutex::new(None),
        }
    }
```

4. Replace `attach_dialling`'s body from `let child = DialledChild::default();` to the end with:

```rust
        let wire = Wire::new(role, name, liveness, dial);
        wire.child.record(connected.child);
        *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = connected.device;
        *wire.writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(connected.writer);
        wire.generation.store(1, Ordering::Relaxed);
        wire.connected.store(true, Ordering::Relaxed);

        Ok(Self::start(Arc::new(wire), Some(connected.reader)))
    }

    /// Dials in the background, and keeps dialling until it connects.
    ///
    /// Returns at once, down, at generation 0 and with an empty
    /// [`Client::device`]. The first connection is generation 1, so a caller
    /// that already rebuilds on a generation change handles a first connect
    /// with no code of its own. Messages sent before then are dropped, as
    /// they are during any outage; [`Client::subscribe`] is remembered and
    /// sent on connecting.
    ///
    /// For a machine that may be asleep: [`Client::attach_over`] would have
    /// nothing to hand back, and so nothing that could try again.
    #[must_use]
    pub fn dial(role: Role, name: &str, liveness: Liveness, dial: Dial) -> Self {
        Self::start(Arc::new(Wire::new(role, name, liveness, dial)), None)
    }

    /// Starts the threads that serve a wire, reading from `reader` when a
    /// connection is already up.
    fn start(wire: Arc<Wire>, reader: Option<Box<dyn Read + Send>>) -> Self {
        let (outbox, outgoing) = channel::<ClientMessage>();
        let (incoming, inbox) = channel::<ServerMessage>();

        if let Some(reader) = reader {
            read_from(reader, &incoming, &wire);
        }
        write_to(outgoing, &wire);
        supervise(incoming, &wire);

        Self {
            handle: Handle {
                outbox,
                wire: Arc::clone(&wire),
            },
            inbox,
            wire,
        }
    }
```

5. Add to `impl Client`, after `generation`:

```rust
    /// Why the last attempt to connect failed, while none has succeeded
    /// since.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.wire
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// How this client reaches its daemon, as one line.
    ///
    /// What a machine with no other name is called until its daemon answers.
    #[must_use]
    pub fn dialled(&self) -> String {
        self.wire.dial.to_string()
    }
```

6. Replace `supervise` with:

```rust
/// Reconnects whenever the connection is down — and, for a client made by
/// [`Client::dial`], connects in the first place.
fn supervise(incoming: Sender<ServerMessage>, wire: &Arc<Wire>) {
    let wire = Arc::clone(wire);

    std::thread::spawn(move || {
        let first_retry = retry_for(&wire.dial).0;
        let mut backoff = first_retry;
        // A client that has never connected dials at once: there is no
        // failure yet to back off from.
        let mut dial_now = wire.generation.load(Ordering::Relaxed) == 0;

        loop {
            if wire.closed.load(Ordering::Relaxed) {
                return;
            }

            if wire.connected.load(Ordering::Relaxed) {
                check_liveness(&wire);
                std::thread::sleep(FIRST_RETRY);
                backoff = first_retry;
                continue;
            }

            if !std::mem::take(&mut dial_now) {
                std::thread::sleep(backoff);
                backoff = next_backoff(backoff, &wire.dial);
            }

            let patience = patience_for(&wire.dial);
            match connect_within(&wire.name, wire.role, &wire.dial, patience) {
                Ok(connected) => {
                    // Recorded, then checked: `Client::drop` sets `closed`
                    // and then takes the pid, so whichever order the two
                    // threads meet in, exactly one of them finds it. Checked
                    // only at the top of the loop, a dial that finished after
                    // the client was dropped left a process nobody would
                    // ever take.
                    wire.child.record(connected.child);
                    if wire.closed.load(Ordering::Relaxed) {
                        reap_dialled(wire.child.take());
                        return;
                    }

                    *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = connected.device;
                    *wire.writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(connected.writer);
                    *wire.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    wire.generation.fetch_add(1, Ordering::Relaxed);
                    wire.heard();
                    wire.connected.store(true, Ordering::Relaxed);
                    read_from(connected.reader, &incoming, &wire);

                    // Sent directly rather than through the queue: the queue's
                    // writer may be mid-message, and a subscribe that arrives
                    // after the first keystroke would lose the panes.
                    if wire.subscribed.load(Ordering::Relaxed) {
                        wire.write(&ClientMessage::Subscribe);
                    }

                    tracing::info!(
                        generation = wire.generation.load(Ordering::Relaxed),
                        dial = %wire.dial,
                        "connected to the daemon"
                    );
                    backoff = first_retry;
                }
                Err(error) => {
                    tracing::debug!(%error, dial = %wire.dial, "the daemon is not answering yet");
                    *wire.last_error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(error.to_string());
                }
            }
        }
    });
}
```

7. Replace `for_test` and add the pending variant and the handle helpers:

```rust
    pub fn for_test() -> (Self, Sender<ServerMessage>, Receiver<ClientMessage>) {
        let wire = Wire::new(Role::Interface, "test", Liveness::default(), Dial::Endpoint(PathBuf::new()));
        wire.connected.store(true, Ordering::Relaxed);
        wire.generation.store(1, Ordering::Relaxed);
        *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = "test-device".to_string();
        Self::without_threads(wire)
    }

    /// Creates a client with no socket behind it that has never connected.
    ///
    /// The companion to [`Client::for_test`] for what [`Client::dial`] hands
    /// back: down, at generation 0, nameless, and dialled as `test-dial`.
    /// Drive it with [`Handle::connect_for_test`] and
    /// [`Handle::fail_for_test`].
    #[doc(hidden)]
    #[must_use]
    pub fn pending_for_test() -> (Self, Sender<ServerMessage>, Receiver<ClientMessage>) {
        let dial = Dial::Command {
            program: "test-dial".into(),
            args: Vec::new(),
        };
        Self::without_threads(Wire::new(Role::Interface, "test", Liveness::default(), dial))
    }

    /// Wraps a wire in a client whose traffic the test holds both ends of.
    fn without_threads(wire: Wire) -> (Self, Sender<ServerMessage>, Receiver<ClientMessage>) {
        // No supervisor runs, so nothing may try to: a dropped test client
        // must not start dialling an empty endpoint.
        wire.closed.store(true, Ordering::Relaxed);
        let wire = Arc::new(wire);

        let (outbox, outgoing) = channel::<ClientMessage>();
        let (incoming, inbox) = channel::<ServerMessage>();

        (
            Self {
                handle: Handle {
                    outbox,
                    wire: Arc::clone(&wire),
                },
                inbox,
                wire,
            },
            incoming,
            outgoing,
        )
    }
```

Keep `for_test`'s existing doc comment and attributes. Add to `impl Handle`, after `rename_for_test`:

```rust
    /// Connects as a dial would, with no dial behind it.
    ///
    /// The companion to [`Client::pending_for_test`]: renames the device,
    /// bumps the generation and clears any failure, in the order the
    /// supervisor does.
    #[doc(hidden)]
    pub fn connect_for_test(&self, device: &str) {
        self.rename_for_test(device);
        *self.wire.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.wire.generation.fetch_add(1, Ordering::Relaxed);
        self.wire.connected.store(true, Ordering::Relaxed);
    }

    /// Fails as a dial would, with no dial behind it.
    #[doc(hidden)]
    pub fn fail_for_test(&self, error: &str) {
        self.wire.connected.store(false, Ordering::Relaxed);
        *self.wire.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-client && cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS, no warnings. The existing reconnect tests still pass: an attached client starts at generation 1, so `dial_now` is false and its loop is unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/dispatch-client
git commit -m "feat(client): dial in the background until a daemon answers

A machine asleep when Dispatch starts could never join: a client existed
only once its first dial had succeeded. Client::dial starts down at
generation 0 and lets the supervisor make the first dial, backing a
command off to thirty seconds rather than two. The supervisor also
reaps a dial that completes after its client was dropped.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Rows for machines that have not answered

**Files:**
- Modify: `crates/dispatch-core/src/device.rs`
- Modify: `dispatch/src/app.rs` (`Attachment`, `attach`, new `attach_named`, `poll_daemon`, `sync_attachment`, `keep`/`unkeep`, `add_project`, `drop_selected_project`, `apply_from`)
- Modify: `dispatch/src/main.rs` (the `--daemon` and `--daemon-command` loops), so `attach_named` has a caller outside tests and clippy's `dead_code` stays quiet

**Interfaces:**
- Consumes: `Client::dial`, `Client::last_error`, `Client::dialled`, `Client::pending_for_test`, `Handle::connect_for_test`, `Handle::fail_for_test` (Task 4); `projects::remember_on`, `forget_on` (Task 3); `ServerMessage::ProjectRefused` (Task 1).
- Produces:
  - `Device::pending(name: impl Into<String>) -> Device` (`reachable: false`)
  - `App::attach_named(&mut self, client: Client, label: Option<String>, roots: Vec<PathBuf>)`
  - private: `Attachment { label: Option<String>, remote: bool, reported: Option<String>, .. }`, `enum KeptList { ThisMachine, Machine(String), Unkept }`, `App::kept_list(&self, DeviceId) -> KeptList`, `App::keep(&mut self, &KeptList, &Path)`, `App::unkeep(&mut self, &KeptList, &Path)`.

- [ ] **Step 1: Write the failing tests**

In `crates/dispatch-core/src/device.rs`'s `mod tests`:

```rust
    #[test]
    fn a_pending_device_is_not_reachable_yet() {
        // A registered machine has a row before its first connection, and
        // that row must not claim a connection it does not have.
        let device = Device::pending("tower");

        assert_eq!(device.name, "tower");
        assert!(!device.reachable);
    }
```

In `dispatch/src/app.rs`'s `mod tests`, add:

```rust
    /// The one device a test attached, by its label.
    fn device_named<'a>(app: &'a App, name: &str) -> &'a Device {
        app.state
            .devices()
            .iter()
            .find(|device| device.name == name)
            .unwrap_or_else(|| panic!("no device named {name}: {:?}", app.state.devices()))
    }

    #[test]
    fn a_machine_not_yet_answering_is_drawn_under_its_registry_name() {
        let (client, _daemon, _sent) = Client::pending_for_test();
        let mut app = App::new(HarnessRegistry::default());

        app.attach_named(client, Some("tower".into()), Vec::new());
        app.poll_daemon();

        assert!(!device_named(&app, "tower").reachable);
    }

    #[test]
    fn a_first_connection_says_connected_and_keeps_the_registry_name() {
        let (client, _daemon, _sent) = Client::pending_for_test();
        let handle = client.handle();
        let mut app = App::new(HarnessRegistry::default());
        app.attach_named(client, Some("tower".into()), Vec::new());
        app.poll_daemon();

        handle.connect_for_test("ubuntu-22");
        app.poll_daemon();

        assert_eq!(app.status, "connected to tower");
        assert!(
            device_named(&app, "tower").reachable,
            "the row keeps the name the user gave it, not the daemon's hostname"
        );
    }

    #[test]
    fn kept_roots_go_out_when_the_machine_first_answers() {
        let (client, _daemon, sent) = Client::pending_for_test();
        let handle = client.handle();
        let mut app = App::new(HarnessRegistry::default());
        app.attach_named(client, Some("tower".into()), vec![PathBuf::from("~/srv/app")]);
        app.poll_daemon();

        assert!(
            sent.try_iter().next().is_none(),
            "nothing is asked of a machine that is not there"
        );

        handle.connect_for_test("tower");
        app.poll_daemon();

        let asked: Vec<ClientMessage> = sent.try_iter().collect();
        assert!(
            asked.contains(&ClientMessage::OpenProject {
                root: PathBuf::from("~/srv/app")
            }),
            "the kept root is asked for once the machine answers: {asked:?}"
        );
    }

    #[test]
    fn an_outage_is_reported_once_not_on_every_retry() {
        let (client, _daemon, _sent) = Client::pending_for_test();
        let handle = client.handle();
        let mut app = App::new(HarnessRegistry::default());
        app.attach_named(client, Some("tower".into()), Vec::new());

        handle.fail_for_test("Permission denied (publickey)");
        app.poll_daemon();
        assert_eq!(app.status, "tower unreachable: Permission denied (publickey)");

        app.set_status("something else");
        handle.fail_for_test("Permission denied (publickey)");
        app.poll_daemon();
        assert_eq!(app.status, "something else", "the same failure again says nothing new");

        handle.fail_for_test("Connection refused");
        app.poll_daemon();
        assert_eq!(app.status, "tower unreachable: Connection refused");
    }

    #[test]
    fn an_unlabelled_machine_takes_its_daemons_name_once_it_answers() {
        // `--daemon-command` has no registry name: the row shows the command
        // until the daemon says what it is called.
        let (client, _daemon, _sent) = Client::pending_for_test();
        let handle = client.handle();
        let mut app = App::new(HarnessRegistry::default());
        app.attach_named(client, None, Vec::new());
        app.poll_daemon();
        assert!(!device_named(&app, "test-dial").reachable);

        handle.connect_for_test("far");
        app.poll_daemon();

        assert!(device_named(&app, "far").reachable);
    }

    #[test]
    fn a_refused_root_is_forgotten_everywhere() {
        let dir = scratch("refused");
        dispatch_config::projects::remember_on(&dir, "tower", Path::new("~/typo"))
            .expect("written");

        let (client, daemon, sent) = Client::pending_for_test();
        let handle = client.handle();
        let mut app = App::new(HarnessRegistry::default());
        app.keep_projects_in(&dir);
        app.attach_named(client, Some("tower".into()), vec![PathBuf::from("~/typo")]);

        daemon
            .send(ServerMessage::ProjectRefused {
                root: PathBuf::from("~/typo"),
                reason: "No such file or directory".into(),
            })
            .expect("the app is listening");
        app.poll_daemon();

        assert!(
            dispatch_config::projects::load_on(&dir, "tower")
                .expect("it reads back")
                .is_empty(),
            "a root that can never open is not kept"
        );
        assert!(app.status.contains("cannot open ~/typo on tower"), "{}", app.status);

        handle.connect_for_test("tower");
        app.poll_daemon();
        assert!(
            !sent.try_iter().any(|m| matches!(m, ClientMessage::OpenProject { .. })),
            "and it is not asked for again on the next connection"
        );
    }
```

If `scratch` in the test module returns a `PathBuf` that is not removed on drop, follow the existing tests' cleanup (`let _ = std::fs::remove_dir_all(&dir);` at the end).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-core pending; cargo test -p dispatch app::tests`
Expected: compile errors — `Device::pending` and `App::attach_named` do not exist.

- [ ] **Step 3: Implement**

In `crates/dispatch-core/src/device.rs`, after `Device::new`:

```rust
    /// A device named `name` that has not connected yet.
    ///
    /// A registered machine is drawn before it first answers, so a machine
    /// that is asleep still has its row — and that row must not claim a
    /// connection it does not have.
    #[must_use]
    pub fn pending(name: impl Into<String>) -> Self {
        Self {
            reachable: false,
            ..Self::new(name)
        }
    }
```

In `dispatch/src/app.rs`:

1. Extend `Attachment`:

```rust
struct Attachment {
    /// The machine it is, as the sidebar names it.
    device: DeviceId,
    client: Client,
    /// That connection's generation. Per attachment, because one daemon
    /// restarting says nothing about the others. 0 until it first connects.
    generation: u64,
    /// Roots asked of this daemon, so its own reconnect can ask again.
    opened: Vec<PathBuf>,
    /// The registry name, which the row keeps whatever the daemon calls
    /// itself: it exists before the first connection, and it is unique where
    /// hostnames are not.
    label: Option<String>,
    /// Whether this is another machine rather than this one's own daemon.
    ///
    /// Only this machine's filesystem can be browsed; a path on any other
    /// has to be typed.
    remote: bool,
    /// The failure last put on the status line, so a machine retrying every
    /// thirty seconds says so once rather than every time.
    reported: Option<String>,
}
```

2. Add beside `Attachment`:

```rust
/// Which kept list a root opened on an attachment belongs on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeptList {
    /// This machine's: the top-level list.
    ThisMachine,
    /// A registered machine's, by name.
    Machine(String),
    /// Nowhere. A machine reached by `--daemon` or `--daemon-command` has no
    /// name to keep it under, and nothing would dial it again next start.
    Unkept,
}
```

3. Replace `App::attach` and add `attach_named` and a shared helper:

```rust
    pub fn attach(&mut self, client: Client) {
        let device = Device::new(client.device());
        self.hold(client, device, None, false, Vec::new());
    }

    /// Adds another machine's daemon to the fleet, whether or not it has
    /// answered yet.
    ///
    /// The row is drawn at once — under `label`, or the dial itself when
    /// there is no label — so a machine that is asleep is visibly there
    /// rather than missing. `roots` go out the first time it connects.
    pub fn attach_named(&mut self, client: Client, label: Option<String>, roots: Vec<PathBuf>) {
        let name = label.clone().unwrap_or_else(|| client.dialled());
        // A client the add overlay has already proven arrives connected.
        let device = if client.is_connected() {
            Device::new(name)
        } else {
            Device::pending(name)
        };
        self.hold(client, device, label, true, roots);
    }

    /// Registers `device` and keeps `client` as its attachment.
    fn hold(
        &mut self,
        client: Client,
        device: Device,
        label: Option<String>,
        remote: bool,
        opened: Vec<PathBuf>,
    ) {
        // ... the existing comment and `if let Some(local) = self.local.take()`
        // block from `attach`, unchanged ...

        let device = self.state.add_device(device);
        let generation = client.generation();

        let attachment = Attachment {
            device,
            client,
            generation,
            opened,
            label,
            remote,
            reported: None,
        };

        match &mut self.mode {
            Mode::Attached(attachments) => attachments.push(attachment),
            Mode::Standalone => self.mode = Mode::Attached(vec![attachment]),
        }
    }
```

Keep `attach`'s existing doc comment on `attach`, and move its body comment about the standalone machine into `hold`.

4. In `poll_daemon`, carry the last error through the snapshot:

```rust
        let snapshot: Vec<(DeviceId, u64, bool, String, Option<String>, Vec<ServerMessage>)> =
            attachments
                .iter()
                .map(|attachment| {
                    (
                        attachment.device,
                        attachment.client.generation(),
                        attachment.client.is_connected(),
                        attachment.client.device(),
                        attachment.client.last_error(),
                        attachment.client.poll(),
                    )
                })
                .collect();

        let mut changed = false;

        for (device, generation, connected, name, error, messages) in snapshot {
            changed |= self.sync_attachment(device, generation, connected, &name, error.as_deref());

            for message in messages {
                changed |= self.apply_from(device, message);
            }
        }
```

5. In `sync_attachment`, add the `error: Option<&str>` parameter and change three things:

```rust
    fn sync_attachment(
        &mut self,
        device: DeviceId,
        generation: u64,
        connected: bool,
        name: &str,
        error: Option<&str>,
    ) -> bool {
        let was = self.state.device(device).is_some_and(|d| d.reachable);
        self.state.set_device_reachable(device, connected);
        let mut changed = was != connected;

        // A registered machine keeps the name the user gave it. An unnamed
        // one takes its daemon's name once there is one: until the first
        // handshake `name` is empty, and an empty row names nothing.
        let labelled = self
            .attachments()
            .iter()
            .any(|a| a.device == device && a.label.is_some());
        if !labelled
            && !name.is_empty()
            && self.state.device(device).is_some_and(|d| d.name != name)
        {
            self.state.set_device_name(device, name);
            changed = true;
        }

        changed |= self.report_outage(device, connected, error);

        // ... the existing lookup of `attachment` and the
        // `attachment.generation == generation` early return, unchanged ...

        let first = attachment.generation == 0;
        attachment.generation = generation;

        // ... the existing re-send of `opened`, `reopening_selection`
        // handling and `forget_device(device)`, unchanged ...

        let shown = self
            .state
            .device(device)
            .map_or_else(|| name.to_string(), |d| d.name.clone());
        self.status = if first {
            format!("connected to {shown}")
        } else {
            format!("reattached to {shown}")
        };

        true
    }

    /// Says that a machine cannot be reached, and why — once per failure,
    /// not once per retry.
    ///
    /// A machine asleep for an hour is retried a hundred times; the status
    /// line should change when something does, not every thirty seconds.
    fn report_outage(&mut self, device: DeviceId, connected: bool, error: Option<&str>) -> bool {
        let Mode::Attached(attachments) = &mut self.mode else {
            return false;
        };
        let Some(attachment) = attachments.iter_mut().find(|a| a.device == device) else {
            return false;
        };

        if connected {
            attachment.reported = None;
            return false;
        }

        let Some(error) = error else {
            return false;
        };
        if attachment.reported.as_deref() == Some(error) {
            return false;
        }
        attachment.reported = Some(error.to_string());

        let name = self
            .state
            .device(device)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        self.status = format!("{name} unreachable: {error}");
        true
    }
```

Delete the old `self.status = format!("reattached to {name}");` line — it is replaced by the `first` branch above.

6. Replace `keep` and `unkeep`, and add `kept_list`:

```rust
    /// Which kept list a root opened on `device` belongs on.
    fn kept_list(&self, device: DeviceId) -> KeptList {
        match self.attachments().iter().find(|a| a.device == device) {
            // Standalone: the only machine there is, is this one.
            None => KeptList::ThisMachine,
            Some(attachment) if !attachment.remote => KeptList::ThisMachine,
            Some(Attachment {
                label: Some(label),
                ..
            }) => KeptList::Machine(label.clone()),
            Some(_) => KeptList::Unkept,
        }
    }

    /// Adds `root` to `list`, if this client keeps one.
    ///
    /// A list that cannot be written is reported in the status line rather
    /// than fatal: the project is open either way, and losing it on exit is
    /// not worth refusing to run over.
    fn keep(&mut self, list: &KeptList, root: &Path) {
        let Some(dir) = self.kept.clone() else {
            return;
        };

        let written = match list {
            KeptList::ThisMachine => dispatch_config::projects::remember(&dir, root),
            KeptList::Machine(name) => dispatch_config::projects::remember_on(&dir, name, root),
            KeptList::Unkept => return,
        };

        if let Err(error) = written {
            self.status = format!("could not keep {}: {error}", root.display());
        }
    }

    /// Takes `root` off `list`, if this client keeps one.
    fn unkeep(&mut self, list: &KeptList, root: &Path) {
        let Some(dir) = self.kept.clone() else {
            return;
        };

        let written = match list {
            KeptList::ThisMachine => dispatch_config::projects::forget(&dir, root),
            KeptList::Machine(name) => dispatch_config::projects::forget_on(&dir, name, root),
            KeptList::Unkept => return,
        };

        if let Err(error) = written {
            self.status = format!("could not drop {}: {error}", root.display());
        }
    }
```

7. In `add_project`, replace `self.keep(&root);` with:

```rust
        // Kept on the list of whichever machine it is about to be asked of.
        let list = self
            .attachments()
            .first()
            .map_or(KeptList::ThisMachine, |a| self.kept_list(a.device));
        self.keep(&list, &root);
```

8. In `drop_selected_project`, replace `self.unkeep(&root);` and the loop over every attachment's `opened` with the project's own machine only:

```rust
        let device = self
            .state
            .projects()
            .iter()
            .find(|p| p.id == project)
            .map(|p| p.device);
        let list = device.map_or(KeptList::ThisMachine, |d| self.kept_list(d));
        self.unkeep(&list, &root);

        if let (Mode::Attached(attachments), Some(device)) = (&mut self.mode, device)
            && let Some(attachment) = attachments.iter_mut().find(|a| a.device == device)
        {
            attachment.opened.retain(|kept| kept != &root);
        }
```

9. In `dispatch/src/main.rs`, replace both the `for endpoint in &args.daemons` loop and the `for command in &args.daemon_commands` loop with background dials. This fixes parked item 1 for these flags: a machine that is down no longer blocks startup for thirty seconds.

```rust
    use dispatch_client::{Client, Dial, Liveness};
    use dispatch_proto::Role;

    // Dialled in the background: the interface is drawn at once, and each
    // machine's row lights up when it answers. Attaching in turn cost thirty
    // seconds per machine that was down, before anything was on screen.
    for endpoint in &args.daemons {
        let client = Client::dial(
            Role::Interface,
            CLIENT_NAME,
            Liveness::default(),
            Dial::Endpoint(endpoint.clone()),
        );
        client.subscribe();
        app.attach_named(client, None, Vec::new());
    }

    for command in &args.daemon_commands {
        let mut words = command.split_whitespace().map(std::ffi::OsString::from);
        let Some(program) = words.next() else {
            app.set_status("--daemon-command was empty".to_string());
            continue;
        };

        let client = Client::dial(
            Role::Interface,
            CLIENT_NAME,
            Liveness::default(),
            Dial::Command {
                program,
                args: words.collect(),
            },
        );
        client.subscribe();
        app.attach_named(client, None, Vec::new());
    }
```

Update the `--daemon` and `--daemon-command` doc comments on `Args`: drop "`machine add` will fill these in from the machine list once it can reach another host", keep "A program whose path contains a space needs the machine registry", and add "Dialled in the background and retried until it answers."

10. In `apply_from`, give `ProjectRefused` its own arm (and remove it from the ignored arm Task 1 added it to):

```rust
            ServerMessage::ProjectRefused { root, reason } => {
                // Forgotten everywhere it was remembered: kept, it would be
                // asked for again on every start and every reconnection, and
                // it never becomes a row the user could delete it from.
                let list = self.kept_list(device);
                self.unkeep(&list, &root);

                if let Mode::Attached(attachments) = &mut self.mode
                    && let Some(attachment) = attachments.iter_mut().find(|a| a.device == device)
                {
                    attachment.opened.retain(|kept| kept != &root);
                }

                let name = self
                    .state
                    .device(device)
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                self.status = format!("cannot open {} on {name}: {reason}", root.display());
                true
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Run as well: `cargo build --workspace && cargo test -p dispatch --test end_to_end two_daemons a_machine_reached_over_a_bridge`
Expected: PASS, no warnings. Both federation end-to-end tests still pass: an unlabelled row shows the dial string until the handshake and the daemon's own name after it, and the bridge test's reconnect still says `reattached to far` (its first connection says `connected to far`, which that test does not wait for).

- [ ] **Step 5: Commit**

```bash
git add crates/dispatch-core/src/device.rs dispatch/src/app.rs dispatch/src/main.rs
git commit -m "feat(dispatch): draw a machine before it answers, and keep its projects apart

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Every machine is dialled at startup, in the background

**Files:**
- Modify: `dispatch/src/main.rs:130-260` (the part of `main` between loading harnesses and `keep_projects_in`)
- Modify: `dispatch/tests/end_to_end.rs` (a `wait_for_within` helper and one new test)

**Interfaces:**
- Consumes: `dispatch_config::machines::load`, `Machine::dial` (Task 2); `projects::load_on` (Task 3); `Client::dial`, `Dial` (Task 4); `App::attach_named` and the background `--daemon`/`--daemon-command` loops (Task 5).
- Produces: startup behaviour only. With `--attach` or any registered machine, Dispatch attaches to the local daemon and dials every registered machine with `Client::dial`.

- [ ] **Step 1: Write the failing test**

In `dispatch/tests/end_to_end.rs`, split `Harness::wait_for` so a test can be more patient:

```rust
    /// Waits until the screen satisfies `predicate`, returning whether it did.
    fn wait_for(&mut self, predicate: impl Fn(&[String]) -> bool) -> bool {
        self.wait_for_within(SETTLE, predicate)
    }

    /// As `wait_for`, for something that takes longer than a redraw: a
    /// machine joining is gated on a backoff measured in seconds.
    fn wait_for_within(
        &mut self,
        patience: Duration,
        predicate: impl Fn(&[String]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + patience;
        // ... the existing loop body of `wait_for`, unchanged ...
    }
```

Then add the test at the end of the file:

```rust
#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_machine_asleep_at_startup_joins_when_it_wakes() {
    // The slice's claim: a registered machine that does not answer at
    // startup is drawn anyway, retried in the background, and joins — with
    // its kept project — once it does. No `--attach`: a registered machine
    // implies it.
    let here = Fixture::new("wake-a");
    let there = Fixture::new("wake-b");

    // The flag stands in for the machine being asleep. `--stdio` would
    // otherwise start the far daemon itself on the very first dial.
    let flag = there.config.path().join("awake");
    let script = format!(
        "test -e {} && exec {} --stdio --endpoint {}",
        flag.display(),
        dispatchd_binary().display(),
        there.config.path().join("dispatchd.sock").display()
    );
    std::fs::write(
        here.config.path().join("machines.toml"),
        format!(
            "[[machine]]\nname = \"tower\"\ntarget = \"tower\"\n\
             command = {{ program = \"sh\", args = [\"-c\", {script:?}] }}\n"
        ),
    )
    .expect("temp dir is writable");
    std::fs::write(
        here.config.path().join("projects.toml"),
        format!("[machines.tower]\nroots = [{:?}]\n", there.project.display().to_string()),
    )
    .expect("temp dir is writable");

    let mut app = Harness::spawn(&here, Size::new(200, 30), &[]);

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "tower") && sidebar_contains(lines, "unreachable")),
        "the machine is drawn before it answers"
    );

    std::fs::write(&flag, b"").expect("temp dir is writable");

    assert!(
        app.wait_for_within(Duration::from_secs(45), |lines| contains(lines, "connected to tower")),
        "it joins once it wakes"
    );

    app.select_project("tower");
    app.spawn_shell();
    app.send(b"echo woke-up\r");
    assert!(
        app.wait_for(|lines| contains(lines, "woke-up")),
        "and its kept project runs real work"
    );

    drop(app);
    stop_recorded_daemon(&here);
    stop_recorded_daemon(&there);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo build --workspace && cargo test -p dispatch --test end_to_end a_machine_asleep_at_startup`
Expected: FAIL — `machines.toml` is ignored, so no `tower` row ever appears ("the machine is drawn before it answers").

- [ ] **Step 3: Implement**

In `dispatch/src/main.rs`:

1. Move `let config_dir = dispatch_os::paths::config_dir()...;` up so it comes before `let mut roots`, and after it load the registry:

```rust
    // Read before deciding how to run: any registered machine means the
    // agents belong to daemons, this machine's included.
    let machines = dispatch_config::machines::load(&config_dir)
        .context("failed to read the registered machines")?;
```

2. Replace `let mut app = if args.attach {` with `let mut app = if args.attach || !machines.is_empty() {`, and update the comment above `attach`'s call:

```rust
        // A registered machine implies attaching: standalone agents are this
        // process's children and cannot share a sidebar with a daemon's.
        // Fails before the terminal is taken over, so the reason is readable.
```

3. Before the `for endpoint in &args.daemons` loop Task 5 wrote, add the registry's own loop (reusing that block's `use` lines — move them above this loop):

```rust
    for machine in &machines {
        // Not `args`: that is the command line's own, still read below.
        let (program, arguments) = machine.dial();
        let client = Client::dial(
            Role::Interface,
            CLIENT_NAME,
            Liveness::default(),
            Dial::Command {
                program,
                args: arguments,
            },
        );
        client.subscribe();

        let roots = dispatch_config::projects::load_on(&config_dir, &machine.name)
            .context("failed to read the kept projects")?;
        app.attach_named(client, Some(machine.name.clone()), roots);
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS, including `two_daemons_share_one_sidebar` and `a_machine_reached_over_a_bridge_outlives_its_transport` (an unlabelled row shows the daemon's own name after the handshake, and the bridge test's reconnect still says `reattached to far`).

- [ ] **Step 5: Commit**

```bash
git add dispatch/src/main.rs dispatch/tests/end_to_end.rs
git commit -m "feat(dispatch): dial every registered machine in the background at startup

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: `dispatch machine add`, `list` and `remove`

**Files:**
- Create: `dispatch/src/machine.rs`
- Modify: `dispatch/src/main.rs` (`mod machine;`, `Command::Machine`, the subcommand dispatch at the top of `main`)
- Create: `dispatch/tests/machine_verbs.rs`

**Interfaces:**
- Consumes: `machines::{Machine, Command, default_name, check, add, remove, load}`, `Machine::dial`, `Machine::describe` (Task 2); `projects::forget_machine` (Task 3); `Client::attach_over` (existing).
- Produces: `machine::Action` (clap subcommand) and `machine::run(action: Action) -> anyhow::Result<ExitCode>`.

- [ ] **Step 1: Write the failing tests**

Create `dispatch/tests/machine_verbs.rs`:

```rust
//! `dispatch machine`, run as a user runs it.
//!
//! No ssh: a command override after `--` is how a test reaches a machine,
//! the same way `dispatch/tests/end_to_end.rs` reaches one.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Output;

/// A directory of this test's own, removed afterwards.
///
/// Short labels: a daemon's socket lives in here, and a Unix socket address
/// is limited to about a hundred bytes.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("dispatch-mv-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir is writable");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // The bridge started a daemon here; it outlives the bridge on purpose.
        if let Ok(pid) = std::fs::read_to_string(self.0.join("dispatchd.pid")) {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", pid.trim()])
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dispatchd_binary() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_dispatch"));
    path.set_file_name("dispatchd");
    path
}

/// Runs `dispatch machine …` against `config`.
fn machine(config: &Path, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_dispatch"))
        .arg("machine")
        .args(args)
        .env(dispatch_os::paths::CONFIG_DIR_ENV, config)
        .output()
        .expect("the dispatch binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_machine_that_answers_is_added_listed_and_removed() {
    let config = Scratch::new("ok-cfg");
    let there = Scratch::new("ok-far");
    let dispatchd = dispatchd_binary();
    let endpoint = there.0.join("dispatchd.sock");

    let added = machine(
        &config.0,
        &[
            "add",
            "far",
            "--name",
            "far",
            "--",
            dispatchd.to_str().expect("a UTF-8 path"),
            "--stdio",
            "--endpoint",
            endpoint.to_str().expect("a UTF-8 path"),
        ],
    );
    assert!(added.status.success(), "{}", stderr(&added));
    assert!(stdout(&added).contains("added far"), "{}", stdout(&added));

    let listed = machine(&config.0, &["list"]);
    assert!(stdout(&listed).contains("far"), "{}", stdout(&listed));

    let removed = machine(&config.0, &["remove", "far"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert!(stdout(&removed).contains("left running"), "{}", stdout(&removed));

    let listed = machine(&config.0, &["list"]);
    assert!(stdout(&listed).contains("no machines"), "{}", stdout(&listed));
}

#[test]
fn a_machine_that_cannot_be_reached_is_not_added() {
    let config = Scratch::new("bad-cfg");

    let added = machine(
        &config.0,
        &["add", "gone", "--", "/nonexistent/dispatchd", "--stdio"],
    );

    assert_eq!(added.status.code(), Some(1));
    assert!(stderr(&added).contains("/nonexistent/dispatchd"), "{}", stderr(&added));
    assert!(
        !config.0.join("machines.toml").exists(),
        "nothing is saved for a machine that did not answer"
    );
}

#[test]
fn a_name_already_taken_is_refused_before_dialling() {
    let config = Scratch::new("dup-cfg");

    let first = machine(&config.0, &["add", "z", "--no-check", "--", "true"]);
    assert!(first.status.success(), "{}", stderr(&first));

    let second = machine(&config.0, &["add", "z", "--", "/nonexistent/dispatchd"]);
    assert_eq!(second.status.code(), Some(1));
    assert!(
        stderr(&second).contains("already registered"),
        "refused on the name, before any dial: {}",
        stderr(&second)
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo build --workspace && cargo test -p dispatch --test machine_verbs`
Expected: FAIL — `dispatch machine` is not a subcommand (clap exits 2 with "unrecognized subcommand").

- [ ] **Step 3: Implement**

Create `dispatch/src/machine.rs`:

```rust
//! `dispatch machine`: the machine registry's command-line verbs.
//!
//! None of these draw an interface or start a local daemon. `add` dials the
//! machine once, to prove it has a `dispatchd` that answers, and hangs up.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use dispatch_client::{Client, Liveness};
use dispatch_config::machines::{self, Machine};
use dispatch_proto::Role;

/// What to do to the registry.
#[derive(Debug, clap::Subcommand)]
pub enum Action {
    /// Register a machine, after proving its daemon answers.
    Add {
        /// Where ssh connects: a host, `user@host`, or an alias from your ssh
        /// configuration.
        target: String,

        /// What to call it. Defaults to the target's host.
        #[arg(long)]
        name: Option<String>,

        /// Save it without dialling it, for a machine that is asleep now.
        #[arg(long)]
        no_check: bool,

        /// The command that reaches its daemon, in place of
        /// `ssh <target> dispatchd --stdio`. Given after `--`.
        #[arg(last = true, value_name = "PROGRAM ARGS")]
        command: Vec<String>,
    },

    /// List the registered machines. Dials nothing.
    List,

    /// Forget a machine. Its daemon and agents are left running.
    Remove {
        /// The machine's name, as `list` prints it.
        name: String,
    },
}

/// Runs one verb against this user's configuration.
pub fn run(action: Action) -> Result<ExitCode> {
    let dir = dispatch_os::paths::config_dir().context("failed to locate the config dir")?;

    match action {
        Action::Add {
            target,
            name,
            no_check,
            command,
        } => add(&dir, target, name, no_check, command),
        Action::List => list(&dir),
        Action::Remove { name } => remove(&dir, &name),
    }
}

fn add(
    dir: &Path,
    target: String,
    name: Option<String>,
    no_check: bool,
    command: Vec<String>,
) -> Result<ExitCode> {
    let Some(name) = name.or_else(|| machines::default_name(&target)) else {
        eprintln!("cannot make a machine name from {target:?}; pass --name");
        return Ok(ExitCode::from(1));
    };

    let mut words = command.into_iter();
    let command = words.next().map(|program| machines::Command {
        program,
        args: words.collect(),
    });
    let machine = Machine {
        name,
        target,
        command,
    };
    let host = dispatch_os::host::hostname();

    // Before the dial: being told the name is taken should not cost the
    // thirty seconds a dial to a slow host can take.
    if let Err(error) = machines::check(dir, &machine.name, &host) {
        eprintln!("{error}");
        return Ok(ExitCode::from(1));
    }

    let said = if no_check {
        format!("added {} without checking it", machine.name)
    } else {
        let (program, args) = machine.dial();
        match Client::attach_over(
            Role::Interface,
            crate::CLIENT_NAME,
            Liveness::default(),
            program,
            args,
        ) {
            // Dropped at once: the check was whether it answers, and
            // dropping the client ends the command it started.
            Ok(client) => format!(
                "added {} (its daemon calls itself {})",
                machine.name,
                client.device()
            ),
            Err(error) => {
                eprintln!("could not reach {}: {error}", machine.name);
                eprintln!("ran: {}", machine.describe());
                return Ok(ExitCode::from(1));
            }
        }
    };

    if let Err(error) = machines::add(dir, machine, &host) {
        eprintln!("{error}");
        return Ok(ExitCode::from(1));
    }

    println!("{said}");
    Ok(ExitCode::SUCCESS)
}

fn list(dir: &Path) -> Result<ExitCode> {
    let registered = machines::load(dir)?;

    if registered.is_empty() {
        println!("no machines registered; add one with `dispatch machine add <ssh-target>`");
        return Ok(ExitCode::SUCCESS);
    }

    for machine in registered {
        let how = match &machine.command {
            Some(_) => machine.describe(),
            None => machine.target.clone(),
        };
        println!("{}\t{how}", machine.name);
    }

    Ok(ExitCode::SUCCESS)
}

fn remove(dir: &Path, name: &str) -> Result<ExitCode> {
    if !machines::remove(dir, name)? {
        eprintln!("no machine named {name}");
        return Ok(ExitCode::from(1));
    }

    dispatch_config::projects::forget_machine(dir, name)?;
    println!("removed {name}; its daemon and agents on that machine were left running");
    Ok(ExitCode::SUCCESS)
}
```

In `dispatch/src/main.rs`:

1. Add `mod machine;` with the other modules.
2. Add to `enum Command`:

```rust
    /// Register, list or remove the machines Dispatch reaches over ssh.
    Machine {
        #[command(subcommand)]
        action: machine::Action,
    },
```

3. Replace the `if let Some(Command::Delegate { .. }) = args.command { .. }` block with:

```rust
    match args.command {
        Some(Command::Delegate {
            harness,
            size,
            task,
        }) => return delegate::run(harness, size, &task),
        Some(Command::Machine { action }) => return machine::run(action),
        None => {}
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add dispatch/src/machine.rs dispatch/src/main.rs dispatch/tests/machine_verbs.rs
git commit -m "feat(dispatch): add, list and remove machines from the command line

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: A one-line prompt

**Files:**
- Create: `crates/dispatch-tui/src/prompt.rs`
- Create: `crates/dispatch-tui/src/prompt/tests.rs`
- Modify: `crates/dispatch-tui/src/picker.rs` (`centred` and `write` become `pub(crate)`)
- Modify: `crates/dispatch-tui/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct Prompt` with `new(title: impl Into<String>, hint: impl Into<String>) -> Prompt`, `with_input(self, input: impl Into<String>) -> Prompt`, `title(&self) -> &str`, `input(&self) -> &str`, `answer(&self) -> Option<&str>`, `push(&mut self, c: char)`, `backspace(&mut self)`, `note(&self) -> Option<&Note>`, `set_note(&mut self, note: Option<Note>)`; `impl Widget for &Prompt`.
  - `pub enum Note { Busy(String), Error(String) }`
  - Re-exported: `dispatch_tui::{Prompt, Note}`.

- [ ] **Step 1: Write the failing tests**

Create `crates/dispatch-tui/src/prompt/tests.rs`:

```rust
//! Tests for the one-line prompt.

use super::*;

fn render(prompt: &Prompt, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    prompt.render(area, &mut buf);

    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .filter_map(|x| buf.cell((x, y)))
                .map(|c| c.symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn what_is_typed_is_what_is_shown() {
    let mut prompt = Prompt::new("Open on tower", "a directory on that machine");
    for c in "~/code".chars() {
        prompt.push(c);
    }

    let screen = render(&prompt, 80, 20);
    assert!(screen.contains("Open on tower"), "{screen}");
    assert!(screen.contains("~/code"), "{screen}");
    assert!(screen.contains("a directory on that machine"), "{screen}");
}

#[test]
fn an_answer_is_what_was_typed_without_its_edges() {
    let mut prompt = Prompt::new("t", "h");
    assert_eq!(prompt.answer(), None, "nothing typed is no answer");

    for c in "   ".chars() {
        prompt.push(c);
    }
    assert_eq!(prompt.answer(), None, "spaces alone are no answer");

    for c in "~/app ".chars() {
        prompt.push(c);
    }
    assert_eq!(prompt.answer(), Some("~/app"));
}

#[test]
fn typing_clears_an_error_but_not_work_in_progress() {
    // An error is about what was typed before; a check in progress is not.
    let mut prompt = Prompt::new("t", "h");
    prompt.set_note(Some(Note::Error("no such host".into())));
    prompt.push('a');
    assert_eq!(prompt.note(), None);

    prompt.set_note(Some(Note::Busy("checking…".into())));
    prompt.push('b');
    assert_eq!(prompt.note(), Some(&Note::Busy("checking…".into())));
}

#[test]
fn a_note_is_drawn_under_the_input() {
    let mut prompt = Prompt::new("t", "h").with_input("tower");
    prompt.set_note(Some(Note::Error("tower is already registered".into())));

    let screen = render(&prompt, 80, 20);
    assert!(screen.contains("tower is already registered"), "{screen}");
}

#[test]
fn backspace_takes_the_last_character_and_stops_at_nothing() {
    let mut prompt = Prompt::new("t", "h").with_input("ab");
    prompt.backspace();
    assert_eq!(prompt.input(), "a");
    prompt.backspace();
    prompt.backspace();
    assert_eq!(prompt.input(), "");
}

#[test]
fn a_long_input_shows_its_end() {
    // The end is where the cursor is, and where the user is typing.
    let long = format!("{}-the-end", "x".repeat(200));
    let prompt = Prompt::new("t", "h").with_input(long);

    let screen = render(&prompt, 60, 10);
    assert!(screen.contains("-the-end"), "{screen}");
}

#[test]
fn a_tiny_area_draws_nothing_rather_than_panicking() {
    let prompt = Prompt::new("t", "h").with_input("abc");
    let _ = render(&prompt, 3, 2);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui prompt`
Expected: compile error — module `prompt` not found.

- [ ] **Step 3: Implement**

In `crates/dispatch-tui/src/picker.rs`, change `fn centred(` to `pub(crate) fn centred(` and `fn write(` to `pub(crate) fn write(`.

In `crates/dispatch-tui/src/lib.rs`, add `pub mod prompt;` and `pub use prompt::{Note, Prompt};`.

Create `crates/dispatch-tui/src/prompt.rs`:

```rust
//! One line of typed input, for the questions a list cannot answer.
//!
//! A machine's ssh target and a path on another machine have nothing to pick
//! from: this client cannot see the other machine's filesystem, and the hosts
//! a user can reach live in their ssh configuration, not in Dispatch's.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::picker::{centred, write};

/// A line under the input saying what is happening, or what went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// Work in progress, such as a machine being dialled.
    Busy(String),
    /// Why the last answer was not accepted.
    Error(String),
}

/// A question and what has been typed in answer.
#[derive(Debug, Clone)]
pub struct Prompt {
    title: String,
    hint: String,
    input: String,
    note: Option<Note>,
}

/// The narrowest a prompt is drawn, so a short title still leaves room to
/// type a path.
const MIN_WIDTH: u16 = 44;

impl Prompt {
    /// An empty prompt.
    #[must_use]
    pub fn new(title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            hint: hint.into(),
            input: String::new(),
            note: None,
        }
    }

    /// The same prompt with something already typed, for a default the user
    /// can accept or edit.
    #[must_use]
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }

    /// The question.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Everything typed, as typed.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// What was typed without surrounding whitespace, or `None` when that
    /// leaves nothing.
    ///
    /// Enter on nothing must do nothing: an empty path or an empty machine
    /// name is never what the user meant.
    #[must_use]
    pub fn answer(&self) -> Option<&str> {
        let trimmed = self.input.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    /// Types a character.
    ///
    /// Clears an error, which was about what was typed before, but not a
    /// busy note, which is about work still going on.
    pub fn push(&mut self, c: char) {
        self.input.push(c);
        if matches!(self.note, Some(Note::Error(_))) {
            self.note = None;
        }
    }

    /// Removes the last character, if there is one.
    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// The line under the input, if there is one.
    #[must_use]
    pub fn note(&self) -> Option<&Note> {
        self.note.as_ref()
    }

    /// Replaces the line under the input.
    pub fn set_note(&mut self, note: Option<Note>) {
        self.note = note;
    }
}

impl Widget for &Prompt {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 8 || area.height < 5 {
            return;
        }

        let widest = [
            self.title.chars().count(),
            self.hint.chars().count(),
            self.input.chars().count(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let width = u16::try_from(widest + 6)
            .unwrap_or(u16::MAX)
            .clamp(MIN_WIDTH.min(area.width), area.width);

        // Border, input, hint, note.
        let rect = centred(area, width, 5);

        // Floats over the grid, so whatever it covers is erased rather than
        // left showing through.
        Clear.render(rect, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", self.title))
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(rect);
        block.render(rect, buf);

        // The end of a long input rather than its start: the end is where the
        // user is typing.
        let room = usize::from(inner.width.saturating_sub(3));
        let count = self.input.chars().count();
        let shown: String = self.input.chars().skip(count.saturating_sub(room)).collect();
        let x = write(buf, inner, inner.x, inner.y, "> ", Style::default().fg(Color::Cyan));
        let x = write(buf, inner, x, inner.y, &shown, Style::default());
        write(buf, inner, x, inner.y, "▏", Style::default().fg(Color::Cyan));

        write(
            buf,
            inner,
            inner.x,
            inner.y + 1,
            &self.hint,
            Style::default().fg(Color::DarkGray),
        );

        if let Some(note) = &self.note {
            let (text, colour) = match note {
                Note::Busy(text) => (text, Color::Yellow),
                Note::Error(text) => (text, Color::Red),
            };
            write(buf, inner, inner.x, inner.y + 2, text, Style::default().fg(colour));
        }
    }
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-tui && cargo clippy -p dispatch-tui --all-targets`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/dispatch-tui
git commit -m "feat(tui): a one-line prompt for questions a list cannot answer

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: `^a o` asks which machine, and a remote one gets a path

**Files:**
- Modify: `dispatch/src/app.rs` (`Overlay`, `OverlayKind`, `Action::OpenProject` dispatch, `choose`, `handle_overlay`, `draw_overlay`, new `start_open`, `open_machine_picker`, `open_path_prompt`, `handle_open_on_key`, `add_project_on`)

**Interfaces:**
- Consumes: `Prompt` (Task 8); `Attachment.remote`, `kept_list`, `keep` (Task 5).
- Produces: private `Overlay::Machine(Picker)`, `Overlay::OpenOn { device: DeviceId, prompt: Prompt }`, `OverlayKind::Machine`, `App::add_project_on(&mut self, device: DeviceId, root: PathBuf)`.

- [ ] **Step 1: Write the failing tests**

In `dispatch/src/app.rs`'s `mod tests`:

```rust
    /// The prefix, then `o`.
    fn open_project(app: &mut App) {
        app.handle(
            &Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Size::new(100, 30),
        )
        .expect("a keystroke is handled");
        press(app, KeyCode::Char('o'));
    }

    /// Types `text` one key at a time.
    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// This machine's daemon and one registered machine, `tower`.
    fn near_and_tower() -> (App, Receiver<ClientMessage>, Receiver<ClientMessage>) {
        let (near, _near_daemon, near_sent) = Client::for_test();
        near.handle().rename_for_test("near");
        let (tower, _tower_daemon, tower_sent) = Client::for_test();

        let mut app = App::new(HarnessRegistry::default());
        app.attach(near);
        app.attach_named(tower, Some("tower".into()), Vec::new());
        app.poll_daemon();

        (app, near_sent, tower_sent)
    }

    #[test]
    fn opening_with_two_machines_asks_which_one_first() {
        let (mut app, _near, _tower) = near_and_tower();

        open_project(&mut app);

        let Some(Overlay::Machine(picker)) = &app.overlay else {
            panic!("expected the machine picker");
        };
        let labels: Vec<&str> = picker.items().iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["near", "tower"]);
    }

    #[test]
    fn this_machine_still_gets_the_browser() {
        let (mut app, _near, _tower) = near_and_tower();

        open_project(&mut app);
        press(&mut app, KeyCode::Enter);

        assert!(matches!(app.overlay, Some(Overlay::Browse(_))));
    }

    #[test]
    fn a_remote_machine_gets_a_path_prompt_and_only_it_is_asked() {
        let dir = scratch("open-on");
        let (mut app, near_sent, tower_sent) = near_and_tower();
        app.keep_projects_in(&dir);

        open_project(&mut app);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Some(Overlay::OpenOn { .. })));

        type_text(&mut app, "~/code/app");
        press(&mut app, KeyCode::Enter);

        let asked: Vec<ClientMessage> = tower_sent.try_iter().collect();
        assert!(
            asked.contains(&ClientMessage::OpenProject {
                root: PathBuf::from("~/code/app")
            }),
            "tower is asked: {asked:?}"
        );
        assert!(
            !near_sent
                .try_iter()
                .any(|m| matches!(m, ClientMessage::OpenProject { .. })),
            "this machine is not"
        );
        assert_eq!(
            dispatch_config::projects::load_on(&dir, "tower").expect("it reads back"),
            [PathBuf::from("~/code/app")],
            "kept under the machine it was opened on"
        );
        assert!(app.overlay.is_none());
    }

    #[test]
    fn enter_on_an_empty_path_does_nothing() {
        let (mut app, _near, tower_sent) = near_and_tower();

        open_project(&mut app);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "   ");
        press(&mut app, KeyCode::Enter);

        assert!(matches!(app.overlay, Some(Overlay::OpenOn { .. })), "still asking");
        assert!(
            !tower_sent
                .try_iter()
                .any(|m| matches!(m, ClientMessage::OpenProject { .. })),
            "nothing was asked for"
        );
    }

    #[test]
    fn one_remote_machine_goes_straight_to_the_prompt() {
        let (tower, _daemon, _sent) = Client::for_test();
        let mut app = App::new(HarnessRegistry::default());
        app.attach_named(tower, Some("tower".into()), Vec::new());

        open_project(&mut app);

        assert!(matches!(app.overlay, Some(Overlay::OpenOn { .. })));
    }

    #[test]
    fn one_local_machine_still_opens_the_browser() {
        let (mut app, _project, _daemon, _sent) = attached_app();

        open_project(&mut app);

        assert!(matches!(app.overlay, Some(Overlay::Browse(_))));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch app::tests::opening_with_two app::tests::a_remote_machine app::tests::enter_on_an_empty_path app::tests::one_`
Expected: compile errors — `Overlay::Machine` and `Overlay::OpenOn` do not exist.

- [ ] **Step 3: Implement**

In `dispatch/src/app.rs`:

1. Import `dispatch_tui::Prompt` beside `Picker`.

2. Add to `enum Overlay`:

```rust
    /// A machine to open a project on, when there is more than one.
    Machine(Picker),
    /// A path to open on a machine this client cannot browse.
    OpenOn {
        /// The machine it will be opened on.
        device: DeviceId,
        /// What has been typed.
        prompt: Prompt,
    },
```

Add `Machine` to `OverlayKind`. In `picker()` and `picker_mut()` add `Overlay::Machine(picker)` to the arm that returns `Some(picker)`, and `Overlay::OpenOn { .. }` to the arm that returns `None`. In `kind()` add `Overlay::Machine(_) => Some(OverlayKind::Machine),` and `Overlay::OpenOn { .. }` to the `None` arm.

3. Change `Action::OpenProject => self.open_browser(),` to `Action::OpenProject => self.start_open(),` and add:

```rust
    /// Opens a project: straight to the one machine there is, or asks which.
    fn start_open(&mut self) {
        match self.attachments() {
            [] => self.open_browser(),
            [only] if !only.remote => self.open_browser(),
            [only] => {
                let device = only.device;
                self.open_path_prompt(device);
            }
            _ => self.open_machine_picker(),
        }
    }

    /// Offers every attached machine, starting on the one the view is in.
    fn open_machine_picker(&mut self) {
        let here = self.state.selected_project().and_then(|selected| {
            self.state
                .projects()
                .iter()
                .find(|p| p.id == selected)
                .map(|p| p.device)
        });

        let items: Vec<Item> = self
            .attachments()
            .iter()
            .filter_map(|attachment| {
                let device = self.state.device(attachment.device)?;
                let item = Item::new(attachment.device.to_string(), &device.name);
                Some(if !attachment.remote {
                    item.with_detail("this machine")
                } else if !device.reachable {
                    // Allowed: the root waits until the machine answers.
                    item.with_detail("unreachable")
                } else {
                    item
                })
            })
            .collect();

        let start = here
            .and_then(|device| items.iter().position(|i| i.id == device.to_string()))
            .unwrap_or(0);

        let mut picker = Picker::new("Open on", items);
        for _ in 0..start {
            picker.next();
        }
        self.overlay = Some(Overlay::Machine(picker));
    }

    /// Asks for a path on a machine this client cannot browse.
    fn open_path_prompt(&mut self, device: DeviceId) {
        let name = self
            .state
            .device(device)
            .map(|d| d.name.clone())
            .unwrap_or_default();

        self.overlay = Some(Overlay::OpenOn {
            device,
            prompt: Prompt::new(
                format!("Open on {name}"),
                "a directory on that machine, such as ~/code/app",
            ),
        });
    }
```

4. In `choose`, add:

```rust
            OverlayKind::Machine => {
                let chosen = self
                    .attachments()
                    .iter()
                    .find(|a| a.device.to_string() == id)
                    .map(|a| (a.device, a.remote));

                match chosen {
                    Some((device, true)) => self.open_path_prompt(device),
                    Some((_, false)) => self.open_browser(),
                    None => {}
                }
            }
```

5. In `handle_overlay`, after the `Overlay::Browse` early return, add:

```rust
        // Plain letters are what is being typed, so this takes every key.
        if matches!(self.overlay, Some(Overlay::OpenOn { .. })) {
            self.handle_open_on_key(key);
            return Ok(());
        }
```

and add the handler:

```rust
    /// Acts on one key while a remote path is being typed.
    fn handle_open_on_key(&mut self, key: &KeyEvent) {
        let Some(Overlay::OpenOn { device, prompt }) = &mut self.overlay else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Backspace => prompt.backspace(),
            KeyCode::Enter => {
                let Some(answer) = prompt.answer() else {
                    return;
                };
                let device = *device;
                let root = PathBuf::from(answer);
                let name = prompt.title().trim_start_matches("Open on ").to_string();

                self.overlay = None;
                self.add_project_on(device, root.clone());
                self.status = format!("opening {} on {name}", root.display());
            }
            KeyCode::Char(c) if !ctrl => prompt.push(c),
            _ => {}
        }
    }

    /// Asks one machine to open `root`, and keeps it on that machine's list.
    ///
    /// Sent even to a machine that is down: the send is dropped, but the root
    /// is in `opened`, and goes out when the machine connects.
    fn add_project_on(&mut self, device: DeviceId, root: PathBuf) {
        let list = self.kept_list(device);
        self.keep(&list, &root);

        let Mode::Attached(attachments) = &mut self.mode else {
            return;
        };
        let Some(attachment) = attachments.iter_mut().find(|a| a.device == device) else {
            return;
        };

        attachment
            .client
            .send(ClientMessage::OpenProject { root: root.clone() });
        if !attachment.opened.contains(&root) {
            attachment.opened.push(root);
        }
    }
```

Rather than recovering the machine's name from the prompt's title, it is acceptable to look it up again with `self.state.device(device)` after `self.overlay = None`; do whichever reads cleaner, but the status must name the machine.

6. In `draw_overlay`, after the `Overlay::Browse` branch:

```rust
        if let Overlay::OpenOn { prompt, .. } = overlay {
            frame.render_widget(prompt, panes_area);
            return;
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS, no warnings. The existing browser tests still pass: with one local machine `^a o` opens the browser as before.

- [ ] **Step 5: Commit**

```bash
git add dispatch/src/app.rs
git commit -m "feat(dispatch): open a project on a remote machine by typing its path

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: `^a m` adds a machine

**Files:**
- Modify: `crates/dispatch-tui/src/input.rs` (`Action::AddMachine`, bound to `m`)
- Modify: `crates/dispatch-tui/src/input/tests.rs` (the prefix-command table)
- Create: `dispatch/src/add_machine.rs`
- Modify: `dispatch/src/main.rs` (`mod add_machine;`)
- Modify: `dispatch/src/app.rs` (`Overlay::AddMachine`, a `checker` field, `open_add_machine`, `handle_add_machine_key`, `poll_add_machine`, `draw_overlay`)

**Interfaces:**
- Consumes: `Prompt`, `Note` (Task 8); `machines::{Machine, default_name, check, add}` (Task 2); `App::attach_named` (Task 5); `Client::attach_over`, `Client::for_test`.
- Produces:
  - `Action::AddMachine`
  - `add_machine::AddMachine` with `new() -> AddMachine`, `prompt(&self) -> &Prompt`, `key(&mut self, key: &KeyEvent, validate: impl Fn(&str) -> Result<(), String>) -> Step`, `checking(&mut self, machine: Machine, answer: Receiver<Answer>)`, `poll(&mut self) -> Checked`
  - `add_machine::Step { Stay, Close, Check(Machine) }`, `add_machine::Checked { Waiting, Failed, Passed(Machine, Client) }`, `add_machine::Answer = Result<Client, String>`, `add_machine::check(machine: &Machine) -> Receiver<Answer>`
  - App field `checker: Box<dyn Fn(&Machine) -> Receiver<Answer>>`, `add_machine::check` by default.

- [ ] **Step 1: Write the failing tests**

In `crates/dispatch-tui/src/input/tests.rs`, add `('m', Action::AddMachine),` to the table that already holds `('o', Action::OpenProject),`.

Create the test module at the bottom of `dispatch/src/add_machine.rs` (the file is created in Step 3; write the tests into it first, above an empty `AddMachine` stub if that helps you see the failure):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(add: &mut AddMachine, text: &str) {
        for c in text.chars() {
            let _ = add.key(&key(KeyCode::Char(c)), |_| Ok(()));
        }
    }

    #[test]
    fn a_target_is_followed_by_a_name_that_defaults_from_it() {
        let mut add = AddMachine::new();
        type_text(&mut add, "me@tower.lan");

        assert!(matches!(add.key(&key(KeyCode::Enter), |_| Ok(())), Step::Stay));
        assert_eq!(add.prompt().input(), "tower");

        let Step::Check(machine) = add.key(&key(KeyCode::Enter), |_| Ok(())) else {
            panic!("a valid name starts the check");
        };
        assert_eq!(machine, Machine::new("tower", "me@tower.lan"));
    }

    #[test]
    fn enter_on_an_empty_target_does_nothing() {
        let mut add = AddMachine::new();
        type_text(&mut add, "  ");

        assert!(matches!(add.key(&key(KeyCode::Enter), |_| Ok(())), Step::Stay));
        assert_eq!(add.prompt().input(), "  ", "still on the target");
    }

    #[test]
    fn a_name_that_is_refused_says_why_and_dials_nothing() {
        let mut add = AddMachine::new();
        type_text(&mut add, "tower");
        let _ = add.key(&key(KeyCode::Enter), |_| Ok(()));

        let step = add.key(&key(KeyCode::Enter), |_| Err("tower is already registered".into()));

        assert!(matches!(step, Step::Stay));
        assert_eq!(
            add.prompt().note(),
            Some(&Note::Error("tower is already registered".into()))
        );
    }

    #[test]
    fn a_failed_check_goes_back_to_the_target_with_the_reason() {
        let mut add = AddMachine::new();
        type_text(&mut add, "towr");
        let _ = add.key(&key(KeyCode::Enter), |_| Ok(()));
        let Step::Check(machine) = add.key(&key(KeyCode::Enter), |_| Ok(())) else {
            panic!("the check starts");
        };

        let (done, answer) = std::sync::mpsc::channel();
        add.checking(machine, answer);
        done.send(Err("ssh: Could not resolve hostname towr".into()))
            .expect("the overlay is listening");

        assert!(matches!(add.poll(), Checked::Failed));
        assert_eq!(add.prompt().input(), "towr", "the target, ready to fix");
        assert_eq!(
            add.prompt().note(),
            Some(&Note::Error("ssh: Could not resolve hostname towr".into()))
        );
    }

    #[test]
    fn keys_wait_while_checking_except_escape() {
        let mut add = AddMachine::new();
        type_text(&mut add, "tower");
        let _ = add.key(&key(KeyCode::Enter), |_| Ok(()));
        let Step::Check(machine) = add.key(&key(KeyCode::Enter), |_| Ok(())) else {
            panic!("the check starts");
        };
        let (_done, answer) = std::sync::mpsc::channel();
        add.checking(machine, answer);

        type_text(&mut add, "zzz");
        assert_eq!(add.prompt().input(), "tower", "typing waits for the answer");
        assert!(matches!(add.poll(), Checked::Waiting));
        assert!(matches!(add.key(&key(KeyCode::Esc), |_| Ok(())), Step::Close));
    }
}
```

In `dispatch/src/app.rs`'s `mod tests`:

```rust
    /// The prefix, then `m`.
    fn add_machine(app: &mut App) {
        app.handle(
            &Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Size::new(100, 30),
        )
        .expect("a keystroke is handled");
        press(app, KeyCode::Char('m'));
    }

    /// A checker that hands back whatever the test sends, once.
    fn scripted_checker() -> (
        Box<dyn Fn(&Machine) -> Receiver<crate::add_machine::Answer>>,
        Sender<crate::add_machine::Answer>,
    ) {
        let (done, answer) = std::sync::mpsc::channel();
        let answer = std::sync::Mutex::new(Some(answer));
        (
            Box::new(move |_| {
                answer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .expect("one check per test")
            }),
            done,
        )
    }

    #[test]
    fn adding_a_machine_is_refused_while_standalone() {
        // Attaching tears the standalone machine down, panes and all. A
        // keystroke must never do that.
        let mut app = App::new(HarnessRegistry::default());

        add_machine(&mut app);

        assert!(app.overlay.is_none());
        assert!(app.status.contains("--attach"), "{}", app.status);
        assert!(app.local.is_some(), "the standalone machine is untouched");
    }

    #[test]
    fn a_machine_that_passes_its_check_is_saved_and_attached() {
        let dir = scratch("add-machine");
        let (mut app, _project, _daemon, _sent) = attached_app();
        app.keep_projects_in(&dir);
        let (checker, done) = scripted_checker();
        app.checker = checker;

        add_machine(&mut app);
        type_text(&mut app, "me@tower.lan");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);

        let (client, _far, _far_sent) = Client::for_test();
        client.handle().rename_for_test("ubuntu-22");
        done.send(Ok(client)).expect("the overlay is waiting");
        app.poll_daemon();

        assert!(app.overlay.is_none());
        assert_eq!(
            dispatch_config::machines::load(&dir).expect("it reads back"),
            [Machine::new("tower", "me@tower.lan")]
        );
        assert!(device_named(&app, "tower").reachable, "attached, already connected");
        assert!(app.status.contains("added tower"), "{}", app.status);
    }

    #[test]
    fn closing_the_overlay_mid_check_saves_nothing() {
        let dir = scratch("add-machine-esc");
        let (mut app, _project, _daemon, _sent) = attached_app();
        app.keep_projects_in(&dir);
        let (checker, done) = scripted_checker();
        app.checker = checker;
        let devices = app.state.devices().len();

        add_machine(&mut app);
        type_text(&mut app, "tower");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);

        let (client, _far, _far_sent) = Client::for_test();
        assert!(
            done.send(Ok(client)).is_err(),
            "nobody is waiting: the late client is dropped with the send"
        );
        app.poll_daemon();

        assert!(dispatch_config::machines::load(&dir).expect("it reads").is_empty());
        assert_eq!(app.state.devices().len(), devices, "no row was added");
    }
```

Import `dispatch_config::machines::Machine` into the test module if it is not already in scope through `super::*`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui; cargo test -p dispatch add_machine`
Expected: compile errors — `Action::AddMachine`, `AddMachine`, `Step`, `Checked` do not exist.

- [ ] **Step 3: Implement**

In `crates/dispatch-tui/src/input.rs`, add to `Action` after `OpenProject`:

```rust
    /// Open the overlay that registers a machine, to add one without
    /// restarting.
    AddMachine,
```

and to `command_for`, after `'o'`:

```rust
        KeyCode::Char('m') => Action::AddMachine,
```

Create `dispatch/src/add_machine.rs`:

```rust
//! The `^a m` overlay: a target, a name, and proof that the machine answers.
//!
//! Its own module because it is a small state machine with a background
//! check in the middle, and none of that needs the rest of the application
//! to be tested.

use std::sync::mpsc::{Receiver, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dispatch_client::{Client, Liveness};
use dispatch_config::machines::{self, Machine};
use dispatch_proto::Role;
use dispatch_tui::{Note, Prompt};

/// What a check hands back: a connected client, or why there is none.
pub type Answer = Result<Client, String>;

/// What the application should do after a key.
#[derive(Debug)]
pub enum Step {
    /// Nothing.
    Stay,
    /// Close the overlay.
    Close,
    /// Dial this machine, then hand the answer to [`AddMachine::checking`].
    Check(Machine),
}

/// Where a check stands.
pub enum Checked {
    /// No answer yet, or no check running.
    Waiting,
    /// It failed; the overlay is asking again and says why.
    Failed,
    /// The machine answered: save it and attach this client.
    Passed(Machine, Client),
}

/// The overlay's state.
pub struct AddMachine {
    prompt: Prompt,
    stage: Stage,
}

enum Stage {
    /// Asking where ssh should connect, remembering a name the user already
    /// chose if a failed check brought them back here.
    Target { name: Option<String> },
    /// Asking what to call it.
    Name { target: String },
    /// Waiting on the check.
    Checking {
        machine: Machine,
        answer: Receiver<Answer>,
    },
}

/// The first question.
fn target_prompt(target: &str) -> Prompt {
    Prompt::new("Add a machine", "an ssh target: host, user@host, or an ssh alias")
        .with_input(target)
}

/// The second question.
fn name_prompt(name: &str) -> Prompt {
    Prompt::new("Call it", "letters, digits, - and _").with_input(name)
}

impl AddMachine {
    /// A fresh overlay, asking for a target.
    #[must_use]
    pub fn new() -> Self {
        Self {
            prompt: target_prompt(""),
            stage: Stage::Target { name: None },
        }
    }

    /// What to draw.
    #[must_use]
    pub fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    /// Acts on one key.
    ///
    /// `validate` answers whether a name could be registered, so a taken name
    /// is refused before thirty seconds are spent dialling.
    pub fn key(&mut self, key: &KeyEvent, validate: impl Fn(&str) -> Result<(), String>) -> Step {
        if key.code == KeyCode::Esc {
            return Step::Close;
        }

        // Keys wait for the answer: an edit now would describe a machine
        // other than the one being dialled.
        if matches!(self.stage, Stage::Checking { .. }) {
            return Step::Stay;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Backspace => self.prompt.backspace(),
            KeyCode::Char(c) if !ctrl => self.prompt.push(c),
            KeyCode::Enter => return self.submit(validate),
            _ => {}
        }
        Step::Stay
    }

    /// Accepts what is typed, when something is.
    fn submit(&mut self, validate: impl Fn(&str) -> Result<(), String>) -> Step {
        let Some(answer) = self.prompt.answer().map(str::to_string) else {
            return Step::Stay;
        };

        match &self.stage {
            Stage::Target { name } => {
                let name = name
                    .clone()
                    .or_else(|| machines::default_name(&answer))
                    .unwrap_or_default();
                self.prompt = name_prompt(&name);
                self.stage = Stage::Name { target: answer };
                Step::Stay
            }
            Stage::Name { target } => {
                if let Err(reason) = validate(&answer) {
                    self.prompt.set_note(Some(Note::Error(reason)));
                    return Step::Stay;
                }
                let machine = Machine::new(answer, target.clone());
                self.prompt
                    .set_note(Some(Note::Busy(format!("checking {}…", machine.name))));
                Step::Check(machine)
            }
            Stage::Checking { .. } => Step::Stay,
        }
    }

    /// Waits on a check the application has started.
    pub fn checking(&mut self, machine: Machine, answer: Receiver<Answer>) {
        self.stage = Stage::Checking { machine, answer };
    }

    /// Where the check stands.
    ///
    /// A failure goes back to the target, not the name: a dial that fails is
    /// almost always the target's fault, and the name chosen is kept for the
    /// next try.
    pub fn poll(&mut self) -> Checked {
        let Stage::Checking { answer, .. } = &self.stage else {
            return Checked::Waiting;
        };

        let result = match answer.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Checked::Waiting,
            Err(TryRecvError::Disconnected) => Err("the check ended without an answer".into()),
        };

        let Stage::Checking { machine, .. } =
            std::mem::replace(&mut self.stage, Stage::Target { name: None })
        else {
            return Checked::Waiting;
        };

        match result {
            Ok(client) => Checked::Passed(machine, client),
            Err(reason) => {
                self.prompt = target_prompt(&machine.target);
                self.prompt.set_note(Some(Note::Error(reason)));
                self.stage = Stage::Target {
                    name: Some(machine.name),
                };
                Checked::Failed
            }
        }
    }
}

/// Dials `machine` on a thread of its own and hands the answer back.
///
/// A thread because the dial may take thirty seconds and the interface must
/// keep drawing. If the overlay has been closed by the time it answers, the
/// send fails and the client is dropped with it, which ends its command.
pub fn check(machine: &Machine) -> Receiver<Answer> {
    let (done, answer) = std::sync::mpsc::channel();
    let (program, args) = machine.dial();

    std::thread::spawn(move || {
        let result = Client::attach_over(
            Role::Interface,
            crate::CLIENT_NAME,
            Liveness::default(),
            program,
            args,
        )
        .map_err(|error| error.to_string());
        let _ = done.send(result);
    });

    answer
}
```

(The `#[cfg(test)] mod tests` block from Step 1 goes at the end of this file.)

In `dispatch/src/main.rs`, add `mod add_machine;`.

In `dispatch/src/app.rs`:

1. `use crate::add_machine::{self, AddMachine, Checked, Step};` and `use dispatch_config::machines::{self, Machine};`.

2. Add to `enum Overlay`:

```rust
    /// A machine being registered.
    AddMachine(AddMachine),
```

Add `Overlay::AddMachine(_)` to the `None` arms of `picker()`, `picker_mut()` and `kind()`.

3. Add a field to `App`, after `kept`:

```rust
    /// How the add overlay proves a machine answers.
    ///
    /// A field so a test can answer for a machine that does not exist; the
    /// real one dials it.
    checker: Box<dyn Fn(&Machine) -> std::sync::mpsc::Receiver<add_machine::Answer>>,
```

initialised in `App::new` as `checker: Box::new(add_machine::check),`.

4. In `handle`, add `Action::AddMachine => self.open_add_machine(),`, and add:

```rust
    /// Opens the add overlay, when this client can take another machine.
    fn open_add_machine(&mut self) {
        // Attaching tears the standalone machine down, panes and all. The
        // registry makes the next start attached; this keystroke must not
        // make this one.
        if matches!(self.mode, Mode::Standalone) {
            self.status =
                "machines need the daemon: restart Dispatch with --attach to add one".into();
            return;
        }
        if self.kept.is_none() {
            self.status = "there is no configuration directory to save a machine in".into();
            return;
        }

        self.overlay = Some(Overlay::AddMachine(AddMachine::new()));
    }
```

5. In `handle_overlay`, after the `OpenOn` early return:

```rust
        if matches!(self.overlay, Some(Overlay::AddMachine(_))) {
            self.handle_add_machine_key(key);
            return Ok(());
        }
```

and add:

```rust
    /// Acts on one key while a machine is being added.
    fn handle_add_machine_key(&mut self, key: &KeyEvent) {
        let dir = self.kept.clone();
        let host = this_machine();
        let validate = |name: &str| match &dir {
            Some(dir) => machines::check(dir, name, &host).map_err(|e| e.to_string()),
            None => Ok(()),
        };

        let Some(Overlay::AddMachine(add)) = &mut self.overlay else {
            return;
        };

        match add.key(key, validate) {
            Step::Stay => {}
            Step::Close => self.overlay = None,
            Step::Check(machine) => {
                let answer = (self.checker)(&machine);
                add.checking(machine, answer);
            }
        }
    }

    /// Acts on the add overlay's check, once it answers.
    fn poll_add_machine(&mut self) -> bool {
        let Some(Overlay::AddMachine(add)) = &mut self.overlay else {
            return false;
        };

        let (machine, client) = match add.poll() {
            Checked::Waiting => return false,
            Checked::Failed => return true,
            Checked::Passed(machine, client) => (machine, client),
        };

        self.overlay = None;

        let Some(dir) = self.kept.clone() else {
            return true;
        };
        if let Err(error) = machines::add(&dir, machine.clone(), &this_machine()) {
            self.status = format!("could not save {}: {error}", machine.name);
            return true;
        }

        let device = client.device();
        client.subscribe();
        self.attach_named(client, Some(machine.name.clone()), Vec::new());
        self.status = format!("added {} (its daemon calls itself {device})", machine.name);
        true
    }
```

6. At the top of `poll_daemon`, before the `let Mode::Attached(attachments) = &self.mode else` line:

```rust
        let added = self.poll_add_machine();
```

and make the function return `added || changed` (and `added` in the standalone early return).

7. In `draw_overlay`, after the `OpenOn` branch:

```rust
        if let Overlay::AddMachine(add) = overlay {
            frame.render_widget(add.prompt(), panes_area);
            return;
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/dispatch-tui/src/input.rs crates/dispatch-tui/src/input/tests.rs dispatch/src/add_machine.rs dispatch/src/main.rs dispatch/src/app.rs
git commit -m "feat(dispatch): add a machine without restarting

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: Say what it is for, and where federation stands

**Files:**
- Modify: `README.md` (the `## More than one machine` section)
- Modify: `docs/superpowers/federation-handoff.md`
- Modify: `docs/superpowers/specs/2026-09-22-federation-machines-design.md` (status line)

- [ ] **Step 1: Rewrite the README section**

Replace the body of `## More than one machine` in `README.md` with:

````markdown
Register a machine once, and every Dispatch after that reaches it over ssh:

```sh
dispatch machine add me@tower          # dials it first; saved only if it answers
dispatch machine add gpu-box --name gpu
dispatch machine list
dispatch machine remove gpu            # its daemon and agents keep running
```

The machine needs `dispatchd` on its `PATH`; nothing is copied to it. Dispatch
runs `ssh -T -o BatchMode=yes -o ConnectTimeout=10 <target> dispatchd --stdio`,
so ssh never prompts: run `ssh <target>` once by hand first to accept its host
key, and use a key or an agent rather than a password. Anything else — a
wrapper, a nix shell, a transport other than ssh — goes after `--`:

```sh
dispatch machine add gpu-box -- /opt/tools/tunnel gpu-box dispatchd --stdio
```

With any machine registered, `dispatch` attaches to its daemons on its own —
this machine's included — and draws a row for each at once. A machine that is
asleep stays dimmed and joins when it wakes. `^a m` adds a machine without
restarting. `^a o` asks which machine to open a project on; a remote one takes
a typed path, such as `~/code/app`.

`--daemon <endpoint>` and `--daemon-command "<command>"` still reach a daemon
for one run without registering it. `--daemon-command` is split on
whitespace, with no shell; a program whose path holds a space needs the
registry.
````

Keep any sentence from the old section that the new text does not cover (for example, the explanation that agents survive the transport dying); fold it in rather than dropping it.

- [ ] **Step 2: Update the handoff**

In `docs/superpowers/federation-handoff.md`:

- In the slices table, set F2b's state to **merged** once the branch lands (**built, on a branch** until then), with its spec path.
- Replace the `## F2b, already decided` section with a `## What F2b built` paragraph: the registry, the verbs, `^a m`, `^a o`'s machine step, `Client::dial` and the per-dial backoff, `ProjectRefused`.
- In `## Parked`, delete item 1 (serial startup) and item 2 (a dial in flight at drop): both are fixed. Renumber the rest. Delete item 6 (several `--daemon-command` failures collapse into one status line) only if the per-attachment outage line makes it moot; otherwise keep it.
- Add to `## Parked`: "**Remote directory browsing.** `^a o` on a remote machine takes a typed path; the browser would need protocol messages that list a remote directory." and "**Removing or renaming a machine in the TUI.** The CLI does both; the overlay only adds."
- In `## Decisions taken on the user's behalf`, delete the `add_project asks the first attachment` bullet (answered: a remote machine is chosen in `^a o`), and add: "`ProjectRefused` is a `ServerMessage`, not a `ProtocolError` variant: `ProtocolError` has no `Unknown`, so a new variant would fail an older peer's frame." and "`^a m` is refused while standalone rather than attaching mid-session, because attaching tears the standalone panes down."
- In `## Where the moving parts live`, add rows: `| The machine registry | crates/dispatch-config/src/machines.rs |`, `| The machine verbs | dispatch/src/machine.rs |`, `| The add overlay | dispatch/src/add_machine.rs |`, `| One-line prompts | crates/dispatch-tui/src/prompt.rs |`.

- [ ] **Step 3: Mark the spec implemented**

Change the spec's `Status:` line to `Status: implemented.`

- [ ] **Step 4: Verify**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/superpowers
git commit -m "docs: say how machines are registered and reached

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```
