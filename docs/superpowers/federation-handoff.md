# Federation: where it stands

Written 2026-09-22, at the end of the slice that made a connection able to be a
child process. Everything below is either in git or explicitly named as not
built. If you are picking this up cold, read this, then the two specs it points
at.

## The three slices

| Slice | State | Spec |
|---|---|---|
| F1 — many daemons in one client | **merged** | `specs/2026-09-21-federation-many-daemons-design.md` |
| F2a — a connection that is not a socket | **merged** | `specs/2026-09-22-federation-stdio-transport-design.md` |
| F2b — machines: SSH, a registry, a supervisor | **not started** | not written |

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

## F2b, already decided

These came from the user during F2a's design and do not need re-litigating:

- **Bridge, don't be the daemon.** `dispatchd --stdio` connects to that
  machine's own daemon and starts one if none is listening. Already built and
  merged — F2b consumes it.
- **`dispatchd --stdio` by default, with a per-machine `command` override** for
  a non-PATH install, a wrapper, or a nix shell. Fails naming the exact command
  it tried.
- **`machines.toml` beside `projects.toml`**, holding program and arguments as
  separate fields rather than one string — `--daemon-command`'s whitespace
  split cannot express a program path containing a space, and the registry is
  where that is answered.
- **`dispatch machine add <ssh-target> [--name]`, `list`, `remove`**, *and* an
  in-TUI overlay to add one while Dispatch runs, the way `^a o` adds a project.
- **A supervisor per registered machine**, with backoff, so a machine that was
  asleep at startup joins when it wakes. This closes F1's known gap.
- **No binary copying.** A machine either has `dispatchd` or cannot be added.

## Parked, and not in any issue tracker

Ordered by how much they would hurt on a real fleet.

1. **Attaching is serial and synchronous at startup.** Each unreachable
   `--daemon-command` costs up to 30 seconds (`COMMAND_HANDSHAKE_TIMEOUT`)
   before the interface appears, and N down machines cost N × 30s. F2b's
   supervisor is the right place to fix it: attach in the background and let
   rows fill in.
2. **A dial in flight when `Client::drop` runs can leave one process behind.**
   `closed` is only checked at the top of the supervisor loop; a dial that
   completes after the check records a pid nobody will take. The cheap fix is a
   `closed` check in `supervise`'s `Ok` arm before `record`.
3. **Windows `process::terminate_tree` only calls `TerminateProcess` on the one
   handle**, despite a comment about job objects. Pre-existing, but F2a's
   command transport now *depends* on tree termination — an `ssh.exe` with
   children would outlive a dropped connection there. Must be looked at before
   any Windows fleet.
4. **Reaping a group whose child is a zombie the parked reader still holds**
   returns EPERM on macOS, so an ordinary disconnect logs `failed to stop the
   process behind a dial` — noise, not a leak. On Linux the same shape costs a
   bounded ~2s inside `Client::drop` over a command dial.
5. **Focus after a reconnect replay** lands on the last pane the daemon
   replayed rather than the one the user was in (`state.rs` focuses every
   replayed pane while its project is selected). Invisible with one pane.
6. **Several `--daemon-command` failures collapse into one status line** — the
   last one wins. `--daemon` has the same shape.
7. **`StderrHint::first_line` is read after a bounded 50ms poll.** A command
   that dies slower than that still reports the bare error. Lengthening the
   wait trades a failing attach's latency for a better message.

## Decisions taken on the user's behalf during execution

Recorded because they were judgement calls, not requirements:

- A project whose device is not registered is dropped from the sidebar
  entirely. Safe only because `App::attach` registers a machine before its
  projects can arrive; there is a test pinning that ordering.
- `add_project` asks the first attachment. With two machines attached, opening
  a path puts it on the wrong one. F2b's registry owns the question of which
  machine a directory lives on.
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

## How this work was run

Each slice: a spec in `docs/superpowers/specs/`, a plan in
`docs/superpowers/plans/`, then execution one task at a time with a review
after each and a whole-branch review at the end. The plans' checkboxes were
never ticked — they record what was asked for, and git records what happened.
