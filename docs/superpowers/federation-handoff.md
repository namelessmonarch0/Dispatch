# Federation: where it stands

Written 2026-09-22, at the end of the slice that made a connection able to be a
child process, and updated 2026-09-23 when the machines slice merged.
Everything below is either in git or explicitly named as not built. If you are
picking this up cold, read this, then the three specs it points at.

## The three slices

| Slice | State | Spec |
|---|---|---|
| F1 — many daemons in one client | **merged** | `specs/2026-09-21-federation-many-daemons-design.md` |
| F2a — a connection that is not a socket | **merged** | `specs/2026-09-22-federation-stdio-transport-design.md` |
| F2b — machines: SSH, a registry, a supervisor | **merged** | `specs/2026-09-22-federation-machines-design.md` |

What works today, end to end: one client holds a connection per machine; every
project and pane is attributed to the machine it is on; the sidebar draws a row
per machine once there is more than one; a machine whose daemon dies keeps its
rows and refuses writes out loud while reading still works; and a machine can
be reached over any command that pipes bytes, including `ssh host dispatchd
--stdio`, with the agents surviving the transport's death because they belong
to the daemon rather than the pipe.

```sh
dispatch --attach ~/code/thing --daemon-command "ssh tower dispatchd --stdio"
```

## What F2b built

A `machines.toml` registry beside `projects.toml`
(`crates/dispatch-config/src/machines.rs`) holding each machine's name, its
ssh target, and an optional command override for a non-PATH install, a
wrapper, or a nix shell — program and arguments kept as separate fields,
because `--daemon-command`'s whitespace split cannot express a program path
containing a space. `dispatch machine add <ssh-target> [--name]`, `list`, and
`remove` (`dispatch/src/machine.rs`) manage it from the command line; `add`
dials the machine before saving it, unless told `--no-check`, so a typo or an
unreachable host is caught at the command rather than at the next start.
`^a m` does the same from a running Dispatch, and `^a o` grew a machine
step: with more than one machine attached it asks which one a project
belongs to, and picking a remote machine asks for a typed path rather than
walking the local directory browser, which has no way to list a remote
machine's files. Both live behind `dispatch_tui::Prompt`, the one-line
widget this slice added for a question a list cannot answer.

Underneath, `Client::dial` now retries a `Command` dial the way a socket
dial already did — starting at 1s and doubling to a 30s ceiling — so a
machine asleep at startup joins once it wakes rather than needing a
restart; this closes F1's known gap. A machine that cannot open a project
(a bad path, not a directory, permission denied) says so with
`ServerMessage::ProjectRefused` rather than leaving the client to guess
from silence; a root already open is not refused, the daemon hands back the
existing project. A machine that does open one tells the asking client, with
`ServerMessage::ProjectResolved`, what its root resolved to — `~/code/app`
becomes `/home/me/code/app` — so the client can rewrite what it keeps and
dropping the row forgets it. A refusal forgets the root only when the user
asked for it this session; a root kept from an earlier run stays kept, since
a mount not up yet is refused the same way as a directory that is gone. No
binary copying: a machine either has `dispatchd` on its `PATH` or cannot be
added.

## Parked, and not in any issue tracker

Ordered by how much they would hurt on a real fleet.

1. ~~**Windows `process::terminate_tree` only calls `TerminateProcess` on the
   one handle.**~~ Fixed on the `audit-remediation` branch (audit A03): panes
   and command transports are created suspended inside a Job Object, and
   `terminate_tree` ends the job.
2. **Reaping a group whose child is a zombie the parked reader still holds**
   returns EPERM on macOS. Partly addressed on `audit-remediation`: the
   Linux half — the bounded ~2s cost inside `Client::drop` over a command
   dial — is fixed (`03e6c77`, `crates/dispatch-os/src/ipc.rs`'s `reap` now
   signals, waits for the leader, and only then waits for the rest of the
   tree). The macOS EPERM case still happens, but the disconnect it used to
   log at `warn` (`failed to stop the process behind a dial`, in
   `dispatch-client`) moved to `dispatch-os`'s `Closer::close` and dropped to
   `debug` (`failed to end a command transport`, `c292c86`), so an ordinary
   disconnect no longer logs it as a warning.
3. **Focus after a reconnect replay** lands on the last pane the daemon
   replayed rather than the one the user was in (`state.rs` focuses every
   replayed pane while its project is selected). Invisible with one pane.
4. **Several machines going bad at once still collapse into one status
   line.** Each machine now dedupes its own outage — `report_outage` says so
   once per failure, not once per retry — but `App::status` is a single
   field, so two machines failing close together still leave only the last
   writer's line on screen. `--daemon` and `--daemon-command` have the same
   shape.
5. **`StderrHint::first_line` is read after a bounded 250ms poll** (raised
   from 50ms on `audit-remediation`, `5d799d1`, `dispatch-client`'s
   `HINT_PATIENCE`). A command that dies slower than that still reports the
   bare error. Lengthening the wait further trades more of a failing
   attach's latency for a better message.
6. **Remote directory browsing.** `^a o` on a remote machine takes a typed
   path; the browser would need protocol messages that list a remote
   directory.
7. **Removing or renaming a machine in the TUI.** The CLI can remove one
   (`dispatch machine remove`); neither it nor the overlay can rename one —
   the overlay only adds.
8. **A remote `dispatchd` older than the client** answers `~/x` with a plain
   `Error` and no `~` expansion. The root is then kept and re-sent without
   ever becoming a row. Keep remote `dispatchd` at the client's version.

## Decisions taken on the user's behalf during execution

Recorded because they were judgement calls, not requirements:

- A project whose device is not registered is dropped from the sidebar
  entirely. Safe only because `App::attach` registers a machine before its
  projects can arrive; there is a test pinning that ordering.
- A command dial gets 30s of patience; a socket dial keeps 2s. A local daemon
  silent for two seconds is genuinely wrong; a network round trip plus a remote
  process start is not.
- `--stdio --endpoint <path>` starts its daemon with `DISPATCH_CONFIG_DIR` set
  to that path's parent, because `ipc::endpoint()` is
  `<config dir>/dispatchd.sock` by construction.
- The fold ladder reads the collapse flags rather than remembering a direction,
  so a mouse click cannot desync it.
- `dispatchd --device` defaults to the real hostname via `dispatch_os::host`,
  not `$HOSTNAME` — which bash sets but does not export.
- `ProjectRefused` and `ProjectResolved` are `ServerMessage`s, not
  `ProtocolError` variants: `ProtocolError` has no `Unknown`, so a new
  variant would fail an older peer's frame.
- `^a m` is refused while standalone rather than attaching mid-session,
  because attaching tears the standalone panes down.

## Where the moving parts live

| Concern | File |
|---|---|
| Devices, projects, panes, collapse | `crates/dispatch-core/src/state.rs`, `device.rs` |
| The sidebar's tree, hit testing, glyphs | `crates/dispatch-tui/src/sidebar.rs` |
| The directory browser | `crates/dispatch-tui/src/browser.rs` |
| One connection per machine, routing, reconnect | `dispatch/src/app.rs` (`Mode`, `Attachment`, `sync_attachment`) |
| Dialling, and remembering how | `crates/dispatch-client/src/lib.rs` (`Dial`, `attach_over`) |
| The transports themselves | `crates/dispatch-os/src/ipc.rs` (`over_command`, `ChildReader`) |
| The bridge | `dispatchd/src/bridge.rs` |
| Kept projects | `crates/dispatch-config/src/projects.rs` |
| The machine registry | `crates/dispatch-config/src/machines.rs` |
| The machine verbs | `dispatch/src/machine.rs` |
| The add overlay | `dispatch/src/add_machine.rs` |
| One-line prompts | `crates/dispatch-tui/src/prompt.rs` |

## How this work was run

Each slice: a spec in `docs/superpowers/specs/`, a plan in
`docs/superpowers/plans/`, then execution one task at a time with a review
after each and a whole-branch review at the end. The plans' checkboxes were
never ticked — they record what was asked for, and git records what happened.
