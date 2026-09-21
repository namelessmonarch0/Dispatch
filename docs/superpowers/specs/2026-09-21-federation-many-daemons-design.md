# F1 — Many daemons in one client

Status: approved design, not yet implemented.
Date: 2026-09-21.

## The problem

A Dispatch client talks to exactly one daemon. `Mode::Attached(Client)` holds
one connection, `App::generation` tracks one connection's lifetime, and every
project on screen came from it. A user with a laptop and a desktop runs two
Dispatches and reads two sidebars.

The goal is one sidebar: every machine's projects and panes in the tree, driven
the same way whichever machine they are on.

This is the first of three slices. It stops where the network starts.

### Not in this slice

- **SSH, `machine add`, `machines.toml`** — F2. This slice dials a socket path,
  which is what F2 will hand it once the far end is reachable.
- **Delegation across machines** — F3. A subagent is spawned by the daemon that
  owns its parent, and nothing here changes that.
- **Moving a project between machines.** A project belongs to the machine its
  directory is on.

## The model

A device is a machine running a daemon. It lives in `dispatch-core` beside the
projects and panes, because the things a user points at — focus, selection,
zoom — are one per screen, not one per machine. Keeping a whole `AppState` per
device would mean a "which state" beside every one of them.

```rust
// dispatch-core::id
pub struct DeviceId(Uuid);

// dispatch-core::device
pub struct Device {
    pub id: DeviceId,
    /// What the daemon calls itself, as `ServerMessage::Hello` reports it.
    pub name: String,
    /// Whether its connection is up. A device that goes quiet keeps its rows.
    pub reachable: bool,
}

// dispatch-core::project
pub struct Project {
    pub id: ProjectId,
    pub device: DeviceId,   // new
    pub name: String,
    pub root: PathBuf,
    pub source: ProjectSource,
}
```

`AppState` gains `devices: Vec<Device>` and `collapsed_devices: HashSet<DeviceId>`,
plus:

- `add_device(Device) -> DeviceId`, updating in place when the id is known, the
  way `add_project` already does.
- `device(DeviceId) -> Option<&Device>`, `devices() -> &[Device]`.
- `set_device_reachable(DeviceId, bool)`.
- `remove_device(DeviceId)` — removes the device, its projects and their panes.
  Unlike `remove_project` this is not refused while panes exist: the panes are
  on a machine that is gone from the fleet, so there is nothing left to refuse
  on their behalf.
- `is_device_collapsed` / `toggle_device_collapsed`, mirroring the project pair.

A pane names its project and a project names its device, so nothing else has to
carry one.

**The protocol does not change in this slice.** A daemon already names itself
in `ServerMessage::Hello { device }`; the client mints the `DeviceId`, stamps it
on every project that connection announces, and the daemon never learns of it.
Two daemons cannot collide: `ProjectId` and `PaneId` are UUIDs minted per
daemon, which is what they were reserved for.

**Standalone is a device too.** A client running its own agents synthesises one
device named after the host, so there is one code path rather than "device or
not" at every use. Its `reachable` is always true.

## The client

```rust
struct Attachment {
    device: DeviceId,
    client: Client,
    /// That connection's generation, tracked per attachment rather than per App.
    generation: u64,
    /// Roots this client asked that daemon for, so its reconnect can ask again.
    opened: Vec<PathBuf>,
}

enum Mode {
    Standalone,
    Attached(Vec<Attachment>),
}
```

**Routing.** A write names a pane; the pane names a project; the project names a
device; the device names the attachment. One helper, `fn attachment(&self,
pane: PaneId) -> Option<&Attachment>`, replaces the `let Mode::Attached(client)
= &self.mode else` at each of the eleven call sites that send today.

**Spawning** routes by project rather than by pane, since there is no pane yet.

**Polling.** `poll_daemon` walks the attachments. A generation bump on one
attachment forgets that device's projects and panes and replays its `opened`
roots at it; the other devices are untouched. `forget_the_fleet` becomes
`forget_device(DeviceId)`, which is the same operation the multi-device case
needed anyway.

**Disconnection.** `Client::is_connected` per attachment sets
`Device::reachable`. An unreachable device keeps its rows so its agents stay
visible; a keystroke aimed at one of its panes is dropped with a status line
naming the machine, because the alternative is typing into a void and believing
an agent received it. The status line says which machines are out of reach, not
"the daemon".

**Attaching to more than one.** `--attach` gains a repeatable companion,
`--daemon <endpoint>`, naming a socket to attach to alongside the usual one.
It is the plumbing F2's `machine add` will drive, and it is what makes this
slice testable with two local daemons and no network.

## The sidebar

Rows become device → project → pane → subagent:

```
 laptop
   Dispatch
      Claude Code
       write the tests
 tower
   render-farm
      codex
```

- A device row carries a twisty, a machine glyph (`U+F109`) and the daemon's
  name. Unreachable devices are drawn dim with their name followed by
  `unreachable`.
- **One device draws no device row.** A single machine is the ordinary case and
  a lone row saying "laptop" costs a line and indents everything under it for
  no information. `rows` emits device rows only when `devices().len() > 1`, and
  the indents shift with it.
- Indents: with device rows, a project sits 2 columns in, a pane 4, a subagent
  6. Without them, 0, 2 and 4 as today.
- `Hit::Device(DeviceId)` — a click on a device row folds it, the way a project
  row folds. There is nothing on it to focus.
- `^a f` folds the device when the focused pane's project is already folded,
  completing the ladder it already walks (pane → project → device).

The module doc in `sidebar.rs` still describes a reserved status column for
"the federation slice". That column became the git mark; this slice rewrites
the paragraph rather than leaving it describing a dot that no longer exists.

## Failure cases

| Case | Behaviour |
|---|---|
| A daemon never answers on startup | Its device is not added; a status line names the endpoint. The other attachments carry on. |
| A daemon dies mid-session | `reachable = false`; rows stay; writes to its panes refused with a named status line; the client's existing reconnect loop keeps trying. |
| It comes back with a new generation | That device's rows are rebuilt from its `Subscribe` replay; other devices untouched. |
| Two daemons announce the same device name | Both rows appear under that name. Names are cosmetic; the `DeviceId` the client minted is what routes. |
| The last device is removed | The sidebar is empty and the status line says so, as it does today with no projects. |

## Testing

- **`dispatch-core`** — device CRUD, `remove_device` cascading to projects and
  panes, collapse, and a project always naming a device.
- **`dispatch-tui`** — device rows drawn only past one device, indents under
  both cases, unreachable styling, `hit_test` returning `Hit::Device`, and the
  fold ladder.
- **`dispatch`** — two attachments: a write reaches the right client and not the
  other; one generation bump forgets one device's rows only; an unreachable
  device refuses keystrokes with a named status.
- **End to end** — two `dispatchd` processes on two config directories, one TUI
  attached to both with `--daemon`, a pane spawned on each, and then one daemon
  killed: its rows go dim, the other's pane still echoes what is typed at it.
  This is the test the slice exists to pass.

## Follow-ups this slice creates

- F2 dials `ssh <target> dispatchd --stdio` and hands the result to the same
  `Attachment`.
- A device with no projects has nothing under it; whether `machine add` should
  open a project immediately is F2's question.
