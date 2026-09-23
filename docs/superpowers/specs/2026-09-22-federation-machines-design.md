# F2b — Machines: a registry, SSH, and a supervisor

Status: implemented.
Date: 2026-09-22.
Follows: `docs/superpowers/specs/2026-09-22-federation-stdio-transport-design.md` (F2a, merged).
Context: `docs/superpowers/federation-handoff.md`.

## The problem

F2a made a machine reachable over any command that pipes bytes, so
`dispatch --attach --daemon-command "ssh tower dispatchd --stdio"` works. But
nothing remembers that command, and three gaps make it unusable as a fleet:

- **A machine that is down at startup never joins.** A `Client` cannot exist
  until its first dial succeeds. Once it exists it reconnects with backoff, but
  a failed first dial leaves no `Client` to retry, so the machine is gone until
  Dispatch restarts.
- **Startup is serial.** Each unreachable `--daemon-command` blocks the
  interface for up to 30 seconds (`COMMAND_HANDSHAKE_TIMEOUT`), and N down
  machines cost N × 30s.
- **Nothing can be opened on a remote machine.** `add_project` sends a root to
  the first attachment, and a daemon started by `dispatchd --stdio` keeps no
  project list of its own. A newly reached machine shows an empty row with no
  way to put anything in it.

This slice adds a registry of machines, the CLI verbs and TUI overlay that edit
it, a supervisor that dials every registered machine in the background, and a
way to open a project on a remote machine by typing its path.

### Not in this slice

- **Remote directory browsing.** The browser lists this machine's filesystem;
  a remote machine gets a typed path. Browsing would need new protocol messages.
- **Removing or renaming a machine from inside the TUI.** The CLI does that.
- **Watching `machines.toml` for changes.** A running Dispatch picks up CLI
  edits on its next start. The overlay is the way to add a machine mid-session.
- **Windows.** `process::terminate_tree` on Windows only terminates one handle
  (handoff, parked item 3), and a command transport depends on tree
  termination. The fleet is Unix-only until that is fixed. Nothing refuses to
  run on Windows; it is simply not supported or tested there.
- **Parked items 4–7** from the handoff.

## Decisions taken with the user

Carried from F2a's design, not re-litigated:

- `dispatchd --stdio` bridges to that machine's own daemon, starting one if
  none is listening. Already built.
- The default command runs `dispatchd --stdio` over ssh, with a per-machine
  override that holds the program and its arguments as separate fields.
- `machines.toml` beside `projects.toml`.
- `dispatch machine add/list/remove`, and an in-TUI overlay to add one.
- A supervisor per registered machine, with backoff.
- No binary copying. A machine has `dispatchd` or cannot be added.

Taken during this design:

- **Remote projects are opened by typed path.** `^a o` asks which machine when
  more than one is attached; a remote machine gets a path prompt, not the
  browser. Kept projects are recorded per machine.
- **A registered machine implies attached mode.** With any machine registered,
  plain `dispatch` attaches to the local daemon (starting one if needed) and to
  every registered machine. Standalone remains for a user with no machines:
  standalone agents are this process's children and cannot share a sidebar with
  daemon-owned ones.
- **`machine add` proves the machine is reachable before saving it**, with
  `--no-check` to register a machine that is asleep right now.
- **The sidebar shows the registry name**, not the daemon's hostname. It exists
  before the first connect, and it is unique by construction where hostnames
  are not.

## The registry

`machines.toml`, in the configuration directory beside `projects.toml`:

```toml
[[machine]]
name = "tower"            # --name, else the host part of the target
target = "me@tower"       # an ssh target

[[machine]]
name = "gpu"
target = "gpu-box"
# Replaces the whole default command, so it can be a transport other than
# ssh, or a local program whose path contains a space.
command = { program = "/Applications/My Tools/tunnel", args = ["gpu-box", "--", "dispatchd", "--stdio"] }
```

A new module, `dispatch_config::machines`, shaped like `projects.rs`:

```rust
pub struct Machine {
    pub name: String,
    pub target: String,
    pub command: Option<Command>,
}

/// Strings rather than `OsString`s: serde writes an `OsString` into TOML as a
/// platform-tagged byte array nobody could edit by hand. A path with a space
/// is still expressible, which is the point of the separate fields.
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
}

impl Machine {
    /// The program and arguments that reach this machine's daemon.
    pub fn dial(&self) -> (OsString, Vec<OsString>);
}

pub fn load(dir: &Path) -> Result<Vec<Machine>, ConfigError>;
pub fn save(dir: &Path, machines: &[Machine]) -> Result<(), ConfigError>;
/// Refuses a duplicate name.
pub fn add(dir: &Path, machine: Machine) -> Result<(), ConfigError>;
/// Answers whether it was there to remove.
pub fn remove(dir: &Path, name: &str) -> Result<bool, ConfigError>;
```

No file means no machines, as with `projects.toml`. A file that fails to parse
fails startup and names the file.

### The default command

```
ssh -T -o BatchMode=yes -o ConnectTimeout=10 <target> dispatchd --stdio
```

- `-T`: no pseudoterminal. The bytes are frames, not a session.
- `BatchMode=yes`: without it, `ssh` prompts for a password or an unknown host
  key on `/dev/tty`, which draws over the TUI and hangs the dial. With it,
  `ssh` fails at once with a line on stderr — `Permission denied (publickey)`,
  `Host key verification failed.` — that the client already surfaces. A user
  accepts a new host key by running `ssh <target>` once by hand.
- `ConnectTimeout=10`: an asleep host fails a dial in ten seconds rather than
  the kernel's TCP timeout.

### What separate fields do and do not buy

`program` and `args` are separate so a **local** program path containing a
space can be expressed, which `--daemon-command`'s whitespace split cannot.
They do not protect **remote** arguments: `ssh` joins everything after the
target into one string and the remote shell parses it again. A remote path
with a space in it needs quoting inside that argument, as it would on any ssh
command line.

### Names

A name is one or more of `A-Z a-z 0-9 - _`. It is a TOML table key in
`projects.toml` and a word typed on the command line, so nothing that would
need quoting in either.

`--name`, or else a default taken from the target's host: an `ssh://` prefix,
any `user@` and any `:port` are dropped, then the host is cut at its first dot
— `tower` from `me@tower.lan`, `tower` from `ssh://me@tower:2222`. A host that
is an IPv4 address keeps all four parts with the dots made dashes:
`192-168-1-5`. A target that yields no valid name needs `--name`.

`add` refuses an invalid name, a name already registered, and a name equal to
this machine's own hostname, since the local daemon's row already carries that
name.

It also refuses a target that is empty, starts with `-`, or holds whitespace
(`machines::valid_target`): ssh reads a leading `-` as an option wherever it
stands, and `-oProxyCommand=…` runs a local command. The CLI checks this
before dialling, and the `^a m` overlay refuses it at the target step.

### Kept projects, per machine

`projects.toml` keeps its top-level `roots` for this machine's projects,
unchanged, and gains a table per remote machine:

```toml
roots = ["/Users/me/code/thing"]

[machines.tower]
roots = ["~/code/server"]
```

A file from before this slice loads as it always did, and so does one written
by this slice read by an older build: serde ignores the unknown table.
`projects::load`, `remember` and `forget` keep their meaning for this machine,
and gain siblings for a remote one: `load_on`, `remember_on`, `forget_on`, and
`forget_machine`, which `machine remove` calls to drop the whole table.

## The supervisor

### `dispatch-client`

A new constructor that cannot fail:

```rust
/// A client that dials in the background, and keeps dialling until it
/// connects.
///
/// Returns at once, disconnected, at generation 0 and with an empty
/// `device()`. The first connection is generation 1 — a caller that already
/// rebuilds on a generation change handles a first connect with no new code.
pub fn dial(role: Role, name: &str, liveness: Liveness, dial: Dial) -> Client;
```

The `supervise` thread makes the first dial without sleeping first, then runs
as it does today. Messages queued while disconnected are handled as they are
during any outage; the caller re-sends what it needs on the generation change.

`attach_at` and `attach_over` stay synchronous. `dispatch delegate`, the local
attach and the `machine add` check each need an answer they can wait for.

**Backoff depends on the dial:**

| Dial | First retry | Longest gap |
|---|---|---|
| `Endpoint` | 100ms | 2s (unchanged) |
| `Command` | 1s | 30s |

A 2s ceiling respawns `ssh` against an asleep host thirty times a minute. With
a 30s ceiling and `ConnectTimeout=10`, a machine that wakes joins within about
forty seconds.

**`Client::last_error() -> Option<String>`**: the most recent failed dial's
message, with the command's first stderr line when there is one. Cleared when a
dial connects.

**The dial-in-flight race** (handoff, parked item 2) is closed here:
`supervise`'s `Ok` arm checks `closed` before `record`, and tears the new
connection's process down if the client has already been dropped.

### `dispatch`

- `Attachment` gains `label: Option<String>`, the registry name, and `remote:
  bool`. `sync_attachment` writes the daemon's own name into the row only when
  there is no label, and never writes an empty one.
- `Device::pending(name)`: a device that has not connected yet,
  `reachable: false`. `Device::new` keeps its meaning.
- `App::attach_named(client, label, roots)` registers a pending device — named
  by the label, or by the dial string when there is none — holds the client,
  and pre-fills its `opened` with `roots`, which go out on first connect.
  `App::attach` is unchanged, and is what makes an attachment *this machine's*:
  every `attach_named` attachment is remote.
- Kept projects are recorded under the attachment's label. This machine's go
  in the top-level list; a remote attachment with no label (one reached by
  `--daemon` or `--daemon-command`) keeps nothing, because nothing would dial
  it again on the next start.
- A first connect is the move from generation 0 to 1. `sync_attachment`
  already re-sends `opened` roots and the supervisor already re-sends
  `Subscribe`, so the only difference is the status text: `connected to tower`
  rather than `reattached to tower`.
- Roots added for a machine that is down are held in its `opened` list and go
  out when it connects.
- **One status line per outage, not one per retry.** When a machine goes from
  reachable to not, or its `last_error` changes, the status line reads
  `tower unreachable: Permission denied (publickey)`. A machine retrying every
  thirty seconds says nothing new until something changes.

### Startup

1. With `--attach`, or with any machine registered, attach to the local daemon
   synchronously, starting one if none is listening — today's `attach()`.
   `--no-start` still requires `--attach`.
2. For every registered machine: `Client::dial` with its command, then
   `attach_named` with its name, then `subscribe`. The kept roots under
   `[machines.<name>]` go into that attachment's `opened`.
3. `--daemon` and `--daemon-command` switch from `attach_at`/`attach_over` to
   `Client::dial` as well. Their rows have no label, so they show the dial
   string until the handshake and the daemon's own name after it.
4. The interface draws at once. Machines that have not answered are dimmed
   rows that light up as they connect.

## The CLI

```
dispatch machine add <target> [--name NAME] [--no-check] [-- PROGRAM ARGS…]
dispatch machine list
dispatch machine remove <name>
```

**`add`** builds the machine's command: the default, or the override after
`--`, taken as separate arguments so nothing is split on whitespace. Unless
`--no-check` is given, it runs `Client::attach_over` with that command and the
command dial's 30s budget:

- Success saves the entry and prints `added tower (its daemon calls itself
  ubuntu-22)`. Exit 0.
- Failure saves nothing, prints the error with the command it ran and the
  command's first stderr line, and exits 1.

**`list`** prints one line per machine: its name, then its target or its
command. It dials nothing.

**`remove`** deletes the entry and the machine's kept projects, and prints that
the remote daemon and its agents were left running. It never contacts the
machine.

None of the verbs start the interface or a local daemon.

## The TUI

### `^a m`: add a machine

A new widget, `dispatch_tui::prompt::Prompt`: one line of input with a title, a
hint and an error line. It is the first free-text overlay; `Picker` has none.

The overlay has two steps:

1. **Target.** The user types an ssh target and presses Enter.
2. **Name.** Pre-filled with the default name for that target, editable. Enter
   starts the check.

`^a m` is refused while Dispatch runs standalone, with a status line saying to
restart with `--attach`. Attaching tears down the standalone machine and every
pane running on it; a keystroke must not do that.

The check runs `Client::attach_over` on a background thread while the overlay
shows `checking tower…`. The name is validated before the check starts, with
the same rules as `add`.

- **Success:** the machine is saved to `machines.toml`, and the client the
  check connected becomes the attachment through `attach_named`. There is no
  second dial.
- **Failure:** the error stays in the overlay and the input stays editable, so
  a typo in the target can be fixed and retried.
- **Esc** closes the overlay. A check still in flight is abandoned; its result,
  and the client with it, is dropped when it arrives.

The command override and `--no-check` are CLI-only.

### `^a o` with more than one machine attached

1. A machine picker opens first, with the selected project's machine
   pre-selected. Machines that are down are listed and marked. Picking one is
   allowed: the root waits in `opened` until the machine connects.
2. **This machine:** the directory browser, as today.
3. **A remote machine:** a `Prompt` titled `Open on tower:`. Enter sends
   `OpenProject` with the typed path to that machine's attachment and keeps it
   under `[machines.tower]`.

With one machine attached, `^a o` goes straight to that machine's step: the
browser for this machine, exactly as today, or the path prompt for a remote
one.

## The daemon

Three changes to `open_project_for`, with a new helper,
`dispatch_os::paths::expand_home`:

- **A leading `~` expands against the daemon's own home.** A path typed by hand
  for a remote machine almost always starts with one, and `paths::resolve`
  would otherwise look for a directory literally named `~` in the daemon's
  working directory.
- **A message of its own for a root that cannot be opened.** Its two
  `ServerMessage::Error { error: ProtocolError::Other(..) }` replies become:

  ```rust
  /// A root asked for in `OpenProject` could not be opened. Sent only to the
  /// client that asked.
  ProjectRefused {
      /// The root exactly as the client sent it.
      root: PathBuf,
      /// Why: not a directory, no such file, permission denied.
      reason: String,
  },
  ```

  A client that receives it removes that root from the attachment's `opened`,
  so the connection stops asking, and puts the reason on the status line.
  Without this, a mistyped remote path would be kept and re-sent on every
  start, and it would never become a project row the user could delete it
  from.

  **Only a root asked for this session is forgotten.** Each attachment holds
  the roots the user asked for in this session (`add_project`,
  `add_project_on`) that the daemon has not yet answered with
  `ProjectResolved`. A refused root in that set is also removed from the kept
  list. A root kept from an earlier run — handed to `attach_named` at startup,
  or re-sent on a reconnect — opened once, and a refusal now is as likely a
  mount not up yet or a directory briefly renamed as one that is gone. It
  stays in `projects.toml`, and the status line says so: `cannot open <root>
  on <name>: <reason> (still kept; delete it from projects.toml if it is
  gone)`.

  A `ServerMessage` variant rather than a `ProtocolError` one. `ProtocolError`
  is externally tagged and has no `Unknown`, so a variant added there fails an
  older peer's whole frame — and a fleet is exactly where an older `dispatchd`
  meets a newer client. `ServerMessage` has `#[serde(other)] Unknown`: an older
  client skips `ProjectRefused` and loses only the status line. Adding a
  variant is not a version bump; `VERSION` stays 1.1.

- **A message telling the asker what its root became.** A client keeps a root
  as typed — `~/code/app` — but the row it gets back carries the root the
  daemon resolved — `/home/me/code/app` — so dropping that row would match
  nothing it keeps, and the root would be re-sent on the next connection. On
  success `open_project_for` therefore sends, to the asking client only and
  before the `ProjectOpened` broadcast:

  ```rust
  ProjectResolved {
      /// The root exactly as the client sent it.
      root: PathBuf,
      /// The project's root, as every client will see it.
      resolved: PathBuf,
  },
  ```

  It is sent even when the two are equal, which keeps the daemon simple. A
  client that receives it with the two different replaces `root` with
  `resolved` in the attachment's `opened` and its kept list, if `root` is
  still there. An older client skips it as `Unknown`, as with
  `ProjectRefused`; `VERSION` stays 1.1.

## Failure cases

| Case | Behaviour |
|---|---|
| A registered machine is asleep at startup | Its row is drawn at once, dimmed. The supervisor retries with backoff up to 30s. When the handshake succeeds the row lights up, and its kept roots and `Subscribe` go out. |
| ssh authentication or host-key failure | `BatchMode` makes `ssh` fail fast. The status line shows the first stderr line, once per outage. |
| `dispatchd` is not installed on the remote | The same path, reported as `dispatchd: command not found` or similar. `add` refuses to save the machine unless `--no-check` is given. |
| A typed remote path is wrong | The daemon answers `ProjectRefused`; the client removes the root from its kept list and `opened` and says why. |
| A kept root cannot be opened at startup or on a reconnect | The daemon answers `ProjectRefused`; the client stops asking for it this session but keeps it in `projects.toml`, and says so. |
| `^a m` pressed while standalone | Refused with a status line. Nothing is torn down. |
| Enter on an empty prompt | Nothing happens. No `OpenProject` is sent and no machine is saved. |
| `machines.toml` does not parse | Startup fails, naming the file. |
| The client is dropped while a dial is in flight | The `closed` check in the `Ok` arm tears the new connection down. No process is left behind. |
| The overlay is closed while its check is in flight | The result and its client are dropped on arrival. |
| A machine is removed by the CLI while Dispatch runs | The running interface keeps its attachment until it exits. The next start does not dial it. |

## Testing

- **`dispatch-config`**
  - `machines` round-trips through `save` and `load`, with and without a
    command override.
  - `add` refuses a duplicate name.
  - The default command is the ssh line above; an override replaces it whole.
  - Default names: `me@tower.lan` → `tower`, `ssh://me@tower:2222` → `tower`,
    `192.168.1.5` → `192-168-1-5`; a name with a space is refused.
  - An empty `machines.toml` loads as no machines.
  - A `projects.toml` in the old shape still loads; per-machine tables
    round-trip; `machine remove` drops its table.
- **`dispatch-client`**
  - `Client::dial` against an endpoint nothing listens on starts disconnected
    at generation 0. Once a daemon listens there, it reaches generation 1 and
    has sent `Subscribe`.
  - The backoff arithmetic for each dial, as a pure function: a command
    starts at 1s and stops doubling at 30s; a socket keeps 100ms and 2s.
  - A client dialling a command that always fails spawns it at most twice in
    its first 1.5 seconds.
  - `last_error` carries a failing command's first stderr line, and clears on
    connect.
  - The dial-in-flight race: a client dropped while its dial is completing
    leaves no child process.
- **`dispatch-daemon`**
  - `~/x` opens `$HOME/x`.
  - A missing root is answered with `ProjectRefused` carrying the root as sent,
    to the asking client only.
  - `~` is answered with `ProjectResolved { root: "~", resolved: <home> }`,
    to the asking client only.
- **`dispatch` app**, with `Client::for_test`
  - A row added by `attach_named` shows the label and is unreachable.
  - A first connect says `connected to`, not `reattached to`.
  - `ProjectRefused` for a root typed this session removes it from the kept
    list and from `opened`; for a root kept from an earlier run it removes it
    from `opened` only.
  - `ProjectResolved` rewrites a typed root to the resolved one, so dropping
    that project's row forgets it and a reconnect does not re-send it.
  - `^a m` while standalone is refused and leaves local panes running.
  - `^a o` with two machines opens the machine picker; choosing a remote
    machine opens the path prompt and sends to that machine only.
  - The `^a m` overlay's steps: target, name, checking, success, failure, and
    Esc while checking.
- **End to end** (`dispatch/tests/end_to_end.rs`). There is no ssh in tests:
  the command override is how a test reaches a machine.
  - A temporary configuration directory whose `machines.toml` holds a machine
    whose command is `sh -c 'test -e <flag> && exec <dispatchd> --stdio
    --endpoint <second dir>/dispatchd.sock'`. The flag stands in for the
    machine being asleep: `--stdio` would otherwise start the far daemon
    itself. Dispatch's interface is up and the row is drawn unreachable while
    the flag is absent; once the flag is created, the row comes up with its
    kept project.
  - `dispatch machine add x --name x -- <dispatchd> --stdio --endpoint …`
    passes its check, saves the entry and exits 0.
  - `dispatch machine add y -- /nonexistent/dispatchd --stdio` exits 1, names
    the program in its message, and saves nothing. (A missing endpoint would
    not do: `dispatchd --stdio` starts a daemon when none is listening.)

## Follow-ups this slice creates

- Remote directory browsing, so `^a o` on a remote machine can offer the
  browser rather than a bare prompt.
- Removing and renaming machines from inside the TUI.
- Windows tree termination (handoff, parked item 3) before any Windows fleet.
