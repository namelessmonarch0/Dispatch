# Federation F1 — Many Daemons In One Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One Dispatch client holds connections to several daemons at once, with every project and pane attributed to the machine it is on.

**Architecture:** A `Device` lives in `dispatch-core` beside projects and panes; a `Project` names its device. The client's `Mode::Attached` grows from one `Client` to a `Vec<Attachment>`, each with its own connection, generation and reconnect. Writes route pane → project → device → attachment. The sidebar grows a device level, drawn only when there is more than one machine.

**Tech Stack:** Rust 2024 (1.85+), ratatui, crossterm, serde, `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt`.

**Spec:** `docs/superpowers/specs/2026-09-21-federation-many-daemons-design.md`

## Global Constraints

- Rust edition 2024, rust-version as pinned in the workspace `Cargo.toml`. Do not raise it.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets` (zero warnings) and `cargo test --workspace` must pass before every commit.
- TDD: every task writes the failing test first and runs it to watch it fail.
- Doc comments on every public item; the house style explains *why*, not *what*. Match the surrounding prose.
- The wire protocol does not change in this slice. `dispatch-proto` is not edited.
- Commit messages: Conventional Commits, and end with `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.
- `DeviceId` already exists in `crates/dispatch-core/src/id.rs`. Reuse it; do not mint a second id type.

---

### Task 1: Device in `dispatch-core`

**Files:**
- Create: `crates/dispatch-core/src/device.rs`
- Modify: `crates/dispatch-core/src/lib.rs` (add `pub mod device;`, re-export `Device`)
- Modify: `crates/dispatch-core/src/id.rs` (`DeviceId` doc says "Unused in Slice 1" — rewrite it)
- Modify: `crates/dispatch-core/src/project.rs` (add the `device` field and `with_device`)
- Modify: `crates/dispatch-core/src/state.rs` (devices, collapse, CRUD; tests at the bottom of the file)

**Interfaces:**
- Produces: `Device { id: DeviceId, name: String, reachable: bool }`, `Device::new(name: impl Into<String>) -> Device`; `Project::with_device(DeviceId) -> Project`, `Project::device: DeviceId`; on `AppState`: `add_device(Device) -> DeviceId`, `devices() -> &[Device]`, `device(DeviceId) -> Option<&Device>`, `set_device_reachable(DeviceId, bool)`, `remove_device(DeviceId)`, `forget_device_projects(DeviceId)`, `is_device_collapsed(DeviceId) -> bool`, `toggle_device_collapsed(DeviceId)`.

- [ ] **Step 1: Write the failing test for `Device` itself**

Create `crates/dispatch-core/src/device.rs` with only the tests:

```rust
//! Machines running a Dispatch daemon.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_is_reachable_until_it_is_not() {
        // A device is created from a connection that just answered, so the
        // honest starting point is "reachable".
        let device = Device::new("laptop");

        assert_eq!(device.name, "laptop");
        assert!(device.reachable);
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p dispatch-core device`
Expected: FAIL — `cannot find type Device in this scope` (and `file not found for module device` until `lib.rs` declares it).

- [ ] **Step 3: Write `Device` and declare the module**

In `crates/dispatch-core/src/device.rs`, above the tests:

```rust
use serde::{Deserialize, Serialize};

use crate::id::DeviceId;

/// A machine running a daemon, and whether its connection is up.
///
/// The client mints the id: a daemon names itself in `ServerMessage::Hello`
/// but knows nothing of the other machines a client is holding, so identity
/// across the fleet is the client's to assign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Stable identifier.
    pub id: DeviceId,
    /// What the daemon calls itself.
    pub name: String,
    /// Whether its connection is up. A device that goes quiet keeps its rows:
    /// its agents are still running, and hiding them would say otherwise.
    pub reachable: bool,
}

impl Device {
    /// A device named `name`, assumed reachable.
    ///
    /// Reachable because a device is made from a connection that has just
    /// answered; anything else would have failed before reaching here.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: DeviceId::new(),
            name: name.into(),
            reachable: true,
        }
    }
}
```

In `crates/dispatch-core/src/lib.rs`, add `pub mod device;` next to `pub mod id;` and extend the re-exports:

```rust
pub use device::Device;
```

- [ ] **Step 4: Run the test and watch it pass**

Run: `cargo test -p dispatch-core device`
Expected: PASS.

- [ ] **Step 5: Write the failing tests for devices in `AppState`**

Append to the `mod tests` block at the bottom of `crates/dispatch-core/src/state.rs`:

```rust
    #[test]
    fn a_device_is_registered_and_found_again() {
        let mut state = AppState::new();
        let id = state.add_device(Device::new("laptop"));

        assert_eq!(state.device(id).map(|d| d.name.as_str()), Some("laptop"));
        assert_eq!(state.devices().len(), 1);
    }

    #[test]
    fn registering_a_device_twice_is_one_device() {
        // A reconnect announces the same machine again, and two rows for one
        // daemon would give its projects two places to be drawn.
        let mut state = AppState::new();
        let device = Device::new("laptop");
        let id = device.id;

        state.add_device(device.clone());
        state.add_device(device);

        assert_eq!(state.devices().len(), 1);
        assert_eq!(state.device(id).map(|d| d.id), Some(id));
    }

    #[test]
    fn a_device_that_goes_quiet_is_marked_unreachable() {
        let mut state = AppState::new();
        let id = state.add_device(Device::new("tower"));

        state.set_device_reachable(id, false);

        assert_eq!(state.device(id).map(|d| d.reachable), Some(false));
    }

    #[test]
    fn removing_a_device_takes_its_projects_and_their_panes() {
        // Unlike a project, this is not refused while panes are running: the
        // machine has left the fleet, so there is nothing left to refuse on
        // their behalf.
        let mut state = AppState::new();
        let device = state.add_device(Device::new("tower"));
        let project = state.add_project(
            Project::new("/tmp/one", ProjectSource::LocalDir).with_device(device),
        );
        let pane = state
            .spawn_pane(project, harness("claude"))
            .expect("the project exists");

        state.remove_device(device);

        assert!(state.devices().is_empty());
        assert!(state.projects().is_empty());
        assert!(state.pane(pane).is_none());
    }

    #[test]
    fn a_device_is_expanded_until_it_is_collapsed() {
        let mut state = AppState::new();
        let id = state.add_device(Device::new("laptop"));

        assert!(!state.is_device_collapsed(id));

        state.toggle_device_collapsed(id);
        assert!(state.is_device_collapsed(id));

        state.toggle_device_collapsed(id);
        assert!(!state.is_device_collapsed(id));
    }

    #[test]
    fn a_project_names_the_device_it_is_on() {
        let device = DeviceId::new();
        let project = Project::new("/tmp/one", ProjectSource::LocalDir).with_device(device);

        assert_eq!(project.device, device);
    }
```

Add `Device` to the test module's imports (`use crate::device::Device;` beside the existing `use crate::project::ProjectSource;`).

- [ ] **Step 6: Run them and watch them fail**

Run: `cargo test -p dispatch-core`
Expected: FAIL — `no method named add_device`, `no field device on Project`, `no method with_device`.

- [ ] **Step 7: Add the `device` field to `Project`**

In `crates/dispatch-core/src/project.rs`, inside `pub struct Project`, after `pub id: ProjectId,`:

```rust
    /// Which machine this project is on.
    ///
    /// Skipped on the wire: the daemon sends this very type in
    /// `ServerMessage::ProjectOpened` and knows nothing about the other
    /// machines a client is holding, so the client stamps the device on as it
    /// adopts the project. Default until it does.
    #[serde(skip)]
    pub device: DeviceId,
```

Import `DeviceId` at the top (`use crate::id::{DeviceId, ProjectId};`), set `device: DeviceId::default()` in `Project::new`, and add the builder beside `with_name`:

```rust
    /// Says which machine the project is on.
    #[must_use]
    pub fn with_device(mut self, device: DeviceId) -> Self {
        self.device = device;
        self
    }
```

- [ ] **Step 8: Add devices to `AppState`**

In `crates/dispatch-core/src/state.rs`, add to the struct:

```rust
    /// The machines whose projects are on screen, in the order they answered.
    devices: Vec<Device>,
    /// Devices whose projects the sidebar hides.
    collapsed_devices: HashSet<DeviceId>,
```

and the methods, beside the project ones:

```rust
    /// Registers a machine, or updates one already known by that id.
    pub fn add_device(&mut self, device: Device) -> DeviceId {
        let id = device.id;

        if let Some(existing) = self.devices.iter_mut().find(|d| d.id == id) {
            *existing = device;
            return id;
        }

        self.devices.push(device);
        id
    }

    /// Every machine, in the order they answered.
    #[must_use]
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// Looks up one machine.
    #[must_use]
    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        self.devices.iter().find(|device| device.id == id)
    }

    /// Records whether a machine's connection is up.
    pub fn set_device_reachable(&mut self, id: DeviceId, reachable: bool) {
        if let Some(device) = self.devices.iter_mut().find(|device| device.id == id) {
            device.reachable = reachable;
        }
    }

    /// Forgets everything a machine was showing, keeping the machine itself.
    ///
    /// What a reconnect needs: that daemon's `Subscribe` replay describes its
    /// fleet afresh, and a row kept from the old connection would be a pane
    /// nothing can reach. The device stays so its row does not blink out and
    /// back while it reattaches.
    pub fn forget_device_projects(&mut self, id: DeviceId) {
        let projects: Vec<ProjectId> = self
            .projects
            .iter()
            .filter(|project| project.device == id)
            .map(|project| project.id)
            .collect();

        self.panes.retain(|pane| !projects.contains(&pane.project));
        self.projects.retain(|project| project.device != id);

        for project in projects {
            self.collapsed_projects.remove(&project);
        }

        self.repair_selection();
    }

    /// Forgets a machine, its projects and their panes.
    ///
    /// Not refused while panes are running, unlike [`Self::remove_project`]:
    /// the machine has left the fleet, so there is no daemon left to reach
    /// them through and nothing to refuse on their behalf.
    pub fn remove_device(&mut self, id: DeviceId) {
        self.forget_device_projects(id);
        self.devices.retain(|device| device.id != id);
        self.collapsed_devices.remove(&id);
    }

    /// Points the selection and the focus at something that still exists.
    ///
    /// Shared by every removal: a selection naming a project that has gone is
    /// a sidebar with nothing highlighted and a grid drawing nobody's panes.
    fn repair_selection(&mut self) {
        if self
            .selected_project
            .is_some_and(|selected| !self.projects.iter().any(|p| p.id == selected))
        {
            self.selected_project = self.projects.first().map(|p| p.id);
            self.focused_pane = None;
            self.zoomed_pane = None;
        }

        if self
            .focused_pane
            .is_some_and(|focused| self.pane(focused).is_none())
        {
            self.focused_pane = None;
        }
    }

    /// Whether the sidebar hides `device`'s projects.
    #[must_use]
    pub fn is_device_collapsed(&self, device: DeviceId) -> bool {
        self.collapsed_devices.contains(&device)
    }

    /// Hides `device`'s projects, or shows them again.
    pub fn toggle_device_collapsed(&mut self, device: DeviceId) {
        if !self.collapsed_devices.remove(&device) {
            self.collapsed_devices.insert(device);
        }
    }
```

Import `Device` and `DeviceId` at the top of the file.

- [ ] **Step 9: Run the tests and watch them pass**

Run: `cargo test -p dispatch-core`
Expected: PASS, all of them.

- [ ] **Step 10: Rewrite the stale `DeviceId` doc**

In `crates/dispatch-core/src/id.rs`, replace the "Unused in Slice 1" paragraph:

```rust
id_type! {
    /// Identifies a machine running a Dispatch daemon.
    ///
    /// Minted by the client rather than the daemon: a daemon names itself but
    /// knows nothing of the other machines a client is holding, so identity
    /// across a fleet is the client's to assign.
    DeviceId
}
```

- [ ] **Step 11: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add crates/dispatch-core
git commit -m "feat(core): give a project the machine it is on

$(printf 'A fleet is several daemons, and the things a user points at --\nfocus, selection, zoom -- are one per screen rather than one per\nmachine. So the device lives beside the projects and panes rather\nthan in a second AppState per machine.\n\nThe field is skipped on the wire: the daemon sends Project itself and\nknows nothing of the other machines a client holds, so the client\nstamps the device on as it adopts one.\n\nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>')"
```

---

### Task 2: The sidebar's device level

**Files:**
- Modify: `crates/dispatch-tui/src/sidebar.rs`
- Modify: `crates/dispatch-tui/src/sidebar/tests.rs`

**Interfaces:**
- Consumes: `AppState::devices`, `is_device_collapsed`, `toggle_device_collapsed`, `Project::device` (Task 1).
- Produces: `Row::Device(DeviceId)` (private), `Hit::Device(DeviceId)` (public), `pub const MACHINE: &str`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-tui/src/sidebar/tests.rs`:

```rust
/// State with two devices, each holding one project.
fn fleet() -> (AppState, DeviceId, DeviceId) {
    let mut state = AppState::new();
    let laptop = state.add_device(Device::new("laptop"));
    let tower = state.add_device(Device::new("tower"));

    state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir).with_device(laptop));
    state.add_project(Project::new("/tmp/beta", ProjectSource::LocalDir).with_device(tower));

    (state, laptop, tower)
}

#[test]
fn one_machine_draws_no_device_row() {
    // The ordinary case. A lone row naming this machine costs a line and
    // indents everything under it to say what the user already knows.
    let mut state = AppState::new();
    let laptop = state.add_device(Device::new("laptop"));
    state.add_project(Project::new("/tmp/alpha", ProjectSource::LocalDir).with_device(laptop));

    let lines = render_lines(&state, WIDTH, 8);

    assert!(
        lines[TOP as usize].contains("alpha"),
        "the project is the first row: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("laptop")),
        "and the machine is not drawn at all: {lines:#?}"
    );
}

#[test]
fn several_machines_each_get_a_row_above_their_projects() {
    let (state, _, _) = fleet();
    let lines = render_lines(&state, WIDTH, 10);

    let laptop = lines
        .iter()
        .position(|line| line.contains("laptop"))
        .expect("the first machine has a row");
    let alpha = lines
        .iter()
        .position(|line| line.contains("alpha"))
        .expect("its project is listed");
    let tower = lines
        .iter()
        .position(|line| line.contains("tower"))
        .expect("the second machine has a row");

    assert!(laptop < alpha && alpha < tower, "{lines:#?}");
    assert!(
        column_of(&lines[alpha], "alpha") > column_of(&lines[laptop], "laptop"),
        "a project is indented under its machine: {lines:#?}"
    );
}

#[test]
fn a_collapsed_device_hides_its_projects() {
    let (mut state, laptop, _) = fleet();

    state.toggle_device_collapsed(laptop);
    let text = render_lines(&state, WIDTH, 10).join("\n");

    assert!(!text.contains("alpha"), "{text}");
    assert!(text.contains("laptop"), "the machine stays: {text}");
    assert!(text.contains("beta"), "the other machine is unaffected: {text}");
}

#[test]
fn an_unreachable_device_says_so() {
    let (mut state, _, tower) = fleet();

    state.set_device_reachable(tower, false);
    let lines = render_lines(&state, WIDTH, 10);
    let row = lines
        .iter()
        .find(|line| line.contains("tower"))
        .expect("the machine has a row");

    assert!(row.contains("unreachable"), "{row:?}");
}

#[test]
fn a_click_on_a_device_row_finds_the_device() {
    let (state, laptop, _) = fleet();
    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit_test(&state, area, LEFT, TOP), Some(Hit::Device(laptop)));
}
```

Add `Device` and `DeviceId` to the test imports.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p dispatch-tui sidebar`
Expected: FAIL — `no variant Device on Hit`, and the device rows are absent from the rendered text.

- [ ] **Step 3: Add the device level to `rows`, the render and `hit_test`**

In `crates/dispatch-tui/src/sidebar.rs`:

```rust
/// The mark on a machine's row.
pub const MACHINE: &str = "\u{f109}";

/// What a machine's row says when its connection is down.
const UNREACHABLE: &str = "unreachable";
```

Extend the row enum and the walk:

```rust
enum Row {
    /// A machine, when there is more than one.
    Device(DeviceId),
    /// A project heading.
    Project(ProjectId, u16),
    /// A pane, drawn `indent` columns in from the list's edge.
    Pane(PaneId, u16),
}

fn rows(state: &AppState) -> Vec<Row> {
    let mut rows = Vec::new();

    // One machine draws no machine row: it would say what the user already
    // knows and indent everything under it to say it.
    let federated = state.devices().len() > 1;
    let step = if federated { 2 } else { 0 };

    let devices: Vec<Option<DeviceId>> = if federated {
        state.devices().iter().map(|d| Some(d.id)).collect()
    } else {
        vec![None]
    };

    for device in devices {
        if let Some(device) = device {
            rows.push(Row::Device(device));

            if state.is_device_collapsed(device) {
                continue;
            }
        }

        for project in state.projects() {
            if device.is_some_and(|device| project.device != device) {
                continue;
            }

            rows.push(Row::Project(project.id, step));

            if state.is_project_collapsed(project.id) {
                continue;
            }

            for pane in state.panes_for(project.id) {
                if pane.parent.is_some() {
                    continue;
                }

                rows.push(Row::Pane(pane.id, step + 2));

                if state.is_pane_collapsed(pane.id) {
                    continue;
                }

                for child in state.children_of(pane.id) {
                    rows.push(Row::Pane(child.id, step + 4));
                }
            }
        }
    }

    rows
}
```

`render` gains the device arm, and `render_project` takes the indent it is now given:

```rust
            match row {
                Row::Device(id) => self.render_device(buf, area, y, id),
                Row::Project(id, indent) => self.render_project(buf, area, y, id, indent, selected),
                Row::Pane(id, indent) => { /* unchanged */ }
            }
```

```rust
    /// Draws one machine's row.
    ///
    /// Dim and labelled when its connection is down: its agents are still
    /// running, so the row stays, but a row that looks live while nothing can
    /// reach it is worse than no row.
    fn render_device(&self, buf: &mut Buffer, area: Rect, y: u16, id: DeviceId) {
        let Some(device) = self.state.device(id) else {
            return;
        };

        let style = if device.reachable {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let has_projects = self
            .state
            .projects()
            .iter()
            .any(|project| project.device == id);

        write(
            buf,
            area,
            area.x,
            y,
            twisty(has_projects, self.state.is_device_collapsed(id)),
            style,
        );
        write(buf, area, area.x + 1, y, MACHINE, style);

        let name = if device.reachable {
            device.name.clone()
        } else {
            format!("{} — {UNREACHABLE}", device.name)
        };
        let room = (area.x + area.width).saturating_sub(area.x + NAME) as usize;
        write(buf, area, area.x + NAME, y, &truncate(&name, room), style);
    }
```

`Hit` gains its variant and `hit_test` its arm:

```rust
pub enum Hit {
    /// A machine's row. Folding is all there is to do on one.
    Device(DeviceId),
    Project(ProjectId),
    Twisty(PaneId),
    Pane(PaneId),
}
```

```rust
    match *rows(state).get(index)? {
        Row::Device(id) => Some(Hit::Device(id)),
        Row::Project(id, _) => Some(Hit::Project(id)),
        Row::Pane(id, indent) => { /* unchanged */ }
    }
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p dispatch-tui`
Expected: PASS. Existing sidebar tests still pass because a single device draws no device row and the indents fall back to today's.

- [ ] **Step 5: Teach `dispatch` about the new `Hit` variant**

`dispatch/src/app.rs` matches on `Hit`; add the arm beside the project one:

```rust
                sidebar::Hit::Device(id) => self.state.toggle_device_collapsed(id),
```

- [ ] **Step 6: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add crates/dispatch-tui dispatch/src/app.rs
git commit -m "feat(tui): draw a row per machine above its projects

$(printf 'Drawn only past one machine: a lone row naming this one costs a line\nand indents every row under it to say what the user already knows.\nAn unreachable machine keeps its rows, dimmed and labelled -- its\nagents are still running, and hiding them would say otherwise.\n\nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>')"
```

---

### Task 3: Attach to a named endpoint

**Files:**
- Modify: `crates/dispatch-client/src/lib.rs`
- Modify: `crates/dispatch-client/src/tests.rs`

**Interfaces:**
- Produces: `Client::attach_at(role: Role, name: &str, liveness: Liveness, endpoint: PathBuf) -> Result<Client, ClientError>`, with `attach_with_as` delegating to it.

- [ ] **Step 1: Write the failing test**

In `crates/dispatch-client/src/tests.rs`, following the file's existing harness for standing a daemon up on a temporary endpoint:

```rust
#[test]
fn a_client_attaches_to_an_endpoint_it_is_given() {
    // Federation dials one socket per machine, so the endpoint cannot come
    // from this process's own configuration.
    let daemon = TestDaemon::start("attach-at");

    let client = Client::attach_at(
        Role::Interface,
        "test",
        Liveness::default(),
        daemon.endpoint(),
    )
    .expect("the daemon is listening");

    assert!(client.is_connected());
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p dispatch-client attaches_to_an_endpoint`
Expected: FAIL — `no function or associated item named attach_at`.

- [ ] **Step 3: Add `attach_at` and route the existing constructors through it**

In `crates/dispatch-client/src/lib.rs`, change `attach_with_as` to resolve the endpoint and delegate:

```rust
    pub fn attach_with_as(role: Role, name: &str, liveness: Liveness) -> Result<Self, ClientError> {
        let endpoint = dispatch_os::ipc::endpoint()?;
        Self::attach_at(role, name, liveness, endpoint)
    }

    /// Connects to a daemon listening on `endpoint` rather than this
    /// configuration's own.
    ///
    /// Federation holds one connection per machine, so the endpoint is the
    /// caller's to name: a client attaching to three daemons cannot take all
    /// three from one configuration directory.
    pub fn attach_at(
        role: Role,
        name: &str,
        liveness: Liveness,
        endpoint: PathBuf,
    ) -> Result<Self, ClientError> {
        let (reader, writer, device) = connect_within(name, role, &endpoint, HANDSHAKE_TIMEOUT)?;
        // Move the rest of `attach_with_as`'s body here verbatim, from
        // `let wire = Arc::new(Wire { … })` through the `Ok(Self { … })`. It
        // does not change: the only thing that moved is where the endpoint
        // came from.
    }
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p dispatch-client`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add crates/dispatch-client
git commit -m "feat(client): attach to an endpoint the caller names

$(printf 'A client holding three daemons cannot take all three endpoints from\none configuration directory, so the endpoint becomes an argument.\nThe existing constructors resolve it the way they always did and\ndelegate.\n\nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>')"
```

---

### Task 4: The client holds many daemons

**Files:**
- Modify: `dispatch/src/app.rs`

**Interfaces:**
- Consumes: Task 1's `AppState` device API, Task 3's `Client::attach_at`.
- Produces: `struct Attachment { device: DeviceId, client: Client, generation: u64, opened: Vec<PathBuf> }`; `Mode::Attached(Vec<Attachment>)`; `App::attach(client: Client)` (adds one attachment, replacing `App::attached`); `App::attachment_for_pane(PaneId) -> Option<&Attachment>`; `App::attachment_for_project(ProjectId) -> Option<&Attachment>`.

- [ ] **Step 1: Write the failing tests**

In the `mod tests` block of `dispatch/src/app.rs`:

```rust
    /// Two attached daemons, each with a project of its own.
    fn two_daemons() -> (App, ProjectId, ProjectId, Receiver<ClientMessage>, Receiver<ClientMessage>) {
        let (first, first_daemon, first_sent) = Client::for_test();
        let (second, second_daemon, second_sent) = Client::for_test();

        let mut app = App::new(HarnessRegistry::default());
        app.attach(first);
        app.attach(second);

        let alpha = Project::new("/tmp/alpha", ProjectSource::LocalDir);
        let beta = Project::new("/tmp/beta", ProjectSource::LocalDir);
        let (alpha_id, beta_id) = (alpha.id, beta.id);

        first_daemon
            .send(ServerMessage::ProjectOpened { project: alpha })
            .expect("the app is listening");
        second_daemon
            .send(ServerMessage::ProjectOpened { project: beta })
            .expect("the app is listening");
        app.poll_daemon();

        (app, alpha_id, beta_id, first_sent, second_sent)
    }

    #[test]
    fn each_daemons_projects_are_attributed_to_its_own_device() {
        let (app, alpha, beta, _, _) = two_daemons();

        let device_of = |id: ProjectId| {
            app.state
                .projects()
                .iter()
                .find(|project| project.id == id)
                .map(|project| project.device)
        };

        assert_eq!(app.state.devices().len(), 2);
        assert_ne!(device_of(alpha), device_of(beta));
    }

    #[test]
    fn a_keystroke_reaches_the_daemon_the_pane_is_on() {
        // The whole point of the slice: one screen, two machines, and no
        // chance of typing into the wrong one.
        let (mut app, _alpha, beta, first_sent, second_sent) = two_daemons();
        let pane = PaneId::new();
        // The pane belongs to the second daemon's project.
        app.apply(spawned_message(pane, beta, "shell"));
        let _ = app.state.focus(pane);

        press(&mut app, KeyCode::Char('x'));

        assert!(
            std::iter::from_fn(|| second_sent.try_recv().ok())
                .any(|m| matches!(m, ClientMessage::WritePane { pane: p, .. } if p == pane)),
            "the daemon that owns the pane hears it"
        );
        assert!(
            !std::iter::from_fn(|| first_sent.try_recv().ok())
                .any(|m| matches!(m, ClientMessage::WritePane { .. })),
            "and the other one hears nothing"
        );
    }

    #[test]
    fn one_daemon_restarting_rebuilds_only_its_own_rows() {
        let (mut app, alpha, beta, _, _) = two_daemons();
        let device = app
            .state
            .projects()
            .iter()
            .find(|project| project.id == alpha)
            .map(|project| project.device)
            .expect("the project is on a machine");

        app.state.forget_device_projects(device);

        assert!(
            !app.state.projects().iter().any(|p| p.id == alpha),
            "that machine's project is gone"
        );
        assert!(
            app.state.projects().iter().any(|p| p.id == beta),
            "and the other machine's is not"
        );
        assert_eq!(app.state.devices().len(), 2, "both machines keep their rows");
    }

    #[test]
    fn a_standalone_client_is_a_machine_too() {
        // One code path rather than "device or not" at every use.
        let app = App::new(HarnessRegistry::default());

        assert_eq!(app.state.devices().len(), 1);
    }

    #[test]
    fn a_keystroke_for_an_unreachable_machine_is_refused_out_loud() {
        let (mut app, _alpha, beta, _, second_sent) = two_daemons();
        let pane = PaneId::new();
        app.apply(spawned_message(pane, beta, "shell"));
        let _ = app.state.focus(pane);

        let device = app
            .state
            .pane(pane)
            .and_then(|pane| {
                app.state
                    .projects()
                    .iter()
                    .find(|project| project.id == pane.project)
            })
            .map(|project| project.device)
            .expect("the pane is on a machine");
        app.state.set_device_reachable(device, false);

        press(&mut app, KeyCode::Char('x'));

        assert!(
            !std::iter::from_fn(|| second_sent.try_recv().ok())
                .any(|m| matches!(m, ClientMessage::WritePane { .. })),
            "nothing is sent into the void"
        );
        assert!(
            app.status.contains("unreachable"),
            "and the user is told: {:?}",
            app.status
        );
    }
```

Add a `spawned_message(pane, project, harness)` helper beside the existing `spawned` one if the existing signature does not fit.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p dispatch --bin dispatch daemons`
Expected: FAIL — `no method named attach`, `Mode::Attached` takes one `Client`.

- [ ] **Step 3: Give a standalone client its own device**

`App::with_mode` registers one, so every project has a machine whether or not
a daemon is involved:

```rust
        let mut state = AppState::new();

        // Standalone runs its agents itself, but it is still a machine: one
        // code path beats asking "device or not" at every use.
        if matches!(mode, Mode::Standalone) {
            state.add_device(Device::new(this_machine()));
        }
```

```rust
/// What to call the machine Dispatch is running on.
///
/// The hostname, because a fleet of rows all saying "local" names nothing.
fn this_machine() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this machine".to_string())
}
```

- [ ] **Step 4: Reshape `Mode` and add the routing helpers**

```rust
/// One daemon this client is holding.
struct Attachment {
    /// The machine it is, as the sidebar names it.
    device: DeviceId,
    client: Client,
    /// That connection's generation. Per attachment, because one daemon
    /// restarting says nothing about the others.
    generation: u64,
    /// Roots asked of this daemon, so its own reconnect can ask again.
    opened: Vec<PathBuf>,
}

enum Mode {
    /// The agents are this process's children.
    Standalone,
    /// The agents belong to daemons — one per machine.
    Attached(Vec<Attachment>),
}
```

```rust
    /// Adds a daemon to the fleet, registering the machine it names itself as.
    pub fn attach(&mut self, client: Client) {
        let device = self.state.add_device(Device::new(client.device()));
        let generation = client.generation();

        let attachment = Attachment {
            device,
            client,
            generation,
            opened: Vec::new(),
        };

        match &mut self.mode {
            Mode::Attached(attachments) => attachments.push(attachment),
            Mode::Standalone => self.mode = Mode::Attached(vec![attachment]),
        }
    }

    /// The daemon a pane is on.
    fn attachment_for_pane(&self, pane: PaneId) -> Option<&Attachment> {
        let project = self.state.pane(pane)?.project;
        self.attachment_for_project(project)
    }

    /// The daemon a project is on.
    fn attachment_for_project(&self, project: ProjectId) -> Option<&Attachment> {
        let device = self
            .state
            .projects()
            .iter()
            .find(|candidate| candidate.id == project)?
            .device;

        match &self.mode {
            Mode::Attached(attachments) => attachments.iter().find(|a| a.device == device),
            Mode::Standalone => None,
        }
    }
```

Every existing `let Mode::Attached(client) = &self.mode else { … }` becomes a lookup through one of those two, refusing with a status line when the device is unreachable:

```rust
    /// The daemon a pane is on, or `None` with the reason said out loud.
    fn reachable_for_pane(&mut self, pane: PaneId) -> Option<&Attachment> {
        let attachment = self.attachment_for_pane(pane)?;
        let device = attachment.device;

        if self.state.device(device).is_some_and(|d| d.reachable) {
            return self.attachment_for_pane(pane);
        }

        let name = self
            .state
            .device(device)
            .map_or_else(|| "that machine".to_string(), |d| d.name.clone());
        self.status = format!("{name} is unreachable");
        None
    }
```

- [ ] **Step 5: Make polling per-attachment**

```rust
    pub fn poll_daemon(&mut self) -> bool {
        let Mode::Attached(attachments) = &self.mode else {
            return false;
        };

        // Collected first: applying a message borrows `self` mutably, and the
        // attachments are borrowed from it.
        let snapshot: Vec<(DeviceId, u64, bool, String, Vec<ServerMessage>)> = attachments
            .iter()
            .map(|attachment| {
                (
                    attachment.device,
                    attachment.client.generation(),
                    attachment.client.is_connected(),
                    attachment.client.device(),
                    attachment.client.poll(),
                )
            })
            .collect();

        let mut changed = false;

        for (device, generation, connected, name, messages) in snapshot {
            changed |= self.sync_attachment(device, generation, connected, &name);

            for message in messages {
                changed |= self.apply_from(device, message);
            }
        }

        changed
    }
```

```rust
    /// Brings one attachment's device up to date, and rebuilds its rows when
    /// the connection behind them has been replaced.
    ///
    /// Returns whether anything changed on screen.
    fn sync_attachment(
        &mut self,
        device: DeviceId,
        generation: u64,
        connected: bool,
        name: &str,
    ) -> bool {
        let was = self.state.device(device).is_some_and(|d| d.reachable);
        self.state.set_device_reachable(device, connected);
        let mut changed = was != connected;

        let Mode::Attached(attachments) = &mut self.mode else {
            return changed;
        };
        let Some(attachment) = attachments.iter_mut().find(|a| a.device == device) else {
            return changed;
        };

        if attachment.generation == generation {
            return changed;
        }

        // Everything this machine was showing was described by a connection
        // that is gone. Its `Subscribe` replay describes the fleet afresh, so
        // the rows are rebuilt from what it says rather than patched.
        attachment.generation = generation;
        let roots = attachment.opened.clone();
        let client = &attachment.client;

        for root in &roots {
            client.send(ClientMessage::OpenProject { root: root.clone() });
        }

        self.state.forget_device_projects(device);
        self.status = format!("reattached to {name}");
        changed = true;

        changed
    }
```

`apply_from(device, message)` is today's `apply`, with `ServerMessage::ProjectOpened` stamping the device:

```rust
            ServerMessage::ProjectOpened { project } => {
                self.state.add_project(project.with_device(device));
                true
            }
```

- [ ] **Step 6: Refuse the verbs an unreachable machine cannot serve**

Every send routes through `reachable_for_pane` (keystrokes, paste, mouse,
resize, close) or its project-shaped twin for spawning:

```rust
    /// The daemon a project is on, or `None` with the reason said out loud.
    fn reachable_for_project(&mut self, project: ProjectId) -> Option<&Attachment> {
        let device = self.attachment_for_project(project)?.device;

        if self.state.device(device).is_some_and(|d| d.reachable) {
            return self.attachment_for_project(project);
        }

        let name = self
            .state
            .device(device)
            .map_or_else(|| "that machine".to_string(), |d| d.name.clone());
        self.status = format!("{name} is unreachable");
        None
    }
```

Reading is untouched: a pane keeps the output it has and scrollback still
works, because neither asks the daemon anything.

- [ ] **Step 7: Say which machines are out of reach in the status line**

`draw_status` says "waiting for the daemon" today, which names nothing on a
fleet. Replace the `disconnected` snapshot with the machines themselves:

```rust
        // Asked now, not remembered: a connection can come back on another
        // thread at any moment, and a notice that outlives the disconnection
        // says the agents are unreachable when they are not.
        let unreachable: Vec<String> = self
            .state
            .devices()
            .iter()
            .filter(|device| !device.reachable)
            .map(|device| device.name.clone())
            .collect();
```

and the text it produces:

```rust
            let base = if !unreachable.is_empty() {
                format!(
                    "waiting for {} — its agents are still running",
                    unreachable.join(", ")
                )
            } else {
```

- [ ] **Step 8: Run the tests and watch them pass**

Run: `cargo test -p dispatch --bin dispatch`
Expected: PASS, including every test that used a single attachment: `App::attached(harnesses, client)` keeps working by calling `attach` once.

- [ ] **Step 9: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add dispatch/src/app.rs
git commit -m "feat(dispatch): hold a connection per machine

$(printf 'Mode::Attached grows from one client to one per machine, each with\nits own generation and its own roots to replay. A write routes pane\nto project to device to connection, so two machines cannot be typed\ninto by accident, and one daemon restarting rebuilds only its own\nrows.\n\nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>')"
```

---

### Task 5: `--daemon`, and two daemons end to end

**Files:**
- Modify: `dispatch/src/main.rs`
- Modify: `dispatch/tests/end_to_end.rs`

**Interfaces:**
- Consumes: `App::attach` (Task 4), `Client::attach_at` (Task 3).

- [ ] **Step 1: Write the failing end-to-end test**

In `dispatch/tests/end_to_end.rs`:

```rust
#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn two_daemons_share_one_sidebar() {
    // The slice in one test: two machines, one screen, and one of them going
    // down without taking the other with it.
    let here = Fixture::new("fed-a");
    let there = Fixture::new("fed-b");

    let first = Daemon::start(&here);
    let second = Daemon::start(&there);

    let mut app = Harness::spawn(
        &here,
        Size::new(120, 30),
        &[
            "--attach".to_string(),
            "--daemon".to_string(),
            there.config.path().join("dispatchd.sock").display().to_string(),
        ],
    );

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "fed-a") && sidebar_contains(lines, "fed-b")),
        "both machines are listed"
    );

    second.stop();

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "unreachable")),
        "the machine that went down says so"
    );
    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "fed-a")),
        "and the other one is still there"
    );

    first.stop();
}
```

`dispatchd` already takes `--device <name>`, so `Daemon::start` gains a name:
give it `Daemon::start_named(&fixture, "fed-a")`, passing `--device` through to
the spawned binary, and keep `Daemon::start` as `start_named(fixture, "local")`
so the existing daemon tests are untouched.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --test end_to_end two_daemons_share_one_sidebar`
Expected: FAIL — `unexpected argument '--daemon'`.

- [ ] **Step 3: Name a daemon after its machine by default**

`dispatchd`'s `--device` defaults to `"local"`, which names nothing once there
are several. In `dispatchd/src/main.rs`:

```rust
    /// Name this daemon reports to clients, so two can be told apart.
    ///
    /// The hostname by default: a fleet whose rows all say "local" is a fleet
    /// you cannot read.
    #[arg(long, default_value_t = default_device_name())]
    device: String,
```

```rust
/// The hostname, or a plain fallback when the environment does not say.
fn default_device_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "local".to_string())
}
```

- [ ] **Step 4: Add the flag and attach to each endpoint**

In `dispatch/src/main.rs`, on the `Cli` struct:

```rust
    /// Also attach to a daemon listening on this endpoint. Repeatable.
    ///
    /// The plumbing federation is built on: `machine add` will fill these in
    /// from the machine list once it can reach another host.
    #[arg(long = "daemon", value_name = "ENDPOINT")]
    daemons: Vec<PathBuf>,
```

and after the existing attach:

```rust
    for endpoint in &args.daemons {
        match Client::attach_at(
            Role::Interface,
            CLIENT_NAME,
            Liveness::default(),
            endpoint.clone(),
        ) {
            Ok(client) => {
                client.subscribe();
                app.attach(client);
            }
            // One machine being down is not a reason to refuse to start: the
            // others are the reason the user opened Dispatch.
            Err(error) => tracing::warn!(%error, endpoint = %endpoint.display(), "could not attach"),
        }
    }
```

- [ ] **Step 5: Run the test and watch it pass**

Run: `cargo test --test end_to_end two_daemons_share_one_sidebar`
Expected: PASS.

- [ ] **Step 6: Prove the test can fail**

Temporarily make `sync_attachment` skip `set_device_reachable`, run the test, and confirm it fails on the "unreachable" assertion. Put it back.

- [ ] **Step 7: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add dispatch/src/main.rs dispatch/tests/end_to_end.rs
git commit -m "feat(dispatch): attach to more than one daemon

$(printf 'A repeatable --daemon names another endpoint to hold alongside this\nconfiguration own. It is the plumbing machine add will drive once\nfederation can reach another host, and it is what makes the slice\ntestable with two local daemons and no network.\n\nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>')"
```

---

### Task 6: The fold ladder and the documentation

**Files:**
- Modify: `dispatch/src/app.rs` (`toggle_fold`)
- Modify: `crates/dispatch-tui/src/sidebar.rs` (module doc)
- Modify: `README.md`

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn folding_past_a_folded_project_folds_its_machine() {
        // The ladder: subagents, then the project, then the machine it is on.
        let mut app = App::new(HarnessRegistry::default());
        let device = app.state.add_device(Device::new("laptop"));
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir).with_device(device));
        let pane = app
            .state
            .spawn_pane(project, HarnessId::new("shell"))
            .expect("the project exists");
        let _ = app.state.focus(pane);

        command(&mut app, 'f');
        assert!(app.state.is_project_collapsed(project));

        command(&mut app, 'f');
        assert!(app.state.is_device_collapsed(device), "and then the machine");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p dispatch --bin dispatch folding_past`
Expected: FAIL — the second `^a f` unfolds the project instead.

- [ ] **Step 3: Add the device rung to `toggle_fold`**

```rust
            if let Some(project) = self.state.pane(pane).map(|pane| pane.project) {
                // The ladder: a pane's subagents, then its project, then the
                // machine that project is on. Each rung is reached by folding
                // the one below it first.
                if !self.state.is_project_collapsed(project) {
                    self.state.toggle_project_collapsed(project);
                    return;
                }

                if let Some(device) = self
                    .state
                    .projects()
                    .iter()
                    .find(|candidate| candidate.id == project)
                    .map(|candidate| candidate.device)
                {
                    self.state.toggle_device_collapsed(device);
                    return;
                }
            }
```

- [ ] **Step 4: Run the test and watch it pass**

Run: `cargo test -p dispatch --bin dispatch`
Expected: PASS.

- [ ] **Step 5: Rewrite the stale sidebar module doc**

`crates/dispatch-tui/src/sidebar.rs` opens by describing a reserved federation column that no longer exists. Replace it:

```rust
//! The project sidebar.
//!
//! A tree: machines, the projects on each, and the panes in each project with
//! their subagents beneath them. The machine level is drawn only when there is
//! more than one, because a lone row naming this machine costs a line and
//! indents everything under it to say what the user already knows.
```

- [ ] **Step 6: Document federation in the README**

Under "Keeping projects", add:

```markdown
## More than one machine

Each machine runs its own `dispatchd`, and one client can hold several of them:
`--daemon <endpoint>`, repeatable, attaches to another alongside the usual one.
Every project is drawn under the machine it is on, and a machine whose daemon
goes down keeps its rows -- dimmed and labelled `unreachable` -- because its
agents are still running. Keystrokes aimed at an unreachable machine are
refused rather than swallowed.

Reaching a machine over SSH, and `dispatch machine add`, are the next slice.
```

- [ ] **Step 7: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add dispatch/src/app.rs crates/dispatch-tui/src/sidebar.rs README.md
git commit -m "feat(dispatch): fold a machine, and say what the sidebar is now

$(printf 'The fold ladder gains its top rung: subagents, then the project,\nthen the machine. The sidebar module doc still described a reserved\nfederation column that became the git mark two commits ago.\n\nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>')"
```
