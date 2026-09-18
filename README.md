# Dispatch

An agent orchestration TUI. One control surface for multiple coding agents
(Claude Code, Codex, agy, opencode): a project sidebar, tiled live agent
terminals, and — in later slices — an orchestrator that delegates work to
spawned subagents under explicit approval.

Status: **early**. Slice 1 (local TUI + PTY multiplexer) is in progress.

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
| `dispatch-pty` | PTY supervision and VT screen state. |
| `dispatch-proto` | The client-daemon wire protocol. |
| `dispatch-daemon` | The daemon's loop: it owns the agents. |
| `dispatch-tui` | Rendering, input routing, keymap. |
| `dispatch` | The client binary. |
| `dispatchd` | The daemon binary. |
| `xtask` | Build tooling — regenerates FFI bindings on version bumps. |

## Running the daemon

`dispatchd` owns the agents, so they survive a client exiting. It runs in the
foreground and logs to a file:

```sh
cargo run -p dispatchd -- /path/to/project
```

Projects given on the command line are served immediately; a client can open
more over the socket. One daemon per configuration directory: a second refuses
to start rather than splitting the fleet in two. `SIGTERM`, `SIGINT`, or a closed console stops it
and terminates its panes. `DISPATCH_CONFIG_DIR` gives a separate daemon its own
endpoint, harnesses, and log.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE) for attribution of
vendored and derived code.
