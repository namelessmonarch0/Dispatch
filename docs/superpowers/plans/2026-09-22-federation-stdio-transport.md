# Federation F2a — Stdio Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Dispatch connection can be a child process's stdin and stdout rather than a Unix socket, so a later slice can make that child `ssh <host> dispatchd --stdio`.

**Architecture:** `ipc::Connection` holds boxed halves plus an optional child it reaps on drop. A new `over_command` constructor spawns a program and uses its pipes. `Wire` remembers a `Dial` — an endpoint or a command — so the existing supervisor can reconnect either kind. `dispatchd --stdio` connects to a daemon's socket and pumps bytes between it and its own stdin/stdout, never parsing what it carries.

**Tech Stack:** Rust 2024 (1.85+), std only (`std::process`, `std::os::unix::net`), tracing, clap. `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt`.

**Spec:** `docs/superpowers/specs/2026-09-22-federation-stdio-transport-design.md`

## Global Constraints

- Rust edition 2024, rust-version as pinned in the workspace `Cargo.toml`. Do not raise it.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets` (zero warnings) and `cargo test --workspace` must pass before every commit.
- TDD: write the failing test, RUN it, and record the failure before implementing.
- `dispatch-proto` is not edited. The wire protocol does not change in this slice.
- No new third-party dependencies. `std::process` covers everything here.
- A command transport must never parse the frames it carries — a bridge that understood the protocol would break a client the daemon itself could have served.
- Doc comments on every public item, in the house style: say WHY, not what. Match the prose of the file being edited.
- Tests that need a POSIX helper binary (`cat`, `sh`) are gated `#[cfg(unix)]` and say so, matching how `dispatch/tests/end_to_end.rs` gates its shell harness.
- Commit messages: Conventional Commits, ending with exactly these two lines and nothing after them:
  `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_01SpFTyYMpiVZcMnxN1neQno`

## File structure

| File | Responsibility |
|---|---|
| `crates/dispatch-os/src/ipc.rs` | `Connection` with boxed halves, `from_halves`, `over_command`, `Drop` reaping the child, `StderrHint`. |
| `crates/dispatch-os/src/ipc/tests.rs` (existing inline `mod tests` in `ipc.rs`) | Transport tests: pair, loopback through `cat`, child reaped, missing program. |
| `crates/dispatch-client/src/lib.rs` | `Dial`, `Client::attach_over`, `connect`/`connect_within`/`supervise` taking a `Dial`. |
| `dispatchd/src/main.rs` | `--stdio` and `--endpoint`: connect to a daemon and pump bytes; start one if none is listening. |
| `dispatchd/src/bridge.rs` (new) | The pump itself, so `main.rs` stays argument handling and `bridge` is testable on its own. |
| `dispatch/src/main.rs` | `--daemon-command`, repeatable, whitespace-split. |
| `dispatch/tests/end_to_end.rs` | Two daemons, one reached over a socket and one over a bridged child; the bridge killed and the pane surviving. |
| `README.md` | What `--daemon-command` and `dispatchd --stdio` are for. |

---

### Task 1: A connection that does not name its stream type

**Files:**
- Modify: `crates/dispatch-os/src/ipc.rs:60-95` (the `Connection` struct, `connect_to`, `split`)
- Modify: `crates/dispatch-client/src/lib.rs:438` and `crates/dispatch-daemon/src/session.rs:1280` (the two production `split()` callers)
- Test: the inline `mod tests` at the bottom of `crates/dispatch-os/src/ipc.rs`

**Interfaces:**
- Produces: `Connection { reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>, child: Option<std::process::Child> }`; `Connection::from_halves(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Connection`; `Connection::split(self) -> (Box<dyn Read + Send>, Box<dyn Write + Send>)`.

- [ ] **Step 1: Write the failing test**

Add to the inline `mod tests` in `crates/dispatch-os/src/ipc.rs`:

```rust
    #[test]
    #[cfg(unix)]
    fn a_connection_can_be_built_from_halves_the_caller_already_has() {
        // Federation's transports are not all sockets: one of them is a child
        // process's two pipes. A connection has to be assemblable from
        // whatever the caller has.
        use std::os::unix::net::UnixStream;

        let (mine, theirs) = UnixStream::pair().expect("a socket pair");
        let reader = theirs.try_clone().expect("a reader half");

        let connection = Connection::from_halves(Box::new(reader), Box::new(mine));
        let (mut reader, mut writer) = connection.split();

        writer.write_all(b"ping").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).expect("reading succeeds");
        assert_eq!(&buf, b"ping");

        drop(theirs);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p dispatch-os built_from_halves`
Expected: FAIL — `no function or associated item named from_halves found for struct Connection`.

- [ ] **Step 3: Box the halves**

Replace the struct and `connect_to` in `crates/dispatch-os/src/ipc.rs`:

```rust
pub struct Connection {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    /// A child process whose pipes these are, when the transport is a
    /// command rather than a socket.
    ///
    /// Kept so that dropping the connection reaps the process: a client
    /// reconnects by dialling again, and a transport that left its child
    /// behind would leak one per attempt — invisibly, since nothing else
    /// holds a handle to it.
    child: Option<std::process::Child>,
}
```

```rust
    pub fn connect_to(endpoint: &std::path::Path) -> Result<Self, IpcError> {
        let (reader, writer) = pairing::dial(|| imp::connect(endpoint))?;
        Ok(Self {
            reader: Box::new(reader),
            writer: Box::new(writer),
            child: None,
        })
    }

    /// A connection over halves the caller already holds.
    ///
    /// The pairing dance that [`Self::connect_to`] runs exists to turn one
    /// dialable address into two one-way streams, which is what Windows needs
    /// and what a child process's pipes already are. A caller holding both
    /// halves has nothing left to pair.
    #[must_use]
    pub fn from_halves(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        Self {
            reader,
            writer,
            child: None,
        }
    }
```

and change `split`:

```rust
    /// Splits into a reader and a writer.
    ///
    /// The loop reads on one thread and writes from another, so neither
    /// blocks the other. Boxed rather than `impl Trait`: a connection's
    /// transport is chosen at runtime, and the type cannot be named at the
    /// boundary.
    pub fn split(self) -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
        (self.reader, self.writer)
    }
```

- [ ] **Step 4: Run the whole workspace**

Run: `cargo test --workspace`
Expected: PASS. The two production `split()` callers bind the halves and pass them on, so they need no change; if either fails to compile, bind the boxes rather than re-adding a type parameter.

- [ ] **Step 5: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add crates/dispatch-os crates/dispatch-client crates/dispatch-daemon
git commit  # message: refactor(os): let a connection carry any pair of halves
```

---

### Task 2: A connection over a command

**Files:**
- Modify: `crates/dispatch-os/src/ipc.rs` (add `over_command`, `StderrHint`, `impl Drop for Connection`)
- Test: the inline `mod tests` at the bottom of `crates/dispatch-os/src/ipc.rs`

**Interfaces:**
- Consumes: Task 1's `Connection` and `from_halves`.
- Produces: `Connection::over_command(program: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> Result<Connection, IpcError>`; `Connection::hint(&self) -> StderrHint`; `#[derive(Clone, Default)] pub struct StderrHint` with `pub fn first_line(&self) -> Option<String>`; `IpcError::Spawn { command: String, source: std::io::Error }`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    #[cfg(unix)]
    fn a_command_carries_bytes_both_ways() {
        // `cat` is a byte-for-byte loopback, so this proves the pipes are
        // wired the right way round and that nothing in between reframes.
        let connection = Connection::over_command(
            std::ffi::OsStr::new("cat"),
            &[],
        )
        .expect("cat exists");

        let (mut reader, mut writer) = connection.split();
        writer.write_all(b"ping\n").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).expect("reading succeeds");
        assert_eq!(&buf, b"ping\n");
    }

    #[test]
    fn a_command_that_is_not_there_names_itself() {
        // Over SSH this is the common failure — the far side has no dispatchd
        // — and a bare "not found" would not say what was looked for.
        let error = Connection::over_command(
            std::ffi::OsStr::new("dispatch-no-such-program"),
            &[],
        )
        .expect_err("it does not exist");

        assert!(
            error.to_string().contains("dispatch-no-such-program"),
            "{error}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn dropping_a_command_connection_reaps_its_child() {
        // A client reconnects by dialling again. A transport that left its
        // child running would leak one per attempt, invisibly.
        let connection = Connection::over_command(std::ffi::OsStr::new("cat"), &[])
            .expect("cat exists");
        let pid = connection.child_id().expect("a command transport has a child");

        drop(connection);

        // A reaped child's pid answers no signal; an unreaped one does.
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        assert!(!alive, "the child is gone");
    }

    #[test]
    #[cfg(unix)]
    fn a_commands_stderr_is_kept_for_the_error_that_needs_it() {
        // "Permission denied (publickey)" only exists on stderr, and without
        // it a failed attach is indistinguishable from a silent one.
        let connection = Connection::over_command(
            std::ffi::OsStr::new("sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from("echo nope 1>&2; sleep 5"),
            ],
        )
        .expect("sh exists");

        let hint = connection.hint();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while hint.first_line().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(hint.first_line().as_deref(), Some("nope"));
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p dispatch-os command`
Expected: FAIL — `no function or associated item named over_command`.

- [ ] **Step 3: Add the error variant**

In `crates/dispatch-os/src/ipc.rs`, beside the existing `IpcError` variants:

```rust
    /// A command that was supposed to speak for a daemon could not be started.
    #[error("cannot run {command}: {source}")]
    Spawn {
        /// The command line, for a message that says what was looked for.
        command: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
```

- [ ] **Step 4: Add `StderrHint`**

```rust
/// The first line a command wrote to stderr, if it wrote one.
///
/// A command transport fails in ways only its stderr explains — a refused SSH
/// key, a missing binary on the far side — and by the time the handshake times
/// out, that line is the only evidence of which happened.
#[derive(Clone, Default)]
pub struct StderrHint(std::sync::Arc<Mutex<Option<String>>>);

impl std::fmt::Debug for StderrHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StderrHint")
    }
}

impl StderrHint {
    /// The first line, once there is one.
    #[must_use]
    pub fn first_line(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn remember(&self, line: &str) {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if held.is_none() {
            *held = Some(line.to_string());
        }
    }
}
```

- [ ] **Step 5: Add `over_command`, `hint`, `child_id` and `Drop`**

```rust
impl Connection {
    /// A connection to the daemon a command speaks for.
    ///
    /// The command's stdout is the reader and its stdin is the writer. Its
    /// stderr is drained on a thread of its own: every line goes to the log,
    /// and the first is kept for the error that a failed handshake will
    /// otherwise report as mere silence.
    pub fn over_command(
        program: &std::ffi::OsStr,
        args: &[std::ffi::OsString],
    ) -> Result<Self, IpcError> {
        use std::process::{Command, Stdio};

        let mut described = program.to_string_lossy().into_owned();
        for arg in args {
            described.push(' ');
            described.push_str(&arg.to_string_lossy());
        }

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| IpcError::Spawn {
                command: described.clone(),
                source,
            })?;

        let reader = child.stdout.take().expect("stdout was piped");
        let writer = child.stdin.take().expect("stdin was piped");
        let hint = StderrHint::default();

        if let Some(stderr) = child.stderr.take() {
            let hint = hint.clone();
            let described = described.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                    tracing::warn!(command = %described, "{line}");
                    hint.remember(&line);
                }
            });
        }

        Ok(Self {
            reader: Box::new(reader),
            writer: Box::new(writer),
            child: Some(child),
            hint,
        })
    }

    /// What the command said on stderr, for an error that needs it.
    #[must_use]
    pub fn hint(&self) -> StderrHint {
        self.hint.clone()
    }

    /// The child's process id, when the transport is a command.
    #[must_use]
    pub fn child_id(&self) -> Option<u32> {
        self.child.as_ref().map(std::process::Child::id)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Asked to stop and then waited for: a child left unreaped is a
        // zombie, and one left running is an `ssh` nobody can see.
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
```

Add `hint: StderrHint` to the struct, and `hint: StderrHint::default()` to `connect_to` and `from_halves`. `split` now has to move the halves out of a type that has a `Drop` impl. Replace Task 1's version with this one, which keeps the child alive by handing it to the reader:

```rust
    /// Splits into a reader and a writer.
    ///
    /// The child, when there is one, rides with the reader: the halves outlive
    /// this `Connection`, and killing the process when it goes would close the
    /// transport the caller just took.
    pub fn split(mut self) -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
        let child = self.child.take();
        let reader = std::mem::replace(&mut self.reader, Box::new(std::io::empty()));
        let writer = std::mem::replace(&mut self.writer, Box::new(std::io::sink()));

        match child {
            Some(child) => (Box::new(ChildReader { reader, child }), writer),
            None => (reader, writer),
        }
    }
```

```rust
/// A reader that reaps its process when it is dropped.
///
/// `split` hands the halves to a client that may hold them for the life of a
/// connection; the child has to be owned by one of them or it would be killed
/// the moment the `Connection` went out of scope.
struct ChildReader {
    reader: Box<dyn Read + Send>,
    child: std::process::Child,
}

impl Read for ChildReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Drop for ChildReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
```

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo test -p dispatch-os`
Expected: PASS. `dropping_a_command_connection_reaps_its_child` drops the `Connection` without splitting it, so the `Drop` impl is the one under test; `a_command_carries_bytes_both_ways` splits, so `ChildReader` is what keeps `cat` alive.

- [ ] **Step 7: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add crates/dispatch-os
git commit  # message: feat(os): connect to the daemon a command speaks for
```

---

### Task 3: A dial that can be repeated

**Files:**
- Modify: `crates/dispatch-client/src/lib.rs` (`Wire::endpoint` → `dial`, `connect`, `connect_within`, `supervise`, plus the new constructor)
- Test: `crates/dispatch-client/src/tests.rs`

**Interfaces:**
- Consumes: Task 2's `Connection::over_command`, `Connection::hint`, `StderrHint`.
- Produces: `pub enum Dial { Endpoint(PathBuf), Command { program: OsString, args: Vec<OsString> } }`; `Client::attach_over(role: Role, name: &str, liveness: Liveness, program: OsString, args: Vec<OsString>) -> Result<Client, ClientError>`; `Client::attach_at` keeps its signature and builds `Dial::Endpoint` internally.

- [ ] **Step 1: Write the failing test**

In `crates/dispatch-client/src/tests.rs`. That file's tests serialise on a process-wide `LOCK` because they mutate `DISPATCH_CONFIG_DIR`; take it here too. This test needs no fake daemon — a command that refuses is exactly the case under test:

```rust
#[test]
#[cfg(unix)]
fn a_client_dialling_a_command_reports_what_the_command_said() {
    // The dial plumbing, without a bridge: a command that refuses and exits
    // must fail the attach with its own words rather than a bare timeout.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let error = Client::attach_over(
        Role::Interface,
        "test",
        Liveness::default(),
        std::ffi::OsString::from("sh"),
        vec![
            std::ffi::OsString::from("-c"),
            std::ffi::OsString::from("echo 'dispatchd: command not found' 1>&2; exit 127"),
        ],
    )
    .expect_err("the command refuses");

    assert!(
        error.to_string().contains("command not found"),
        "the command's own words reach the caller: {error}"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p dispatch-client dialling_a_command`
Expected: FAIL — `no function or associated item named attach_over`.

- [ ] **Step 3: Add `Dial`**

```rust
/// How a client reaches a daemon, and reaches it again after a drop.
///
/// Remembered rather than resolved once: a reconnection has to repeat the
/// original dial, and for a command transport that means respawning the
/// process. Without this a dropped connection over SSH could never come back.
#[derive(Debug, Clone)]
pub enum Dial {
    /// A socket on this machine.
    Endpoint(PathBuf),
    /// A command that speaks for a daemon on its own machine.
    Command {
        /// The program to run.
        program: OsString,
        /// Its arguments.
        args: Vec<OsString>,
    },
}

impl std::fmt::Display for Dial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Dial::Endpoint(path) => write!(f, "{}", path.display()),
            Dial::Command { program, args } => {
                write!(f, "{}", program.to_string_lossy())?;
                for arg in args {
                    write!(f, " {}", arg.to_string_lossy())?;
                }
                Ok(())
            }
        }
    }
}
```

- [ ] **Step 4: Dial through it**

Replace `Wire::endpoint: PathBuf` with `dial: Dial`, and give `connect` the two cases:

```rust
fn connect(name: &str, role: Role, dial: &Dial) -> Result<Connected, ClientError> {
    let (connection, hint) = match dial {
        Dial::Endpoint(endpoint) => match Connection::connect_to(endpoint) {
            Ok(connection) => (connection, None),
            Err(IpcError::NotRunning(_)) => {
                return Err(ClientError::NotRunning(endpoint.display().to_string()));
            }
            Err(error) => return Err(error.into()),
        },
        Dial::Command { program, args } => {
            let connection = Connection::over_command(program, args)?;
            let hint = connection.hint();
            (connection, Some(hint))
        }
    };

    let (mut reader, mut writer) = connection.split();

    // …the existing handshake, unchanged…
}
```

Three places in `connect` can fail after the transport is up: writing the
`Hello`, reading the answer, and an answer that is a refusal. Wrap each of
those three `Err` paths — and only those — with the command's own words:

```rust
/// The command's own words, folded into a failure that would otherwise read
/// as silence.
///
/// A command transport's real reason lives on its stderr: an SSH key refused,
/// a binary missing on the far side. Neither reaches the protocol, so neither
/// reaches the caller unless it is carried here.
fn with_hint(error: ClientError, hint: Option<&StderrHint>) -> ClientError {
    match hint.and_then(StderrHint::first_line) {
        Some(line) => ClientError::Handshake(format!("{error}: {line}")),
        None => error,
    }
}
```

The handshake *timeout* in `connect_within` keeps its current message: it is
raised outside the thread that holds the hint, and the thread may still be
waiting. What it gains instead is the dial in the text, which for a command is
the command line — change its message to
`format!("{dial} did not answer within {patience:?}")`.

`connect_within` takes `&Dial` and clones it for its thread. `supervise`'s
reconnect arm calls `connect_within(&wire.name, wire.role, &wire.dial,
HANDSHAKE_TIMEOUT)` — which now respawns a command transport without knowing
that it has.

- [ ] **Step 5: Add the constructor**

```rust
    /// Connects and shakes hands with the daemon a command speaks for.
    ///
    /// The command is respawned on every reconnection, so it has to be one
    /// that can be run again — `ssh host dispatchd --stdio` is the case this
    /// exists for.
    pub fn attach_over(
        role: Role,
        name: &str,
        liveness: Liveness,
        program: OsString,
        args: Vec<OsString>,
    ) -> Result<Self, ClientError> {
        Self::attach_dialling(role, name, liveness, Dial::Command { program, args })
    }
```

Rename the body of today's `attach_at` to `attach_dialling(role, name, liveness, dial: Dial)`, and keep `attach_at` as the `Dial::Endpoint` caller so no existing call site changes.

- [ ] **Step 6: Write the reconnect test**

```rust
#[test]
#[cfg(unix)]
fn a_command_that_dies_is_respawned() {
    // The supervisor reconnects by repeating the dial. For a command that
    // means running it again, which is the whole reason `Dial` is remembered
    // rather than resolved once.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let dir = std::env::temp_dir().join(format!("dispatch-client-{}-respawn", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir is writable");
    let counter = dir.join("runs");

    // A canned Hello, written exactly as `Frame` writes one, so the client's
    // handshake completes without a daemon behind it. Then the command exits,
    // which is the event under test: the reader sees EOF, the connection is
    // lost, and the supervisor has to dial again.
    let mut encoded = Vec::new();
    Frame::write(&mut encoded, &welcome()).expect("writing succeeds");
    let escaped: String = encoded.iter().map(|b| format!("\\x{b:02x}")).collect();

    let program = std::ffi::OsString::from("sh");
    let args = vec![
        std::ffi::OsString::from("-c"),
        std::ffi::OsString::from(format!(
            "echo ran >> {}; printf '{}'; sleep 0.2",
            counter.display(),
            escaped
        )),
    ];

    let client = Client::attach_over(
        Role::Interface,
        "test",
        Liveness::default(),
        program,
        args,
    )
    .expect("the command answers the handshake");

    let deadline = Instant::now() + PATIENCE;
    while client.generation() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        client.generation() >= 2,
        "the supervisor dialled again after the command exited"
    );
    assert!(
        std::fs::read_to_string(&counter)
            .unwrap_or_default()
            .lines()
            .count()
            >= 2,
        "the command ran more than once"
    );

    drop(client);
    let _ = std::fs::remove_dir_all(&dir);
}
```

`Liveness::default()` is enough: the command exits, so the reader reaches EOF
and the connection is marked lost immediately — the liveness timer is for a
socket that goes quiet while staying open, which is not this case. `printf`
with `\xNN` escapes is POSIX `sh`, which is why the test is Unix-only.

- [ ] **Step 7: Run both tests and watch them pass**

Run: `cargo test -p dispatch-client`
Expected: PASS, including every existing test — `attach_at` still resolves an endpoint and dials it.

- [ ] **Step 8: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add crates/dispatch-client
git commit  # message: feat(client): remember how a daemon was dialled
```

---

### Task 4: `dispatchd --stdio`

**Files:**
- Create: `dispatchd/src/bridge.rs`
- Modify: `dispatchd/src/main.rs` (the `Args` struct and an early return before the listener is bound)
- Test: `dispatchd/tests/serves_clients.rs`

**Interfaces:**
- Consumes: Task 1's `Connection::from_halves` and the existing `Connection::connect_to`.
- Produces: `dispatchd --stdio [--endpoint <path>]`; `bridge::run(endpoint: &Path) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing test**

In `dispatchd/tests/serves_clients.rs`, following that file's existing helpers for standing a daemon up on a temporary configuration directory:

```rust
#[test]
#[cfg_attr(windows, ignore = "the bridge test drives a POSIX pipeline")]
fn a_bridge_carries_a_clients_hello_to_the_daemon() {
    // The bridge is a byte pump: what a client writes to its stdin has to
    // reach the daemon, and the daemon's answer has to come back on stdout.
    // It must never parse the frames — a bridge that understood the protocol
    // would break a client this daemon could otherwise serve.
    let fixture = Fixture::new("bridge");
    let daemon = Daemon::start(&fixture);

    let client = dispatch_client::Client::attach_over(
        dispatch_proto::Role::Interface,
        "bridge-test",
        dispatch_client::Liveness::default(),
        dispatchd_binary().into(),
        vec![
            "--stdio".into(),
            "--endpoint".into(),
            fixture.endpoint().into(),
        ],
    )
    .expect("the bridge reaches the daemon");

    assert!(client.is_connected());
    assert_eq!(client.device(), "local", "the daemon named itself through the pipe");

    drop(client);
    daemon.stop();
}
```

Add `Fixture::endpoint()` returning `<config dir>/dispatchd.sock` and a `dispatchd_binary()` helper returning `env!("CARGO_BIN_EXE_dispatchd")` if the file does not already have them.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p dispatchd a_bridge_carries`
Expected: FAIL — `unexpected argument '--stdio' found`.

- [ ] **Step 3: Write the pump**

Create `dispatchd/src/bridge.rs`:

```rust
//! Carrying one client's frames to a daemon over this process's own pipes.
//!
//! What makes a remote machine reachable: `ssh host dispatchd --stdio` puts
//! this on the far end of an SSH session, and the client on the near end
//! speaks the ordinary protocol into it.
//!
//! Deliberately ignorant of what it carries. A bridge that parsed frames
//! would refuse a version it did not know — and so break a client the daemon
//! behind it could have served perfectly well.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use dispatch_os::ipc::Connection;

/// Pumps bytes between this process's stdin/stdout and the daemon on
/// `endpoint`, until either side closes.
pub fn run(endpoint: &Path) -> Result<()> {
    let connection = Connection::connect_to(endpoint)
        .with_context(|| format!("cannot reach the daemon on {}", endpoint.display()))?;
    let (mut from_daemon, mut to_daemon) = connection.split();

    // One thread each way: a pump that read and wrote on one thread would
    // deadlock the moment both directions had something to say.
    let outward = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let _ = std::io::copy(&mut stdin, &mut to_daemon);
        // Closing tells the daemon the client has gone, rather than leaving
        // it holding a connection nothing will ever speak on again.
        drop(to_daemon);
    });

    let mut stdout = std::io::stdout().lock();
    let _ = std::io::copy(&mut from_daemon, &mut stdout);
    let _ = stdout.flush();

    // The daemon closed. The client's next write fails and it redials, so
    // waiting on the other pump would only delay this process's exit.
    drop(outward);
    Ok(())
}
```

Declare it in `dispatchd/src/main.rs` with `mod bridge;`.

- [ ] **Step 4: Add the arguments**

On `Args` in `dispatchd/src/main.rs`:

```rust
    /// Carry one client's frames to this machine's daemon over stdin and
    /// stdout instead of listening on a socket.
    ///
    /// What `ssh host dispatchd --stdio` runs. The agents stay with the
    /// long-lived daemon, so the SSH connection dropping costs the view and
    /// nothing else.
    #[arg(long)]
    stdio: bool,

    /// The daemon to bridge to, when it is not this configuration's own.
    #[arg(long, value_name = "PATH")]
    endpoint: Option<PathBuf>,
```

and in `main`, after logging is initialised and before the harnesses are written or the listener is bound:

```rust
    if args.stdio {
        let endpoint = match args.endpoint.clone() {
            Some(endpoint) => endpoint,
            None => dispatch_os::ipc::endpoint().context("failed to locate the endpoint")?,
        };

        // Nothing listening yet: start a daemon the way a client does, then
        // bridge to it. The agents have to outlive this pipe.
        if dispatch_os::ipc::Connection::connect_to(&endpoint).is_err() {
            start_daemon_for(&endpoint, &args.projects)?;
        }

        return bridge::run(&endpoint).map(|()| ());
    }
```

- [ ] **Step 5: Start a daemon when none is listening**

Also in `dispatchd/src/main.rs`:

```rust
/// Starts a daemon for `endpoint` and waits until it answers.
///
/// The bridge is not the daemon: it exits with its SSH session, and agents
/// that died with it would make a remote machine useless for the one thing
/// the daemon exists to provide.
fn start_daemon_for(endpoint: &Path, projects: &[PathBuf]) -> Result<()> {
    let program = std::env::current_exe().context("failed to locate this binary")?;
    let args: Vec<std::ffi::OsString> = projects.iter().map(Into::into).collect();

    let pid = dispatch_os::process::spawn_detached(&program, &args)
        .with_context(|| format!("failed to start {}", program.display()))?;
    tracing::info!(pid, "started a daemon to bridge to");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if dispatch_os::ipc::Connection::connect_to(endpoint).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    anyhow::bail!("a daemon was started but never listened on {}", endpoint.display())
}
```

- [ ] **Step 6: Run the test and watch it pass**

Run: `cargo test -p dispatchd`
Expected: PASS.

- [ ] **Step 7: Write the second test — no daemon listening**

```rust
#[test]
#[cfg_attr(windows, ignore = "the bridge test drives a POSIX pipeline")]
fn a_bridge_starts_a_daemon_when_none_is_listening() {
    // A machine nobody has used yet still has to answer: the bridge starts
    // the daemon it needs, rather than failing and leaving the user to ssh in
    // and do it by hand.
    let fixture = Fixture::new("bridge-cold");

    let client = dispatch_client::Client::attach_over(
        dispatch_proto::Role::Interface,
        "bridge-test",
        dispatch_client::Liveness::default(),
        dispatchd_binary().into(),
        vec![
            "--stdio".into(),
            "--endpoint".into(),
            fixture.endpoint().into(),
            fixture.project().into(),
        ],
    )
    .expect("the bridge starts a daemon and reaches it");

    assert!(client.is_connected());

    drop(client);
    stop_recorded_daemon(&fixture);
}
```

`stop_recorded_daemon` kills the daemon by the pid file the way `dispatch/tests/end_to_end.rs` does; copy that helper into this file if it is not already there, since a daemon started by a test outlives it by design.

- [ ] **Step 8: Run it and watch it pass**

Run: `cargo test -p dispatchd a_bridge_starts_a_daemon`
Expected: PASS. If it fails because the started daemon inherits the wrong configuration directory, pass `DISPATCH_CONFIG_DIR` through in `start_daemon_for` — `spawn_detached` inherits this process's environment, which the bridge got from its own caller.

- [ ] **Step 9: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add dispatchd
git commit  # message: feat(dispatchd): bridge a client's frames to this machine's daemon
```

---

### Task 5: `--daemon-command`, and the whole path end to end

**Files:**
- Modify: `dispatch/src/main.rs` (the `Args` struct and the attach loop)
- Test: `dispatch/tests/end_to_end.rs`

**Interfaces:**
- Consumes: Task 3's `Client::attach_over`, Task 4's `dispatchd --stdio --endpoint`, and F1's `App::attach`.

- [ ] **Step 1: Write the failing end-to-end test**

In `dispatch/tests/end_to_end.rs`:

```rust
#[test]
#[cfg_attr(windows, ignore = "the test harness spawns a POSIX shell")]
fn a_machine_reached_over_a_bridge_outlives_its_transport() {
    // The slice's claim: the transport can die without the agents dying,
    // because the agents were never the transport's.
    let here = Fixture::new("stdio-a");
    let there = Fixture::new("stdio-b");

    let near = Daemon::start_named(&here, "near");
    let far = Daemon::start_named(&there, "far");

    let bridge = format!(
        "{} --stdio --endpoint {}",
        dispatchd_binary().display(),
        there.config.path().join("dispatchd.sock").display()
    );

    let mut app = Harness::spawn(
        &here,
        Size::new(200, 30),
        &[
            "--attach".to_string(),
            "--daemon-command".to_string(),
            bridge,
        ],
    );

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "near") && sidebar_contains(lines, "far")),
        "both machines are listed"
    );

    app.select_project("far");
    app.spawn_shell();
    app.send(b"echo over-the-bridge\r");
    assert!(
        app.wait_for(|lines| contains(lines, "over-the-bridge")),
        "a pane on the far machine echoes through the bridge"
    );

    // Kill the bridge child, not the daemon behind it.
    kill_bridge_to(&there.config.path().join("dispatchd.sock"));

    assert!(
        app.wait_for(|lines| sidebar_contains(lines, "far")),
        "the machine is still listed after its transport died"
    );
    app.send(b"echo still-here\r");
    assert!(
        app.wait_for(|lines| contains(lines, "still-here")),
        "and the pane still answers once the client has redialled"
    );

    near.stop();
    far.stop();
}
```

Add the helper it uses to that file, matching on the bridge's own endpoint
path so it cannot reach another test's bridge running in parallel — every
fixture's socket path is unique:

```rust
/// Kills the bridge processes talking to `endpoint`, leaving the daemon behind
/// them running.
///
/// Killing the transport rather than the daemon is the point: what the test
/// proves is that the agents were never the transport's to lose. Matched on
/// the endpoint path so a bridge belonging to another test running in parallel
/// is left alone.
#[cfg(unix)]
fn kill_bridge_to(endpoint: &std::path::Path) {
    let _ = std::process::Command::new("pkill")
        .args(["-f", &format!("--endpoint {}", endpoint.display())])
        .status();
}
```

and call it as `kill_bridge_to(&there.config.path().join("dispatchd.sock"))` in
place of `kill_bridge_children()`.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --test end_to_end a_machine_reached_over_a_bridge`
Expected: FAIL — `unexpected argument '--daemon-command' found`.

`dispatchd_binary()` may not exist in `end_to_end.rs` yet — that file finds
the daemon by taking `env!("CARGO_BIN_EXE_dispatch")` and replacing its file
name. Add the same helper here if it is missing:

```rust
/// The `dispatchd` binary beside the client binary under test.
fn dispatchd_binary() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_BIN_EXE_dispatch"));
    path.set_file_name(if cfg!(windows) { "dispatchd.exe" } else { "dispatchd" });
    path
}
```

- [ ] **Step 3: Add the flag**

On `Args` in `dispatch/src/main.rs`:

```rust
    /// Also attach to the daemon a command speaks for. Repeatable.
    ///
    /// Split on whitespace, with no shell: every command this is for —
    /// `ssh user@host dispatchd --stdio`, a wrapper, an absolute path — is
    /// whitespace-separated. A program whose path contains a space needs the
    /// machine registry, which holds the program and its arguments apart.
    #[arg(long = "daemon-command", value_name = "COMMAND")]
    daemon_commands: Vec<String>,
```

and beside the existing `--daemon` loop:

```rust
    for command in &args.daemon_commands {
        let mut words = command.split_whitespace().map(std::ffi::OsString::from);
        let Some(program) = words.next() else {
            app.set_status("--daemon-command was empty".to_string());
            continue;
        };
        let rest: Vec<std::ffi::OsString> = words.collect();

        match Client::attach_over(Role::Interface, CLIENT_NAME, Liveness::default(), program, rest)
        {
            Ok(client) => {
                client.subscribe();
                app.attach(client);
            }
            // One machine being unreachable is not a reason to refuse to
            // start: the others are why the user opened Dispatch. Not retried
            // until the machine registry supervises them.
            Err(error) => {
                tracing::warn!(%error, %command, "could not attach over a command");
                app.set_status(format!(
                    "{command} did not answer; not retried — restart Dispatch once it does"
                ));
            }
        }
    }
```

- [ ] **Step 4: Run the test and watch it pass**

Run: `cargo test --test end_to_end a_machine_reached_over_a_bridge`
Expected: PASS.

- [ ] **Step 5: Prove the test can fail**

Temporarily change `Dial::Command`'s reconnect arm in `crates/dispatch-client/src/lib.rs` to return without redialling, re-run this test, and confirm it fails at "the pane still answers once the client has redialled". Put it back. Paste both outputs into the report — this assertion is the slice's whole claim, and a test that passes without the respawn proves nothing.

- [ ] **Step 6: Check and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets && cargo test --workspace
git add dispatch
git commit  # message: feat(dispatch): attach to a daemon a command speaks for
```

---

### Task 6: Say what it is for

**Files:**
- Modify: `README.md` (the "More than one machine" section added by F1)

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Extend the section**

Under the existing "More than one machine" text, replace the closing line about SSH being the next slice with:

```markdown
A machine does not have to be reachable by a socket. `dispatchd --stdio` carries
one client's frames to whatever daemon its machine is running -- starting one if
none is listening -- so the transport can be anything that can run a command and
pipe bytes:

```sh
dispatch --attach --daemon-command "ssh tower dispatchd --stdio"
```

The agents belong to the daemon on that machine, not to the pipe, so a dropped
connection costs the view and nothing else: the client redials the same command
and the panes are still there. `--daemon-command` is split on whitespace and
runs no shell.

Remembering machines between runs -- `dispatch machine add`, and retrying one
that was asleep -- is the next slice.
```

- [ ] **Step 2: Check and commit**

```bash
cargo fmt --check && cargo test --workspace
git add README.md
git commit  # message: docs: say how a machine is reached over a command
```
