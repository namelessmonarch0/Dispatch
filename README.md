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

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE) for attribution of
vendored and derived code.
