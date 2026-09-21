# Dispatch

An agent orchestration TUI. One control surface for multiple coding agents
(Claude Code, Codex, agy, opencode): a project sidebar, tiled live agent
terminals, and — in later slices — an orchestrator that delegates work to
spawned subagents under explicit approval.

Status: **early**. The TUI multiplexes local agents, and `dispatchd` can own
them instead so they outlive the interface.

## Building

Requires:

- Rust 1.85 or newer (edition 2024)
- **Zig 0.16.0** — builds the vendored `libghostty-vt` terminal engine

```sh
brew install zig        # macOS
cargo build --workspace
```

The first build runs `zig build` against `vendor/libghostty-vt`, which fetches
Zig dependencies into `vendor/libghostty-vt/zig-pkg/` (gitignored) and needs
network access. For an offline or hermetic build, point Zig at a pre-fetched
package set:

```sh
LIBGHOSTTY_VT_ZIG_SYSTEM_DIR=/path/to/packages cargo build --workspace
```

On Windows, build for the GNU ABI so Zig supplies its own MinGW libc and no
Visual Studio install is needed:

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --workspace --target x86_64-pc-windows-gnu
```

## Layout

| Crate | Responsibility |
|---|---|
| `dispatch-core` | Domain types and state. Zero I/O. |
| `dispatch-layout` | Tiling algorithm. Pure functions. |
| `dispatch-config` | Harness definitions, config loading. |
| `dispatch-os` | All platform-specific code. The only crate with `#[cfg(windows)]`. |
| `dispatch-pty` | PTY supervision, VT screen state, title scanning. |
| `dispatch-proto` | The client-daemon wire protocol. |
| `dispatch-client` | The client half of that protocol. |
| `dispatch-daemon` | The daemon's loop: it owns the agents. |
| `dispatch-tui` | Rendering, input routing, keymap. |
| `dispatch` | The client binary. |
| `dispatchd` | The daemon binary. |
| `xtask` | Build tooling — regenerates FFI bindings on version bumps. |

## The grid

Every pane is drawn inside a thin border carrying its title, so one agent's
output cannot be mistaken for the next one's or for the sidebar.

At most four panes are tiled at once. A fifth does not shrink the other four —
it opens a second tab, and `^a 1` through `^a 9` move between them (`^a Tab`
walks them in order). The sidebar always lists every pane, whichever tab it is
on, and the status row says which tab you are looking at.

A pane whose process exits gives its tile back straight away and the remaining
panes spread into the space. It stays in the sidebar, where selecting it shows
what it printed — `^a x` is what removes it for good.

## The sidebar

The project list is framed on the left. It is a tree: each project carries a
twisty, and so does any pane running subagents. Clicking a project's row moves
the view to it and folds its panes away; clicking a pane's twisty folds its
subagents, and clicking anywhere else on a pane's row focuses it. The project
the grid is showing is highlighted across the full width of the row.

A row is marked on both sides. On the left, a project shows whether it is a git
repository or a plain directory, and a pane shows the icon of the harness
running in it -- the `icon` key in that harness's TOML, so a harness you
register yourself can have one too. On the right, one glyph says what the pane
is doing: starting, running, idle, exited cleanly, exited badly, or closed and
still listed for the sake of a subagent under it.

Every glyph is a Nerd Font one, so Dispatch wants a patched font in the
terminal it runs in.

## Running the daemon

Dispatch works on its own, with the agents as its children. Started that way,
closing it closes them.

`dispatchd` owns the agents instead, so they survive a client exiting. `--attach`
starts one if none is listening, so the daemon stays an implementation detail:

```sh
dispatch --attach /path/to/project             # starts a daemon if needed
dispatch --attach --no-start                   # or insist on one already there
dispatchd /path/to/project                     # or run it yourself
```

A daemon a client starts is detached from that client's terminal: it keeps
running when the client exits, and a Ctrl-C meant for the interface does not
reach the agents. It records its process id in `dispatchd.pid` beside the socket,
which is what to stop when you want it gone.

Several clients can attach at once and see the same panes. A client attaching to
a pane that is already running is replayed the last 256 KiB it printed, so
reattaching shows the work rather than a blank rectangle.

An attached client reconnects on its own: restart the daemon, or lose the socket,
and it waits, says so, and rebuilds its view from what the daemon reports when it
answers again. A connection that goes quiet is asked whether it is still there,
so a socket that is up but carrying nothing is noticed rather than waited on.

The daemon runs in the foreground and logs to a file. Projects given on its
command line are served immediately; an attached client opens more over the
socket. One daemon per configuration directory: a second refuses to start rather
than splitting the fleet in two. `SIGTERM`, `SIGINT`, or a closed console stops
it and terminates its panes. `DISPATCH_CONFIG_DIR` gives a separate daemon its
own endpoint, harnesses, and log.

## Delegation

An agent in a pane can ask for a second agent to work on something:

```sh
dispatch delegate "write the tests for the http client"
```

Dispatch asks you first, every time — unless you have approved that pane
wholesale with `A`, which lasts until the daemon stops. The subagent runs as a
pane under the one that asked, and the caller gets its output and exit code when
it finishes.

Delegation needs two things. The daemon must own the panes (`--attach`), because
it is what starts the subagent; and the harness must declare a non-interactive
form, since an interactive agent never exits:

```toml
# ~/.config/dispatch/harnesses/claude.toml
[task]
args = ["-p", "{task}"]

[task.platform.windows]
args = ["/c", "claude", "-p", "{task}"]
```

`claude` and `codex` ship with one. Caps live in `config.toml`, and refuse rather
than prompt:

```toml
[delegation]
max_depth = 1              # a subagent cannot delegate
max_live_per_parent = 4
request_timeout_secs = 600 # no value disables this; 0 refuses on the next tick
```

There is deliberately no way to turn the deadline off: an agent on an unattended
daemon would otherwise wait for a person who is not there. To wait longer, raise
the number.

The approval prompt takes `a` to approve, `d` to deny, `A` to approve everything
from that pane for this daemon's lifetime, and `Esc` to defer. The status line
reports how many are waiting and which key reopens them — that key is `^a a`.

There are also keyboard bindings to open and close a subagent pane: `^a s` expands
the focused pane's next child into the tiled grid, and `^a c` collapses it back out.

The subagent's output goes to stdout and every status line to stderr, so
`dispatch delegate "…" > result.md` captures the work and nothing else. Fan-out
needs no feature: the agent's own shell does it:

```sh
dispatch delegate "write the tests" > tests.md &
dispatch delegate "write the docs"  > docs.md  &
wait
```

Exit codes follow `sysexits(3)` so an agent can branch without parsing prose.
Dispatch's own codes (69, 75, 77, 78) sit inside the same 0–125 band a subagent's
own exit code comes from, so a subagent that exits 78 is indistinguishable from a
refusal. An agent branching on exit codes should keep that in mind.

| Code | Meaning |
|---|---|
| 0–125 | the subagent's own exit code |
| 69 | no daemon is listening |
| 75 | timed out, or the connection dropped, or the daemon does not know the asking pane, or the subagent was stopped before it finished |
| 77 | denied by the user |
| 78 | refused: caps, or the harness has no `[task]` form |

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE) for attribution of
vendored and derived code.
