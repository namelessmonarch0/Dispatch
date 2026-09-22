# F2a — A connection that is not a Unix socket

Status: approved design, not yet implemented.
Date: 2026-09-22.
Follows: `docs/superpowers/specs/2026-09-21-federation-many-daemons-design.md` (F1, merged).

## The problem

A client can hold a connection per machine, but every one of those connections
is a Unix socket on this machine. `dispatch_os::ipc::Connection` holds two
concrete `imp::Stream`s, `Client` dials a `PathBuf`, and `Wire` reconnects by
redialling that same path. Nothing in the stack can reach another host.

The transport that gets there is SSH, and SSH hands you a child process with a
stdin and a stdout. This slice makes that shape a connection Dispatch can hold,
and proves it against a plain local child — no network, no credentials, no
remote host. F2b then supplies `ssh <target> dispatchd --stdio` as the command
and the registry that remembers it.

### Not in this slice

- **`machines.toml`, `dispatch machine add/list/remove`, the in-TUI add
  overlay, and the per-machine supervisor with backoff** — F2b.
- **SSH itself.** Nothing here mentions ssh; it is one command string among
  others, and F2b is where it is spelled.
- **Copying `dispatchd` to a remote.** A machine either has it or cannot be
  added.

## The transport

`Connection` stops naming its stream type:

```rust
pub struct Connection {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    /// A child whose pipes these are, kept so dropping the connection reaps
    /// the process rather than leaving an `ssh` behind for every attach.
    child: Option<std::process::Child>,
}
```

Boxed rather than generic: `Client`, `Wire` and the daemon's connection table
would otherwise carry type parameters for a choice none of them makes, and the
table holds mixed transports anyway. Dynamic dispatch on frame I/O is nothing
beside the syscall under it.

`split()` returns `(Box<dyn Read + Send>, Box<dyn Write + Send>)` instead of
`impl Read + Send`. Its three real callers (`dispatch-client/src/lib.rs:438`,
`dispatch-daemon/src/session.rs:1280`, and the ipc tests) bind the halves and
pass them on, so the change is the signature and nothing else.

Two constructors join `connect_to`:

```rust
/// A connection over halves the caller already has.
pub fn from_halves(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self;

/// A connection to the daemon a command speaks for.
///
/// The command's stdout is the reader and its stdin is the writer, which is
/// already the pair the pairing dance exists to produce — so a command
/// transport skips it. Its stderr is drained to the log: when `ssh` refuses a
/// key, that refusal is the only thing that explains the failure.
pub fn over_command(program: &OsStr, args: &[OsString]) -> Result<Self, IpcError>;
```

The pairing token dance stays exactly as it is for sockets. Windows still
cannot duplex one pipe, and a command transport already arrives as two.

**`Drop` for `Connection`** kills the child if there is one. An `ssh` left
running per dropped connection is a process leak the user cannot see, and the
supervisor drops connections on every reconnect.

## Dialling, and dialling again

`Wire::endpoint: PathBuf` becomes:

```rust
enum Dial {
    /// A socket on this machine.
    Endpoint(PathBuf),
    /// A command that speaks for a daemon on its own machine.
    Command { program: OsString, args: Vec<OsString> },
}
```

`connect`, `connect_within` and the reconnect arm in `supervise` take a `&Dial`
instead of a `&Path`. Without this a dropped SSH connection could never come
back, which would make the supervisor F2b asks for impossible.

`Client` gains one constructor beside `attach_at`:

```rust
pub fn attach_over(
    role: Role,
    name: &str,
    liveness: Liveness,
    program: OsString,
    args: Vec<OsString>,
) -> Result<Self, ClientError>;
```

Everything else about a `Client` is unchanged: the same handshake, the same
generation counter, the same outbox, the same liveness checks. A remote daemon
is a daemon.

## The far side: `dispatchd --stdio`

```
dispatchd --stdio [projects…]
```

Does not listen. It connects to *its own machine's* endpoint and pumps bytes:
stdin into the socket, the socket into stdout, until either end closes. Two
threads, no framing — it is a pipe, and it must never parse what it carries, or
a protocol version it does not know would break a client the daemon could have
served.

If nothing is listening, it starts a daemon the way the client does — spawn the
`dispatchd` binary detached with the project arguments, wait for the socket —
and then bridges to it. The agents belong to that long-lived daemon, so an SSH
connection dropping costs the view and nothing else. That is the whole reason
the daemon exists, and a `--stdio` that owned the agents itself would give it
away.

Exit status says what happened: 0 when either side closed cleanly, non-zero
with a message on stderr when the daemon could not be reached or started. The
client surfaces that stderr, so `dispatchd: command not found` reaches the user
rather than a bare timeout.

## Reaching it from the command line

`--daemon <endpoint>` gains a sibling:

```
dispatch --attach --daemon-command "dispatchd --stdio"
```

Repeatable, like `--daemon`. The value is split on whitespace: no quoting, no
shell. Every command this is for — `ssh user@host dispatchd --stdio`, a
wrapper, an absolute path — is whitespace-separated, and F2b's registry holds
the program and its arguments as separate fields rather than a string to
re-parse. A command whose path has a space in it is the one case this cannot
express, and F2b's registry is where that is answered.

A command that fails to start is reported the way F1 reports an endpoint that
does not answer: a status line naming it, and no retry until F2b's supervisor
exists.

## Failure cases

| Case | Behaviour |
|---|---|
| The command does not exist | `over_command` fails with the program name; the client reports it and Dispatch starts without that machine. |
| The command starts and says nothing | The existing handshake timeout fires, with the command in the message. |
| The command exits mid-session | The reader sees EOF, `Wire` marks the connection lost, and the supervisor respawns the command — the same path a closed socket takes. |
| `dispatchd --stdio` cannot reach or start its daemon | It exits non-zero with a message on stderr; that stderr is in the client's log and its first line is the status the user sees. |
| The connection is dropped | `Drop` kills the child. No stray `ssh`. |
| The remote daemon restarts underneath the bridge | The bridge's socket closes, the bridge exits, the client respawns it, and the new connection's generation bump rebuilds that machine's rows — F1's per-attachment reconnect, unchanged. |

## Testing

- **`dispatch-os`** — `from_halves` carries frames over a `UnixStream::pair()`;
  `over_command` against `cat`, which is a byte-for-byte loopback, proves the
  pump and the framing survive a process boundary; a dropped connection leaves
  no child (check the pid is gone); a command that does not exist fails with
  its name in the error. The `cat` tests are Unix-only and say so.
- **`dispatch-client`** — `attach_over` completes a handshake against a fake
  daemon reached through `cat`-style loopback or a spawned bridge; killing the
  child bumps the generation and the client reconnects by respawning, with the
  same `Subscribe` replay a socket reconnect does.
- **`dispatchd`** — `--stdio` bridges to a listening daemon: frames written to
  its stdin reach the daemon and its answers come back on stdout. A second test
  covers no daemon listening: the bridge starts one, then bridges.
- **End to end** — `dispatch --attach --daemon-command "<dispatchd path>
  --stdio"` against a temporary config directory: both machines listed (the
  local socket and the bridged one, which is the same daemon reached two ways
  — so the test uses a second config directory for the bridge), a pane spawned
  through the bridge that echoes what is typed, then the bridge child killed
  and the pane still there after the client respawns it. That last step is the
  slice's claim: the transport can die without the agents dying.

## Follow-ups this slice creates

- F2b: `machines.toml`, the `machine` verbs, the in-TUI overlay, and a
  supervisor per registered machine so a machine that was down at startup joins
  when it wakes.
- A command with a space in its path needs the registry's structured form; the
  `--daemon-command` flag cannot express it.
