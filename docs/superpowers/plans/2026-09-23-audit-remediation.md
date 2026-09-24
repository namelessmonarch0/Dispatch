# Codebase Audit Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the eleven findings (A01–A11) in the 2026-09-23 audit: contain a stalled pane or peer, bound what any one client can cost, enforce the delegation cap and the handshake, make the registries transactional, make the Windows transport, process trees and task prompts safe, and bring the dependency and compiler policy in line with what CI checks.

**Architecture:** Three shared building blocks carry most of the fixes. A `Closer` in `dispatch-os` ends a connection from outside the threads using it, so the daemon can hang up on a client and the client can abandon a peer without first taking a lock a blocked write holds. Byte budgets sit on every queue that used to be unbounded — pane input, pane output, each client's outbox — and every budget is enforced by refusing or hanging up, never by blocking the daemon's loop. On Windows, Dispatch creates its own processes (ConPTY panes and command transports) suspended and inside a Job Object, so a tree is killed as a tree; task prompts reach `cmd.exe`-wrapped agents through a file on standard input, never on a command line.

**Tech Stack:** Rust 2024, MSRV 1.89 after Task 2. std threads and `mpsc`, `windows-sys` 0.61 (Windows only), `portable-pty` 0.9 (Unix only after Task 16), ratatui 0.30 / crossterm 0.29 after Task 3, `cargo-audit` in CI.

**Spec:** `docs/2026-09-23-codebase-audit-handoff.md` (the audit handoff; its "Acceptance" paragraphs are the requirements). The probe it cites is `docs/2026-09-23-audit-probes.rs`; each probe becomes a regression test below.

## Decisions already taken with the user

- All eleven findings are in scope. Windows work (A01–A03) is verified by pushing the work branch and running the existing Windows CI job through a draft pull request; nothing merges to `main` without the user's say-so.
- A03 is fixed by owning the Windows spawn: processes are created suspended, placed in a Job Object, then resumed. `portable-pty` stays for Unix only.
- A01 is fixed by delivering the task on standard input from a file. Built-in harness files still unedited are upgraded in place; edited ones are refused on Windows with a message saying how to fix them.
- A07: a request approved when its parent is already at the cap is refused (`DelegateOutcome::Refused`), not queued.
- A11: the floor becomes Rust 1.89 — 1.88 already compiles the workspace, but Task 13 uses `std::fs::File::lock`, stable since 1.89.
- CI only runs on `push` to `main` and on `pull_request`, so the branch gets a draft PR as soon as Task 1 lands.

## Global Constraints

- Rust edition 2024. `rust-version` is `1.89` from Task 2 on; nothing may need a newer compiler (CI's MSRV job enforces it).
- Work on branch `audit-remediation`, cut from `main` at `bd49298`. Never push to `main`. Open the draft PR after Task 1.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked` pass before every commit. Run the workspace, not one package: `dispatch`'s end-to-end tests use the `dispatchd` binary the last workspace build left beside it.
- Any task touching `#[cfg(windows)]` code also passes the Windows cross-check before its commit:
  `RUSTUP_HOME=<isolated rustup home> ~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings`
  (set `LIBGHOSTTY_VT_ZIG_SYSTEM_DIR` to the main checkout's `vendor/libghostty-vt/zig-pkg` so Zig does not refetch). This compiles; it does not run. Windows behaviour is only proven by the CI job.
- TDD: write the failing test, RUN it, and record the failure before implementing. A Windows-only test cannot be run locally; say so in the task's notes and rely on CI.
- `#[cfg(windows)]` appears only in `crates/dispatch-os` (the crate's own doc says so). Tests elsewhere that only mean something on Windows use `#[cfg_attr(not(windows), ignore = "...")]`; tests that need a POSIX shell use `#[cfg(unix)]`, matching the existing files.
- No new third-party dependencies, except: `windows-sys` features in `dispatch-os`, `portable-pty` moving from `dispatch-pty` to `dispatch-os` (Unix only), and the ratatui/crossterm upgrade.
- `ProtocolError` gains no variant (externally tagged, no `Unknown`): new reasons travel as `ProtocolError::Other`. `dispatch_proto::VERSION` stays `1.1`; nothing on the wire changes shape.
- Budgets (one place each, named constants with a doc comment saying why that number):
  - pane input waiting for a pane: `dispatch_pty::INPUT_BUDGET` = 8 MiB, a write refused whole past it unless the queue is empty;
  - pane output in flight: 32 chunks of 8 KiB (`OUTPUT_CHUNKS`); one `drain` hands over at most `DRAIN_BUDGET` = 128 KiB (plus the chunk that crosses it);
  - per-client live traffic waiting in the daemon: `Budgets::outbox_bytes` = 32 MiB;
  - daemon event backlog: 1024 events (`EVENT_BACKLOG`);
  - connected clients: `Budgets::max_clients` = 64;
  - a client's `Hello`: `Budgets::handshake` = 10 s; a started frame: `Budgets::frame` = 30 s;
  - a connection's pairing preamble: 2 s (unchanged); connections still announcing themselves at once: 32 (`MAX_ANNOUNCING`); halves waiting for a partner: 64, each for at most 10 s;
  - frame payload read in chunks of at most 64 KiB, never reserved from the length prefix;
  - a client write that has not finished within `Liveness::silence` loses the connection.
- Doc comments on every public item, and on private items where the file already does so, in the house style: say WHY, not what. Match the prose of the file being edited.
- Commit messages: Conventional Commits, ending with exactly this line and nothing after it:
  `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`

## Review Focus

1. **A pane closed while its input is stalled.** Someone pastes into an agent that has stopped reading, then closes the pane. Expect the close to take effect at once and nothing left holding the pane. Pinned in Task 8 (`a_stalled_pane_does_not_stall_the_daemon`, its close step).
2. **A client hung up on for falling behind, reconnecting.** Expect the replay it gets to describe every pane and its recent output, the same as any late subscriber. Pinned in Task 11 (`a_client_that_stops_reading_is_hung_up_and_can_come_back`).
3. **A peer that keeps talking but stops reading.** Silence never trips, so only the write deadline can notice. Expect the client to redial within the silence bound after a large send. Pinned in Task 12 (`a_peer_that_talks_but_never_reads_is_given_up_on`).
4. **Two Dispatch processes keeping projects at the same moment.** Expect both roots kept and the file never unreadable. Pinned in Task 13 (`two_processes_remembering_at_once_keep_both`).
5. **A Windows user whose profile path has a space.** The task file lives under it. Expect the prompt delivered exactly. Pinned in Task 17 (`a_task_reaches_a_cmd_wrapped_agent_exactly`, which puts the task directory under a path with a space).

## File structure

| File | Responsibility |
|---|---|
| `crates/dispatch-os/Cargo.toml` | `windows-sys` features; `portable-pty` (Unix) from Task 16. |
| `crates/dispatch-os/src/host.rs` | Correct `GetComputerNameW` import (Task 1). |
| `crates/dispatch-os/src/ipc.rs` | `Closer`; `Listener::bind_to`; preamble reads off the accept loop with a deadline; owner-only, local-only Windows pipe; command transports spawned contained. |
| `crates/dispatch-os/src/ipc/pairing.rs` | Halves expire and are capped. |
| `crates/dispatch-os/src/process.rs` | `spawn_contained`; Windows Job Object registry behind `terminate_tree`; test helpers `is_running`, `descendants`. |
| `crates/dispatch-os/src/pty.rs` (new, Task 16) + `pty/windows.rs` | Starting a process in a pseudoterminal: `portable-pty` on Unix, Dispatch's own ConPTY spawn on Windows. |
| `crates/dispatch-os/src/paths.rs` | `create_private` for task files (Task 17). |
| `crates/dispatch-proto/src/frame.rs` | Chunked payload read; `Frame::read_watched`. |
| `crates/dispatch-pty/src/session.rs` | Input writer thread with `INPUT_BUDGET`; bounded output channel with `DRAIN_BUDGET`; spawns through `dispatch_os::pty` from Task 16. |
| `crates/dispatch-daemon/src/session.rs` | Handshake state, cap re-check at approval, hang-ups, deadlines, quotas, task files. |
| `crates/dispatch-daemon/src/outbox.rs` (new) | Per-client queue counted in bytes: `Outbox`, `Inbox`. |
| `crates/dispatch-daemon/src/budgets.rs` (new) | `Budgets`, the per-client limits. |
| `crates/dispatch-daemon/src/task_file.rs` (new, Task 17) | A task written to a private file and removed with its pane. |
| `crates/dispatch-daemon/src/delegation.rs` | One refusal predicate used at request and at approval. |
| `crates/dispatch-client/src/lib.rs` | `Line` per connection; lifecycle without the writer lock; write deadline; Welcome version check. |
| `crates/dispatch-config/src/store.rs` (new) | Locked read-modify-write with atomic replace. |
| `crates/dispatch-config/src/projects.rs`, `machines.rs` | Every change through `store::update`. |
| `crates/dispatch-config/src/harness.rs`, `defaults.rs`, `lib.rs` | `TaskInput`, `TaskRun`, the Windows shell check, upgrading unedited built-ins. |
| `crates/dispatch-config/harnesses/*.toml`, `harnesses/superseded/*.toml` (new) | New Windows task forms; the exact bodies they replace. |
| `Cargo.toml`, `Cargo.lock`, `README.md`, `.github/workflows/ci.yml` | MSRV 1.89, ratatui 0.30, MSRV and advisory jobs. |
| `dispatch/tests/end_to_end.rs` | Windows build fix (Task 1). |
| `docs/security-model.md` (new), `docs/superpowers/federation-handoff.md` | The trust model; the Windows process-tree item closed. |

---

### Task 1: Put CI back to green

Main has been red on all three platforms since 2026-09-23 00:17. Nothing below can be judged by CI until it is green again, so this comes first.

Three causes, each confirmed:

- Windows does not compile: `GetComputerNameW` lives in `Win32::System::WindowsProgramming` in `windows-sys` 0.61, not `SystemInformation`; `ipc.rs`'s test module imports `Duration` that only Unix tests use; `dispatch/tests/end_to_end.rs` compiles a test on Windows that calls the Unix-only `kill_bridge_to`. (Reproduced with the Windows cross-check; with these three fixed it passes.)
- Linux: three client tests build a canned `Welcome` for `sh -c "printf '...'"` with `\xHH` escapes. Ubuntu's `/bin/sh` is dash, whose `printf` has no `\x` — the bytes arrive as text, the handshake fails, and the tests panic on `expect("the command answers the handshake")`. POSIX `printf` understands three-digit octal escapes everywhere.
- macOS: `a_dial_that_never_answers_leaves_no_process_behind` checks the grandchild the instant the dial gives up. It has been sent SIGTERM, but until launchd reaps it, `kill(pid, 0)` still succeeds on the zombie. Every sibling test polls to a deadline; this one must too.

**Files:**
- Modify: `crates/dispatch-os/src/host.rs:49`
- Modify: `crates/dispatch-os/Cargo.toml` (the `windows-sys` feature list)
- Modify: `crates/dispatch-os/src/ipc.rs:749`
- Modify: `dispatch/tests/end_to_end.rs:1243-1244`
- Modify: `crates/dispatch-client/src/tests.rs:742,898,1185` and the assertion near `:870`

**Interfaces:**
- Consumes: nothing new.
- Produces: `octal(bytes: &[u8]) -> String` in `crates/dispatch-client/src/tests.rs`, a test helper later client tests reuse to script a peer's bytes through `printf`.

- [ ] **Step 1: Create the branch**

```bash
git switch -c audit-remediation bd49298
```

- [ ] **Step 2: Reproduce the Windows failure locally**

Isolated toolchain, so the user's own rustup is untouched (skip the install if `$RUSTUP_HOME` already has it):

```bash
export RUSTUP_HOME=/private/tmp/dispatch-rustup   # any scratch directory
~/.cargo/bin/rustup toolchain install stable --profile minimal -c clippy -t x86_64-pc-windows-gnu --no-self-update
export LIBGHOSTTY_VT_ZIG_SYSTEM_DIR=$PWD/vendor/libghostty-vt/zig-pkg
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings
```

Expected: FAIL with `unresolved import windows_sys::Win32::System::SystemInformation::GetComputerNameW`.

- [ ] **Step 3: Fix the three Windows build errors**

`crates/dispatch-os/src/host.rs`, in `mod imp` (Windows):

```rust
    use windows_sys::Win32::System::WindowsProgramming::GetComputerNameW;
```

`crates/dispatch-os/Cargo.toml`, the Windows dependency line becomes:

```toml
[target."cfg(windows)".dependencies]
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_System_JobObjects", "Win32_System_Threading", "Win32_System_LibraryLoader", "Win32_System_Pipes", "Win32_Storage_FileSystem", "Win32_System_SystemInformation", "Win32_System_WindowsProgramming"] }
```

`crates/dispatch-os/src/ipc.rs`, top of `mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::time::Duration;
```

`dispatch/tests/end_to_end.rs`: the bridge test calls `kill_bridge_to`, which only exists on Unix, so it cannot be compiled on Windows at all — `ignore` is not enough:

```rust
#[test]
#[cfg(unix)]
fn a_machine_reached_over_a_bridge_outlives_its_transport() {
```

- [ ] **Step 4: Re-run the Windows cross-check**

Same command as Step 2. Expected: `Finished`, no warnings.

- [ ] **Step 5: Make the scripted peers portable**

In `crates/dispatch-client/src/tests.rs`, add beside `welcome()`:

```rust
/// `bytes` as a `printf` format string.
///
/// Three-digit octal escapes rather than `\xHH`: `printf` must understand
/// them under POSIX, and Ubuntu's `/bin/sh` is dash, whose `printf` has no
/// `\x` at all -- it printed the escapes as text and the handshake never
/// completed.
fn octal(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:03o}")).collect()
}
```

Replace each of the three `let escaped: String = encoded.iter().map(|b| format!("\\x{b:02x}")).collect();` lines with:

```rust
    let escaped = octal(&encoded);
```

- [ ] **Step 6: Poll for the grandchild in the dial test**

In `a_dial_that_never_answers_leaves_no_process_behind`, replace

```rust
    let grandchild = first_recorded_pid(&pid_file);
    assert!(
        !pid_is_alive(grandchild),
        "the dial's process tree outlived the handshake that walked away from it"
    );
```

with

```rust
    let grandchild = first_recorded_pid(&pid_file);

    // Polled, as every sibling test polls: the tree has been signalled, but a
    // killed process answers `kill(pid, 0)` until whoever inherited it reaps
    // it, and on macOS that is launchd, on its own schedule.
    let deadline = Instant::now() + PATIENCE;
    while pid_is_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !pid_is_alive(grandchild),
        "the dial's process tree outlived the handshake that walked away from it"
    );
```

- [ ] **Step 7: Prove the escapes under dash-like `printf`**

macOS has no dash, but POSIX `printf` in `/bin/sh -o posix` behaves the same for octal. Check one byte by hand:

```bash
sh -c "printf '\\101\\102'"; echo
```

Expected: `AB`.

- [ ] **Step 8: Run the gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Expected: all pass (baseline is 658 tests).

- [ ] **Step 9: Commit, push, open the draft PR**

```bash
git add crates/dispatch-os/src/host.rs crates/dispatch-os/Cargo.toml crates/dispatch-os/src/ipc.rs dispatch/tests/end_to_end.rs crates/dispatch-client/src/tests.rs Cargo.lock
git commit -m "fix: build on Windows again and make the command-dial tests portable

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push -u origin audit-remediation
gh pr create --draft --base main --title "Audit remediation (A01–A11)" --body "Work branch for docs/2026-09-23-codebase-audit-handoff.md. Draft so CI runs on every push; not for merge until reviewed.

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
```

- [ ] **Step 10: Wait for CI and read every job**

```bash
gh pr checks --watch
```

Expected: all three `test` jobs green. If Windows reveals test failures beyond compilation, they are pre-existing and are fixed here, one commit each, before Task 2 — the rest of the plan needs a green Windows job to mean anything.

---

### Task 2: Advertise the Rust version the workspace needs (A11)

The workspace says 1.85; `crates/dispatch-daemon/src/session.rs:1151` uses let chains, stable in 1.88. Checked on this machine: `cargo +1.88.0 check --workspace --all-targets --locked` passes today, and Task 13 will use `File::lock`, stable in 1.89 (`cargo +1.88.0` rejects it with E0658 `file_lock`). So the floor is 1.89, and CI builds with exactly that.

**Files:**
- Modify: `Cargo.toml:21`
- Modify: `README.md:15`
- Modify: `.github/workflows/ci.yml` (new `msrv` job)

**Interfaces:**
- Consumes: nothing.
- Produces: a CI job named `msrv (1.89)`.

- [ ] **Step 1: Show the old floor fails**

```bash
~/.cargo/bin/rustup toolchain install 1.85.0 --profile minimal --no-self-update   # with the isolated RUSTUP_HOME
~/.cargo/bin/cargo +1.85.0 check --workspace --all-targets --locked --target-dir target/msrv-1.85 2>&1 | grep -m3 -E "error|let chains"
```

Expected: FAIL (let chains / edition-2024 features unavailable).

- [ ] **Step 2: Raise the floor**

`Cargo.toml`:

```toml
rust-version = "1.89"
```

`README.md`, in "Building":

```markdown
- Rust 1.89 or newer (edition 2024). CI builds with exactly 1.89, so a
  change that needs a newer compiler fails there first.
```

- [ ] **Step 3: Build with exactly 1.89**

```bash
~/.cargo/bin/rustup toolchain install 1.89.0 --profile minimal --no-self-update
~/.cargo/bin/cargo +1.89.0 build --workspace --all-targets --locked --target-dir target/msrv-1.89
```

Expected: `Finished`.

- [ ] **Step 4: Add the CI job**

Append to `.github/workflows/ci.yml` under `jobs:`:

```yaml
  msrv:
    name: msrv (1.89)
    # The floor `rust-version` advertises, built exactly. `stable` moves every
    # six weeks and would never notice a let chain or a new std API arriving
    # under a floor that says otherwise -- which is how 1.85 went stale.
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v5

      - name: Install Rust 1.89
        uses: dtolnay/rust-toolchain@1.89.0

      - name: Install Zig 0.16.0
        uses: mlugg/setup-zig@v2
        with:
          version: 0.16.0

      - uses: Swatinem/rust-cache@v2
        with:
          key: msrv

      - name: Build
        run: cargo build --workspace --all-targets --locked
```

- [ ] **Step 5: Run the gates, commit, push**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add Cargo.toml README.md .github/workflows/ci.yml
git commit -m "build: advertise Rust 1.89 and build it exactly in CI

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
```

Expected: the `msrv (1.89)` job goes green on the PR.

---

### Task 3: Leave the unsound `lru` and unmaintained `paste` behind (A10)

Both come in through ratatui 0.29. ratatui 0.30.2 depends on `ratatui-core` 0.1.2, which takes `lru ^0.18` (resolves 0.18.5, past both advisories' patched ranges), and nothing in the 0.30 graph uses `paste`. Tried on a scratch checkout: with crossterm moved to 0.29 to match, the workspace compiles with no source change, all 658 tests pass (layout snapshots and approval wrapping included), and `cargo audit --deny warnings` scans 249 locked crates with nothing to report.

**Files:**
- Modify: `Cargo.toml:37-45` (ratatui, crossterm and the comment above them)
- Modify: `Cargo.lock`
- Create: `.cargo/audit.toml`
- Modify: `.github/workflows/ci.yml` (new `audit` job)

**Interfaces:**
- Consumes: the `msrv` job from Task 2 (it proves the new graph still builds on 1.89 — `time` 0.3.55, pulled in by ratatui's calendar widget, needs 1.88).
- Produces: a CI job named `advisories`.

- [ ] **Step 1: Show the warnings today**

```bash
cargo install cargo-audit --locked --root target/tools
target/tools/bin/cargo-audit audit --deny warnings
```

Expected: FAIL — `RUSTSEC-2026-0253` and `RUSTSEC-2026-0002` (lru 0.12.5), `RUSTSEC-2024-0436` (paste 1.0.15).

- [ ] **Step 2: Upgrade**

`Cargo.toml`:

```toml
# Interface
# `unstable-rendered-line-info` exposes `Paragraph::line_count`, the exact
# wrapped row count the render path itself computes. Three attempts at
# reimplementing that count by hand (a character sum, a per-line `div_ceil`,
# a two-row probe) were each wrong in a way tests at only one terminal width
# missed; the minor version is pinned, so "unstable" here means "the API may
# move under us on the next minor," not "unverified now." 0.30 rather than
# 0.29 because 0.29's layout cache pulled in an `lru` with two soundness
# advisories and the unmaintained `paste`.
ratatui = { version = "0.30", features = ["unstable-rendered-line-info"] }
crossterm = { version = "0.29", features = ["event-stream"] }
```

```bash
cargo update -p ratatui -p crossterm
cargo tree --locked -i lru
cargo tree --locked -i paste
```

Expected: `lru v0.18.x` under `ratatui-core`; `paste` — "did not match any packages".

- [ ] **Step 3: Scan again**

```bash
target/tools/bin/cargo-audit audit --deny warnings
```

Expected: exit 0, no warnings.

- [ ] **Step 4: Record the exception policy**

`.cargo/audit.toml`:

```toml
# CI runs `cargo audit --deny warnings`: a vulnerability, an unsound crate, an
# unmaintained crate and a yanked version all fail the build. That is the
# policy; an advisory is not waited out.
#
# An exception goes in `ignore` below only with a comment beside it naming
# the advisory, why Dispatch cannot reach the affected code or cannot yet
# upgrade, and a date by which it is looked at again. An exception without a
# date is not one.
[advisories]
ignore = []
```

- [ ] **Step 5: Add the CI job**

Append to `.github/workflows/ci.yml` under `jobs:`:

```yaml
  advisories:
    name: advisories
    # `cargo audit` alone exits 0 on unsound and unmaintained crates -- which
    # is how two lru soundness advisories sat in the lockfile unnoticed.
    # `--deny warnings` makes every class fail; exceptions live, dated, in
    # .cargo/audit.toml.
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v5
      - uses: taiki-e/install-action@v2
        with:
          tool: cargo-audit
      - name: Audit
        run: cargo audit --deny warnings
```

- [ ] **Step 6: Re-verify what the upgrade could have moved**

```bash
cargo test -p dispatch-layout --locked
cargo test -p dispatch --locked approval
~/.cargo/bin/cargo +1.89.0 build --workspace --all-targets --locked --target-dir target/msrv-1.89
```

Expected: pass (snapshots unchanged, no `.snap.new` files), and the 1.89 build finishes.

- [ ] **Step 7: Run the gates, commit, push**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add Cargo.toml Cargo.lock .cargo/audit.toml .github/workflows/ci.yml
git commit -m "build(deps): move to ratatui 0.30 and fail CI on any advisory

ratatui 0.29 pulled in lru 0.12.5 (RUSTSEC-2026-0253, RUSTSEC-2026-0002)
and paste (RUSTSEC-2024-0436). 0.30 resolves lru 0.18 and drops paste.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
```

Expected: `advisories` and `msrv (1.89)` green on the PR.

---
### Task 4: Enforce the live-child cap where subagents start (A07)

`delegate_request` checks the cap when a request arrives; `approve` never looks again. Two requests from a pane capped at one each see a free slot while pending, and both start when approved — the audit probe produced two running children with `max_live_per_parent = 1`. The fix is to ask the same question at the spawn boundary, with the same predicate, and refuse the loser out loud.

**Files:**
- Modify: `crates/dispatch-daemon/src/session.rs:820-927` (`approve`)
- Test: `crates/dispatch-daemon/src/session/tests.rs`

**Interfaces:**
- Consumes: `crate::delegation::refusal(depth, live, limits, has_task_form, harness) -> Option<String>` (unchanged), `Daemon::depth_of`, `Daemon::live_children`.
- Produces: test helper `long_task() -> &'static str` in `session/tests.rs` — a task that keeps running for 30 s on every platform (`sleep 30` is not a command under `cmd.exe`, so on Windows the old tests' "long" tasks exit at once).

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-daemon/src/session/tests.rs`:

```rust
/// A task that is still running thirty seconds from now, on every platform.
///
/// `sleep` is not a command under `cmd.exe`: there it fails at once, and a
/// test about what is *running* would pass for the wrong reason. No `>nul`:
/// from Task 17 the Windows fixture runs its task under PowerShell, where
/// that redirection fails.
fn long_task() -> &'static str {
    if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    }
}

/// Every `DelegateResolved` outcome among `messages`.
fn outcomes(messages: &[ServerMessage]) -> Vec<DelegateOutcome> {
    messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::DelegateResolved { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn approving_more_requests_than_the_cap_allows_starts_only_what_fits() {
    // Both requests are asked about while nothing is running, so both pass
    // the check on arrival. Approving them one after the other must still
    // start only one: the cap is on what runs, not on what is asked.
    let (mut daemon, project, _dir) = daemon_with_limits(
        "cap-at-approval",
        DelegationLimits {
            max_depth: 1,
            max_live_per_parent: 1,
            request_timeout_secs: 600,
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let first = ask_as(&mut daemon, 8, parent, long_task());
    let second = ask_as(&mut daemon, 9, parent, long_task());
    let requests: Vec<RequestId> = drain(&ui)
        .iter()
        .filter_map(|m| match m {
            ServerMessage::DelegatePending { request, .. } => Some(*request),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 2, "both fit while nothing runs yet");

    for request in requests {
        daemon.request_for_test(
            1,
            ClientMessage::DelegateDecision {
                request,
                approve: true,
                blanket: false,
            },
        );
    }

    assert_eq!(daemon.pane_count(), 2, "the parent and exactly one subagent");

    let mut told = outcomes(&drain(&first));
    told.extend(outcomes(&drain(&second)));
    assert_eq!(
        told.iter()
            .filter(|o| matches!(o, DelegateOutcome::Approved { .. }))
            .count(),
        1,
        "one caller is told it runs: {told:?}"
    );
    assert!(
        told.iter()
            .any(|o| matches!(o, DelegateOutcome::Refused { reason } if reason.contains("cap"))),
        "the other is told why it does not: {told:?}"
    );
}

#[test]
fn approvals_from_two_interfaces_do_not_share_one_slot() {
    // The same race with the approvals coming from two people at two
    // screens, which is the ordinary shape on a shared fleet.
    let (mut daemon, project, _dir) = daemon_with_limits(
        "cap-two-uis",
        DelegationLimits {
            max_depth: 1,
            max_live_per_parent: 1,
            request_timeout_secs: 600,
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let other_ui = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);
    let _ = drain(&other_ui);

    let _first = ask_as(&mut daemon, 8, parent, long_task());
    let _second = ask_as(&mut daemon, 9, parent, long_task());
    let requests: Vec<RequestId> = drain(&ui)
        .iter()
        .filter_map(|m| match m {
            ServerMessage::DelegatePending { request, .. } => Some(*request),
            _ => None,
        })
        .collect();

    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request: requests[0],
            approve: true,
            blanket: false,
        },
    );
    daemon.request_for_test(
        2,
        ClientMessage::DelegateDecision {
            request: requests[1],
            approve: true,
            blanket: false,
        },
    );

    assert_eq!(daemon.pane_count(), 2, "the parent and exactly one subagent");
}

#[test]
fn a_blanket_approved_pane_at_its_cap_is_refused_rather_than_started() {
    let (mut daemon, project, _dir) = daemon_with_limits(
        "cap-blanket",
        DelegationLimits {
            max_depth: 1,
            max_live_per_parent: 1,
            request_timeout_secs: 600,
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let _first = ask_as(&mut daemon, 8, parent, long_task());
    let request = pending(&drain(&ui)).expect("the first is asked about");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: true,
        },
    );

    let second = ask_as(&mut daemon, 9, parent, long_task());

    assert_eq!(daemon.pane_count(), 2, "blanket approval is not a second slot");
    assert!(
        outcomes(&drain(&second))
            .iter()
            .any(|o| matches!(o, DelegateOutcome::Refused { .. })),
        "the second caller is refused"
    );
}
```

Add `use dispatch_core::RequestId;` and `use dispatch_proto::DelegateOutcome;` to the test module's imports if `super::*` does not already bring them (it brings `RequestId` and `DelegateOutcome` from `session.rs`'s own imports).

- [ ] **Step 2: Run them to see the first two fail**

```bash
cargo test -p dispatch-daemon --locked cap -- --nocapture
```

Expected: `approving_more_requests_than_the_cap_allows_starts_only_what_fits` and `approvals_from_two_interfaces_do_not_share_one_slot` FAIL with `left: 3, right: 2`. The blanket test already passes (the request-time check covers it) and stays as a guard.

- [ ] **Step 3: Re-check at the spawn boundary**

In `approve`, replace the `let Some(launch) = ... else { ... }` block with:

```rust
        let launch = self
            .harnesses
            .get(harness)
            .and_then(|def| def.task_launch(task));

        // Asked again here, and not only when the request arrived: several
        // requests can each see a free slot while they wait, and every one
        // of them would start on approval. The cap is on what runs, so it is
        // enforced where things start running -- with the same predicate the
        // request was first judged by, so the two can never disagree.
        let depth = self.depth_of(parent);
        let live = self.live_children(parent);
        if let Some(reason) = crate::delegation::refusal(
            depth,
            live,
            self.limits,
            launch.is_some(),
            harness,
        ) {
            tracing::info!(%parent, %harness, %reason, "refused an approved delegation");
            self.resolve(request, caller, DelegateOutcome::Refused { reason });
            return;
        }
        let Some(launch) = launch else {
            // `refusal` refuses a missing form first, so this cannot be
            // reached; kept as a refusal rather than a panic all the same.
            self.resolve(
                request,
                caller,
                DelegateOutcome::Refused {
                    reason: format!("harness {harness:?} has no [task] form"),
                },
            );
            return;
        };
```

- [ ] **Step 4: Run the tests again**

```bash
cargo test -p dispatch-daemon --locked
```

Expected: all pass.

- [ ] **Step 5: Run the gates and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-daemon/src/session.rs crates/dispatch-daemon/src/session/tests.rs
git commit -m "fix(daemon): enforce the live-subagent cap when a request is approved

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Nothing is acted on for a client that has not been welcomed (A08, daemon state)

A rejected `Hello` removes the client from `self.clients`, but its reader thread goes on forwarding requests and `handle_request` never asks who sent them — the probe spawned a pane from a refused client. Requests before any `Hello` are served too. This task gives each client a handshake state and gates every request on it. Closing the rejected connection's two halves needs Task 6's `Closer` and is wired in Task 11; here the rejected client is forgotten, which is what stops its requests.

**Files:**
- Modify: `crates/dispatch-daemon/src/session.rs:92-102` (`Client`), `:266-323` (`handle`, `handle_request`'s `Hello` arm)
- Modify: `crates/dispatch-client/src/lib.rs:802-814` (`connect`: check the `Welcome`'s version)
- Test: `crates/dispatch-daemon/src/session/tests.rs`, `crates/dispatch-client/src/tests.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `Daemon::hang_up(&mut self, id: ClientId)` — forgets a client and everything it was waiting on. Task 10 and Task 11 extend it to close the connection.

- [ ] **Step 1: Write the failing daemon tests**

Append to `crates/dispatch-daemon/src/session/tests.rs`:

```rust
fn spawn_request(project: ProjectId) -> ClientMessage {
    ClientMessage::SpawnPane {
        project,
        harness: "shell".into(),
        size: (80, 24),
    }
}

#[test]
fn nothing_is_acted_on_before_a_hello() {
    let (mut daemon, project, _dir) = daemon("before-hello");
    let inbox = daemon.attach_for_test(1);

    daemon.request_for_test(1, spawn_request(project));

    assert_eq!(daemon.pane_count(), 0, "a request before Hello starts nothing");
    assert!(
        drain(&inbox)
            .iter()
            .any(|m| matches!(m, ServerMessage::Error { .. })),
        "and says why"
    );

    // The connection is over: a Hello now is too late to rescue it.
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, spawn_request(project));
    assert_eq!(daemon.pane_count(), 0);
    assert!(drain(&inbox).is_empty(), "nothing more reaches it");
}

#[test]
fn a_refused_client_cannot_act_afterwards() {
    // The audit's probe: an incompatible Hello is answered with an error, and
    // then a SpawnPane from the same client started a pane anyway.
    let (mut daemon, project, _dir) = daemon("refused-acts");
    let refused = daemon.attach_for_test(2);
    daemon.request_for_test(
        2,
        ClientMessage::Hello {
            version: dispatch_proto::Version {
                major: 99,
                minor: 0,
            },
            client: "incompatible".into(),
            role: dispatch_proto::Role::Interface,
        },
    );
    assert!(matches!(
        refused.try_recv(),
        Ok(ServerMessage::Error {
            error: ProtocolError::IncompatibleVersion { .. }
        })
    ));

    daemon.request_for_test(2, spawn_request(project));

    assert_eq!(daemon.pane_count(), 0, "a refused client spawned a pane");
}

#[test]
fn a_detached_client_cannot_act() {
    let (mut daemon, project, _dir) = daemon("detached-acts");
    let _inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.detach_for_test(1);

    // A request already read off the socket arrives after the reader said
    // the client left: events from one client are ordered, but a request
    // queued by a thread that is gone is still a request from nobody.
    daemon.request_for_test(1, spawn_request(project));

    assert_eq!(daemon.pane_count(), 0);
}
```

- [ ] **Step 2: Run them to see them fail**

```bash
cargo test -p dispatch-daemon --locked -- nothing_is_acted_on_before_a_hello a_refused_client_cannot_act_afterwards a_detached_client_cannot_act
```

Expected: all three FAIL with `left: 1, right: 0` (a pane was spawned).

- [ ] **Step 3: Give the client a handshake state and gate on it**

In `session.rs`, `struct Client` gains:

```rust
    /// Whether its `Hello` has been accepted.
    ///
    /// Nothing but a `Hello` is acted on before then: a peer that has not
    /// said which protocol it speaks may mean something else by every byte
    /// that follows, and one that was refused must not get to act anyway.
    ready: bool,
```

`handle`'s `Event::Attached` arm initialises `ready: false`.

At the top of `handle_request`:

```rust
    fn handle_request(&mut self, id: ClientId, message: ClientMessage) {
        let Some(client) = self.clients.get(&id) else {
            // Refused, hung up on, or detached. Its reader may still be
            // forwarding frames it had already read -- a peer can send a
            // request right behind a Hello it is about to be refused for --
            // and none of them is anyone's to act on.
            tracing::debug!(client = id, "ignoring a request from a client that is gone");
            return;
        };

        if !client.ready && !matches!(message, ClientMessage::Hello { .. }) {
            tracing::info!(client = id, "a client spoke before its Hello");
            self.send(
                id,
                ServerMessage::Error {
                    error: ProtocolError::Other(
                        "the connection must begin with a Hello".into(),
                    ),
                },
            );
            self.hang_up(id);
            return;
        }

        match message {
```

The `Hello` arm's refusal replaces `self.clients.remove(&id);` with `self.hang_up(id);`, and its acceptance becomes:

```rust
                if let Some(existing) = self.clients.get_mut(&id) {
                    existing.role = role;
                    existing.ready = true;
                }
```

Add beside `send`:

```rust
    /// Forgets a client, and whatever it was waiting on.
    ///
    /// What the daemon does to a client it will not serve any longer: a
    /// refused handshake, a protocol violation. Its outbox goes with it, so
    /// the writer thread sends what was already queued -- the refusal among
    /// it -- and stops.
    fn hang_up(&mut self, id: ClientId) {
        if self.clients.remove(&id).is_some() {
            tracing::info!(client = id, "hung up on a client");
        }
        self.abandon(id);
    }
```

- [ ] **Step 4: Run the daemon tests**

```bash
cargo test -p dispatch-daemon --locked
```

Expected: all pass, including the existing `an_incompatible_major_version_is_refused_and_the_client_dropped`.

- [ ] **Step 5: Write the failing client test**

Append to `crates/dispatch-client/src/tests.rs`:

```rust
#[test]
fn a_daemon_speaking_another_major_version_is_refused_by_the_client() {
    // The daemon checks the client's version; until now the client never
    // checked the daemon's, and would read every frame of a protocol it
    // does not speak.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("future-daemon");

    let _server = serve_one(
        ServerMessage::Welcome {
            version: Version {
                major: 99,
                minor: 0,
            },
            device: "future".into(),
        },
        |_| {},
        After::Silence,
    );

    let error = Client::attach_with("test", Liveness::default())
        .expect_err("a daemon from another major version is refused");

    assert!(
        matches!(
            error,
            ClientError::Refused(ProtocolError::IncompatibleVersion { .. })
        ),
        "expected an incompatible version, got {error:?}"
    );
}
```

- [ ] **Step 6: Run it to see it fail**

```bash
cargo test -p dispatch-client --locked a_daemon_speaking_another_major_version
```

Expected: FAIL — `attach_with` returns `Ok`, so `expect_err` panics.

- [ ] **Step 7: Check the version in `connect`**

In `crates/dispatch-client/src/lib.rs`, `connect`:

```rust
    let device = match Frame::read::<_, ServerMessage>(&mut reader) {
        Ok(ServerMessage::Welcome { version, device }) => {
            // Checked here as the daemon checks ours: a major version apart,
            // every frame after this one could mean something else.
            if !dispatch_proto::VERSION.is_compatible_with(version) {
                return Err(ClientError::Refused(ProtocolError::IncompatibleVersion {
                    peer: version,
                    ours: dispatch_proto::VERSION,
                }));
            }
            device
        }
```

- [ ] **Step 8: Run the gates and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-daemon/src/session.rs crates/dispatch-daemon/src/session/tests.rs crates/dispatch-client/src/lib.rs crates/dispatch-client/src/tests.rs
git commit -m "fix(daemon): act on nothing from a client that has not been welcomed

A refused Hello dropped the client's outbox but its reader kept forwarding,
and requests before any Hello were served. The client now checks the
daemon's Welcome version too.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---
### Task 6: A connection can be ended from outside, and a silent one holds up nobody (A06, A02 transport half)

Three transport gaps share one fix. The accept loop reads each preamble itself, so one client that connects and says nothing stalls every client behind it (two seconds on Unix, forever on Windows). A half whose partner never arrives waits forever. And nothing can end a connection whose threads are parked in a read or write — the client needs that to abandon a peer (Task 12), the daemon to hang up on one (Tasks 10–11). This task adds `Closer`, moves preamble reads onto their own threads with a deadline on every platform, and bounds the halves table.

**Files:**
- Modify: `crates/dispatch-os/src/ipc.rs` (imports, `Connection`, `Listener`, `imp` on both platforms, tests)
- Modify: `crates/dispatch-os/src/ipc/pairing.rs` (`Token` visibility, `Halves` with time)
- Modify: `crates/dispatch-os/Cargo.toml` (`Win32_System_IO`)

**Interfaces:**
- Consumes: `crate::process::terminate_tree`.
- Produces:
  - `pub struct Closer` (`Clone`, `Default`, `Debug`) with `pub fn close(&self)`; `Closer::default()` closes nothing.
  - `Connection::closer(&self) -> Closer`.
  - `Listener::bind_to(path: &Path) -> Result<Listener, IpcError>`.
  - `Listener` stops accepting and frees its endpoint when dropped (the drop waits for that).

- [ ] **Step 1: Write the failing tests**

In `crates/dispatch-os/src/ipc.rs`'s `mod tests`, make the `Duration` import unconditional again (these tests use it on every platform):

```rust
    use super::*;
    use std::time::Duration;
```

and append:

```rust
    #[test]
    fn a_client_that_never_announces_itself_does_not_hold_up_the_next() {
        // One connection says nothing at all. The listener used to read its
        // preamble itself, so every client behind it waited out the whole
        // two seconds -- and on Windows, where the read has no timeout,
        // waited forever.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("silent-first");

        let listener = Listener::bind().expect("binding succeeds");
        let path = endpoint().expect("resolves");

        let (served, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (reader, _writer) = listener.accept().expect("accepting succeeds").split();
            let _ = served.send(reading(reader).recv_timeout(PATIENCE));
        });

        let _silent = imp::connect(&path).expect("connecting succeeds");
        // Let the listener take the silent one first, as it would in life.
        std::thread::sleep(Duration::from_millis(50));

        let started = std::time::Instant::now();
        let (_reader, mut writer) = Connection::connect().expect("connecting succeeds").split();
        writer.write_all(b"next").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");

        assert_eq!(done.recv_timeout(PATIENCE), Ok(Ok(*b"next")));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the next client waited {:?} behind a silent one",
            started.elapsed()
        );
    }

    #[test]
    fn a_client_that_never_announces_itself_is_let_go() {
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("silent-closed");

        let _listener = Listener::bind().expect("binding succeeds");
        let path = endpoint().expect("resolves");

        let mut silent = imp::connect(&path).expect("connecting succeeds");
        let (ended, end) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            // End of file on Unix, a broken pipe on Windows: either way the
            // listener has let the connection go.
            let _ = silent.read(&mut byte);
            let _ = ended.send(());
        });

        assert!(
            end.recv_timeout(PATIENCE).is_ok(),
            "a connection that never said which half it was is still open"
        );
    }

    #[test]
    fn dropping_a_listener_frees_its_endpoint() {
        // Accepting now runs on a thread of its own. A drop that did not wait
        // for it would leave the endpoint answering for a moment, and the
        // next bind would take the old listener for a running daemon.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("rebind");

        let first = Listener::bind().expect("binding succeeds");
        drop(first);

        let _second = Listener::bind().expect("the endpoint is free once the first is dropped");
    }

    #[test]
    fn a_listener_can_be_bound_to_a_path_it_is_given() {
        let _guard = crate::env_lock();
        let endpoint_guard = Endpoint::new("bind-to");
        let path = endpoint_guard.dir.join("elsewhere.sock");

        let listener = Listener::bind_to(&path).expect("binding succeeds");
        std::thread::spawn(move || {
            let (reader, mut writer) = listener.accept().expect("accepting succeeds").split();
            let heard = reading(reader).recv_timeout(PATIENCE);
            if let Ok(heard) = heard {
                let _ = writer.write_all(&heard);
                let _ = writer.flush();
            }
        });

        let (reader, mut writer) = Connection::connect_to(&path).expect("connecting succeeds").split();
        writer.write_all(b"echo").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");
        assert_eq!(reading(reader).recv_timeout(PATIENCE), Ok(*b"echo"));
    }

    #[test]
    fn a_closer_ends_a_connection_whose_reader_is_parked() {
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("closer");

        let listener = Listener::bind().expect("binding succeeds");
        let server = std::thread::spawn(move || listener.accept().expect("accepting succeeds"));

        let client = Connection::connect().expect("connecting succeeds");
        let closer = client.closer();
        let _server_side = server.join().expect("the server thread finishes");

        let (reader, mut writer) = client.split();
        // `reading` sends only on a full read; when the read fails instead,
        // its sender is dropped and the receiver sees a disconnect.
        let parked = reading(reader);

        closer.close();

        assert_eq!(
            parked.recv_timeout(PATIENCE),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the parked read is still parked"
        );

        // Unix shuts the sockets down for good. Windows cancels what was in
        // flight; the pipe itself ends when the threads holding it let go.
        if cfg!(unix) {
            assert!(
                writer
                    .write_all(&[0u8; 64 * 1024])
                    .and_then(|()| writer.flush())
                    .is_err(),
                "a write after closing still went through"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_closer_ends_a_command_transport() {
        let connection = Connection::over_command(
            std::ffi::OsStr::new("sh"),
            &[std::ffi::OsString::from("-c"), std::ffi::OsString::from("sleep 30")],
        )
        .expect("sh exists");
        let pid = connection.child_id().expect("a command transport has a child");
        let closer = connection.closer();
        let (reader, _writer) = connection.split();
        let parked = reading(reader);

        closer.close();

        assert_eq!(
            parked.recv_timeout(PATIENCE),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the read on a killed command's stdout is still parked"
        );
        let _ = pid;
    }
```

In `crates/dispatch-os/src/ipc/pairing.rs`'s tests, append:

```rust
    #[test]
    fn a_half_whose_partner_never_comes_is_let_go() {
        // A client that died between its two connects used to leave its
        // first half in the table for the life of the daemon.
        let mut halves = Halves::new();
        let start = Instant::now();
        let token = token();

        assert!(halves.offer(token, TO_SERVER, "early", start).is_none());

        let late = start + HALF_PATIENCE + Duration::from_secs(1);
        assert!(
            halves.offer(token, TO_CLIENT, "late", late).is_none(),
            "a partner arriving after the wait is over finds nothing to pair with"
        );
    }

    #[test]
    fn only_so_many_halves_wait_at_once() {
        let mut halves = Halves::new();
        let now = Instant::now();

        let first = token();
        assert!(halves.offer(first, TO_SERVER, 0, now).is_none());
        let mut last = first;
        for i in 1..=MAX_WAITING {
            last = token();
            let at = now + Duration::from_millis(u64::try_from(i).expect("small"));
            assert!(halves.offer(last, TO_SERVER, i, at).is_none());
        }

        let later = now + Duration::from_secs(1);
        assert!(
            halves.offer(first, TO_CLIENT, 1000, later).is_none(),
            "the oldest was let go to make room"
        );
        assert!(
            halves.offer(last, TO_CLIENT, 1001, later).is_some(),
            "the newest is still waiting"
        );
    }
```

- [ ] **Step 2: Run them to see them fail**

```bash
cargo test -p dispatch-os --locked 2>&1 | tail -20
```

Expected: compile errors (`Closer`, `closer()`, `bind_to`, `offer` taking four arguments, `HALF_PATIENCE`, `MAX_WAITING` do not exist). Temporarily stub nothing; the red here is the missing API. After Step 3's pairing change alone, `a_client_that_never_announces_itself_does_not_hold_up_the_next` still FAILS with "the next client waited 2.0…s behind a silent one" — record that.

- [ ] **Step 3: Bound the halves table**

In `pairing.rs`:

```rust
use std::time::{Duration, Instant};

/// How long a half waits for its partner.
///
/// A client connects its two halves back to back, microseconds apart; ten
/// seconds is a client that died between them, and its half is let go.
pub(super) const HALF_PATIENCE: Duration = Duration::from_secs(10);

/// How many halves may wait at once.
///
/// Each is an open connection. Past this the oldest goes, so a burst of
/// clients that each connect once and die costs a bounded table, not one
/// entry per corpse.
pub(super) const MAX_WAITING: usize = 64;

/// Names the two halves of one connection to each other.
pub(super) type Token = [u8; TOKEN_BYTES];
```

(`type Token` loses its old private declaration.) `Halves` becomes:

```rust
/// Halves waiting for the connection they belong with.
pub(super) struct Halves<S> {
    waiting: HashMap<Token, Waiting<S>>,
}

/// One half, and since when it has waited.
struct Waiting<S> {
    role: u8,
    stream: S,
    since: Instant,
}

impl<S> Halves<S> {
    pub(super) fn new() -> Self {
        Self {
            waiting: HashMap::new(),
        }
    }

    /// Files one half at `now`, returning `(reader, writer)` from the
    /// *server's* side once both halves of a token are in.
    ///
    /// A second half claiming a role its partner already claimed is a broken
    /// client, so both are dropped rather than one of them believed. Halves
    /// older than [`HALF_PATIENCE`] are let go first, and the oldest is let go
    /// to keep the table at [`MAX_WAITING`].
    pub(super) fn offer(&mut self, token: Token, role: u8, stream: S, now: Instant) -> Option<(S, S)> {
        self.waiting
            .retain(|_, half| now.saturating_duration_since(half.since) < HALF_PATIENCE);

        if let Some(held) = self.waiting.remove(&token) {
            if held.role == role {
                return None;
            }

            // The server reads what the client writes, and writes what it reads.
            return if role == TO_SERVER {
                Some((stream, held.stream))
            } else {
                Some((held.stream, stream))
            };
        }

        if self.waiting.len() >= MAX_WAITING {
            let oldest = self
                .waiting
                .iter()
                .min_by_key(|(_, half)| half.since)
                .map(|(token, _)| *token);
            if let Some(oldest) = oldest {
                self.waiting.remove(&oldest);
            }
        }

        self.waiting.insert(token, Waiting { role, stream, since: now });
        None
    }
}
```

Update the four existing pairing tests' `offer(...)` calls to pass `Instant::now()` as the fourth argument.

- [ ] **Step 4: Add `Closer` and give every `Connection` one**

In `ipc.rs`, imports become:

```rust
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
```

Add after `endpoint()`:

```rust
/// How long a connection may take to say which half it is.
///
/// A client writes its thirteen bytes the moment it connects. One that has
/// not in two seconds is not a Dispatch client, or not a working one.
const PREAMBLE_PATIENCE: Duration = Duration::from_secs(2);

/// How many connections may be announcing themselves at once.
///
/// Each holds a thread until it has said which half it is or run out of
/// patience. Past this a new connection is closed at once, so a flood of
/// silent connections costs this many threads for two seconds rather than
/// one thread each.
const MAX_ANNOUNCING: usize = 32;

/// Ends a connection from outside the threads using it.
///
/// A thread parked in a read or a write on a peer that has stopped answering
/// holds its half until the peer lets go -- for a socket under a dead SSH
/// session, the kernel's quarter of an hour. Nothing outside that thread can
/// drop the half, so this is the way in: it fails what is in flight on both
/// halves, the parked threads return with an error, and each lets its half
/// go.
///
/// On Unix both sockets are shut down, which also fails everything after.
/// On Windows the pipe operations in flight are cancelled, and the pipe ends
/// once the threads parked in them drop their halves -- which they do when
/// their operation fails. A command transport's process tree is killed,
/// which ends its pipes from the far side.
///
/// Cheap to clone; every clone ends the same connection, and closing twice
/// does nothing. `Closer::default()` closes nothing: it is what a
/// connection built from halves the caller already owns hands out.
#[derive(Clone, Default)]
pub struct Closer(Arc<Mutex<Option<Ending>>>);

/// What ending one connection takes.
enum Ending {
    /// A second handle onto each of the connection's two streams.
    Streams(Vec<imp::Stream>),
    /// The process behind a command transport.
    Process(u32),
}

impl std::fmt::Debug for Closer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Closer")
    }
}

impl Closer {
    /// A closer for the connection paired from `reader` and `writer`.
    fn streams(reader: &imp::Stream, writer: &imp::Stream) -> Result<Self, IpcError> {
        let second =
            |stream: &imp::Stream| imp::try_clone(stream).map_err(|e| IpcError::io("preparing a connection to be closed", e));
        Ok(Self::ending(Ending::Streams(vec![second(reader)?, second(writer)?])))
    }

    /// A closer for the command transport running as `pid`.
    fn process(pid: u32) -> Self {
        Self::ending(Ending::Process(pid))
    }

    fn ending(ending: Ending) -> Self {
        Self(Arc::new(Mutex::new(Some(ending))))
    }

    /// Makes both halves of the connection fail, whoever holds them.
    pub fn close(&self) {
        let ending = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();

        match ending {
            None => {}
            Some(Ending::Streams(streams)) => {
                for stream in &streams {
                    imp::interrupt(stream);
                }
            }
            Some(Ending::Process(pid)) => {
                if let Err(error) = crate::process::terminate_tree(pid, TEARDOWN_GRACE) {
                    tracing::debug!(%error, pid, "failed to end a command transport");
                }
            }
        }
    }
}
```

`Connection` gains a field and a constructor, and every construction sets it:

```rust
pub struct Connection {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    /// The process behind a command transport, kept so it can be reaped.
    child: Option<std::process::Child>,
    hint: StderrHint,
    /// Ends this connection from outside; see [`Closer`].
    closer: Closer,
}
```

```rust
    pub fn connect_to(endpoint: &std::path::Path) -> Result<Self, IpcError> {
        let (reader, writer) = pairing::dial(|| imp::connect(endpoint))?;
        Self::over_streams(reader, writer)
    }

    /// A connection over the two streams a dial or a listener paired.
    fn over_streams(reader: imp::Stream, writer: imp::Stream) -> Result<Self, IpcError> {
        let closer = Closer::streams(&reader, &writer)?;
        Ok(Self {
            reader: Box::new(reader),
            writer: Box::new(writer),
            child: None,
            hint: StderrHint::default(),
            closer,
        })
    }
```

`from_halves` sets `closer: Closer::default()`. `over_command` sets `closer: Closer::process(child.id())` (take the id before moving `child` into the struct). Add:

```rust
    /// Ends this connection from outside, whoever ends up holding its halves.
    ///
    /// Taken before [`Connection::split`], which hands the halves to threads
    /// that may park in them.
    #[must_use]
    pub fn closer(&self) -> Closer {
        self.closer.clone()
    }
```

- [ ] **Step 5: Accept on a thread, and read each preamble on its own**

Replace `Listener` and its `impl` with:

```rust
/// Accepts client connections.
///
/// Accepting runs on a thread of its own from the moment the endpoint is
/// bound, and each connection's preamble is read on a thread of its own
/// again, against [`PREAMBLE_PATIENCE`]. A client that connects and says
/// nothing -- a wedged build, or on Windows a handle opened read-only --
/// costs that one thread for two seconds and holds up nobody.
pub struct Listener {
    /// Connections whose two halves have both arrived and announced themselves.
    paired: Mutex<Receiver<Result<Connection, IpcError>>>,
    /// Asks the accepting thread to stop, once something wakes it.
    stopping: Arc<AtomicBool>,
    /// Where it listens, so dropping it can wake the accepting thread.
    endpoint: PathBuf,
    accepting: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Listener")
    }
}

impl Listener {
    /// Starts listening, refusing to start beside a running daemon.
    pub fn bind() -> Result<Self, IpcError> {
        Self::bind_to(&endpoint()?)
    }

    /// Starts listening on `path`, refusing to start beside a running daemon.
    ///
    /// For a daemon whose endpoint is not this configuration's own: a test
    /// that stands one up, without steering the process-wide configuration
    /// directory to put it there.
    pub fn bind_to(path: &Path) -> Result<Self, IpcError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| IpcError::io(format!("creating {}", parent.display()), e))?;
        }

        let inner = imp::bind(path)?;
        let (sender, paired) = channel();
        let stopping = Arc::new(AtomicBool::new(false));

        let accepting = {
            let stopping = Arc::clone(&stopping);
            std::thread::spawn(move || accept_all(&inner, &sender, &stopping))
        };

        Ok(Self {
            paired: Mutex::new(paired),
            stopping,
            endpoint: path.to_path_buf(),
            accepting: Some(accepting),
        })
    }

    /// Waits for the next client, meaning both halves of one.
    pub fn accept(&self) -> Result<Connection, IpcError> {
        let paired = self.paired.lock().unwrap_or_else(|e| e.into_inner());
        match paired.recv() {
            Ok(result) => result,
            // The accepting thread reports why before it stops, so an empty,
            // closed queue means it stopped without a reason to give.
            Err(_) => Err(IpcError::io(
                "accepting a connection",
                std::io::Error::from(std::io::ErrorKind::BrokenPipe),
            )),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);

        // The accepting thread is parked in the platform's accept; one
        // connection of our own wakes it to see the flag. Joined, so the
        // endpoint is free once this returns: a daemon restarted in the same
        // process, or a test binding the same path again, must not find the
        // old listener still answering.
        let _ = imp::connect(&self.endpoint);
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
    }
}

/// Accepts until the listener fails or is dropped, reading each preamble on
/// a thread of its own and handing on each connection once both of its
/// halves are in.
fn accept_all(
    inner: &imp::Listener,
    paired: &Sender<Result<Connection, IpcError>>,
    stopping: &AtomicBool,
) {
    let halves = Arc::new(Mutex::new(pairing::Halves::new()));
    let announcing = Arc::new(AtomicUsize::new(0));

    loop {
        let stream = match imp::accept(inner) {
            Ok(stream) => stream,
            Err(error) => {
                let _ = paired.send(Err(error));
                return;
            }
        };

        if stopping.load(Ordering::Relaxed) {
            return;
        }

        if announcing.load(Ordering::Relaxed) >= MAX_ANNOUNCING {
            tracing::warn!(
                "closing a connection: {MAX_ANNOUNCING} others have not yet said which half they are"
            );
            continue;
        }

        announcing.fetch_add(1, Ordering::Relaxed);
        let halves = Arc::clone(&halves);
        let announcing = Arc::clone(&announcing);
        let paired = paired.clone();

        std::thread::spawn(move || {
            let mut stream = stream;
            let half = imp::read_preamble(&mut stream, PREAMBLE_PATIENCE);
            announcing.fetch_sub(1, Ordering::Relaxed);

            // A client that vanished, stalled, or was speaking to something
            // else. Nothing is owed to it.
            let Ok((token, role)) = half else { return };

            let offered = halves
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .offer(token, role, stream, Instant::now());

            if let Some((reader, writer)) = offered {
                let _ = paired.send(Connection::over_streams(reader, writer));
            }
        });
    }
}
```

In `mod imp` (Unix), replace `bound_preamble_wait` and `unbounded_reads` with:

```rust
    /// A second handle onto the same socket, for a [`super::Closer`].
    pub(super) fn try_clone(stream: &Stream) -> std::io::Result<Stream> {
        stream.try_clone()
    }

    /// Shuts both directions down: what is parked on the socket fails now,
    /// and everything after fails too.
    pub(super) fn interrupt(stream: &Stream) {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    /// Reads a connection's preamble, giving up after `patience`.
    ///
    /// A failure to set the timeout only loses the bound, which is why it is
    /// ignored; the frame loop that follows expects blocking reads again.
    pub(super) fn read_preamble(
        stream: &mut Stream,
        patience: std::time::Duration,
    ) -> Result<(super::pairing::Token, u8), IpcError> {
        let _ = stream.set_read_timeout(Some(patience));
        let half = super::pairing::listen_for(stream);
        let _ = stream.set_read_timeout(None);
        half
    }
```

In `mod imp` (Windows), add the import `use windows_sys::Win32::System::IO::CancelIoEx;`, and replace `bound_preamble_wait` and `unbounded_reads` with:

```rust
    /// A second handle onto the same pipe, for a [`super::Closer`].
    pub(super) fn try_clone(stream: &Stream) -> std::io::Result<Stream> {
        stream.0.try_clone().map(Stream)
    }

    /// Cancels what is in flight on the pipe, from whichever thread issued it.
    pub(super) fn interrupt(stream: &Stream) {
        use std::os::windows::io::AsRawHandle;

        // SAFETY: the handle is a live duplicate the caller holds for the
        // length of the call, and cancelling reads or writes no memory of
        // ours.
        unsafe { CancelIoEx(stream.0.as_raw_handle() as HANDLE, std::ptr::null()) };
    }

    /// Reads a connection's preamble, giving up after `patience`.
    ///
    /// A synchronous pipe read has no timeout of its own, so a watchdog
    /// cancels it. The watchdog holds its own handle, so the one it cancels
    /// cannot have been closed and reused under it; it is joined before this
    /// returns, so it cannot cancel anything the paired connection does
    /// later; and a cancel that lands just after the read finished finds
    /// nothing in flight.
    pub(super) fn read_preamble(
        stream: &mut Stream,
        patience: std::time::Duration,
    ) -> Result<(super::pairing::Token, u8), IpcError> {
        let watched =
            try_clone(stream).map_err(|e| IpcError::io("watching a connection's preamble", e))?;
        let (finished, wait) = std::sync::mpsc::channel::<()>();

        let watchdog = std::thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = wait.recv_timeout(patience) {
                interrupt(&watched);
            }
        });

        let half = super::pairing::listen_for(stream);
        drop(finished);
        let _ = watchdog.join();
        half
    }
```

`crates/dispatch-os/Cargo.toml`: add `"Win32_System_IO"` to the `windows-sys` features.

- [ ] **Step 6: Run the tests**

```bash
cargo test -p dispatch-os --locked
cargo test --workspace --locked
```

Expected: all pass. The client crate's tests exercise the new listener heavily (`Server` drops and rebinds); a hang there means the drop's wake-and-join is wrong.

- [ ] **Step 7: Windows cross-check**

```bash
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings
```

Expected: `Finished`.

- [ ] **Step 8: Run the gates, commit, push**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-os
git commit -m "feat(os): end a connection from outside, and let a silent one hold up nobody

A Closer fails both halves of a connection whoever holds them. Preambles
are read on their own threads against a deadline on every platform, and
halves waiting for a partner expire and are capped.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
```

Expected: the Windows CI job runs the new listener tests natively; green before Task 7.

---

### Task 7: A frame costs what arrived, not what it announced (A06, framing)

`Frame::read` allocates the whole announced payload (up to 64 MiB) before a byte of it arrives. It now grows the buffer as bytes arrive, so a peer that announces 64 MiB and sends ten has cost about one chunk. It also gains a hook called once a frame has begun, which Task 11 uses to notice a peer that starts a frame and never finishes it.

**Files:**
- Modify: `crates/dispatch-proto/src/frame.rs:78-109`
- Test: `crates/dispatch-proto/src/frame/tests.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `Frame::read_watched<R: Read, T: DeserializeOwned>(reader: &mut R, started: impl FnOnce()) -> Result<T, FrameError>` — calls `started` after the length prefix has arrived and before the payload is read; not called when the stream ends between frames. `Frame::read` behaves as before.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-proto/src/frame/tests.rs`:

```rust
/// A stream that records the largest read it was asked for.
struct Measured<'a> {
    bytes: &'a [u8],
    largest: usize,
}

impl std::io::Read for Measured<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.largest = self.largest.max(buf.len());
        let n = buf.len().min(self.bytes.len());
        buf[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        Ok(n)
    }
}

#[test]
fn a_frame_announcing_more_than_arrives_is_not_reserved_up_front() {
    // Just under the cap, then ten bytes, then the peer goes away. Reading
    // into a buffer sized from the prefix asked for all 64 MiB at once.
    let mut bytes = (MAX_FRAME_BYTES - 1).to_be_bytes().to_vec();
    bytes.extend_from_slice(&[0u8; 10]);
    let mut stream = Measured {
        bytes: &bytes,
        largest: 0,
    };

    let result = Frame::read::<_, ClientMessage>(&mut stream);

    assert!(matches!(result, Err(FrameError::Truncated)), "{result:?}");
    assert!(
        stream.largest <= 64 * 1024,
        "a single read asked for {} bytes of a payload that never came",
        stream.largest
    );
}

#[test]
fn the_watch_hook_runs_once_a_frame_has_begun() {
    let mut bytes = Vec::new();
    Frame::write(&mut bytes, &ClientMessage::Ping { token: 3 }).expect("writing succeeds");

    let mut began = 0;
    let message: ClientMessage =
        Frame::read_watched(&mut bytes.as_slice(), || began += 1).expect("reading succeeds");

    assert_eq!(message, ClientMessage::Ping { token: 3 });
    assert_eq!(began, 1);
}

#[test]
fn the_watch_hook_does_not_run_for_a_stream_that_ended_between_frames() {
    let mut began = 0;
    let result = Frame::read_watched::<_, ClientMessage>(&mut [].as_slice(), || began += 1);

    assert!(matches!(result, Err(FrameError::Disconnected)));
    assert_eq!(began, 0, "nothing began");
}
```

(Import `ClientMessage` in the test module if it is not already: `use crate::ClientMessage;`.)

- [ ] **Step 2: Run them to see them fail**

```bash
cargo test -p dispatch-proto --locked frame
```

Expected: compile error for `read_watched`; with that stubbed to call `read`, `a_frame_announcing_more_than_arrives_is_not_reserved_up_front` FAILS: "a single read asked for 67108863 bytes".

- [ ] **Step 3: Implement**

In `frame.rs`:

```rust
/// The most of a payload read at once.
///
/// A payload is read into a buffer grown as bytes arrive rather than one
/// sized from its length prefix: a peer that announces 64 MiB and sends ten
/// bytes has then cost about this much, not the whole announcement.
const READ_CHUNK: usize = 64 * 1024;
```

```rust
    /// Reads one frame and decodes it.
    pub fn read<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, FrameError> {
        Self::read_watched(reader, || {})
    }

    /// Reads one frame, calling `started` once its length has arrived.
    ///
    /// The daemon uses the hook to notice a peer that begins a frame and
    /// never finishes it: between frames a quiet peer is an idle one, but
    /// part-way through one it is a stalled one. Not called when the stream
    /// ends between frames.
    pub fn read_watched<R: Read, T: DeserializeOwned>(
        reader: &mut R,
        started: impl FnOnce(),
    ) -> Result<T, FrameError> {
        let mut length = [0u8; 4];

        match reader.read_exact(&mut length) {
            Ok(()) => {}
            // Nothing at all means the peer closed between frames, which is
            // an orderly end rather than an error.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(FrameError::Disconnected);
            }
            Err(e) => return Err(e.into()),
        }

        let size = u32::from_be_bytes(length);
        if size > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge { size });
        }

        started();

        let payload = read_payload(reader, size as usize)?;
        rmp_serde::from_slice(&payload).map_err(|e| FrameError::Decode(e.to_string()))
    }
}

/// Reads exactly `size` bytes, growing the buffer as they arrive.
fn read_payload<R: Read>(reader: &mut R, size: usize) -> Result<Vec<u8>, FrameError> {
    let mut payload = Vec::new();

    while payload.len() < size {
        let start = payload.len();
        let want = (size - start).min(READ_CHUNK);
        payload.resize(start + want, 0);

        match reader.read(&mut payload[start..]) {
            // Stopping part-way through is a broken connection, not a clean
            // close, and the caller treats the two differently.
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => payload.truncate(start + n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => payload.truncate(start),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(FrameError::Truncated);
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(payload)
}
```

- [ ] **Step 4: Run the tests, the gates, and commit**

```bash
cargo test -p dispatch-proto --locked
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-proto
git commit -m "fix(proto): grow a frame's buffer as its payload arrives

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---
### Task 8: A pane that stops reading stalls nobody but itself (A04)

`Daemon::handle_request` calls the pseudoterminal's blocking `write_all` on its own thread. A paste into an agent that has stopped reading fills the terminal's input buffer, and until the agent reads, the daemon serves no client, pumps no output, expires no request and never sees shutdown — the probe measured 2.004 s for one 2 MiB write, bounded only because its shell exited. Input now goes through a writer thread per pane with a byte budget; a write that would overrun it is refused whole, and the client that sent it is told.

**Files:**
- Modify: `crates/dispatch-pty/src/session.rs` (`PtyError`, `Pty` fields, `Pty::spawn`, `Pty::write`, new `spawn_writer`, `INPUT_BUDGET`)
- Modify: `crates/dispatch-pty/src/lib.rs` (export `INPUT_BUDGET`)
- Modify: `crates/dispatch-daemon/src/session.rs:413-427` (`WritePane`)
- Test: `crates/dispatch-pty/src/session/tests.rs`, `crates/dispatch-daemon/src/session/tests.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub const INPUT_BUDGET: usize = 8 * 1024 * 1024;` exported from `dispatch_pty`.
  - `PtyError::InputFull { waiting: usize }`.
  - `Pty::write` and `PtySession::write` never block: they queue, or refuse with `InputFull` when input is already waiting and this write would take it past the budget. A write into an empty queue is always accepted, whatever its size (one paste bigger than the budget still reaches a pane that reads it).
  - Test fixture: a `stall` harness in the daemon tests' `harnesses()` — a shell that puts its terminal in raw mode, prints `READY`, and never reads.

- [ ] **Step 1: Write the failing `dispatch-pty` test**

Append to `crates/dispatch-pty/src/session/tests.rs`:

```rust
/// Drains until `needle` has been printed, or panics.
#[cfg(unix)]
fn drain_until(pty: &mut Pty, needle: &str) -> Vec<u8> {
    let deadline = std::time::Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains(needle) {
        assert!(
            std::time::Instant::now() < deadline,
            "never saw {needle:?}; saw {:?}",
            String::from_utf8_lossy(&seen)
        );
        seen.extend(pty.drain());
        std::thread::sleep(Duration::from_millis(10));
    }
    seen
}

#[test]
#[cfg(unix)]
fn a_pane_that_stops_reading_does_not_block_its_writer() {
    // Raw mode so the terminal buffers what it is sent rather than
    // processing lines, then never read: the shape of an agent busy with
    // something else when a paste arrives.
    let mut pty = Pty::spawn(
        &shell("stty raw -echo; echo READY; sleep 30"),
        &cwd(),
        Size::new(80, 24),
    )
    .expect("the shell starts");
    drain_until(&mut pty, "READY");

    // Larger than the budget, into an empty queue: accepted, and at once.
    let started = std::time::Instant::now();
    pty.write(&vec![b'x'; INPUT_BUDGET + 1])
        .expect("a paste into an empty queue is accepted whatever its size");
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "the write waited {:?} for a pane that is not reading",
        started.elapsed()
    );

    // Anything more is refused whole, and says why.
    let refused = pty.write(b"y");
    assert!(
        matches!(refused, Err(PtyError::InputFull { .. })),
        "expected the input to be full, got {refused:?}"
    );

    pty.terminate();
}
```

- [ ] **Step 2: Run it to see it fail**

```bash
cargo test -p dispatch-pty --locked a_pane_that_stops_reading
```

Expected: compile error (`INPUT_BUDGET`, `PtyError::InputFull`). With those declared but `write` unchanged, the test hangs in `write` for the thirty seconds the shell sleeps, then fails on the elapsed-time assertion.

- [ ] **Step 3: Queue input on a thread of its own**

In `crates/dispatch-pty/src/session.rs`, imports gain `use std::sync::Arc;` and `use std::sync::atomic::{AtomicUsize, Ordering};`. Add:

```rust
/// How much input may wait for a pane that is not reading it.
///
/// Past any paste a person makes, and far short of what an unread queue
/// would otherwise grow to. A write that would take the waiting input past
/// this is refused whole -- half a paste arriving later would be worse than
/// none -- so a pane that has stopped reading costs this much memory and no
/// more. A write into an empty queue is always taken, whatever its size: a
/// paste bigger than the budget still reaches a pane that reads it.
pub const INPUT_BUDGET: usize = 8 * 1024 * 1024;
```

`PtyError` gains:

```rust
    /// The pane has not read the input already sent to it.
    #[error("the pane is not reading its input; {waiting} bytes are still waiting for it")]
    InputFull {
        /// How much was already queued.
        waiting: usize,
    },
```

`Pty`'s `writer: Box<dyn Write + Send>` field becomes:

```rust
    /// Input waiting for the pane, written by a thread of its own.
    ///
    /// A write to a pseudoterminal blocks once its buffer is full, and it
    /// stays full for as long as the program behind it is not reading. Done
    /// on the caller's thread, that wait is the daemon's whole loop.
    input: std::sync::mpsc::Sender<Vec<u8>>,
    /// How many bytes are queued and not yet written.
    waiting: Arc<AtomicUsize>,
```

In `Pty::spawn`, after `answer_inherit_cursor_handshake(&mut writer);`:

```rust
        let (input, queued) = channel();
        let waiting = Arc::new(AtomicUsize::new(0));
        spawn_writer(writer, queued, Arc::clone(&waiting));
```

and the struct literal sets `input, waiting` instead of `writer`. Replace `Pty::write` with:

```rust
    /// Queues bytes for the child, as if typed.
    ///
    /// Never blocks. Refused with [`PtyError::InputFull`] when input is
    /// already waiting and this would take it past [`INPUT_BUDGET`].
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        if bytes.is_empty() {
            return Ok(());
        }

        let waiting = self.waiting.load(Ordering::Acquire);
        if waiting > 0 && waiting.saturating_add(bytes.len()) > INPUT_BUDGET {
            return Err(PtyError::InputFull { waiting });
        }

        self.waiting.fetch_add(bytes.len(), Ordering::AcqRel);
        if self.input.send(bytes.to_vec()).is_err() {
            self.waiting.fetch_sub(bytes.len(), Ordering::AcqRel);
            return Err(PtyError::Write(std::io::Error::from(
                std::io::ErrorKind::BrokenPipe,
            )));
        }

        Ok(())
    }
```

Add beside `spawn_reader`:

```rust
/// Writes queued input to the pseudoterminal, in order, until the pane goes.
///
/// Keeps taking from the queue after a write fails -- a child that has
/// exited stops accepting input -- so the count of what is waiting stays
/// true and nothing sent later blocks on a thread that has stopped.
fn spawn_writer(
    mut writer: Box<dyn Write + Send>,
    queued: Receiver<Vec<u8>>,
    waiting: Arc<AtomicUsize>,
) {
    std::thread::spawn(move || {
        let mut broken = false;

        while let Ok(bytes) = queued.recv() {
            if !broken
                && let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush())
            {
                tracing::debug!(%error, "a pane stopped accepting input");
                broken = true;
            }
            waiting.fetch_sub(bytes.len(), Ordering::AcqRel);
        }
    });
}
```

`crates/dispatch-pty/src/lib.rs`: `pub use session::{INPUT_BUDGET, Pty, PtyError, PtySession, RunState};`.

- [ ] **Step 4: Run the `dispatch-pty` tests**

```bash
cargo test -p dispatch-pty --locked
```

Expected: all pass, including the two existing tests that type into a shell.

- [ ] **Step 5: Write the failing daemon test**

In `crates/dispatch-daemon/src/session/tests.rs`'s `harnesses()`, write one more harness after `no-task-args.toml`:

```rust
    // Puts its terminal in raw mode so input is buffered rather than
    // processed, says so, and never reads: an agent busy elsewhere when a
    // paste arrives.
    let stall = if cfg!(windows) {
        "id = \"stall\"\ndisplay_name = \"Stall\"\ncommand = \"cmd.exe\"\nargs = [\"/c\", \"echo READY & ping -n 30 127.0.0.1 >nul\"]\n"
    } else {
        "id = \"stall\"\ndisplay_name = \"Stall\"\ncommand = \"sh\"\nargs = [\"-c\", \"stty raw -echo; echo READY; sleep 30\"]\n"
    };
    std::fs::write(dir.join("stall.toml"), stall).expect("temp dir is writable");
```

Append the test:

```rust
#[test]
#[cfg(unix)]
fn a_stalled_pane_does_not_stall_the_daemon() {
    let (mut daemon, project, _dir) = daemon_with_limits(
        "stalled",
        DelegationLimits {
            request_timeout_secs: 1,
            ..DelegationLimits::default()
        },
    );
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);

    // A shell whose output must keep flowing, and a request whose deadline
    // must keep counting, while another pane is stalled.
    let shell = spawn_pane_for_test(&mut daemon, &ui, project);
    let caller = ask(&mut daemon, shell, "echo never");

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "stall".into(),
            size: (80, 24),
        },
    );
    let seen = wait_for(&mut daemon, &ui, |m| {
        m.iter().any(
            |m| matches!(m, ServerMessage::PaneSpawned { harness, .. } if harness == "stall"),
        )
    });
    let stalled = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSpawned { pane, harness, .. } if harness == "stall" => Some(*pane),
            _ => None,
        })
        .expect("the stalled pane was spawned");
    wait_for(&mut daemon, &ui, |m| output_of(m, stalled).contains("READY"));

    // The audit's probe: this one request held the loop for as long as the
    // pane went on not reading.
    let started = Instant::now();
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: stalled,
            bytes: vec![b'x'; 2 * 1024 * 1024],
        },
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "one write held the daemon for {:?}",
        started.elapsed()
    );

    // Another client is still answered.
    let other = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    let _ = drain(&other);
    daemon.request_for_test(2, ClientMessage::Ping { token: 5 });
    assert_eq!(drain(&other), vec![ServerMessage::Pong { token: 5 }]);

    // Another pane's output still flows.
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: shell,
            bytes: b"echo still-flowing\n".to_vec(),
        },
    );
    wait_for(&mut daemon, &ui, |m| output_of(m, shell).contains("still-flowing"));

    // The pending request still runs out of time.
    wait_for(&mut daemon, &caller, |m| {
        m.iter().any(|m| {
            matches!(
                m,
                ServerMessage::DelegateResolved {
                    outcome: DelegateOutcome::Expired { .. },
                    ..
                }
            )
        })
    });

    // More than the budget is refused out loud rather than queued.
    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane: stalled,
            bytes: vec![b'y'; dispatch_pty::INPUT_BUDGET],
        },
    );
    let told = drain(&ui);
    assert!(
        told.iter().any(|m| matches!(
            m,
            ServerMessage::Error { error: ProtocolError::Other(text) } if text.contains("not reading")
        )),
        "the writer is told its input was dropped, got {told:#?}"
    );

    // Closing the stalled pane takes effect at once.
    let started = Instant::now();
    daemon.request_for_test(1, ClientMessage::ClosePane { pane: stalled });
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(daemon.pane_count(), 1, "only the shell is left");

    // And the daemon still stops when asked.
    daemon.shutdown_handle().request();
    let started = Instant::now();
    daemon.run();
    assert!(started.elapsed() < Duration::from_secs(5));
}
```

- [ ] **Step 6: Run it to see it fail**

```bash
cargo test -p dispatch-daemon --locked a_stalled_pane
```

Expected: FAIL on the missing refusal: `Pty::write` no longer blocks (Step 3), so the timing asserts pass, but the daemon still only logs the error — "the writer is told its input was dropped".

- [ ] **Step 7: Tell the writer**

In `session.rs`'s `WritePane` arm, replace the `if let Err(error) = target.session.write(&bytes)` block with:

```rust
                match target.session.write(&bytes) {
                    Ok(()) => {}
                    // Said to the one client that sent it, which is the one
                    // whose paste just went nowhere; the rest of the fleet
                    // has no use for it.
                    Err(dispatch_pty::PtyError::InputFull { waiting }) => {
                        let dropped = bytes.len();
                        self.send(
                            id,
                            ServerMessage::Error {
                                error: ProtocolError::Other(format!(
                                    "pane {pane} is not reading its input: {waiting} bytes are \
                                     still waiting for it, so these {dropped} were dropped"
                                )),
                            },
                        );
                    }
                    Err(error) => tracing::warn!(%error, "failed to write to a pane"),
                }
```

- [ ] **Step 8: Run the gates and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-pty crates/dispatch-daemon
git commit -m "fix(pty): write a pane's input on a thread of its own, within a budget

A paste into an agent that had stopped reading blocked the daemon's loop
for as long as the agent went on not reading. Input is now queued and
written by a thread per pane; past 8 MiB waiting, a write is refused and
its sender told.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: A pane that prints faster than it is drawn is slowed, not stored (A06, pane output)

Output travels from the reader thread to `Pty::drain` over an unbounded channel, and `drain` takes everything there is. A pane printing faster than the loop drains grows that channel without limit, and one `drain` of a flood never ends while the flood lasts. The channel becomes bounded — full, the reader stops reading, the terminal's own buffer fills, and the child blocks on its next write — and one `drain` hands over at most a budget.

**Files:**
- Modify: `crates/dispatch-pty/src/session.rs` (`Pty::spawn`, `Pty::drain`, `spawn_reader`, `spawn_waiter`, new constants)
- Modify: `crates/dispatch-pty/src/lib.rs` (export `DRAIN_BUDGET`)
- Test: `crates/dispatch-pty/src/session/tests.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub const DRAIN_BUDGET: usize = 128 * 1024;` exported from `dispatch_pty`; `Pty::drain` returns at most `DRAIN_BUDGET` plus one read's worth (8 KiB).

- [ ] **Step 1: Write the failing test**

Append to `crates/dispatch-pty/src/session/tests.rs`:

```rust
#[test]
#[cfg(unix)]
fn a_flood_is_handed_over_a_budget_at_a_time() {
    // Three megabytes as fast as the shell can print them, drained slowly.
    // Before, the first drain took whatever had piled up in an unbounded
    // channel; now each takes a bounded slice, and nothing is lost.
    const TOTAL: usize = 3_000_000;
    let mut pty = Pty::spawn(
        &shell(&format!("head -c {TOTAL} /dev/zero | tr '\\0' x")),
        &cwd(),
        Size::new(80, 24),
    )
    .expect("the shell starts");

    let deadline = std::time::Instant::now() + TIMEOUT;
    let mut received = 0;
    while received < TOTAL {
        assert!(
            std::time::Instant::now() < deadline,
            "only {received} of {TOTAL} bytes arrived"
        );
        std::thread::sleep(Duration::from_millis(50));
        let chunk = pty.drain();
        assert!(
            chunk.len() <= DRAIN_BUDGET + 8192,
            "one drain handed over {} bytes",
            chunk.len()
        );
        received += chunk.iter().filter(|&&b| b == b'x').count();
    }

    assert_eq!(received, TOTAL, "every byte arrived, none twice");
}
```

- [ ] **Step 2: Run it to see it fail**

```bash
cargo test -p dispatch-pty --locked a_flood_is_handed_over
```

Expected: compile error for `DRAIN_BUDGET`; declared but unused, the test FAILS: "one drain handed over 1…(well over 136 KiB) bytes".

- [ ] **Step 3: Bound the channel and the drain**

In `session.rs`:

```rust
/// How many reads of output may wait between the reader thread and `drain`.
///
/// Full, the reader stops reading, the pseudoterminal's own buffer fills,
/// and the child blocks on its next write: a pane that prints faster than it
/// is drawn is slowed down, not held in memory.
const OUTPUT_CHUNKS: usize = 32;

/// The most output one [`Pty::drain`] hands over, give or take the read that
/// crosses it.
///
/// Without a limit, draining a pane that prints without pause never
/// finishes: the reader refills the channel as fast as it is emptied, and
/// the daemon's loop never reaches its other panes.
pub const DRAIN_BUDGET: usize = 128 * 1024;
```

Imports: `use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, channel, sync_channel};` (keep `channel` for the input queue; `Sender` is no longer used here).

`Pty::spawn`: `let (tx, events) = sync_channel(OUTPUT_CHUNKS);`.

`spawn_reader(mut reader: Box<dyn Read + Send>, tx: SyncSender<PtyEvent>)` and `spawn_waiter(..., tx: SyncSender<PtyEvent>)`.

`Pty::drain`:

```rust
    /// Takes what the child has produced, up to about [`DRAIN_BUDGET`].
    ///
    /// Never blocks: a pane with nothing to say costs one failed receive.
    /// What is left waits for the next call.
    pub fn drain(&mut self) -> Vec<u8> {
        let mut output = Vec::new();

        while output.len() < DRAIN_BUDGET {
            match self.events.try_recv() {
```

(the loop body is unchanged).

`crates/dispatch-pty/src/lib.rs`: `pub use session::{DRAIN_BUDGET, INPUT_BUDGET, Pty, PtyError, PtySession, RunState};`.

- [ ] **Step 4: Run the gates and commit**

```bash
cargo test -p dispatch-pty --locked
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-pty
git commit -m "fix(pty): bound the output waiting between a pane and its reader

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---
### Task 10: A client that stops reading is hung up on, not queued for forever (A06, outbox)

Each client's outbox is an unbounded channel, and pane output is cloned into it for every subscriber. A client whose writer thread is parked on a socket nobody reads — a slow SSH link, a suspended laptop — accumulates every byte the fleet prints; the 256 KiB history cap bounds each pane's replay, not this queue. The outbox is now counted in bytes. Live traffic past `Budgets::outbox_bytes` hangs the client up (through its `Closer`, so its writer thread lets go too); the client reconnects and its `Subscribe` replays what it missed. What a client asked for — answers, and the replay itself — is never counted against it: a client must not be hung up on for the size of the fleet it asked to see.

**Files:**
- Create: `crates/dispatch-daemon/src/outbox.rs`, `crates/dispatch-daemon/src/budgets.rs`
- Modify: `crates/dispatch-daemon/src/lib.rs`
- Modify: `crates/dispatch-daemon/src/session.rs` (`Event::Attached`, `Client`, `Daemon` fields, `set_budgets`, `send`, `broadcast_except`, `hang_up`, `spawn_client`, `attach_for_test`)
- Modify: `crates/dispatch-daemon/src/session/tests.rs` (`Receiver<ServerMessage>` → `Inbox` in every helper; new fixture harness `flood`; new tests)
- Modify: `docs/2026-09-23-audit-probes.rs` is left as it is: it documents the old behaviour and is not compiled.

**Interfaces:**
- Consumes: `dispatch_os::ipc::Closer` (Task 6), `Listener::bind_to` (Task 6), `dispatch_pty::DRAIN_BUDGET` (Task 9).
- Produces:
  - `pub struct Inbox` (exported from `dispatch_daemon`) with `recv(&self) -> Option<ServerMessage>`, `recv_timeout(&self, Duration) -> Result<ServerMessage, RecvTimeoutError>`, `try_recv(&self) -> Result<ServerMessage, TryRecvError>`, `try_iter(&self) -> impl Iterator<Item = ServerMessage> + '_`. `Daemon::attach_for_test` returns one.
  - `pub struct Budgets { pub outbox_bytes: usize, pub handshake: Duration, pub frame: Duration, pub max_clients: usize }` with `Default` (32 MiB, 10 s, 30 s, 64), exported. This task enforces `outbox_bytes`; Task 11 the rest.
  - `Daemon::set_budgets(&mut self, budgets: Budgets)`.
  - `struct Wiring { outbox: Outbox, closer: Closer }` carried by `Event::Attached(ClientId, Wiring)`; Task 11 adds a field.
  - `Daemon::hang_up` now also closes the connection.
  - Test helpers: `Served`, `served(label, budgets)`, `raw_client`, `stops_listening`, `subscribe_and_collect` in `session/tests.rs`; fixture harness `flood` (`yes`).

- [ ] **Step 1: Switch the tests to the new inbox type**

In `crates/dispatch-daemon/src/session/tests.rs`, replace every `Receiver<ServerMessage>` with `Inbox` (five places: `drain`, `wait_for`, the closure at line ~296, `spawn_pane_for_test`, `ask`, `ask_as`). Nothing else in the tests changes: `Inbox` offers the same `try_recv`/`try_iter`/`recv_timeout` they use.

- [ ] **Step 2: Write the failing tests**

Add to `harnesses()` after the `stall` harness:

```rust
    // Prints as fast as it can, forever.
    let flood = if cfg!(windows) {
        "id = \"flood\"\ndisplay_name = \"Flood\"\ncommand = \"cmd.exe\"\nargs = [\"/c\", \"for /l %i in (0,0,1) do @echo flood\"]\n"
    } else {
        "id = \"flood\"\ndisplay_name = \"Flood\"\ncommand = \"yes\"\nargs = [\"flood\"]\n"
    };
    std::fs::write(dir.join("flood.toml"), flood).expect("temp dir is writable");
```

Append the helpers and tests:

```rust
use std::io::{Read, Write};

/// A daemon serving a real endpoint on a thread of its own, stopped when
/// dropped.
///
/// Most tests here drive the loop directly; these are the ones about what
/// happens to the connection itself, which only a socket can show.
struct Served {
    endpoint: PathBuf,
    project: ProjectId,
    shutdown: Shutdown,
    thread: Option<std::thread::JoinHandle<()>>,
    _dir: TempDir,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.shutdown.request();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Serves a fresh daemon under `budgets`. Keep `label` short: a Unix
/// socket's whole path must fit in about a hundred bytes.
fn served(label: &str, budgets: Budgets) -> Served {
    let (mut daemon, project, dir) = daemon(label);
    daemon.set_budgets(budgets);

    let endpoint = dir.0.join("d.sock");
    let listener = Listener::bind_to(&endpoint).expect("binding succeeds");
    let shutdown = daemon.shutdown_handle();
    let thread = std::thread::spawn(move || {
        let _ = daemon.serve(listener);
    });

    Served {
        endpoint,
        project,
        shutdown,
        thread: Some(thread),
        _dir: dir,
    }
}

type RawReader = Box<dyn Read + Send>;
type RawWriter = Box<dyn Write + Send>;

fn raw_client(endpoint: &Path) -> (RawReader, RawWriter) {
    Connection::connect_to(endpoint)
        .expect("the daemon is listening")
        .split()
}

/// Whether writes to the daemon start failing within `patience` -- that is,
/// whether the daemon has let go of the half it reads from.
fn stops_listening(mut writer: RawWriter, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        if Frame::write(&mut writer, &ClientMessage::Ping { token: 0 }).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Connects as a well-behaved interface client and collects what arrives in
/// `window`.
fn subscribe_and_collect(endpoint: &Path, window: Duration) -> Vec<ServerMessage> {
    let (mut reader, mut writer) = raw_client(endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");
    Frame::write(&mut writer, &ClientMessage::Subscribe).expect("writing succeeds");

    let (heard, hearing) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(message) = Frame::read::<_, ServerMessage>(&mut reader) {
            if heard.send(message).is_err() {
                return;
            }
        }
    });

    let deadline = Instant::now() + window;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match hearing.recv_timeout(left) {
            Ok(message) => seen.push(message),
            Err(_) => break,
        }
    }
    drop(writer);
    seen
}

/// The output bytes among `messages`, however many panes they came from.
fn output_bytes(messages: &[ServerMessage]) -> usize {
    messages
        .iter()
        .map(|m| match m {
            ServerMessage::PaneOutput { bytes, .. } => bytes.len(),
            _ => 0,
        })
        .sum()
}

#[test]
#[cfg(unix)]
fn a_client_that_never_reads_costs_no_more_than_its_budget() {
    const BUDGET: usize = 256 * 1024;
    let (mut daemon, project, _dir) = daemon("unread");
    daemon.set_budgets(Budgets {
        outbox_bytes: BUDGET,
        ..Budgets::default()
    });

    let reading = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let never_reads = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "flood".into(),
            size: (80, 24),
        },
    );

    // Two megabytes reach the client that reads. The other one's queue
    // stops growing at the budget, instead of holding all two.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut delivered = 0;
    while delivered < 2 * 1024 * 1024 {
        assert!(
            Instant::now() < deadline,
            "the flood stopped reaching the client that reads ({delivered} bytes)"
        );
        daemon.tick();
        delivered += output_bytes(&drain(&reading));
        std::thread::sleep(Duration::from_millis(5));
    }

    let backlog = output_bytes(&drain(&never_reads));
    assert!(
        backlog <= BUDGET + dispatch_pty::DRAIN_BUDGET + 8192,
        "{backlog} bytes were queued for a client that never read"
    );
    assert!(
        matches!(
            never_reads.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "the daemon let go of it"
    );
}

#[test]
#[cfg(unix)]
fn a_client_that_stops_reading_is_hung_up_and_can_come_back() {
    let served = served(
        "stop-read",
        Budgets {
            outbox_bytes: 256 * 1024,
            ..Budgets::default()
        },
    );

    let (_reader, mut writer) = raw_client(&served.endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");
    Frame::write(&mut writer, &ClientMessage::Subscribe).expect("writing succeeds");
    Frame::write(
        &mut writer,
        &ClientMessage::SpawnPane {
            project: served.project,
            harness: "flood".into(),
            size: (80, 24),
        },
    )
    .expect("writing succeeds");

    // Never read: the socket fills, the outbox passes its budget, and the
    // daemon hangs up -- both halves, so this side's writes start failing.
    assert!(
        stops_listening(writer, Duration::from_secs(20)),
        "a client that stopped reading is still connected"
    );

    // Coming back is an ordinary late subscription: the pane is described
    // and what it printed recently is replayed.
    let seen = subscribe_and_collect(&served.endpoint, Duration::from_millis(500));
    assert!(
        seen.iter().any(
            |m| matches!(m, ServerMessage::PaneSpawned { harness, .. } if harness == "flood")
        ),
        "the reconnected client is told about the pane"
    );
    assert!(output_bytes(&seen) > 0, "and replayed what it printed");
}
```

- [ ] **Step 3: Run them to see them fail**

```bash
cargo test -p dispatch-daemon --locked 2>&1 | tail -20
```

Expected: compile errors (`Inbox`, `Budgets`, `set_budgets` do not exist). Once Step 4's types exist but `broadcast_except` still queues without a limit: `a_client_that_never_reads_costs_no_more_than_its_budget` FAILS — "2… bytes were queued for a client that never read".

- [ ] **Step 4: Count the outbox**

`crates/dispatch-daemon/src/outbox.rs`:

```rust
//! What the daemon has queued for each client, counted in bytes.
//!
//! A client that stops reading leaves its writer thread parked on the
//! socket, and everything broadcast after that waits in its queue. Counted,
//! the queue can be given a limit: past it the daemon stops queueing and
//! hangs up, and the client's reconnection replays what it missed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::Duration;

use dispatch_proto::ServerMessage;

/// The daemon's end of one client's queue.
#[derive(Debug)]
pub struct Outbox {
    sender: Sender<ServerMessage>,
    queued: Arc<AtomicUsize>,
}

/// The other end, drained by whatever writes to the client.
///
/// Public because a test holds a client's end directly:
/// `Daemon::attach_for_test` hands one back.
#[derive(Debug)]
pub struct Inbox {
    receiver: Receiver<ServerMessage>,
    queued: Arc<AtomicUsize>,
}

/// Why a message was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing drains the queue any more: the client has gone.
    Gone,
    /// The client has let this many bytes pile up.
    Behind {
        /// Bytes already waiting.
        queued: usize,
    },
}

/// A new, empty queue.
pub fn pair() -> (Outbox, Inbox) {
    let (sender, receiver) = channel();
    let queued = Arc::new(AtomicUsize::new(0));
    (
        Outbox {
            sender,
            queued: Arc::clone(&queued),
        },
        Inbox { receiver, queued },
    )
}

impl Outbox {
    /// Queues `message` whatever is already waiting.
    ///
    /// For what a client asked for: an answer, or the replay a subscription
    /// begins with. A client is not hung up on for the size of the fleet it
    /// asked to be shown.
    pub fn send(&self, message: ServerMessage) -> Result<(), Refused> {
        let weight = weight(&message);
        self.queued.fetch_add(weight, Ordering::AcqRel);
        self.sender.send(message).map_err(|_| {
            self.queued.fetch_sub(weight, Ordering::AcqRel);
            Refused::Gone
        })
    }

    /// Queues `message` unless `budget` bytes are already waiting.
    ///
    /// For what the fleet does on its own -- output, statuses, prompts --
    /// which is what piles up behind a client that has stopped reading.
    pub fn send_within(&self, message: ServerMessage, budget: usize) -> Result<(), Refused> {
        let queued = self.queued.load(Ordering::Acquire);
        if queued > budget {
            return Err(Refused::Behind { queued });
        }
        self.send(message)
    }
}

impl Inbox {
    /// Waits for the next message; `None` once the daemon has let go of this
    /// client and everything queued has been taken.
    pub fn recv(&self) -> Option<ServerMessage> {
        let message = self.receiver.recv().ok()?;
        self.took(&message);
        Some(message)
    }

    /// Waits up to `timeout` for the next message.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<ServerMessage, RecvTimeoutError> {
        let message = self.receiver.recv_timeout(timeout)?;
        self.took(&message);
        Ok(message)
    }

    /// Takes the next message if there is one.
    pub fn try_recv(&self) -> Result<ServerMessage, TryRecvError> {
        let message = self.receiver.try_recv()?;
        self.took(&message);
        Ok(message)
    }

    /// Takes every message queued now.
    pub fn try_iter(&self) -> impl Iterator<Item = ServerMessage> + '_ {
        std::iter::from_fn(move || self.try_recv().ok())
    }

    fn took(&self, message: &ServerMessage) {
        self.queued.fetch_sub(weight(message), Ordering::AcqRel);
    }
}

/// Roughly what holding a message costs: its bulk, plus an allowance for
/// the rest. Only output, a subagent's tail and a task are ever large.
fn weight(message: &ServerMessage) -> usize {
    const ENVELOPE: usize = 64;

    ENVELOPE
        + match message {
            ServerMessage::PaneOutput { bytes, .. } => bytes.len(),
            ServerMessage::DelegateFinished { tail, .. } => tail.len(),
            ServerMessage::DelegatePending { task, .. } => task.len(),
            _ => 0,
        }
}
```

`crates/dispatch-daemon/src/budgets.rs` (all four limits are declared here; Task 11 enforces the last three):

```rust
//! What one client may cost the daemon.

use std::time::Duration;

/// Limits on what one client can cost the daemon.
///
/// Every client is the same user, so these are not a defence against an
/// attacker: they keep one slow, stuck or broken client from impairing
/// everyone else's view of the fleet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    /// Bytes of the fleet's own traffic that may wait for one client before
    /// it is hung up on.
    ///
    /// Far above what a reading client ever has queued, far below what an
    /// agent printing for an afternoon produces.
    pub outbox_bytes: usize,
    /// How long a client may take to say `Hello`.
    ///
    /// A client sends it the moment it connects; ten seconds is one that is
    /// not going to.
    pub handshake: Duration,
    /// How long a client may take to finish a frame it has started.
    ///
    /// Between frames a quiet client is an idle one. Part-way through one it
    /// is a stalled one -- its reader thread is parked mid-read -- and
    /// thirty seconds covers a large paste over a slow link.
    pub frame: Duration,
    /// How many clients may be connected at once.
    ///
    /// Each costs two threads and a queue. A fleet is a handful of screens
    /// and a few delegate calls in flight; sixty-four is a leak.
    pub max_clients: usize,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            outbox_bytes: 32 * 1024 * 1024,
            handshake: Duration::from_secs(10),
            frame: Duration::from_secs(30),
            max_clients: 64,
        }
    }
}
```

`crates/dispatch-daemon/src/lib.rs`:

```rust
mod budgets;
mod delegation;
mod outbox;
mod pane;
mod session;

pub use budgets::Budgets;
pub use outbox::Inbox;
pub use session::{Daemon, DaemonError, Shutdown};
```

- [ ] **Step 5: Wire it through the daemon**

In `session.rs`: imports gain `use dispatch_os::ipc::Closer;` and `use crate::outbox::{Inbox, Outbox, Refused};` and `use crate::budgets::Budgets;`. Remove `Receiver`/`Sender`/`channel` from the `std::sync::mpsc` import where no longer used (the event channel still uses them until Task 11). The tests reach `channel` through `use super::*` (`a_requested_shutdown_stops_the_loop_and_kills_the_panes` calls it); whenever it leaves `session.rs`'s imports, add `use std::sync::mpsc::channel;` to the test module.

```rust
/// What connects the loop to one client's threads.
struct Wiring {
    /// Its queue.
    outbox: Outbox,
    /// Ends its connection, both halves, whoever holds them.
    closer: Closer,
}

/// Something the loop reacts to.
enum Event {
    /// A client attached.
    Attached(ClientId, Wiring),
    /// A client said something.
    Request(ClientId, ClientMessage),
    /// A client went away.
    Detached(ClientId),
}
```

`struct Client` replaces `outbox: Sender<ServerMessage>` with `outbox: Outbox` and gains `closer: Closer`. `Daemon` gains `budgets: Budgets`, initialised with `Budgets::default()` in `with_limits`, and:

```rust
    /// Replaces the limits on what one client may cost.
    ///
    /// Before `serve`, which consumes the daemon.
    pub fn set_budgets(&mut self, budgets: Budgets) {
        self.budgets = budgets;
    }
```

`handle`'s `Attached` arm:

```rust
            Event::Attached(id, wiring) => {
                self.clients.insert(
                    id,
                    Client {
                        outbox: wiring.outbox,
                        closer: wiring.closer,
                        subscribed: false,
                        role: Role::default(),
                        ready: false,
                    },
                );
                tracing::info!(client = id, "client attached");
            }
```

`send`:

```rust
    /// Sends to one client.
    ///
    /// Not counted against the client's budget: this is what it asked for.
    fn send(&mut self, id: ClientId, message: ServerMessage) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };

        // A failed send means the writer thread is gone, so the client has
        // disconnected and should be forgotten rather than retried.
        if client.outbox.send(message).is_err() {
            self.clients.remove(&id);
        }
    }
```

`broadcast_except`:

```rust
    fn broadcast_except(&mut self, exclude: Option<ClientId>, message: ServerMessage) {
        let mut gone = Vec::new();
        let mut behind = Vec::new();

        for (id, client) in &self.clients {
            // A delegate caller wants the fate of its own request; the fleet's
            // output and every other pane's prompts are a firehose it never
            // reads.
            if Some(*id) == exclude || !client.subscribed || client.role != Role::Interface {
                continue;
            }
            match client
                .outbox
                .send_within(message.clone(), self.budgets.outbox_bytes)
            {
                Ok(()) => {}
                Err(Refused::Gone) => gone.push(*id),
                Err(Refused::Behind { queued }) => behind.push((*id, queued)),
            }
        }

        for id in gone {
            self.clients.remove(&id);
        }

        // Hung up on rather than skipped: a client that misses output it is
        // never told it missed draws a screen that is quietly wrong. One that
        // reconnects is replayed the lot.
        for (id, queued) in behind {
            tracing::warn!(client = id, queued, "hanging up on a client that stopped reading");
            self.hang_up(id);
        }
    }
```

`hang_up` (from Task 5) becomes:

```rust
    /// Forgets a client, ends its connection, and drops what it was waiting
    /// on.
    ///
    /// Closing is what lets its threads go: a writer parked on a client that
    /// stopped reading, a reader forwarding frames nobody will act on.
    fn hang_up(&mut self, id: ClientId) {
        if let Some(client) = self.clients.remove(&id) {
            client.closer.close();
            tracing::info!(client = id, "hung up on a client");
        }
        self.abandon(id);
    }
```

`spawn_client`:

```rust
fn spawn_client(
    id: ClientId,
    connection: Connection,
    events: &Sender<Event>,
) -> Result<(), dispatch_os::ipc::IpcError> {
    let closer = connection.closer();
    let (mut reader, mut writer) = connection.split();
    let (outbox, inbox) = crate::outbox::pair();

    if events
        .send(Event::Attached(id, Wiring { outbox, closer }))
        .is_err()
    {
        return Ok(());
    }
```

and its writer thread loops `while let Some(message) = inbox.recv() {`.

`attach_for_test`:

```rust
    /// Attaches a fake client and returns its inbox.
    ///
    /// Taking from the inbox is what reading is: a test that never takes is
    /// a client that has stopped reading.
    #[doc(hidden)]
    pub fn attach_for_test(&mut self, id: u64) -> Inbox {
        let (outbox, inbox) = crate::outbox::pair();
        self.handle(Event::Attached(
            id,
            Wiring {
                outbox,
                closer: Closer::default(),
            },
        ));
        inbox
    }
```

- [ ] **Step 6: Run the tests, the gates, and commit**

```bash
cargo test -p dispatch-daemon --locked
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-daemon
git commit -m "fix(daemon): hang up on a client that stops reading instead of queueing for it

Each client's queue is counted in bytes. The fleet's own traffic past the
budget hangs the client up, both halves; what it asked for is never
counted, so a large replay does not.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: Every connection has a deadline and a place in a bounded queue (A06 daemon side, A08 I/O)

What is left of A06 and A08 on the daemon's side: a client that connects and never says `Hello` holds its threads forever; one that starts a frame and stops holds its reader forever; any number may connect; the event channel feeding the loop has no bound; and a refused client's reading half stays open. This task adds the deadlines, the quota and the bound, and makes a refusal close both halves.

**Files:**
- Modify: `crates/dispatch-daemon/src/session.rs` (`Wiring`, `Client`, `Daemon::with_limits`, `serve`, `run`, `tick`, new `enforce_deadlines`, `spawn_client`, `attach_for_test`)
- Test: `crates/dispatch-daemon/src/session/tests.rs`

**Interfaces:**
- Consumes: `Frame::read_watched` (Task 7), `Closer` (Task 6), `Served`/`raw_client`/`stops_listening`/`subscribe_and_collect` (Task 10).
- Produces: enforcement of `Budgets::handshake`, `Budgets::frame` and `Budgets::max_clients`; a bounded event channel.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-daemon/src/session/tests.rs`:

```rust
/// Whether the daemon ends the connection `reader` reads from within
/// `patience`, discarding whatever arrives first.
fn hung_up(mut reader: RawReader, patience: Duration) -> bool {
    let (ended, end) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while Frame::read::<_, ServerMessage>(&mut reader).is_ok() {}
        let _ = ended.send(());
    });
    end.recv_timeout(patience).is_ok()
}

#[test]
fn a_refused_client_is_hung_up_on_and_its_pipelined_request_ignored() {
    // A Hello the daemon refuses, with a SpawnPane right behind it in the
    // same burst: the shape that got a pane started for a refused client.
    let served = served("refused", Budgets::default());
    let (mut reader, mut writer) = raw_client(&served.endpoint);

    Frame::write(
        &mut writer,
        &ClientMessage::Hello {
            version: dispatch_proto::Version {
                major: 99,
                minor: 0,
            },
            client: "future".into(),
            role: dispatch_proto::Role::Interface,
        },
    )
    .expect("writing succeeds");
    let _ = Frame::write(&mut writer, &spawn_request(served.project));

    let answer: ServerMessage = Frame::read(&mut reader).expect("refused out loud");
    assert!(matches!(
        answer,
        ServerMessage::Error {
            error: ProtocolError::IncompatibleVersion { .. }
        }
    ));
    assert!(hung_up(reader, PATIENCE), "the half it reads was left open");
    assert!(
        stops_listening(writer, PATIENCE),
        "the half it writes to was left open"
    );

    let seen = subscribe_and_collect(&served.endpoint, Duration::from_millis(500));
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, ServerMessage::PaneSpawned { .. })),
        "the refused client's request was acted on: {seen:#?}"
    );
}

#[test]
fn a_client_that_never_says_hello_is_hung_up_on() {
    let served = served(
        "no-hello",
        Budgets {
            handshake: Duration::from_millis(200),
            ..Budgets::default()
        },
    );
    let (reader, _writer) = raw_client(&served.endpoint);

    assert!(hung_up(reader, PATIENCE), "a silent client is still connected");
}

#[test]
fn a_client_that_stops_mid_frame_is_hung_up_on() {
    let served = served(
        "mid-frame",
        Budgets {
            frame: Duration::from_millis(200),
            ..Budgets::default()
        },
    );
    let (reader, mut writer) = raw_client(&served.endpoint);
    Frame::write(&mut writer, &hello()).expect("writing succeeds");

    // A length prefix promising a hundred bytes, then three of them.
    writer
        .write_all(&100u32.to_be_bytes())
        .and_then(|()| writer.write_all(b"abc"))
        .and_then(|()| writer.flush())
        .expect("writing succeeds");

    assert!(
        hung_up(reader, PATIENCE),
        "a client stalled part-way through a frame is still connected"
    );
}

#[test]
fn clients_past_the_limit_are_turned_away() {
    let served = served(
        "quota",
        Budgets {
            max_clients: 2,
            ..Budgets::default()
        },
    );

    let first = raw_client(&served.endpoint);
    let second = raw_client(&served.endpoint);
    // Both must be counted before the third arrives.
    std::thread::sleep(Duration::from_millis(200));

    let (third_reader, _third_writer) = raw_client(&served.endpoint);
    assert!(
        hung_up(third_reader, PATIENCE),
        "a third client was let in past a limit of two"
    );

    // The two already in are unaffected.
    for (mut reader, mut writer) in [first, second] {
        Frame::write(&mut writer, &hello()).expect("writing succeeds");
        let answer: ServerMessage = Frame::read(&mut reader).expect("still served");
        assert!(matches!(answer, ServerMessage::Welcome { .. }));
    }
}
```

Add `const PATIENCE: Duration = Duration::from_secs(10);` near the other helpers (the file has none yet).

- [ ] **Step 2: Run them to see them fail**

```bash
cargo test -p dispatch-daemon --locked -- a_refused_client_is_hung_up a_client_that_never_says_hello a_client_that_stops_mid_frame clients_past_the_limit
```

Expected: `a_client_that_never_says_hello_is_hung_up_on`, `a_client_that_stops_mid_frame_is_hung_up_on` and `clients_past_the_limit_are_turned_away` FAIL on their `hung_up` assertions (after `PATIENCE`). `a_refused_client_is_hung_up_on_and_its_pipelined_request_ignored` already passes — Task 5 made a refusal call `hang_up` and Task 10 made `hang_up` close both halves — and stays as the raw-socket regression the audit asked for.

- [ ] **Step 3: Nothing to declare**

`Budgets` already carries `handshake`, `frame` and `max_clients` (Task 10). This task makes the daemon enforce them.

- [ ] **Step 4: Watch each frame and each handshake**

In `session.rs`:

```rust
/// How many events may wait for the loop.
///
/// Full, a client's reader thread waits to hand its next request over, and
/// the socket behind it fills: a client sending faster than the daemon acts
/// is slowed down rather than queued for.
const EVENT_BACKLOG: usize = 1024;

/// When the frame a client is part-way through began, while it is
/// part-way through one.
///
/// Set by the client's reader thread, read by the loop, so a client that
/// starts a frame and stops can be told from one that is merely idle.
#[derive(Clone, Default)]
struct FrameClock(Arc<std::sync::Mutex<Option<Instant>>>);

impl FrameClock {
    fn start(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    fn finish(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn since(&self) -> Option<Instant> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}
```

`Wiring` gains `frame: FrameClock`. `Client` gains `frame: FrameClock` and `attached: Instant`, set in `handle`'s `Attached` arm (`attached: Instant::now()`, `frame: wiring.frame`). `attach_for_test` passes `frame: FrameClock::default()`.

The event channel becomes bounded: `Daemon { events: Receiver<Event>, sender: SyncSender<Event>, .. }`, `let (sender, events) = sync_channel(EVENT_BACKLOG);` in `with_limits`, and `spawn_client` takes `&SyncSender<Event>`. Imports: `use std::sync::mpsc::{Receiver, SyncSender, sync_channel};`.

Add:

```rust
    /// Hangs up on clients that ran out of time: one that never said
    /// `Hello`, and one that began a frame and never finished it.
    fn enforce_deadlines(&mut self) {
        let now = Instant::now();

        let late: Vec<(ClientId, &'static str)> = self
            .clients
            .iter()
            .filter_map(|(id, client)| {
                if !client.ready && now.duration_since(client.attached) >= self.budgets.handshake {
                    return Some((*id, "it never said hello"));
                }
                if client
                    .frame
                    .since()
                    .is_some_and(|began| now.duration_since(began) >= self.budgets.frame)
                {
                    return Some((*id, "it stopped part-way through a message"));
                }
                None
            })
            .collect();

        for (id, why) in late {
            tracing::info!(client = id, why, "hanging up on a client that ran out of time");
            self.hang_up(id);
        }
    }
```

`run` calls `self.enforce_deadlines();` after `self.pump_panes();` in its loop; `tick` does the same after its `pump_panes`.

`spawn_client`'s signature becomes `fn spawn_client(id: ClientId, connection: Connection, events: &SyncSender<Event>, live: &Arc<AtomicUsize>)`; it builds `let frame = FrameClock::default();`, passes `frame: frame.clone()` in the `Wiring`, increments `live` after a successful `Attached` send, and its reader thread becomes:

```rust
    let incoming = events.clone();
    let live = Arc::clone(live);
    std::thread::spawn(move || {
        loop {
            match Frame::read_watched::<_, ClientMessage>(&mut reader, || frame.start()) {
                Ok(message) => {
                    frame.finish();
                    if incoming.send(Event::Request(id, message)).is_err() {
                        break;
                    }
                }
                Err(FrameError::Disconnected) => break,
                Err(error) => {
                    tracing::warn!(client = id, %error, "dropping a client");
                    break;
                }
            }
        }

        live.fetch_sub(1, Ordering::Relaxed);
        let _ = incoming.send(Event::Detached(id));
    });
```

Imports gain `std::sync::atomic::AtomicUsize`.

- [ ] **Step 5: Turn away clients past the quota**

`serve`:

```rust
    pub fn serve(mut self, listener: Listener) -> Result<(), DaemonError> {
        let sender = self.sender.clone();
        let max_clients = self.budgets.max_clients;
        let live = Arc::new(AtomicUsize::new(0));
        let mut next_id = 0;

        // Accepting blocks, so it runs on its own thread and hands each
        // connection to the loop.
        std::thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok(connection) => {
                        // Counted by the readers, which are what a client
                        // costs; closed at once rather than served badly.
                        if live.load(Ordering::Relaxed) >= max_clients {
                            tracing::warn!(max_clients, "turning a client away: too many are connected");
                            connection.closer().close();
                            continue;
                        }
                        next_id += 1;
                        if spawn_client(next_id, connection, &sender, &live).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to accept a connection");
                        break;
                    }
                }
            }
        });

        self.run();
        Ok(())
    }
```

- [ ] **Step 6: Run the tests**

```bash
cargo test -p dispatch-daemon --locked
```

Expected: all pass.

- [ ] **Step 7: Windows cross-check, the gates, commit, push**

```bash
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-daemon
git commit -m "fix(daemon): give every connection a deadline and the daemon a client limit

A client that never says Hello, or stops part-way through a frame, is
hung up on; clients past 64 are turned away; the event queue is bounded;
and a refused client's connection is closed in both directions.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
```

Expected: CI green on all three platforms (the socket tests run over named pipes on Windows).

---
### Task 12: The client can give up on a peer while a write to it is stuck (A05, A06 client side)

`Wire::write` holds the writer mutex for the whole of a frame's transmission, and the supervisor needs that same mutex to ping, to declare the connection lost, and to install a replacement. A peer that welcomes the client and then stops reading therefore freezes the supervisor behind the first large write: the probe saw a client still "connected" 500 ms into a 150 ms silence limit, for as long as the peer lived. And a peer that goes on *talking* while never reading is never silent at all, so nothing would notice even with the lock free.

The rewrite keeps each connection in its own `Line` — its writer, its `Closer`, and when a write on it began. The wire holds the current line behind a lock that is never held across I/O, so declaring a connection lost is: take the line out, mark down, close it. Closing fails the stuck write, which lets the writer thread go. The supervisor pings with `try_lock` and never waits behind a write; a write under way for longer than `Liveness::silence` loses the connection. Every generation check stays: a line's failure only ever ends that line. The handshake's abandoned-dial path uses the `Closer` too, so a socket dial that times out no longer leaves its thread parked until the peer closes.

**Files:**
- Modify: `crates/dispatch-client/src/lib.rs` (`DialledChild`/`reap_dialled`/`DIAL_TEARDOWN_GRACE` → `Dialling`; new `Line`; `Wire` fields and methods; `Connected`; `attach_dialling`; `connect_within`; `connect`; `supervise`; `check_liveness`; `Drop for Client`)
- Test: `crates/dispatch-client/src/tests.rs`

**Interfaces:**
- Consumes: `dispatch_os::ipc::Closer` and `Connection::closer` (Task 6); `octal` (Task 1).
- Produces: no public API change. `Client`, `Handle`, `Liveness`, `Dial` and the `*_for_test` helpers keep their signatures.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-client/src/tests.rs`:

```rust
/// A message far bigger than any pipe or socket buffer, so writing it
/// blocks until the peer reads.
fn huge() -> ClientMessage {
    ClientMessage::OpenProject {
        root: PathBuf::from("x".repeat(4 * 1024 * 1024)),
    }
}

#[test]
#[cfg(unix)]
fn a_peer_that_never_reads_is_given_up_on_despite_a_stuck_write() {
    // The audit's probe: welcomed, then the peer neither reads nor speaks.
    // The big write blocked holding the lock the supervisor needed, and the
    // client went on reporting itself connected long past its silence limit.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut encoded = Vec::new();
    Frame::write(&mut encoded, &welcome()).expect("writing succeeds");

    let client = Client::attach_over(
        Role::Interface,
        "test",
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(300),
        },
        OsString::from("sh"),
        vec![
            OsString::from("-c"),
            OsString::from(format!("printf '{}'; sleep 30", octal(&encoded))),
        ],
    )
    .expect("the command answers the handshake");

    client.send(huge());

    // Given up on and redialled -- the command answers again -- well inside
    // the thirty seconds the first peer would otherwise have held it.
    assert!(
        wait_until(Duration::from_secs(5), || client.generation() >= 2),
        "the client never gave up on a peer it could not write to"
    );

    let started = Instant::now();
    drop(client);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "dropping the client took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_peer_that_talks_but_never_reads_is_given_up_on() {
    // Never silent -- it answers on its own every twenty milliseconds -- so
    // only a deadline on the write itself can notice that nothing sent
    // reaches it. The replacement then answers like a live daemon, and the
    // old connection's stuck write, failing once it is closed, must not take
    // the replacement down.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("talks-only");

    let listener = Listener::bind().expect("binding succeeds");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");
    let (stop, stopped) = std::sync::mpsc::channel::<()>();

    let server = std::thread::spawn(move || {
        let (mut first_reader, mut first_writer) =
            listener.accept().expect("the first dial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut first_reader);
        Frame::write(&mut first_writer, &welcome()).expect("the welcome goes out");
        std::thread::spawn(move || {
            while Frame::write(&mut first_writer, &ServerMessage::Pong { token: 0 }).is_ok() {
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        let (mut second_reader, mut second_writer) =
            listener.accept().expect("the redial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut second_reader);
        Frame::write(&mut second_writer, &welcome()).expect("the welcome goes out");
        std::thread::spawn(move || {
            while let Ok(message) = Frame::read::<_, ClientMessage>(&mut second_reader) {
                if let ClientMessage::Ping { token } = message
                    && Frame::write(&mut second_writer, &ServerMessage::Pong { token }).is_err()
                {
                    return;
                }
            }
        });

        let _ = stopped.recv();
        (listener, first_reader)
    });

    let client = Client::attach_at(
        Role::Interface,
        "test",
        Liveness {
            interval: Duration::from_millis(50),
            silence: Duration::from_millis(300),
        },
        endpoint,
    )
    .expect("the first connection is welcomed");

    client.send(huge());

    assert!(
        wait_until(PATIENCE, || client.generation() == 2 && client.is_connected()),
        "a peer that never read was not given up on"
    );

    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        assert!(
            client.is_connected() && client.generation() == 2,
            "the old connection's stuck write took down its replacement"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(client);
    let _ = stop.send(());
    drop(server.join());
}

#[test]
fn a_socket_dial_that_is_given_up_on_lets_its_thread_go() {
    // A peer that accepts and never answers the handshake. The dial gives
    // up at its patience; the thread it left parked in the read used to stay
    // parked until the peer closed -- one per retry, forever.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _endpoint = Endpoint::new("mute-socket");

    let listener = Listener::bind().expect("binding succeeds");
    let endpoint = dispatch_os::ipc::endpoint().expect("the endpoint resolves");
    let (closed, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut reader, _writer) = listener.accept().expect("the dial arrives").split();
        let _ = Frame::read::<_, ClientMessage>(&mut reader);
        // Now say nothing, and report when the client has let go.
        let mut byte = [0u8; 1];
        while reader.read(&mut byte).is_ok_and(|n| n > 0) {}
        let _ = closed.send(());
        drop(listener);
    });

    let dialled = connect_within(
        "test",
        Role::Interface,
        &Dial::Endpoint(endpoint),
        Duration::from_millis(300),
    );
    assert!(matches!(dialled, Err(ClientError::Handshake(_))));

    assert!(
        ended.recv_timeout(PATIENCE).is_ok(),
        "the abandoned handshake still holds its connection open"
    );
}
```

- [ ] **Step 2: Run them to see them fail**

```bash
cargo test -p dispatch-client --locked -- a_peer_that_never_reads a_peer_that_talks_but_never_reads a_socket_dial_that_is_given_up_on
```

Expected: all three FAIL — generation stays 1 (the first two), and the mute socket is never closed (the third).

- [ ] **Step 3: Replace `DialledChild` with a closer**

Remove `DIAL_TEARDOWN_GRACE`, `DialledChild` and `reap_dialled`. Imports: `use dispatch_os::ipc::{Closer, Connection, IpcError, StderrHint};`. Add:

```rust
/// Where a dial leaves the means to end what it started, for whoever stops
/// waiting on it.
///
/// The handshake runs on a thread of its own, and a peer that accepts and
/// then never speaks leaves that thread parked in a read. Whoever gives up
/// on the handshake closes the connection through this: the parked thread
/// returns and, for a command, the process is ended -- rather than an `ssh`
/// left running, or a socket left open, for as long as the peer takes to
/// let go.
///
/// Recorded before the handshake begins, because the handshake is the part
/// that may never finish.
#[derive(Clone, Default)]
struct Dialling(Arc<Mutex<Option<Closer>>>);

impl Dialling {
    fn record(&self, closer: Closer) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(closer);
    }

    /// Ends whatever the dial started, if it started anything.
    fn abandon(&self) {
        let closer = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(closer) = closer {
            closer.close();
        }
    }
}
```

`Connected` replaces `child: Option<u32>` with:

```rust
    /// Ends this connection, both halves and any process behind it.
    closer: Closer,
```

`connect` takes `started: &Dialling`, and after building `connection`:

```rust
    let closer = connection.closer();
    started.record(closer.clone());

    let (mut reader, mut writer) = connection.split();
```

returning `Connected { reader, writer, device, closer }`.

`connect_within`:

```rust
fn connect_within(
    name: &str,
    role: Role,
    dial: &Dial,
    patience: Duration,
) -> Result<Connected, ClientError> {
    let (done, answer) = channel();
    let name = name.to_string();
    let for_thread = dial.clone();
    let dialling = Dialling::default();
    let recording = dialling.clone();

    std::thread::spawn(move || {
        let _ = done.send(connect(&name, role, &for_thread, &recording));
    });

    match answer.recv_timeout(patience) {
        Ok(result) => result,
        // Raised here rather than by the thread: the thread may still be
        // waiting on a peer that never answers, so the timeout has to come
        // from the caller's side and cannot carry a stderr hint that only the
        // thread holds.
        Err(_) => {
            dialling.abandon();
            Err(ClientError::Handshake(format!(
                "{dial} did not answer within {patience:?}"
            )))
        }
    }
}
```

Update `connect_within`'s doc comment: "giving up on the handshake closes what the dial opened -- the socket, or the command's whole process tree".

- [ ] **Step 4: One `Line` per connection, and a wire that never waits on I/O**

Add above `Wire`:

```rust
/// One connection: where to write, how to end it, and whether a write on it
/// is under way.
///
/// Held by `Arc`, so a write already under way on a connection that has
/// been replaced carries on against the old one -- and fails, once that is
/// closed -- without anything having to wait for it.
struct Line {
    /// Which connection this is.
    generation: u64,
    /// Where everything sent is written, one frame at a time.
    writer: Mutex<Box<dyn Write + Send>>,
    /// Ends both halves, and the process behind a command dial.
    closer: Closer,
    /// When the write under way began, while one is.
    ///
    /// A peer that talks but never reads is never silent, so this is what
    /// notices it: a write not finished within the silence the connection
    /// is allowed is not going to finish.
    writing_since: Mutex<Option<Instant>>,
}

impl Line {
    fn new(generation: u64, writer: Box<dyn Write + Send>, closer: Closer) -> Self {
        Self {
            generation,
            writer: Mutex::new(writer),
            closer,
            writing_since: Mutex::new(None),
        }
    }

    /// How long the write under way has been going, if one is.
    fn stuck_for(&self) -> Option<Duration> {
        self.writing_since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|since| since.elapsed())
    }
}
```

In `Wire`, replace the `writer` and `child` fields with:

```rust
    /// The connection now, if there is one.
    ///
    /// Only ever held for a moment, and never across a read or a write: so
    /// declaring a connection dead, or putting a new one in its place, never
    /// waits on a write that is stuck.
    line: Mutex<Option<Arc<Line>>>,
```

and update the struct's doc ("The writer is behind a lock and behind an `Option` because reconnecting replaces it" → "The connection is behind a lock and an `Option` because reconnecting replaces it"). `Wire::new` sets `line: Mutex::new(None)`.

Replace `Wire::write`, `lost` and `lost_current` with:

```rust
    /// The connection now, if there is one.
    fn current(&self) -> Option<Arc<Line>> {
        self.line.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Writes one message, or reports that the connection is gone.
    fn write(&self, message: &ClientMessage) -> bool {
        let Some(line) = self.current() else {
            // Disconnected: dropped rather than queued. A keystroke that
            // arrives at an agent minutes later, out of order with the rest,
            // is worse than one that never arrives.
            return false;
        };

        let mut writer = line.writer.lock().unwrap_or_else(|e| e.into_inner());
        self.write_on(&line, &mut writer, message)
    }

    /// Writes one message, unless a write is already under way.
    ///
    /// For the supervisor's ping, which must never wait behind a write that
    /// may be stuck: the write deadline deals with that one.
    fn try_write(&self, message: &ClientMessage) {
        let Some(line) = self.current() else { return };

        let mut writer = match line.writer.try_lock() {
            Ok(writer) => writer,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        self.write_on(&line, &mut writer, message);
    }

    /// Writes `message` to `line`, timing the write.
    fn write_on(
        &self,
        line: &Line,
        writer: &mut Box<dyn Write + Send>,
        message: &ClientMessage,
    ) -> bool {
        *line.writing_since.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let written = Frame::write(writer, message);
        *line.writing_since.lock().unwrap_or_else(|e| e.into_inner()) = None;

        if written.is_err() {
            // The writer going is only half of it: a command whose stdin
            // closes need not exit, and `ssh` does not. `lost` ends the lot.
            self.lost(line.generation);
            return false;
        }

        true
    }

    /// Records that connection `generation` has broken, and ends it.
    ///
    /// A connection already replaced is left alone: a reader parked on a
    /// peer liveness gave up on wakes only when that peer finally closes, and
    /// a write stuck on it fails only once it is closed -- by which time the
    /// supervisor may have dialled a replacement, which "whatever is current"
    /// would tear down. Compared under the lock the supervisor installs a
    /// connection under, so the check cannot fall between a replacement being
    /// put in place and its generation being counted.
    ///
    /// Marked down under that lock too, so `connected` never disagrees with
    /// whether there is a line. Closed after, outside it: ending a command's
    /// process tree can take a moment, and nothing else should wait for it.
    fn lost(&self, generation: u64) {
        let line = {
            let mut slot = self.line.lock().unwrap_or_else(|e| e.into_inner());
            if slot.as_ref().is_none_or(|line| line.generation != generation) {
                return;
            }
            self.connected.store(false, Ordering::Relaxed);
            slot.take()
        };

        if let Some(line) = line {
            line.closer.close();
        }
    }

    /// Records that the current connection has broken.
    ///
    /// For callers that are about the connection as it stands rather than
    /// one they were handed: the supervisor, and a queue whose writer thread
    /// has gone.
    fn lost_current(&self) {
        if let Some(line) = self.current() {
            self.lost(line.generation);
        }
    }
```

- [ ] **Step 5: Install and abandon connections through the line**

`attach_dialling`:

```rust
        let connected = connect_within(name, role, &dial, patience_for(&dial))?;

        let wire = Wire::new(role, name, liveness, dial);
        *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = connected.device;
        *wire.line.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Line::new(
            1,
            connected.writer,
            connected.closer,
        )));
        wire.generation.store(1, Ordering::Relaxed);
        wire.connected.store(true, Ordering::Relaxed);

        Ok(Self::start(Arc::new(wire), Some(connected.reader)))
```

`supervise`'s `Ok(connected)` arm:

```rust
                Ok(connected) => {
                    // Checked under the lock `Client::drop` takes to clear the
                    // line, so whichever of the two gets there first, the
                    // connection is closed exactly once. Device, line,
                    // generation and `connected` all change together under
                    // it: an interface that sees the generation move sees
                    // the device that came with it, and `lost` never finds a
                    // line without its generation counted.
                    let generation = {
                        let mut slot = wire.line.lock().unwrap_or_else(|e| e.into_inner());
                        if wire.closed.load(Ordering::Relaxed) {
                            drop(slot);
                            connected.closer.close();
                            return;
                        }

                        *wire.device.lock().unwrap_or_else(|e| e.into_inner()) = connected.device;
                        let generation = wire.generation.fetch_add(1, Ordering::Relaxed) + 1;
                        *slot = Some(Arc::new(Line::new(
                            generation,
                            connected.writer,
                            connected.closer,
                        )));
                        wire.heard();
                        wire.connected.store(true, Ordering::Relaxed);
                        generation
                    };

                    *wire.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    read_from(connected.reader, generation, &incoming, &wire);

                    // Sent directly rather than through the queue: the queue's
                    // writer may be mid-message, and a subscribe that arrives
                    // after the first keystroke would lose the panes.
                    if wire.subscribed.load(Ordering::Relaxed) {
                        wire.write(&ClientMessage::Subscribe);
                    }

                    tracing::info!(generation, dial = %wire.dial, "connected to the daemon");
                    backoff = first_retry;
                }
```

`check_liveness`:

```rust
/// Asks a quiet daemon whether it is there, and gives up on one that never
/// says -- or on one a write has been stuck on for as long as silence is
/// allowed.
fn check_liveness(wire: &Wire) {
    // First, because a peer that talks but never reads is never quiet: its
    // chatter would pass every check below while nothing sent reaches it.
    if let Some(line) = wire.current()
        && line
            .stuck_for()
            .is_some_and(|stuck| stuck >= wire.liveness.silence)
    {
        tracing::info!("a write to the daemon never finished");
        wire.lost(line.generation);
        return;
    }

    let quiet = wire.quiet_for();

    if quiet >= wire.liveness.silence {
        tracing::info!(?quiet, "the daemon stopped answering");
        wire.lost_current();
        return;
    }

    if quiet < wire.liveness.interval {
        return;
    }

    // One question per interval, not one per pass: the supervisor comes round
    // every hundred milliseconds, and a daemon that is merely busy should not be
    // buried in pings while it catches up.
    let mut asked = wire.last_asked.lock().unwrap_or_else(|e| e.into_inner());
    if asked.elapsed() < wire.liveness.interval {
        return;
    }
    *asked = Instant::now();
    drop(asked);

    // Written straight to the socket rather than queued -- the queue carries
    // the interface's traffic -- but only if no write is under way: a ping
    // that waited behind a stuck write would stall the supervisor with it.
    wire.try_write(&ClientMessage::Ping {
        token: u64::try_from(quiet.as_millis()).unwrap_or(u64::MAX),
    });
}
```

`Drop for Client`:

```rust
impl Drop for Client {
    fn drop(&mut self) {
        // Otherwise the supervisor would keep reconnecting to a daemon nobody
        // is listening to, for as long as the process lives.
        self.wire.closed.store(true, Ordering::Relaxed);

        // And the connection -- a socket, or a command's whole process tree
        // -- would outlive the client that wanted it: the reader that owns it
        // is parked, and nothing else is ever going to wake it.
        let line = self
            .wire
            .line
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(line) = line {
            self.wire.connected.store(false, Ordering::Relaxed);
            line.closer.close();
        }
    }
}
```

Nothing else in the file names `wire.writer` or `wire.child`; `cargo check` will find any stragglers.

- [ ] **Step 6: Run the client tests**

```bash
cargo test -p dispatch-client --locked
```

Expected: all pass — the three new tests, and every existing generation, reconnection and teardown test.

- [ ] **Step 7: Run the gates and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-client
git commit -m "fix(client): give up on a peer even while a write to it is stuck

The writer lock was held for a frame's whole transmission and the
supervisor needed it to ping, declare the connection lost or replace it.
Each connection now owns its writer and a Closer; the wire's lock is never
held across I/O, pings never wait on a write, and a write stuck for as
long as silence is allowed loses the connection.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
```

---

### Task 13: Keeping a project or a machine is one step, and never loses another's (A09)

Both registries read, change and `std::fs::write` the whole file. Two processes that read the same contents each keep only their own change; a crash or a failed write after truncation leaves an empty or half file; a reader can see the half. `machines::add` checks the name and saves in two separate reads of the file. Every change now goes through one function that locks a file beside the registry, reads, changes, writes a staged copy, and renames it over the original.

**Files:**
- Create: `crates/dispatch-config/src/store.rs`
- Modify: `crates/dispatch-config/src/lib.rs` (`mod store;`)
- Modify: `crates/dispatch-config/src/projects.rs` (`read`, `write` removed, `save`, `forget_machine`, `remember_in`, `forget_in`)
- Modify: `crates/dispatch-config/src/machines.rs` (`load`, `save`, `check`, `add`, `remove`, new private `refusal`)
- Test: `crates/dispatch-config/src/projects/tests.rs`, `crates/dispatch-config/src/machines/tests.rs`

**Interfaces:**
- Consumes: `std::fs::File::lock` (Rust 1.89, Task 2).
- Produces: `pub(crate) fn store::read<T: Default + DeserializeOwned>(dir: &Path, file: &str) -> Result<T, ConfigError>` and `pub(crate) fn store::update<T, R>(dir: &Path, file: &str, change: impl FnOnce(&mut T) -> Result<(R, bool), ConfigError>) -> Result<R, ConfigError>` where `T: Default + Serialize + DeserializeOwned`; `change` returns what the caller wants back and whether it changed anything.

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-config/src/projects/tests.rs`:

```rust
/// Where `remember_many_as_a_child_process` keeps its roots, and the label
/// it gives them.
const CHILD_DIR: &str = "DISPATCH_TEST_CHILD_DIR";
const CHILD_LABEL: &str = "DISPATCH_TEST_CHILD_LABEL";

/// Not a test of its own: the body each child process runs for
/// `two_processes_remembering_at_once_keep_both`. Run without its
/// variables, it does nothing.
#[test]
fn remember_many_as_a_child_process() {
    let (Some(dir), Some(label)) = (std::env::var_os(CHILD_DIR), std::env::var_os(CHILD_LABEL))
    else {
        return;
    };
    let label = label.to_string_lossy().into_owned();

    for i in 0..50 {
        remember(
            Path::new(&dir),
            &PathBuf::from(format!("/tmp/{label}-{i}")),
        )
        .expect("the directory is writable");
    }
}

#[test]
fn two_processes_remembering_at_once_keep_both() {
    // Two Dispatch processes, each keeping what the user opens: each read
    // the file, added its root, and wrote the whole file back -- so the
    // second write erased the first one's root.
    let dir = TempDir::new("projects-processes");
    let exe = std::env::current_exe().expect("the test binary");

    let children: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|label| {
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "projects::tests::remember_many_as_a_child_process",
                    "--test-threads=1",
                ])
                .env(CHILD_DIR, dir.path())
                .env(CHILD_LABEL, label)
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("the test binary runs")
        })
        .collect();

    for mut child in children {
        assert!(child.wait().expect("the child finishes").success());
    }

    assert_eq!(
        load(dir.path()).expect("the file is readable").len(),
        100,
        "every root from both processes is kept"
    );
}

#[test]
fn threads_remembering_at_once_keep_everything() {
    let dir = TempDir::new("projects-threads");

    let workers: Vec<_> = (0..8)
        .map(|worker| {
            let dir = dir.path().to_path_buf();
            std::thread::spawn(move || {
                for i in 0..20 {
                    remember(&dir, &PathBuf::from(format!("/tmp/{worker}-{i}")))
                        .expect("the directory is writable");
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("the worker finishes");
    }

    assert_eq!(load(dir.path()).expect("it reads back").len(), 160);
}

#[test]
fn a_reader_never_sees_half_a_file() {
    // Writing in place truncates first: a reader landing in between saw an
    // empty file -- no projects at all -- or a torn one it could not parse.
    let dir = TempDir::new("projects-torn");
    remember(dir.path(), Path::new("/tmp/seed")).expect("the directory is writable");

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let dir = dir.path().to_path_buf();
        let stop = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let kept = load(&dir).expect("the file is always readable");
                assert!(
                    kept.contains(&PathBuf::from("/tmp/seed")),
                    "a reader saw a file without the seed: {kept:?}"
                );
            }
        })
    };

    for i in 0..200 {
        remember(dir.path(), &PathBuf::from(format!("/tmp/r-{i}")))
            .expect("the directory is writable");
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    reader.join().expect("the reader never saw half a file");
}

#[test]
fn a_write_that_fails_leaves_the_file_as_it_was() {
    let dir = TempDir::new("projects-failed-write");
    remember(dir.path(), Path::new("/tmp/kept")).expect("the directory is writable");

    // Where the new contents would be staged, something that is not a file.
    std::fs::create_dir(dir.path().join("projects.toml.tmp")).expect("temp dir is writable");

    remember(dir.path(), Path::new("/tmp/lost")).expect_err("staging the new contents fails");

    assert_eq!(
        load(dir.path()).expect("the file is still readable"),
        [PathBuf::from("/tmp/kept")]
    );
}
```

Append to `crates/dispatch-config/src/machines/tests.rs`:

```rust
#[test]
fn two_adds_of_one_name_at_once_keep_one() {
    // `check` and the save were two separate reads of the file, so two adds
    // could each find the name free and both be saved.
    let dir = TempDir::new("machines-race");

    let adds: Vec<_> = (0..8)
        .map(|i| {
            let dir = dir.path().to_path_buf();
            std::thread::spawn(move || add(&dir, Machine::new("tower", format!("host{i}")), "laptop"))
        })
        .collect();
    let results: Vec<_> = adds
        .into_iter()
        .map(|add| add.join().expect("the add finishes"))
        .collect();

    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "{results:?}");
    assert_eq!(load(dir.path()).expect("it reads back").len(), 1);
}
```

- [ ] **Step 2: Run them to see them fail**

```bash
cargo test -p dispatch-config --locked -- projects::tests machines::tests::two_adds
```

Expected: `a_write_that_fails_leaves_the_file_as_it_was` FAILS (the write succeeds: nothing is staged). The concurrency tests are races and usually FAIL (fewer than 100 or 160 roots; more than one add succeeding; a reader seeing no seed); record what was seen.

- [ ] **Step 3: The store**

`crates/dispatch-config/src/store.rs`:

```rust
//! Changing a small TOML file that more than one process changes.
//!
//! Every running Dispatch writes `projects.toml` and `machines.toml` back as
//! the user works. Two of them each reading the file, adding one entry and
//! writing the whole file back would keep only the second entry; a write cut
//! short would leave half a file the next start cannot parse; and a reader
//! could land on the half. Every change goes through [`update`], which does
//! none of those.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::ConfigError;

/// Reads `file` in `dir`. No file is the default value: a first run.
pub(crate) fn read<T: Default + DeserializeOwned>(dir: &Path, file: &str) -> Result<T, ConfigError> {
    let path = dir.join(file);

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(source) => return Err(ConfigError::Io { path, source }),
    };

    toml::from_str(&text).map_err(|source| ConfigError::Toml { path, source })
}

/// Applies `change` to `file`'s current contents and writes the result back,
/// as one step no other `update` of the same file can come between.
///
/// `change` returns what the caller wants back, and whether it changed
/// anything; nothing changed, nothing is written. The file is replaced, never
/// rewritten in place, so a reader sees the old contents or the new, never a
/// part of either, and a write that fails leaves the old contents where they
/// were.
pub(crate) fn update<T, R>(
    dir: &Path,
    file: &str,
    change: impl FnOnce(&mut T) -> Result<(R, bool), ConfigError>,
) -> Result<R, ConfigError>
where
    T: Default + Serialize + DeserializeOwned,
{
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let _held = lock(dir, file)?;

    let mut value: T = read(dir, file)?;
    let (answer, changed) = change(&mut value)?;

    if changed {
        let text = toml::to_string_pretty(&value).expect("a registry serialises");
        replace(&dir.join(file), &text)?;
    }

    Ok(answer)
}

/// Takes `file`'s lock, held until the returned file is dropped.
///
/// A file of its own beside the one it guards: on Windows a lock is
/// mandatory, and one on the registry itself would stop the readers, which
/// take no lock, from reading it.
fn lock(dir: &Path, file: &str) -> Result<std::fs::File, ConfigError> {
    let path = dir.join(format!("{file}.lock"));

    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;

    lock.lock().map_err(|source| ConfigError::Io { path, source })?;
    Ok(lock)
}

/// Writes `text` beside `path`, then moves it into place.
///
/// Staged under a fixed name, which is safe because only the holder of the
/// lock writes it.
fn replace(path: &Path, text: &str) -> Result<(), ConfigError> {
    let staged = path.with_extension("toml.tmp");

    let written = (|| {
        let mut file = std::fs::File::create(&staged)?;
        file.write_all(text.as_bytes())?;
        // On disk before the rename: a rename that reached the disk first
        // would, after a crash, leave the name pointing at nothing.
        file.sync_all()
    })();

    if let Err(source) = written {
        let _ = std::fs::remove_file(&staged);
        return Err(ConfigError::Io {
            path: staged,
            source,
        });
    }

    std::fs::rename(&staged, path).map_err(|source| {
        let _ = std::fs::remove_file(&staged);
        ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}
```

`crates/dispatch-config/src/lib.rs`: add `mod store;` beside the other private modules.

- [ ] **Step 4: Projects through the store**

In `projects.rs`, delete `write`, and replace `read` and the changing functions:

```rust
/// Reads the whole file. No file is an empty one.
fn read(dir: &Path) -> Result<Saved, ConfigError> {
    crate::store::read(dir, FILE)
}
```

```rust
pub fn save(dir: &Path, roots: &[PathBuf]) -> Result<(), ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        saved.roots = roots.to_vec();
        Ok(((), true))
    })
}
```

```rust
pub fn forget_machine(dir: &Path, machine: &str) -> Result<bool, ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        let had = saved.machines.remove(machine).is_some();
        Ok((had, had))
    })
}
```

```rust
fn remember_in(dir: &Path, machine: Option<&str>, root: &Path) -> Result<bool, ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        let list = saved.list_mut(machine);
        if list.iter().any(|kept| kept == root) {
            return Ok((false, false));
        }

        list.push(root.to_path_buf());
        Ok((true, true))
    })
}
```

```rust
fn forget_in(dir: &Path, machine: Option<&str>, root: &Path) -> Result<bool, ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        let list = saved.list_mut(machine);
        let before = list.len();
        list.retain(|kept| kept != root);

        let forgot = list.len() != before;
        Ok((forgot, forgot))
    })
}
```

(Keep each function's existing doc comment; `remember_in`'s "answers whether the file was written" still holds.)

- [ ] **Step 5: Machines through the store, with the name checked inside the change**

In `machines.rs`:

```rust
pub fn load(dir: &Path) -> Result<Vec<Machine>, ConfigError> {
    Ok(crate::store::read::<Saved>(dir, FILE)?.machines)
}

/// Writes the list, replacing whatever was there.
pub fn save(dir: &Path, machines: &[Machine]) -> Result<(), ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        saved.machines = machines.to_vec();
        Ok(((), true))
    })
}

/// Whether `name` could be registered now.
///
/// Its own step so a caller can ask before spending thirty seconds proving the
/// machine answers, only to be told the name was taken. [`add`] asks again,
/// inside the change, because the answer can change in those thirty seconds.
pub fn check(dir: &Path, name: &str, this_host: &str) -> Result<(), ConfigError> {
    refusal(dir, name, this_host, &load(dir)?)
}

/// Why `name` cannot join `registered`, if it cannot.
fn refusal(
    dir: &Path,
    name: &str,
    this_host: &str,
    registered: &[Machine],
) -> Result<(), ConfigError> {
    let refuse = |reason: String| ConfigError::Machine {
        path: dir.join(FILE),
        reason,
    };

    if !valid_name(name) {
        return Err(refuse(format!(
            "{name:?} is not a machine name; use letters, digits, - and _"
        )));
    }

    // Compared with the host's first label too: `laptop` is this machine
    // whether the operating system says `laptop` or `Laptop.local`.
    let short = this_host.split('.').next().unwrap_or(this_host);
    if name.eq_ignore_ascii_case(this_host) || name.eq_ignore_ascii_case(short) {
        return Err(refuse(format!("{name} is this machine's own name")));
    }

    if registered.iter().any(|machine| machine.name == name) {
        return Err(refuse(format!("{name} is already registered")));
    }

    Ok(())
}

/// Registers `machine`, refusing a name [`check`] would refuse and a target
/// [`valid_target`] would.
pub fn add(dir: &Path, machine: Machine, this_host: &str) -> Result<(), ConfigError> {
    // Here as well as in each caller, so nothing can save a target that the
    // next start would hand to ssh as an option.
    if !valid_target(&machine.target) {
        return Err(ConfigError::Machine {
            path: dir.join(FILE),
            reason: format!("{:?} is not an ssh target", machine.target),
        });
    }

    crate::store::update(dir, FILE, |saved: &mut Saved| {
        // Against the list as it is under the lock, not as the caller's
        // earlier `check` saw it: two adds of one name that both passed that
        // check would otherwise both be saved.
        refusal(dir, &machine.name, this_host, &saved.machines)?;
        saved.machines.push(machine);
        Ok(((), true))
    })
}

/// Takes the machine called `name` off the list.
///
/// Answers whether it was there to take off.
pub fn remove(dir: &Path, name: &str) -> Result<bool, ConfigError> {
    crate::store::update(dir, FILE, |saved: &mut Saved| {
        let before = saved.machines.len();
        saved.machines.retain(|machine| machine.name != name);

        let removed = saved.machines.len() != before;
        Ok((removed, removed))
    })
}
```

- [ ] **Step 6: Run the tests, the gates, and commit**

```bash
cargo test -p dispatch-config --locked
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
~/.cargo/bin/cargo +1.89.0 build --workspace --all-targets --locked --target-dir target/msrv-1.89
git add crates/dispatch-config
git commit -m "fix(config): change the registries under a lock, by atomic replace

Two processes keeping a project at once could each overwrite the other's,
a failed write could leave half a file, and a reader could see the half.
Every change now locks a file beside the registry, reads, changes, stages
a copy and renames it into place; adding a machine checks its name inside
that same step.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
```

Expected: CI green, including `msrv (1.89)` (this is the first use of `File::lock`).

---
### Task 14: The Windows pipe admits its owner, and only from this machine (A02)

`CreateNamedPipeW` is given a null security descriptor, under a comment claiming that means creator-only. It does not: the default DACL also grants read access to Everyone and to anonymous logons, and without `PIPE_REJECT_REMOTE_CLIENTS` a client on another machine is refused only by SMB and firewall policy. (Task 6 already stopped a silent or read-only opener from holding up the accept loop; this task stops anyone but the owner opening the pipe at all.)

Everything here is Windows-only. It is compiled locally by the cross-check and run only by the Windows CI job.

**Files:**
- Modify: `crates/dispatch-os/src/ipc.rs` (module doc; Windows `imp`: `OwnerOnly`, `current_user_sid`, `sid_string`, `create_instance`, `Listener`, `bind`, `accept`, `pipe_name` visibility, test helpers; Windows tests)
- Modify: `crates/dispatch-os/Cargo.toml` (`Win32_Security`, `Win32_Security_Authorization`, `Win32_System_SystemServices`)

**Interfaces:**
- Consumes: `Listener`, `Endpoint` test guard, `reading` and `PATIENCE` from Task 6's tests.
- Produces (Windows, `imp`): `struct OwnerOnly` (a DACL admitting the current user alone); `fn current_user_sid() -> io::Result<String>`; `create_instance(name, first, &OwnerOnly)`; test-only `dacl_of(handle) -> io::Result<Vec<(u8, String)>>`.

- [ ] **Step 1: Write the (Windows-only) failing tests**

Append to `crates/dispatch-os/src/ipc.rs`'s `mod tests`:

```rust
    #[test]
    #[cfg(windows)]
    fn the_pipe_admits_its_owner_and_nobody_else() {
        // The null descriptor this replaces took the default DACL, which
        // also let Everyone and anonymous logons open the pipe to read.
        let name = format!(r"\\.\pipe\dispatchd-test-owner-{}", std::process::id());
        let security = imp::OwnerOnly::new().expect("the descriptor builds");
        let pipe = imp::create_instance(&name, true, &security).expect("the pipe is created");

        let entries = imp::dacl_of(pipe).expect("the DACL reads back");
        imp::close_for_test(pipe);

        let me = imp::current_user_sid().expect("this process has a user");
        assert_eq!(
            entries,
            vec![(imp::ACCESS_ALLOWED, me)],
            "exactly one entry, allowing this user"
        );
    }

    #[test]
    #[cfg(windows)]
    fn a_read_only_handle_that_says_nothing_holds_up_nobody() {
        // The shape the audit described: open for reading only, which can
        // never write a preamble, and hold it. It used to park the accept
        // loop for good.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("read-only");

        let listener = Listener::bind().expect("binding succeeds");
        let name = imp::pipe_name(&endpoint().expect("resolves"));

        let (served, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (reader, _writer) = listener.accept().expect("accepting succeeds").split();
            let _ = served.send(reading(reader).recv_timeout(PATIENCE));
        });

        let mut silent = std::fs::OpenOptions::new()
            .read(true)
            .open(&name)
            .expect("the owner may open it");
        std::thread::sleep(Duration::from_millis(50));

        let started = std::time::Instant::now();
        let (_reader, mut writer) = Connection::connect().expect("connecting succeeds").split();
        writer.write_all(b"next").expect("writing succeeds");
        writer.flush().expect("flushing succeeds");
        assert_eq!(done.recv_timeout(PATIENCE), Ok(Ok(*b"next")));
        assert!(started.elapsed() < Duration::from_secs(1));

        let (ended, end) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            let _ = silent.read(&mut byte);
            let _ = ended.send(());
        });
        assert!(
            end.recv_timeout(PATIENCE).is_ok(),
            "the silent read-only handle was never let go"
        );
    }

    #[test]
    #[cfg(windows)]
    fn a_client_addressing_the_pipe_as_another_machine_would_is_refused() {
        // `\\localhost\pipe\…` goes through the SMB redirector, which is what
        // a client on another machine does. Without the Server service
        // running this fails regardless; with it, only
        // PIPE_REJECT_REMOTE_CLIENTS refuses it.
        let _guard = crate::env_lock();
        let _endpoint = Endpoint::new("remote");

        let _listener = Listener::bind().expect("binding succeeds");
        let local = imp::pipe_name(&endpoint().expect("resolves"));
        let remote = local.replacen(r"\\.\", r"\\localhost\", 1);

        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&remote);
        assert!(opened.is_err(), "a remote-style open reached the pipe");
    }
```

- [ ] **Step 2: Confirm they do not compile yet**

```bash
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings 2>&1 | grep -m5 error
```

Expected: errors naming `OwnerOnly`, `dacl_of`, `close_for_test`, `current_user_sid`, `ACCESS_ALLOWED` and `pipe_name`'s privacy. (Red on Windows is observed in CI; locally the red is the missing API.)

- [ ] **Step 3: Build an owner-only descriptor and use it**

`crates/dispatch-os/Cargo.toml`: add `"Win32_Security"`, `"Win32_Security_Authorization"`, `"Win32_System_SystemServices"` to the `windows-sys` features.

In the Windows `mod imp`, add imports:

```rust
    use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Pipes::PIPE_REJECT_REMOTE_CLIENTS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
```

and:

```rust
    /// A security descriptor admitting the user this process runs as, and
    /// nobody else.
    ///
    /// SDDL `D:P(A;;GA;;;<sid>)`: a protected DACL, so nothing is inherited
    /// into it, whose one entry grants everything to this user. The null
    /// descriptor it replaces took the default DACL, which also lets Everyone
    /// and anonymous logons open the pipe for reading.
    pub(super) struct OwnerOnly(PSECURITY_DESCRIPTOR);

    // SAFETY: the descriptor is never changed after it is built, and is freed
    // exactly once, on drop.
    unsafe impl Send for OwnerOnly {}
    // SAFETY: as above -- shared use only ever reads it.
    unsafe impl Sync for OwnerOnly {}

    impl OwnerOnly {
        pub(super) fn new() -> std::io::Result<Self> {
            let sid = current_user_sid()?;
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: `sddl` is NUL-terminated and outlives the call; on
            // success `descriptor` is a LocalAlloc'd descriptor that `Drop`
            // frees.
            let converted = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            };
            if converted == 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(Self(descriptor))
        }

        fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
                    .expect("a small struct"),
                lpSecurityDescriptor: self.0,
                bInheritHandle: 0,
            }
        }
    }

    impl Drop for OwnerOnly {
        fn drop(&mut self) {
            // SAFETY: the descriptor came from LocalAlloc via the conversion
            // above and is freed exactly once, here.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }

    /// The SID of the user this process runs as, as a string (`S-1-5-21-…`).
    pub(super) fn current_user_sid() -> std::io::Result<String> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

        let mut raw: HANDLE = std::ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // closing; `raw` receives a token handle owned below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a handle this call just opened, owned from here on.
        let token = unsafe { OwnedHandle::from_raw_handle(raw as _) };

        let mut needed = 0u32;
        // SAFETY: a null buffer of length zero only asks how much is needed.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle() as HANDLE,
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };

        // u64s, not bytes: TOKEN_USER holds a pointer and must be aligned.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buffer` holds at least `needed` bytes.
        let read = unsafe {
            GetTokenInformation(
                token.as_raw_handle() as HANDLE,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        if read == 0 {
            return Err(std::io::Error::last_os_error());
        }

        // SAFETY: on success the buffer begins with a TOKEN_USER whose SID
        // points into the same buffer, which outlives its use here.
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        sid_string(user.User.Sid)
    }

    /// `sid` as a string (`S-1-5-21-…`).
    pub(super) fn sid_string(sid: PSID) -> std::io::Result<String> {
        let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: `sid` is valid for the call; `text` receives a LocalAlloc'd
        // string freed below.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(std::io::Error::last_os_error());
        }

        // SAFETY: `text` is the NUL-terminated string the call returned.
        let string = unsafe {
            let length = (0..).take_while(|&i| *text.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
        };
        // SAFETY: allocated by ConvertSidToStringSidW, freed exactly once.
        unsafe { LocalFree(text as HLOCAL) };

        Ok(string)
    }
```

`fn pipe_name` becomes `pub(super) fn pipe_name`. The Windows `Listener` gains:

```rust
        /// Who may open each instance: this user, and nobody else.
        security: OwnerOnly,
```

`create_instance` becomes `pub(super) fn create_instance(name: &str, first: bool, security: &OwnerOnly) -> Result<isize, std::io::Error>`, with the call:

```rust
        let attributes = security.attributes();

        // SAFETY: `wide` is a NUL-terminated wide string and `attributes`
        // points at a descriptor `security` keeps alive; both outlive the
        // call.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                flags,
                // Local clients only: a client on another machine reaches a
                // named pipe through SMB, and nothing Dispatch speaks is meant
                // to cross it.
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &attributes,
            )
        };
```

`bind` builds `let security = OwnerOnly::new().map_err(|e| IpcError::io("building the pipe's access list", e))?;` before the first `create_instance(&name, true, &security)`, and stores it; `accept` passes `&listener.security`.

Test helpers, at the end of the Windows `mod imp`:

```rust
    /// `ACCESS_ALLOWED_ACE_TYPE`, as the `u8` an ACE header carries.
    #[cfg(test)]
    pub(super) const ACCESS_ALLOWED: u8 =
        windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE as u8;

    /// Closes a pipe instance a test created directly.
    #[cfg(test)]
    pub(super) fn close_for_test(handle: isize) {
        use windows_sys::Win32::Foundation::CloseHandle;
        // SAFETY: a handle from `create_instance`, closed exactly once.
        unsafe { CloseHandle(handle as HANDLE) };
    }

    /// Each entry of the DACL on `handle`, as (ACE type, SID string).
    #[cfg(test)]
    pub(super) fn dacl_of(handle: isize) -> std::io::Result<Vec<(u8, String)>> {
        use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
        use windows_sys::Win32::Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
            DACL_SECURITY_INFORMATION, GetAce, GetAclInformation,
        };

        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: every out-pointer is valid; on success `descriptor` is
        // LocalAlloc'd and `dacl` points into it.
        let status = unsafe {
            GetSecurityInfo(
                handle as HANDLE,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }

        // SAFETY: an all-zero ACL_SIZE_INFORMATION is valid, and `dacl` is
        // the DACL the call above returned.
        let mut size: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
        let sized = unsafe {
            GetAclInformation(
                dacl,
                (&raw mut size).cast(),
                u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>()).expect("small"),
                AclSizeInformation,
            )
        };
        if sized == 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut entries = Vec::new();
        for index in 0..size.AceCount {
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `index` is within the count the ACL reported.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: every ACE starts with a header; an allowed ACE keeps
            // its SID where ACCESS_ALLOWED_ACE says, and any other type
            // fails the equality the test makes.
            let (kind, sid) = unsafe {
                let header = &*ace.cast::<ACE_HEADER>();
                let sid = (&raw const (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart) as PSID;
                (header.AceType, sid)
            };
            entries.push((kind, sid_string(sid)?));
        }

        // SAFETY: allocated by GetSecurityInfo, freed exactly once.
        unsafe { LocalFree(descriptor as HLOCAL) };
        Ok(entries)
    }
```

Replace the module doc at the top of `ipc.rs`:

```rust
//! Local transport between a Dispatch client and `dispatchd`.
//!
//! A Unix domain socket on POSIX, readable and writable by its owner alone;
//! a named pipe on Windows, whose DACL admits its owner alone and which
//! refuses clients on other machines. That access control is what keeps
//! another user off a daemon that can run arbitrary commands.
```

- [ ] **Step 4: Cross-check, gates, commit, push, and read the Windows job**

```bash
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-os
git commit -m "fix(os): let only the owner open the Windows pipe, and only locally

A null security descriptor gave the pipe the default DACL, which grants
Everyone and anonymous logons read access; remote clients were refused
only by SMB policy. The pipe now carries an owner-only protected DACL and
PIPE_REJECT_REMOTE_CLIENTS.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
gh pr checks --watch
```

Expected: `test (windows-latest)` green, with the three new tests reported as run (check the log: `the_pipe_admits_its_owner_and_nobody_else ... ok`). A failure here is fixed in this task before moving on.

---

### Task 15: On Windows, ending a process ends everything it started (A03, process trees)

`terminate_tree` calls `TerminateProcess` on one handle, beside a comment saying the child was put in a job object. Nothing creates one; `CREATE_NEW_PROCESS_GROUP` contains nothing; Windows does not end a process's children with it. So closing a pane, dropping an `ssh` transport or stopping the daemon leaves descendants running. It also treats every `OpenProcess` failure as "already gone", including access denied, and waits on a handle opened without `SYNCHRONIZE`, which always fails.

This task adds `spawn_contained`: on Windows the child is created suspended, put in a Job Object that kills its members when closed, and only then resumed, so nothing it starts can be outside the job. `terminate_tree` ends a contained pid's whole job. Command transports switch to it here; panes in Task 16.

**Files:**
- Modify: `crates/dispatch-os/src/process.rs` (new `spawn_contained`, `is_running`, `descendants`; Windows `imp` rewritten around jobs; Unix `imp` gains the same three; Windows tests)
- Modify: `crates/dispatch-os/src/ipc.rs` (`over_command` uses `spawn_contained`; `put_in_its_own_group` removed; Windows test)
- Modify: `crates/dispatch-os/Cargo.toml` (`Win32_System_Diagnostics_ToolHelp`)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub fn spawn_contained(command: &mut std::process::Command) -> std::io::Result<std::process::Child>` — replaces any creation flags set on `command` (Windows).
  - `#[doc(hidden)] pub fn is_running(pid: u32) -> bool` and `#[doc(hidden)] pub fn descendants(pid: u32) -> Vec<u32>` — for tests in every crate. On Unix a zombie still counts as running; tests poll.
  - Windows only, crate-visible: `pub(crate) fn contain(process: HANDLE, pid: u32)` — puts an already-created, still-suspended process in a job of its own and records it for `terminate_tree`. Task 16 calls it.

- [ ] **Step 1: Write the failing tests**

In `process.rs`, add a Windows test module after the Unix one:

```rust
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// Waits for `condition`, returning whether it held in time.
    fn eventually(patience: Duration, condition: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + patience;
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        condition()
    }

    /// `cmd.exe` running `ping` for thirty seconds: a child with a grandchild.
    fn tree() -> std::process::Command {
        let mut command = std::process::Command::new("cmd.exe");
        command
            .args(["/d", "/c", "ping -n 30 127.0.0.1 >nul"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }

    #[test]
    fn a_contained_process_runs() {
        // Created suspended: one never resumed would hang this forever.
        let mut command = std::process::Command::new("cmd.exe");
        command.args(["/d", "/c", "exit 3"]);
        let mut child = spawn_contained(&mut command).expect("cmd.exe starts");

        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(child.wait().map(|status| status.code()));
        });
        let status = finished
            .recv_timeout(Duration::from_secs(10))
            .expect("it ran to the end")
            .expect("waiting succeeds");
        assert_eq!(status, Some(3));
    }

    #[test]
    fn terminating_a_contained_tree_ends_the_grandchild_too() {
        let mut child = spawn_contained(&mut tree()).expect("cmd.exe starts");
        let pid = child.id();
        assert!(
            eventually(Duration::from_secs(10), || !descendants(pid).is_empty()),
            "cmd.exe never started ping"
        );
        let everyone: Vec<u32> = std::iter::once(pid).chain(descendants(pid)).collect();

        terminate_tree(pid, DEFAULT_GRACE).expect("the tree is ended");
        let _ = child.wait();

        assert!(
            eventually(Duration::from_secs(5), || everyone.iter().all(|p| !is_running(*p))),
            "a process in the tree outlived it: {everyone:?}"
        );
    }

    #[test]
    fn terminating_a_process_that_has_gone_is_not_an_error() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "exit 0"])
            .spawn()
            .expect("cmd.exe starts");
        let pid = child.id();
        child.wait().expect("it exits");

        terminate_tree(pid, DEFAULT_GRACE).expect("a process that has gone is what was asked for");
    }
}
```

In `ipc.rs`'s tests:

```rust
    #[test]
    #[cfg(windows)]
    fn dropping_a_command_connection_ends_its_whole_tree() {
        // `ssh.exe` forks too. The transport used to kill only the process it
        // started, leaving its children holding the pipes.
        let connection = Connection::over_command(
            std::ffi::OsStr::new("cmd.exe"),
            &[
                std::ffi::OsString::from("/d"),
                std::ffi::OsString::from("/c"),
                std::ffi::OsString::from("ping -n 30 127.0.0.1 >nul"),
            ],
        )
        .expect("cmd.exe exists");
        let pid = connection.child_id().expect("a command transport has a child");

        let deadline = std::time::Instant::now() + PATIENCE;
        let everyone = loop {
            let below = crate::process::descendants(pid);
            if !below.is_empty() {
                break std::iter::once(pid).chain(below).collect::<Vec<_>>();
            }
            assert!(std::time::Instant::now() < deadline, "cmd.exe never started ping");
            std::thread::sleep(Duration::from_millis(20));
        };

        drop(connection);

        let deadline = std::time::Instant::now() + PATIENCE;
        while everyone.iter().any(|p| crate::process::is_running(*p))
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            everyone.iter().all(|p| !crate::process::is_running(*p)),
            "a process behind the transport outlived it: {everyone:?}"
        );
    }
```

And a cross-platform check that the helpers work, in `process.rs`'s Unix test module:

```rust
    #[test]
    fn a_contained_tree_is_found_and_ended_whole() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_contained(&mut command).expect("sh starts");
        let pid = child.id();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while descendants(pid).len() < 2 {
            assert!(std::time::Instant::now() < deadline, "sh never started both sleeps");
            std::thread::sleep(Duration::from_millis(20));
        }
        let everyone: Vec<u32> = std::iter::once(pid).chain(descendants(pid)).collect();

        terminate_tree(pid, DEFAULT_GRACE).expect("the tree is ended");
        child.wait().expect("sh can be reaped");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while everyone.iter().any(|p| is_running(*p)) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(everyone.iter().all(|p| !is_running(*p)), "{everyone:?}");
    }
```

- [ ] **Step 2: See them fail**

```bash
cargo test -p dispatch-os --locked a_contained_tree
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings 2>&1 | grep -m3 error
```

Expected: compile errors for `spawn_contained`, `descendants`, `is_running` on both.

- [ ] **Step 3: The shared API**

In `process.rs`, after `terminate_tree`:

```rust
/// Starts `command` so that [`terminate_tree`] reaches everything it starts.
///
/// On Unix the child leads a session of its own, so its pid names a process
/// group every descendant stays in unless it leaves on purpose. On Windows it
/// is created suspended, put in a Job Object of its own, and only then
/// resumed: it cannot start anything outside the job, because it starts
/// nothing before it is in it. Any creation flags already set on `command`
/// are replaced on Windows.
pub fn spawn_contained(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    imp::spawn_contained(command)
}

/// Whether `pid` names a process that has not exited. For tests.
///
/// On Unix a process that has exited but not been reaped still answers, so
/// a test polls rather than asking once.
#[doc(hidden)]
#[must_use]
pub fn is_running(pid: u32) -> bool {
    imp::is_running(pid)
}

/// Every process descended from `pid` now, nearest first. For tests.
#[doc(hidden)]
#[must_use]
pub fn descendants(pid: u32) -> Vec<u32> {
    imp::descendants(pid)
}

/// Puts a process that has been created suspended in a job of its own, and
/// records it for [`terminate_tree`]. The caller resumes it.
#[cfg(windows)]
pub(crate) use imp::contain;

/// The pids reachable downward from `root` through `(pid, parent)` pairs.
fn below(root: u32, pairs: &[(u32, u32)]) -> Vec<u32> {
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for &(pid, of) in pairs {
            if of == parent && pid != root && !found.contains(&pid) {
                found.push(pid);
                frontier.push(pid);
            }
        }
    }
    found
}
```

Unix `imp` gains:

```rust
    pub(super) fn spawn_contained(
        command: &mut std::process::Command,
    ) -> std::io::Result<std::process::Child> {
        use std::os::unix::process::CommandExt;

        // SAFETY: setsid is async-signal-safe and is the documented way to
        // leave the parent's session and become a process group leader,
        // which is what lets `killpg` reach every descendant later. The
        // closure allocates nothing and touches no shared state.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        command.spawn()
    }

    pub(super) fn is_running(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 sends nothing and only asks whether the pid exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    pub(super) fn descendants(pid: u32) -> Vec<u32> {
        // `ps` rather than /proc, so macOS and Linux answer the same way.
        let Ok(output) = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,ppid="])
            .output()
        else {
            return Vec::new();
        };

        let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .collect();

        super::below(pid, &pairs)
    }
```

- [ ] **Step 4: Windows: jobs behind `terminate_tree`**

`crates/dispatch-os/Cargo.toml`: add `"Win32_System_Diagnostics_ToolHelp"`.

Replace the Windows `mod imp` of `process.rs` (keep `spawn_detached` and `DETACHED` exactly as they are) with:

```rust
#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::os::windows::io::AsRawHandle;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    use super::{Duration, ProcessError};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, GetExitCodeProcess, OpenProcess, OpenThread,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ResumeThread,
        THREAD_SUSPEND_RESUME, TerminateProcess, WaitForSingleObject,
    };

    /// Starts the child in its own process group, with no console of its own.
    ///
    /// `CREATE_NEW_PROCESS_GROUP` is what keeps a Ctrl-C in the parent's console
    /// from reaching it, and `DETACHED_PROCESS` stops it inheriting that console
    /// at all — a daemon has no business writing to the interface's screen.
    const DETACHED: u32 = 0x0000_0008 | 0x0000_0200;

    /// How long to wait for a tree to be gone once it has been ended.
    const KILL_TIMEOUT: Duration = Duration::from_secs(2);

    // `spawn_detached` goes here, unchanged.

    /// Owns a handle so it is closed on every exit path.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle is owned here and closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// A Job Object whose processes are ended when it is closed.
    ///
    /// Closing is how a daemon that dies takes its panes with it -- as a
    /// clean shutdown would -- instead of leaving agents nobody can reach.
    struct Job(Owned);

    // SAFETY: a job handle may be used from any thread; this one is only
    // closed on drop.
    unsafe impl Send for Job {}

    impl Job {
        fn new() -> std::io::Result<Self> {
            // SAFETY: both arguments may be null: default security, no name.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let job = Self(Owned(handle));

            // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION means
            // no limits; one flag is then set.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is the structure this class names, and lives
            // across the call.
            let set = unsafe {
                SetInformationJobObject(
                    job.0.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                        .expect("a small struct"),
                )
            };
            if set == 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(job)
        }

        fn assign(&self, process: HANDLE) -> std::io::Result<()> {
            // SAFETY: both handles are live for the call.
            if unsafe { AssignProcessToJobObject(self.0.0, process) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        fn terminate(&self) -> std::io::Result<()> {
            // SAFETY: a live job handle.
            if unsafe { TerminateJobObject(self.0.0, 1) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        /// How many processes in the job have not exited.
        fn active(&self) -> std::io::Result<u32> {
            // SAFETY: an all-zero accounting structure is valid to fill.
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is the structure this class names.
            let queried = unsafe {
                QueryInformationJobObject(
                    self.0.0,
                    JobObjectBasicAccountingInformation,
                    (&raw mut info).cast(),
                    u32::try_from(std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
                        .expect("a small struct"),
                    std::ptr::null_mut(),
                )
            };
            if queried == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(info.ActiveProcesses)
        }
    }

    /// The job each contained process was put in, by pid.
    ///
    /// Keyed by pid because that is what every caller of `terminate_tree`
    /// holds. A pid the system reuses for a later contained process replaces
    /// the old entry, and closing the old job ends whatever was left in it.
    fn jobs() -> &'static Mutex<HashMap<u32, Job>> {
        static JOBS: OnceLock<Mutex<HashMap<u32, Job>>> = OnceLock::new();
        JOBS.get_or_init(Mutex::default)
    }

    pub(crate) fn contain(process: HANDLE, pid: u32) {
        match Job::new().and_then(|job| job.assign(process).map(|()| job)) {
            Ok(job) => {
                jobs()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(pid, job);
            }
            // Already in a job that forbids another. It still runs, but
            // ending it ends only it; said here rather than discovered when
            // its children outlive it.
            Err(error) => tracing::warn!(
                %error,
                pid,
                "could not put a process in a job of its own; its children will outlive it"
            ),
        }
    }

    pub(super) fn spawn_contained(
        command: &mut std::process::Command,
    ) -> std::io::Result<std::process::Child> {
        use std::os::windows::process::CommandExt;

        // Suspended, so it runs nothing before it is in its job; in a group
        // of its own, as a command transport always was, so a Ctrl-C meant
        // for this process does not reach it.
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
        let child = command.spawn()?;

        contain(child.as_raw_handle() as HANDLE, child.id());

        if let Err(error) = resume(child.id()) {
            // Never resumed, it would never run: end it rather than hand back
            // a process that hangs whoever waits on it.
            let _ = terminate_tree(child.id(), Duration::ZERO);
            return Err(error);
        }

        Ok(child)
    }

    /// Resumes the one thread of a process created suspended.
    ///
    /// `std::process::Child` keeps no handle to the thread, so it is found
    /// by its owner's pid.
    fn resume(pid: u32) -> std::io::Result<()> {
        // SAFETY: a snapshot of every thread; owned below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let snapshot = Owned(snapshot);

        // SAFETY: an all-zero entry with its size set is what the walk expects.
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(std::mem::size_of::<THREADENTRY32>()).expect("small");

        let mut resumed = false;
        // SAFETY: a live snapshot and a correctly sized entry.
        let mut more = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: a thread id the snapshot just reported.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let thread = Owned(thread);
                // SAFETY: a live thread handle with THREAD_SUSPEND_RESUME.
                if unsafe { ResumeThread(thread.0) } == u32::MAX {
                    return Err(std::io::Error::last_os_error());
                }
                resumed = true;
            }
            // SAFETY: as for Thread32First.
            more = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
        }

        if resumed {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the suspended process has no thread to resume",
            ))
        }
    }

    pub(super) fn terminate_tree(pid: u32, grace: Duration) -> Result<(), ProcessError> {
        let map = |source| ProcessError::Terminate { pid, source };

        let job = jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
        let Some(job) = job else {
            return terminate_one(pid, grace);
        };

        // Ended at once: Windows has no polite request a tree of console
        // programs reliably honours, so the wait is for the tree to be gone
        // rather than for it to leave on its own.
        job.terminate().map_err(map)?;

        let deadline = Instant::now() + KILL_TIMEOUT;
        loop {
            match job.active().map_err(map)? {
                0 => return Ok(()),
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                left => {
                    return Err(map(std::io::Error::other(format!(
                        "{left} processes outlived their job"
                    ))));
                }
            }
        }
    }

    /// Ends one process that was never contained -- a daemon started
    /// detached, say.
    fn terminate_one(pid: u32, grace: Duration) -> Result<(), ProcessError> {
        let map = |source| ProcessError::Terminate { pid, source };

        // SAFETY: OpenProcess takes access flags and a pid by value.
        let raw = unsafe {
            OpenProcess(
                PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if raw.is_null() {
            let error = std::io::Error::last_os_error();
            // No such process: it has exited and been waited for, which is
            // what the caller wanted. Anything else -- access denied above
            // all -- says nothing about whether it is gone.
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Ok(());
            }
            return Err(map(error));
        }
        let process = Owned(raw);

        // SAFETY: a live handle opened with PROCESS_TERMINATE.
        if unsafe { TerminateProcess(process.0, 1) } == 0 {
            let error = std::io::Error::last_os_error();
            // Ending a process that has already exited fails; it is gone all
            // the same.
            if has_exited(&process) {
                return Ok(());
            }
            return Err(map(error));
        }

        let millis = u32::try_from(grace.max(KILL_TIMEOUT).as_millis()).unwrap_or(u32::MAX);
        // SAFETY: a live handle opened with SYNCHRONIZE -- without it, as
        // before, this wait failed every time.
        match unsafe { WaitForSingleObject(process.0, millis) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(map(std::io::Error::from(std::io::ErrorKind::TimedOut))),
            _ => Err(map(std::io::Error::last_os_error())),
        }
    }

    fn has_exited(process: &Owned) -> bool {
        let mut code = 0u32;
        // SAFETY: a live handle opened with PROCESS_QUERY_LIMITED_INFORMATION.
        let read = unsafe { GetExitCodeProcess(process.0, &mut code) };
        read != 0 && code != STILL_ACTIVE as u32
    }

    pub(super) fn is_running(pid: u32) -> bool {
        // SAFETY: OpenProcess takes access flags and a pid by value.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if raw.is_null() {
            return false;
        }
        let process = Owned(raw);
        // SAFETY: a live handle opened with SYNCHRONIZE; a zero wait only asks.
        unsafe { WaitForSingleObject(process.0, 0) == WAIT_TIMEOUT }
    }

    pub(super) fn descendants(pid: u32) -> Vec<u32> {
        // SAFETY: a snapshot of every process; owned below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let snapshot = Owned(snapshot);

        // SAFETY: an all-zero entry with its size set is what the walk expects.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).expect("small");

        let mut pairs = Vec::new();
        // SAFETY: a live snapshot and a correctly sized entry.
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        while more {
            pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
            // SAFETY: as for Process32FirstW.
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }

        super::below(pid, &pairs)
    }
}
```

- [ ] **Step 5: Command transports are contained**

In `ipc.rs`'s `over_command`, replace

```rust
        put_in_its_own_group(&mut command);

        let mut child = command.spawn().map_err(|source| IpcError::Spawn {
```

with

```rust
        // In a group or job of its own, so ending it ends everything it
        // forks: `ssh` and `sh -c` both fork.
        let mut child = crate::process::spawn_contained(&mut command).map_err(|source| IpcError::Spawn {
```

Delete both `put_in_its_own_group` functions, and in `reap`'s doc comment replace "which is what [`put_in_its_own_group`] and [`process::terminate_tree`](crate::process::terminate_tree) are for" with "which is what [`process::spawn_contained`](crate::process::spawn_contained) and [`process::terminate_tree`](crate::process::terminate_tree) are for".

- [ ] **Step 6: Local tests, cross-check, gates, commit, push, read the Windows job**

```bash
cargo test -p dispatch-os --locked
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
git add crates/dispatch-os
git commit -m "fix(os): end a Windows process tree as a tree

Nothing ever created the job object terminate_tree's comment described.
Contained processes are now created suspended, put in a kill-on-close Job
Object, then resumed, and terminate_tree ends the whole job. An
OpenProcess failure other than no-such-process is no longer taken for
success. Command transports are contained.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
gh pr checks --watch
```

Expected: the Windows job runs `terminating_a_contained_tree_ends_the_grandchild_too`, `a_contained_process_runs` and `dropping_a_command_connection_ends_its_whole_tree`, all passing.

---

### Task 16: Panes on Windows are created inside their job (A03, panes)

Panes are started by `portable-pty`, which creates the process running; a job attached afterwards races whatever the child starts first. This task moves the platform half of a pane into `dispatch_os::pty` — `portable-pty` on Unix, unchanged in behaviour; on Windows Dispatch's own ConPTY spawn, which creates the process suspended, calls `process::contain`, and resumes it. It keeps `portable-pty` 0.9's ConPTY flags and answers its inherit-cursor handshake, so panes behave as before. `dispatch-pty` then carries no platform code and no `portable-pty` dependency.

Behaviour that deliberately changes on Windows: the environment a pane inherits is this process's own, plus the harness's `env`. `portable-pty` also re-read the user and system environment from the registry at every spawn; a daemon now sees an environment change when it is restarted, as it already does on Unix.

**Files:**
- Create: `crates/dispatch-os/src/pty.rs`, `crates/dispatch-os/src/pty/windows.rs`
- Modify: `crates/dispatch-os/src/lib.rs` (`pub mod pty;`), `crates/dispatch-os/Cargo.toml` (`portable-pty` for Unix; `Win32_System_Console`)
- Modify: `crates/dispatch-pty/src/session.rs` (`Pty` fields, `spawn`, `resize`, `spawn_reader`, `spawn_waiter`, `answer_inherit_cursor_handshake` removed), `crates/dispatch-pty/Cargo.toml` (drop `portable-pty`)
- Test: `crates/dispatch-os/src/pty.rs` (quoting), `crates/dispatch-pty/src/session/tests.rs`, `crates/dispatch-daemon/src/session/tests.rs`

**Interfaces:**
- Consumes: `process::contain` (Task 15), `process::descendants`, `process::is_running`.
- Produces:
  - `dispatch_os::pty::PtyCommand<'a> { program: &'a str, args: &'a [String], env: &'a BTreeMap<String, String>, cwd: &'a Path }`
  - `dispatch_os::pty::spawn(command: &PtyCommand<'_>, rows: u16, cols: u16) -> std::io::Result<PtyProcess>`
  - `PtyProcess { reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>, terminal: Terminal, child: Child, pid: Option<u32> }`
  - `Terminal::resize(&self, rows: u16, cols: u16) -> std::io::Result<()>`; dropping a `Terminal` ends the pseudoterminal.
  - `Child::wait(self) -> i32` — 0 for success, the process's code otherwise, 1 for a failure with no code.

- [ ] **Step 1: Write the failing tests**

`crates/dispatch-os/src/pty.rs` will carry the quoting its Windows spawn uses, platform-neutral so it is tested everywhere. Write its tests first (in the new file, below a placeholder `quote_for_crt` that returns its input unchanged):

```rust
#[cfg(test)]
mod tests {
    use super::quote_for_crt;

    #[test]
    fn a_plain_argument_is_left_alone() {
        assert_eq!(quote_for_crt("claude"), "claude");
        assert_eq!(quote_for_crt("<%DISPATCH_TASK_FILE%"), "<%DISPATCH_TASK_FILE%");
    }

    #[test]
    fn an_argument_with_a_space_is_quoted() {
        assert_eq!(quote_for_crt("two words"), "\"two words\"");
        assert_eq!(quote_for_crt(""), "\"\"");
    }

    #[test]
    fn quotes_and_the_backslashes_before_them_are_escaped() {
        assert_eq!(quote_for_crt(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_for_crt(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_for_crt(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_for_crt(r"C:\a\b c"), r#""C:\a\b c""#);
    }
}
```

Append to `crates/dispatch-pty/src/session/tests.rs`:

```rust
/// A script that starts something and waits, on every platform: a pane
/// with a grandchild.
fn a_tree() -> &'static str {
    if cfg!(windows) {
        "ping -n 30 127.0.0.1 >nul"
    } else {
        "sleep 30 & sleep 30"
    }
}

/// How many processes below the pane `a_tree` starts.
fn tree_size() -> usize {
    if cfg!(windows) { 1 } else { 2 }
}

#[test]
fn terminating_a_pane_ends_everything_it_started() {
    let mut pty = Pty::spawn(&shell(a_tree()), &cwd(), Size::new(80, 24)).expect("the shell starts");
    let pid = pty.pid().expect("a running pane has a pid");

    let deadline = std::time::Instant::now() + TIMEOUT;
    while dispatch_os::process::descendants(pid).len() < tree_size() {
        assert!(std::time::Instant::now() < deadline, "the pane never started its children");
        std::thread::sleep(Duration::from_millis(20));
    }
    let everyone: Vec<u32> = std::iter::once(pid)
        .chain(dispatch_os::process::descendants(pid))
        .collect();

    pty.terminate();

    let deadline = std::time::Instant::now() + TIMEOUT;
    while everyone.iter().any(|p| dispatch_os::process::is_running(*p))
        && std::time::Instant::now() < deadline
    {
        pty.drain();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        everyone.iter().all(|p| !dispatch_os::process::is_running(*p)),
        "a process the pane started outlived it: {everyone:?}"
    );
}
```

(`dispatch-os` is already a dependency of `dispatch-pty`.)

In `crates/dispatch-daemon/src/session/tests.rs`'s `harnesses()`, add a `tree` harness:

```rust
    // A pane with a grandchild, so shutdown can be seen to end the whole tree.
    let tree = if cfg!(windows) {
        "id = \"tree\"\ndisplay_name = \"Tree\"\ncommand = \"cmd.exe\"\nargs = [\"/c\", \"ping -n 30 127.0.0.1 >nul\"]\n"
    } else {
        "id = \"tree\"\ndisplay_name = \"Tree\"\ncommand = \"sh\"\nargs = [\"-c\", \"sleep 30 & sleep 30\"]\n"
    };
    std::fs::write(dir.join("tree.toml"), tree).expect("temp dir is writable");
```

and the test:

```rust
#[test]
fn shutting_down_ends_every_panes_whole_tree() {
    let (mut daemon, project, _dir) = daemon("tree-down");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "tree".into(),
            size: (80, 24),
        },
    );
    wait_for(&mut daemon, &inbox, |m| {
        m.iter().any(|m| matches!(m, ServerMessage::PaneSpawned { .. }))
    });

    let pid = daemon
        .pane_pids_for_test()
        .into_iter()
        .next()
        .expect("the pane has a pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    let everyone = loop {
        let below = dispatch_os::process::descendants(pid);
        if !below.is_empty() {
            break std::iter::once(pid).chain(below).collect::<Vec<_>>();
        }
        assert!(Instant::now() < deadline, "the pane never started its children");
        std::thread::sleep(Duration::from_millis(20));
    };

    daemon.shutdown_handle().request();
    daemon.run();

    let deadline = Instant::now() + Duration::from_secs(10);
    while everyone.iter().any(|p| dispatch_os::process::is_running(*p)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        everyone.iter().all(|p| !dispatch_os::process::is_running(*p)),
        "a process a pane started outlived the daemon: {everyone:?}"
    );
}
```

Add the helper it needs to `session.rs`'s test seam:

```rust
    /// The pid of every pane's process, for tests that watch a tree end.
    #[doc(hidden)]
    #[must_use]
    pub fn pane_pids_for_test(&self) -> Vec<u32> {
        self.panes.values().filter_map(|pane| pane.session.pid()).collect()
    }
```

- [ ] **Step 2: See them fail**

```bash
cargo test -p dispatch-os --locked pty::tests
cargo test -p dispatch-pty --locked terminating_a_pane_ends_everything
cargo test -p dispatch-daemon --locked shutting_down_ends_every_panes_whole_tree
```

Expected: the quoting tests FAIL against the placeholder (`"two words"` comes back unquoted, and so on). The pane and shutdown tests PASS on Unix already (process groups have always worked there) — they exist for the Windows job, where they FAIL until this task: record that CI shows them failing on Windows only if you push before Step 5, or rely on the Step 7 run.

- [ ] **Step 3: `dispatch_os::pty`**

`crates/dispatch-os/Cargo.toml`:

```toml
[target."cfg(unix)".dependencies]
libc = { workspace = true }
portable-pty = { workspace = true }
```

and add `"Win32_System_Console"` to the Windows features. `crates/dispatch-os/src/lib.rs`: `pub mod pty;`.

`crates/dispatch-os/src/pty.rs`:

```rust
//! Starting a process in a pseudoterminal.
//!
//! The platform half of a pane. On Unix this is `portable-pty`. On Windows
//! it is Dispatch's own ConPTY spawn: a pane's process has to be created
//! suspended and put in a Job Object before it runs, or anything it starts
//! first escapes the job -- and `portable-pty` starts it running.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

#[cfg(windows)]
mod windows;

/// What to start.
#[derive(Debug, Clone, Copy)]
pub struct PtyCommand<'a> {
    /// The program, found on `PATH` when not a path itself.
    pub program: &'a str,
    /// Its arguments.
    pub args: &'a [String],
    /// Variables set on top of the environment this process has.
    pub env: &'a BTreeMap<String, String>,
    /// Where it starts.
    pub cwd: &'a Path,
}

/// A process running in a pseudoterminal.
pub struct PtyProcess {
    /// What the process prints.
    pub reader: Box<dyn Read + Send>,
    /// What it reads, as if typed.
    pub writer: Box<dyn Write + Send>,
    /// The pseudoterminal. Held for as long as the process should have one.
    pub terminal: Terminal,
    /// The process, for waiting on.
    pub child: Child,
    /// Its process id, which `process::terminate_tree` ends the tree by.
    pub pid: Option<u32>,
}

impl std::fmt::Debug for PtyProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyProcess").field("pid", &self.pid).finish_non_exhaustive()
    }
}

/// A pseudoterminal. Dropping it ends it.
pub struct Terminal(imp::Terminal);

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Terminal")
    }
}

impl Terminal {
    /// Resizes the pseudoterminal; the process learns of it as a terminal
    /// resize.
    pub fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
        self.0.resize(rows, cols)
    }
}

/// A process to wait on.
pub struct Child(imp::Child);

impl std::fmt::Debug for Child {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Child")
    }
}

impl Child {
    /// Waits for the process to exit.
    ///
    /// 0 for success, the process's code otherwise, and 1 for a failure
    /// that carried no code: a failed exit must never read as a clean one.
    #[must_use]
    pub fn wait(self) -> i32 {
        self.0.wait()
    }
}

/// Starts `command` in a new pseudoterminal of `rows` by `cols`.
pub fn spawn(command: &PtyCommand<'_>, rows: u16, cols: u16) -> std::io::Result<PtyProcess> {
    imp::spawn(command, rows, cols)
}

/// `arg` quoted so the C runtime's command-line parser splits it back out
/// unchanged.
///
/// Left alone when it has no space, tab, newline, vertical tab or quote;
/// otherwise wrapped in quotes, with each quote escaped and the backslashes
/// before a quote -- or before the closing quote -- doubled. The rules
/// `portable-pty` 0.9 followed, so every argument reaches a program as it
/// did. They are not `cmd.exe`'s rules, which is why a task never travels
/// as an argument to `cmd.exe` (see `dispatch_config::TaskInput`).
#[cfg_attr(not(windows), allow(dead_code))]
fn quote_for_crt(arg: &str) -> String {
    let plain = !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']);
    if plain {
        return arg.to_string();
    }

    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        let doubled = if c == '"' { backslashes * 2 + 1 } else { backslashes };
        quoted.extend(std::iter::repeat_n('\\', doubled));
        backslashes = 0;
        quoted.push(c);
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(unix)]
mod imp {
    use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

    pub(super) struct Terminal(Box<dyn MasterPty + Send>);

    impl Terminal {
        pub(super) fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
            self.0
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|error| std::io::Error::other(format!("{error:#}")))
        }
    }

    pub(super) struct Child(Box<dyn portable_pty::Child + Send + Sync>);

    impl Child {
        pub(super) fn wait(mut self) -> i32 {
            match self.0.wait() {
                Ok(status) if status.success() => 0,
                Ok(status) => i32::try_from(status.exit_code()).unwrap_or(1),
                Err(_) => 1,
            }
        }
    }

    pub(super) fn spawn(
        command: &super::PtyCommand<'_>,
        rows: u16,
        cols: u16,
    ) -> std::io::Result<super::PtyProcess> {
        // `portable-pty` reports through `anyhow`; this crate speaks
        // `io::Error`, with the whole chain kept in the message.
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::other(format!("failed to open a pseudoterminal: {e:#}")))?;

        let mut builder = CommandBuilder::new(command.program);
        builder.args(command.args);
        builder.cwd(command.cwd);
        for (key, value) in command.env {
            builder.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;

        // The slave is held open by the child, and dropping this copy is what
        // lets the reader see end-of-file when the child exits.
        drop(pair.slave);

        let pid = child.process_id();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;

        Ok(super::PtyProcess {
            reader,
            writer,
            terminal: super::Terminal(Terminal(pair.master)),
            child: super::Child(Child(child)),
            pid,
        })
    }
}

#[cfg(windows)]
use windows as imp;
```

`crates/dispatch-os/src/pty/windows.rs`:

```rust
//! Dispatch's own ConPTY spawn.
//!
//! What `portable-pty` 0.9 does, with one difference that is the reason
//! this exists: the process is created suspended and put in a Job Object
//! before it runs a single instruction, so nothing it starts can escape the
//! job that `process::terminate_tree` ends.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

/// The flags `portable-pty` 0.9 created every pseudoconsole with, so panes
/// behave as they did: ask the host where its cursor is (answered in
/// `spawn`), redraw correctly on resize, and pass keys in win32-input-mode.
const PSEUDOCONSOLE_INHERIT_CURSOR: u32 = 0x1;
const PSEUDOCONSOLE_RESIZE_QUIRK: u32 = 0x2;
const PSEUDOCONSOLE_WIN32_INPUT_MODE: u32 = 0x4;

/// A pseudoconsole, closed when dropped.
pub(super) struct Terminal(HPCON);

impl Terminal {
    pub(super) fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
        // SAFETY: a live pseudoconsole; the size is passed by value.
        let result = unsafe { ResizePseudoConsole(self.0, coord(rows, cols)) };
        if result != 0 {
            return Err(std::io::Error::other(format!(
                "resizing the pseudoconsole failed: HRESULT {result:#x}"
            )));
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: created by CreatePseudoConsole and closed exactly once,
        // here. The reader keeps draining its pipe until end-of-file (see
        // `dispatch-pty`), which is what lets this return.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

/// A process to wait on.
pub(super) struct Child(OwnedHandle);

impl Child {
    pub(super) fn wait(self) -> i32 {
        let handle = self.0.as_raw_handle() as HANDLE;
        // SAFETY: a live process handle; waiting is always defined.
        unsafe { WaitForSingleObject(handle, INFINITE) };

        let mut code = 0u32;
        // SAFETY: a live process handle and a place for its code.
        if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
            return 1;
        }
        if code == 0 {
            0
        } else {
            i32::try_from(code).unwrap_or(1)
        }
    }
}

pub(super) fn spawn(
    command: &super::PtyCommand<'_>,
    rows: u16,
    cols: u16,
) -> std::io::Result<super::PtyProcess> {
    // ConPTY reads the child's input from one pipe and writes its output to
    // another. This side keeps the writing end of the first and the reading
    // end of the second; ConPTY's ends are closed here once it holds its
    // own, so the pipes end when it does.
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;

    let mut console: HPCON = 0;
    // SAFETY: both handles are live, and `console` receives the new
    // pseudoconsole, owned by `Terminal` from here on.
    let created = unsafe {
        CreatePseudoConsole(
            coord(rows, cols),
            input_read.as_raw_handle() as HANDLE,
            output_write.as_raw_handle() as HANDLE,
            PSEUDOCONSOLE_INHERIT_CURSOR | PSEUDOCONSOLE_RESIZE_QUIRK | PSEUDOCONSOLE_WIN32_INPUT_MODE,
            &mut console,
        )
    };
    if created != 0 {
        return Err(std::io::Error::other(format!(
            "failed to create a pseudoconsole: HRESULT {created:#x}"
        )));
    }
    let terminal = Terminal(console);
    drop(input_read);
    drop(output_write);

    let mut attributes = AttributeList::with_pseudoconsole(console)?;

    // SAFETY: an all-zero STARTUPINFOEXW is valid; the fields that matter are
    // set below.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).expect("small");
    // Explicitly invalid, as `portable-pty` does: otherwise a daemon whose
    // own standard handles point at a log file would hand them to the child,
    // which would write there instead of to its terminal.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attributes.as_mut_ptr();

    let environment = environment(command.env);
    let program = resolve(command.program, &environment);
    let application = wide(program.as_os_str());
    let mut line = command_line(&program, command.args);
    let mut block = environment_block(&environment);
    let cwd = wide(command.cwd.as_os_str());

    // SAFETY: an all-zero PROCESS_INFORMATION is filled by the call.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every string is NUL-terminated and outlives the call; the
    // command line is mutable as CreateProcessW requires; `startup` and its
    // attribute list live across the call.
    let started = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            block.as_mut_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        )
    };
    if started == 0 {
        let error = std::io::Error::last_os_error();
        return Err(std::io::Error::new(
            error.kind(),
            format!("cannot run {}: {error}", program.display()),
        ));
    }

    // SAFETY: both handles were just returned to this process, which owns
    // them from here on.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess as RawHandle) };
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread as RawHandle) };

    crate::process::contain(process.as_raw_handle() as HANDLE, info.dwProcessId);

    // SAFETY: the process's one thread, created suspended.
    if unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) } == u32::MAX {
        let error = std::io::Error::last_os_error();
        let _ = crate::process::terminate_tree(info.dwProcessId, std::time::Duration::ZERO);
        return Err(error);
    }

    let mut writer = std::fs::File::from(input_write);
    answer_inherit_cursor(&mut writer);

    Ok(super::PtyProcess {
        reader: Box::new(std::fs::File::from(output_read)),
        writer: Box::new(writer),
        terminal: super::Terminal(terminal),
        child: super::Child(Child(process)),
        pid: Some(info.dwProcessId),
    })
}

/// Answers the pseudoconsole's inherit-cursor question.
///
/// `PSEUDOCONSOLE_INHERIT_CURSOR` makes ConPTY ask its host where the cursor
/// is and wait for the answer before it pumps anything. A terminal emulator
/// answers because it is one; Dispatch hosts the pseudoconsole instead, so
/// without this the child starts, prints nothing, and never exits. Row 1,
/// column 1. A failure is not fatal on its own, so it is logged.
fn answer_inherit_cursor(writer: &mut std::fs::File) {
    use std::io::Write;

    if let Err(error) = writer.write_all(b"\x1b[1;1R").and_then(|()| writer.flush()) {
        tracing::warn!(%error, "failed to answer the ConPTY inherit-cursor handshake");
    }
}

fn coord(rows: u16, cols: u16) -> COORD {
    COORD {
        X: i16::try_from(cols).unwrap_or(i16::MAX),
        Y: i16::try_from(rows).unwrap_or(i16::MAX),
    }
}

fn pipe() -> std::io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: both out-pointers are valid; default security makes the ends
    // uninheritable, which is what a pseudoconsole's pipes should be.
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both handles were just created and are owned from here on.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read as RawHandle),
            OwnedHandle::from_raw_handle(write as RawHandle),
        )
    })
}

/// A thread attribute list carrying one pseudoconsole.
struct AttributeList(Vec<usize>);

impl AttributeList {
    fn with_pseudoconsole(console: HPCON) -> std::io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a null list only asks how large one must be.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };

        // usizes, not bytes: the list holds pointers and must be aligned.
        let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list: LPPROC_THREAD_ATTRIBUTE_LIST = buffer.as_mut_ptr().cast();
        // SAFETY: `buffer` holds at least `size` bytes.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut attributes = Self(buffer);

        // The pseudoconsole attribute's value is the HPCON itself, passed in
        // the pointer's place, as Microsoft's own example does.
        // SAFETY: an initialised list with room for one attribute.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attributes.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const core::ffi::c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(attributes)
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: initialised in `with_pseudoconsole`, deleted exactly once.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

/// This process's environment with `overrides` on top, keyed as Windows
/// keys it: case-insensitively.
fn environment(overrides: &BTreeMap<String, String>) -> BTreeMap<String, (OsString, OsString)> {
    let mut environment: BTreeMap<String, (OsString, OsString)> = std::env::vars_os()
        .map(|(key, value)| (key.to_string_lossy().to_uppercase(), (key, value)))
        .collect();
    for (key, value) in overrides {
        environment.insert(key.to_uppercase(), (key.into(), value.into()));
    }
    environment
}

/// The environment as `CreateProcessW` takes it: `KEY=VALUE` strings, each
/// NUL-terminated, sorted, ending in one more NUL.
fn environment_block(environment: &BTreeMap<String, (OsString, OsString)>) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in environment.values() {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// Finds `program` as `portable-pty` did: as given when it names a path, and
/// otherwise in each `PATH` directory, as named and then with each `PATHEXT`
/// extension.
fn resolve(program: &str, environment: &BTreeMap<String, (OsString, OsString)>) -> PathBuf {
    let given = Path::new(program);
    if given.is_absolute() || given.components().count() > 1 {
        return given.to_path_buf();
    }

    let extensions = environment
        .get("PATHEXT")
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".EXE".to_string());

    if let Some((_, path)) = environment.get("PATH") {
        for dir in std::env::split_paths(path) {
            let exact = dir.join(program);
            if exact.is_file() {
                return exact;
            }
            for extension in extensions.split(';').filter(|e| !e.is_empty()) {
                let candidate = dir.join(format!("{program}{extension}"));
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }

    given.to_path_buf()
}

/// The command line: the program, then each argument, quoted for the C
/// runtime, NUL-terminated.
fn command_line(program: &Path, args: &[String]) -> Vec<u16> {
    let mut line = super::quote_for_crt(&program.to_string_lossy());
    for arg in args {
        line.push(' ');
        line.push_str(&super::quote_for_crt(arg));
    }
    line.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}
```

- [ ] **Step 4: `dispatch-pty` spawns through it**

`crates/dispatch-pty/Cargo.toml`: remove the `portable-pty` line. In `session.rs`: remove the `portable_pty` import and `answer_inherit_cursor_handshake`. `Pty` replaces `master` and `_slave` with:

```rust
    /// The pseudoterminal, for resizing, held for as long as the pane is: on
    /// Windows, ending it leaves the child writing into a console that is
    /// gone.
    terminal: dispatch_os::pty::Terminal,
```

`Pty::spawn`:

```rust
    pub fn spawn(launch: &Launch, cwd: &Path, size: Size) -> Result<Self, PtyError> {
        // Must happen before the pseudoterminal layer loads anything. On
        // Windows it decides whether ConPTY comes from the kernel or from
        // whatever conpty.dll happens to sit on PATH.
        dispatch_os::dll::restrict_search_path();

        let process = dispatch_os::pty::spawn(
            &dispatch_os::pty::PtyCommand {
                program: &launch.command,
                args: &launch.args,
                env: &launch.env,
                cwd,
            },
            size.rows,
            size.cols,
        )
        .map_err(|source| PtyError::Spawn {
            command: launch.command.clone(),
            source: source.into(),
        })?;

        let (input, queued) = channel();
        let waiting = Arc::new(AtomicUsize::new(0));
        spawn_writer(process.writer, queued, Arc::clone(&waiting));

        let (tx, events) = sync_channel(OUTPUT_CHUNKS);
        spawn_reader(process.reader, tx.clone());
        spawn_waiter(process.child, tx);

        Ok(Self {
            terminal: process.terminal,
            input,
            waiting,
            events,
            size,
            pid: process.pid,
            state: RunState::Running,
            finished: false,
        })
    }
```

`Pty::resize`'s body calls `self.terminal.resize(size.rows, size.cols).map_err(|e| PtyError::Open(e.into()))?;`.

`spawn_waiter`:

```rust
/// Waits for the child and reports its exit status.
fn spawn_waiter(child: dispatch_os::pty::Child, tx: SyncSender<PtyEvent>) {
    std::thread::spawn(move || {
        let _ = tx.send(PtyEvent::Exited(child.wait()));
    });
}
```

`spawn_reader` keeps reading after its receiver has gone, discarding:

```rust
/// Reads the pseudoterminal until end-of-file, forwarding bytes.
///
/// Reads on after the pane has gone, dropping what arrives: on Windows the
/// pseudoconsole only finishes closing once its output pipe is drained, so a
/// reader that stopped would leave that close waiting forever.
fn spawn_reader(mut reader: Box<dyn Read + Send>, tx: SyncSender<PtyEvent>) {
    std::thread::spawn(move || {
        // Large enough that a burst of output is a few reads rather than
        // hundreds, small enough not to sit idle holding memory per pane.
        let mut buf = [0u8; 8192];
        let mut delivering = true;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if delivering && tx.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                        delivering = false;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}
```

Update the module doc of `session.rs` if it names `portable-pty` (it does not); update `PtyError::Open`'s doc to "The pseudoterminal could not be opened or resized."

- [ ] **Step 5: Implement `quote_for_crt` (already in Step 3's file) and run everything local**

```bash
cargo test -p dispatch-os --locked
cargo test -p dispatch-pty --locked
cargo test --workspace --locked
cargo tree --locked -p dispatch-pty -i portable-pty 2>&1 | head -3
```

Expected: all pass; `portable-pty` no longer appears under `dispatch-pty`.

- [ ] **Step 6: Cross-check and the gates**

```bash
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings
```

- [ ] **Step 7: Commit, push, and read the whole Windows test log**

```bash
git add crates/dispatch-os crates/dispatch-pty crates/dispatch-daemon Cargo.lock
git commit -m "fix(pty): create Windows panes inside their job

Panes were started running by portable-pty, so a job attached afterwards
raced whatever the child started first. The platform half of a pane moves
to dispatch_os::pty: portable-pty on Unix as before, and on Windows a
ConPTY spawn of Dispatch's own that creates the process suspended, puts
it in its job, then resumes it.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
gh pr checks --watch
gh run view "$(gh run list --branch audit-remediation --workflow CI --limit 1 --json databaseId -q '.[0].databaseId')" --log | grep -E "windows.*(test .* \.\.\. |test result)"
```

Expected: every existing `dispatch-pty` test passes on Windows (output, exit codes, resize, typed input — the proof the new spawn behaves as `portable-pty` did), plus `terminating_a_pane_ends_everything_it_started` and `shutting_down_ends_every_panes_whole_tree`. Any Windows failure is fixed here, one commit per cause, before Task 17.

---
### Task 17: A task never reaches `cmd.exe`'s command line (A01)

The shipped Windows task forms are `cmd.exe /c claude -p {task}` and `cmd.exe /c codex exec {task}`. `{task}` is substituted verbatim, and the command line is built with C-runtime quoting, which leaves an argument without spaces or quotes bare — so `x&echo.MARKER` is two commands to `cmd.exe`. An approved delegation can run shell operators outside the agent's own permission flow, and ordinary prompts with `&`, `%`, `|` or quotes are mangled. No quoting fixes it: `cmd.exe` cannot carry a newline in its command line at all.

The task now reaches those agents on standard input. The daemon writes it to a file only this user can read; the harness's Windows form redirects that file into the agent (`claude -p` and `codex exec -` both read their prompt from standard input); the file's path reaches `cmd.exe` through an environment variable whose value is the quoted path, so nothing about the task — and nothing but a path Dispatch chose — is ever parsed by a shell. Built-in harness files still exactly as an older Dispatch wrote them are upgraded in place; an edited one keeps working everywhere but Windows, where delegation to it is refused with a message saying how to fix it.

**Files:**
- Modify: `crates/dispatch-config/src/harness.rs` (`TaskInput`, `TASK_FILE_ENV`, `TaskRun`, `TaskArgs::input`, `TaskLaunch::input`, `task_launch_for`, new `task_refusal_for`)
- Modify: `crates/dispatch-config/src/lib.rs` (exports; `write_missing_built_ins` upgrades unedited old built-ins)
- Modify: `crates/dispatch-config/src/defaults.rs` (`BuiltIn::superseded`)
- Modify: `crates/dispatch-config/harnesses/claude.toml`, `codex.toml`
- Create: `crates/dispatch-config/harnesses/superseded/claude-1.toml`, `codex-1.toml` (the current files, byte for byte)
- Modify: `crates/dispatch-os/src/paths.rs` (`create_private`, `redirect_operand`)
- Create: `crates/dispatch-daemon/src/task_file.rs`
- Modify: `crates/dispatch-daemon/src/lib.rs`, `pane.rs` (`task_file`), `session.rs` (`task_dir`, `set_task_dir`, `delegate_request`, `approve`, `pump_panes`, `spawn_pane`)
- Modify: `dispatchd/src/main.rs:128-132` (the comment on writing built-ins)
- Test: `crates/dispatch-config/src/tests.rs`, `crates/dispatch-daemon/src/session/tests.rs`

**Interfaces:**
- Consumes: the own Windows spawn (Task 16) — its command line keeps `<%DISPATCH_TASK_FILE%` bare, since it has no space or quote.
- Produces:
  - `dispatch_config::TaskInput { Argument (default), File }`, serde `"argument"` / `"file"`.
  - `dispatch_config::TASK_FILE_ENV: &str = "DISPATCH_TASK_FILE"`.
  - `dispatch_config::TaskRun { pub launch: Launch, pub input: TaskInput }`; `HarnessDef::task_launch(&self, task) -> Option<TaskRun>` and `task_launch_for(&self, os, task) -> Option<TaskRun>` (were `Option<Launch>`).
  - `HarnessDef::task_refusal_for(&self, os: &str) -> Option<String>`.
  - `dispatch_os::paths::create_private(path: &Path) -> std::io::Result<std::fs::File>` and `dispatch_os::paths::redirect_operand(path: &Path) -> String`.
  - `Daemon::set_task_dir(&mut self, dir: PathBuf)` (`#[doc(hidden)]`).

- [ ] **Step 1: Keep the old bodies**

```bash
mkdir -p crates/dispatch-config/harnesses/superseded
cp crates/dispatch-config/harnesses/claude.toml crates/dispatch-config/harnesses/superseded/claude-1.toml
cp crates/dispatch-config/harnesses/codex.toml crates/dispatch-config/harnesses/superseded/codex-1.toml
```

- [ ] **Step 2: Write the failing configuration tests**

In `crates/dispatch-config/src/tests.rs`, change the existing tests that read a `Launch` from `task_launch`/`task_launch_for` to read `.launch` (`launch.command` → `run.launch.command`, `launch.args` → `run.launch.args`), and replace `a_windows_wrapper_survives_a_one_shot_run` with:

```rust
#[test]
fn a_windows_task_reaches_the_agent_on_standard_input() {
    // claude installs on Windows as a .cmd shim that only cmd.exe can run,
    // and cmd.exe reads its whole command line as shell syntax. The task
    // goes to a file, which cmd.exe redirects into `claude -p`; the command
    // line carries nothing of it.
    let dir = TempDir::new("windows-task-wrapper");
    write_missing_built_ins(dir.path()).expect("the built-ins are written");
    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("they load");

    let hostile = "x & echo DISPATCH_AUDIT_MARKER | %PATH% \"quoted\"\nsecond line";
    for (id, expected) in [
        ("claude", vec!["/d", "/v:off", "/c", "claude", "-p", "<%DISPATCH_TASK_FILE%"]),
        (
            "codex",
            vec!["/d", "/v:off", "/c", "codex", "exec", "-", "<%DISPATCH_TASK_FILE%"],
        ),
    ] {
        let run = registry
            .get(id)
            .expect("the built-in exists")
            .task_launch_for("windows", hostile)
            .expect("it can be delegated to on Windows");

        assert_eq!(run.launch.command, "cmd.exe", "{id}");
        assert_eq!(run.launch.args, expected, "{id}");
        assert_eq!(run.input, TaskInput::File, "{id}");
        assert!(
            !run.launch.args.iter().any(|arg| arg.contains("MARKER")),
            "{id}: the task reached the command line"
        );
    }
}

#[test]
fn a_task_form_that_puts_the_task_on_cmds_command_line_is_refused_on_windows() {
    // Exactly what every earlier Dispatch wrote. A user who never edited it
    // gets the new one written over it; one who did is told what to change.
    let old: HarnessDef =
        toml::from_str(include_str!("../harnesses/superseded/claude-1.toml")).expect("it parses");

    let reason = old
        .task_refusal_for("windows")
        .expect("the old Windows form is refused");
    assert!(
        reason.contains("claude.toml") && reason.contains("DISPATCH_TASK_FILE"),
        "the refusal names the file and the fix: {reason}"
    );

    assert_eq!(old.task_refusal_for("linux"), None, "no shell is involved there");

    let current: HarnessDef =
        toml::from_str(defaults::BUILT_INS[0].toml).expect("the built-in parses");
    assert_eq!(current.task_refusal_for("windows"), None);
}

#[test]
fn an_unedited_built_in_from_an_older_release_is_upgraded() {
    let dir = TempDir::new("upgrade-unedited");
    dir.write(
        "claude.toml",
        include_str!("../harnesses/superseded/claude-1.toml"),
    );

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");

    assert!(written.contains(&"claude"), "the old file was replaced: {written:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("claude.toml")).expect("it reads"),
        defaults::BUILT_INS[0].toml
    );
}

#[test]
fn an_older_built_in_checked_out_with_crlf_is_still_recognised() {
    let dir = TempDir::new("upgrade-crlf");
    dir.write(
        "codex.toml",
        &include_str!("../harnesses/superseded/codex-1.toml").replace('\n', "\r\n"),
    );

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");
    assert!(written.contains(&"codex"), "{written:?}");
}

#[test]
fn an_edited_built_in_from_an_older_release_is_left_alone() {
    let dir = TempDir::new("upgrade-edited");
    let edited = format!(
        "{}\n# mine\n",
        include_str!("../harnesses/superseded/claude-1.toml")
    );
    dir.write("claude.toml", &edited);

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");

    assert!(!written.contains(&"claude"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("claude.toml")).expect("it reads"),
        edited
    );
}
```

Add `use crate::harness::TaskInput;` (or `use super::*` already covers it after Step 4's export) to the test module.

- [ ] **Step 3: Run them to see them fail**

```bash
cargo test -p dispatch-config --locked 2>&1 | tail -15
```

Expected: compile errors (`TaskInput`, `task_refusal_for`, `.launch`); after Step 4's types exist, `a_windows_task_reaches_the_agent_on_standard_input` FAILS on the old `["/c", "claude", "-p", …]` and the upgrade tests FAIL (nothing written over the old file).

- [ ] **Step 4: Task forms that say how the task arrives**

In `harness.rs`, add:

```rust
/// How a one-shot run is given its task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskInput {
    /// `{task}` in the arguments is replaced by the task, as one argument.
    ///
    /// Safe wherever the program is started directly: an argument is not
    /// parsed by anything on the way.
    #[default]
    Argument,
    /// The task is written to a file named by [`TASK_FILE_ENV`], and the
    /// arguments redirect that file into the program's standard input.
    ///
    /// For a program reached through `cmd.exe`, which reads its whole command
    /// line as shell syntax: a task placed there could run its `&`, `|` and
    /// `%VAR%` as commands, and cannot carry a newline at all. A file on
    /// standard input is parsed by nothing.
    File,
}

/// The variable naming a task's file, for a form whose input is
/// [`TaskInput::File`].
///
/// On Windows its value is the path in double quotes, ready for `cmd.exe`'s
/// `<`: the variable is expanded where it stands, so an unquoted path with a
/// space in it -- `C:\Users\Ada Lovelace\…` -- would redirect from the part
/// before the space. Elsewhere it is the bare path, for a shell to quote as
/// `"$DISPATCH_TASK_FILE"`.
pub const TASK_FILE_ENV: &str = "DISPATCH_TASK_FILE";

/// A one-shot run, ready to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRun {
    /// What to start.
    pub launch: Launch,
    /// How the task reaches it.
    pub input: TaskInput,
}
```

`TaskArgs` and `TaskLaunch` each gain, after `args`:

```rust
    /// How the task reaches the program: `{task}` in the arguments, or a
    /// file on its standard input.
    #[serde(default)]
    pub input: TaskInput,
```

(`TaskArgs`'s doc becomes "One platform's form for a one-shot run.") Replace `task_launch` and `task_launch_for`:

```rust
    /// The run for doing `task` once on the current platform.
    #[must_use]
    pub fn task_launch(&self, task: &str) -> Option<TaskRun> {
        self.task_launch_for(std::env::consts::OS, task)
    }

    /// The run for doing `task` once on a named platform.
    ///
    /// Returns `None` when the harness has no one-shot form for that platform,
    /// including when its argument list is empty: without arguments there is no
    /// way to tell the agent what the task is, so there is nothing to run.
    #[must_use]
    pub fn task_launch_for(&self, os: &str, task: &str) -> Option<TaskRun> {
        let form = self.task.as_ref()?;

        let (args, input) = match form.platform.get(os) {
            Some(override_) => (&override_.args, override_.input),
            None => (&form.args, form.input),
        };
        if args.is_empty() {
            return None;
        }

        let mut launch = self.launch_for(os);
        launch.args = match input {
            TaskInput::Argument => args.iter().map(|arg| arg.replace("{task}", task)).collect(),
            // Nothing is substituted: the task goes to a file, and the
            // arguments only ever name the file.
            TaskInput::File => args.clone(),
        };

        Some(TaskRun { launch, input })
    }

    /// Why this harness's one-shot form must not run on `os`, if it must not.
    ///
    /// A form that puts the task on `cmd.exe`'s command line lets the task's
    /// `&`, `|` and `%VAR%` run as commands. The harness file is the user's
    /// and may predate Dispatch knowing that, so such a form is refused with
    /// a way out rather than run.
    #[must_use]
    pub fn task_refusal_for(&self, os: &str) -> Option<String> {
        if os != "windows" {
            return None;
        }

        let form = self.task.as_ref()?;
        let (args, input) = match form.platform.get(os) {
            Some(override_) => (&override_.args, override_.input),
            None => (&form.args, form.input),
        };
        let on_the_command_line =
            input == TaskInput::Argument && args.iter().any(|arg| arg.contains("{task}"));
        if !on_the_command_line || !runs_through_cmd(&self.launch_for(os).command) {
            return None;
        }

        Some(format!(
            "harness {id:?} would put the task on cmd.exe's command line, where characters \
             like & and % run as commands. Change [task.platform.windows] in {id}.toml to \
             redirect the task from %{TASK_FILE_ENV}% with input = \"file\" (the built-in \
             claude.toml shows how), or delete {id}.toml if it is a built-in you never meant \
             to edit, and restart the daemon",
            id = self.id
        ))
    }
```

and, at the bottom of the file:

```rust
/// Whether `command` runs through `cmd.exe`: the program itself, or a batch
/// file, which Windows runs with it.
fn runs_through_cmd(command: &str) -> bool {
    let name = std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();

    name == "cmd" || name == "cmd.exe" || name.ends_with(".cmd") || name.ends_with(".bat")
}
```

`lib.rs`: `pub use harness::{HarnessDef, Launch, SettingDef, SettingKind, TASK_FILE_ENV, TaskArgs, TaskInput, TaskLaunch, TaskRun};`.

- [ ] **Step 5: The new built-in forms, and the upgrade of unedited old ones**

`crates/dispatch-config/harnesses/claude.toml`, replace the `[task.platform.windows]` table:

```toml
# cmd.exe reads its whole command line as shell syntax, so a task placed
# there could run its & and %VAR% as commands, and could not hold a newline.
# Dispatch writes the task to a file instead, names it in DISPATCH_TASK_FILE
# (already quoted for cmd.exe), and cmd.exe redirects it into `claude -p`,
# which reads its prompt from standard input. /d skips AutoRun commands;
# /v:off keeps ! literal.
[task.platform.windows]
args = ["/d", "/v:off", "/c", "claude", "-p", "<%DISPATCH_TASK_FILE%"]
input = "file"
```

`codex.toml`, likewise, with `codex exec -` (the `-` tells it to read the prompt from standard input):

```toml
# See claude.toml: the task reaches codex on standard input, never on
# cmd.exe's command line. `-` tells `codex exec` to read it from there.
[task.platform.windows]
args = ["/d", "/v:off", "/c", "codex", "exec", "-", "<%DISPATCH_TASK_FILE%"]
input = "file"
```

`defaults.rs`:

```rust
/// A built-in harness: its file stem and TOML body.
pub struct BuiltIn {
    /// File stem, without the `.toml` extension.
    pub id: &'static str,
    /// File contents.
    pub toml: &'static str,
    /// Every body an earlier Dispatch wrote for this file.
    ///
    /// A file still exactly one of these was never edited, so it is
    /// replaced with the current body: an unsafe form must not live on in
    /// every installation made before it was fixed. A file that differs
    /// from all of them is the user's, and is left alone.
    pub superseded: &'static [&'static str],
}
```

with `superseded: &[include_str!("../harnesses/superseded/claude-1.toml")]` for claude, the codex one for codex, and `superseded: &[]` for agy and opencode.

`lib.rs`, `write_missing_built_ins`:

```rust
/// Writes the built-in harness files into `dir`: any that are missing, and
/// any still exactly as an earlier Dispatch wrote them.
///
/// Returns the ids written. A file the user has edited is left alone, so an
/// upgrade never discards local changes; one nobody touched is brought up to
/// date, so a fix to a built-in reaches installations made before it.
pub fn write_missing_built_ins(dir: &Path) -> Result<Vec<&'static str>, ConfigError> {
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut written = Vec::new();

    for built_in in defaults::BUILT_INS {
        let path = dir.join(format!("{}.toml", built_in.id));

        if path.exists() {
            // Compared without carriage returns: a file written from a
            // checkout with CRLF line endings is still the same file.
            let unix = |text: &str| text.replace("\r\n", "\n");
            let existing = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
            let untouched = built_in
                .superseded
                .iter()
                .any(|old| unix(old) == unix(&existing));
            if !untouched {
                continue;
            }
            tracing::info!(harness = built_in.id, "upgrading a built-in harness nobody edited");
        }

        std::fs::write(&path, built_in.toml).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        written.push(built_in.id);
    }

    Ok(written)
}
```

In `dispatchd/src/main.rs`, the comment above the call becomes: "Written on first run, and afterwards only while a file is still exactly as an earlier Dispatch wrote it, so local edits survive and fixes to untouched built-ins still arrive. The daemon is the process that starts agents, so it is the one that needs the definitions."

- [ ] **Step 6: Run the configuration tests**

```bash
cargo test -p dispatch-config --locked
```

Expected: all pass (the existing `a_second_run_does_not_overwrite_user_edits` still holds: its edited file is none of the superseded bodies).

- [ ] **Step 7: Write the failing daemon test**

Append to `crates/dispatch-daemon/src/session/tests.rs`:

```rust
/// Not a test of its own: what the `capture` harness runs inside a pane, to
/// record exactly what reached its standard input. Run without
/// `DISPATCH_CAPTURE_TO`, it does nothing.
#[test]
fn capture_standard_input() {
    let Some(out) = std::env::var_os("DISPATCH_CAPTURE_TO") else {
        return;
    };
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .expect("standard input is readable");
    std::fs::write(out, bytes).expect("the capture is writable");
}

#[test]
#[cfg_attr(not(windows), ignore = "exercises cmd.exe")]
fn a_task_reaches_a_cmd_wrapped_agent_exactly() {
    // The audit's A01: through `cmd.exe /c agent {task}`, `&` in a task ran
    // a second command. Here the agent is this test binary, reached through
    // cmd.exe exactly as claude.cmd is, recording what its standard input
    // received.
    let dir = TempDir::new("cmd-task");
    let harness_dir = dir.0.join("harnesses");
    let _ = harnesses(&harness_dir);

    let captured = dir.0.join("captured.bin");
    let exe = std::env::current_exe().expect("the test binary");
    std::fs::write(
        harness_dir.join("capture.toml"),
        format!(
            "id = \"capture\"\ndisplay_name = \"Capture\"\ncommand = \"cmd.exe\"\n\n\
             [env]\nDISPATCH_CAPTURE_TO = '{}'\n\n\
             [task]\nargs = [\"/d\", \"/v:off\", \"/c\", '{}', \"--exact\", \
             \"session::tests::capture_standard_input\", \"--nocapture\", \
             \"<%DISPATCH_TASK_FILE%\"]\ninput = \"file\"\n",
            captured.display(),
            exe.display()
        ),
    )
    .expect("temp dir is writable");

    let registry = HarnessRegistry::load_from_dir(&harness_dir).expect("loading succeeds");
    let mut daemon = Daemon::new(registry, "test-device");
    // A space, as in many Windows profile paths: the file's path has to
    // survive cmd.exe whole.
    let task_dir = dir.0.join("task files");
    daemon.set_task_dir(task_dir.clone());
    let project =
        daemon.open_project(dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves"));

    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let task = "literal & echo DISPATCH_AUDIT_MARKER> marker.txt | \"quoted\" %PATH% !PATH! ^caret\r\n\
                second line \u{fc}n\u{ef}c\u{f8}d\u{e9} \u{2713}";
    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent,
            harness: "capture".into(),
            task: task.into(),
            size: (80, 24),
        },
    );
    let request = pending(&drain(&ui)).expect("the interface is asked");
    daemon.request_for_test(
        1,
        ClientMessage::DelegateDecision {
            request,
            approve: true,
            blanket: false,
        },
    );

    let seen = wait_for(&mut daemon, &caller, |m| {
        m.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { .. }))
    });
    assert!(
        seen.iter()
            .any(|m| matches!(m, ServerMessage::DelegateFinished { exit: 0, .. })),
        "the capture ran and succeeded: {seen:#?}"
    );

    assert_eq!(
        std::fs::read(&captured).expect("the agent recorded its input"),
        task.as_bytes(),
        "the task arrived changed"
    );
    assert!(
        !dir.0.join("marker.txt").exists(),
        "a command inside the task ran"
    );
    assert!(
        std::fs::read_dir(&task_dir).map_or(true, |mut d| d.next().is_none()),
        "the task's file outlived the run"
    );
}

#[test]
#[cfg_attr(not(windows), ignore = "the refusal is for cmd.exe, which only Windows has")]
fn a_task_form_that_would_put_the_task_on_cmds_command_line_is_refused() {
    // What every Windows installation from before this fix still has in any
    // harness file its user edited.
    let dir = TempDir::new("unsafe-form");
    let harness_dir = dir.0.join("harnesses");
    let _ = harnesses(&harness_dir);
    std::fs::write(
        harness_dir.join("old.toml"),
        "id = \"old\"\ndisplay_name = \"Old\"\ncommand = \"cmd.exe\"\n\n[task]\nargs = [\"/c\", \"{task}\"]\n",
    )
    .expect("temp dir is writable");

    let registry = HarnessRegistry::load_from_dir(&harness_dir).expect("loading succeeds");
    let mut daemon = Daemon::new(registry, "test-device");
    let project =
        daemon.open_project(dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves"));
    let ui = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let parent = spawn_pane_for_test(&mut daemon, &ui, project);

    let caller = daemon.attach_for_test(9);
    daemon.request_for_test(
        9,
        ClientMessage::Hello {
            version: dispatch_proto::VERSION,
            client: "delegate".into(),
            role: dispatch_proto::Role::Delegate,
        },
    );
    daemon.request_for_test(
        9,
        ClientMessage::DelegateRequest {
            parent,
            harness: "old".into(),
            task: "x & echo DISPATCH_AUDIT_MARKER".into(),
            size: (80, 24),
        },
    );

    let told = outcomes(&drain(&caller));
    assert!(
        told.iter().any(|o| matches!(
            o,
            DelegateOutcome::Refused { reason } if reason.contains("old.toml") && reason.contains("cmd.exe")
        )),
        "the caller is told which file to fix: {told:?}"
    );
    assert!(pending(&drain(&ui)).is_none(), "nobody is asked to approve it");
    assert_eq!(daemon.pane_count(), 1, "nothing started");
}
```

The daemon's own `shell` fixture is `cmd.exe /c {task}` on Windows — the very shape now refused — so every delegation test that runs on Windows would be refused. Its Windows `[task]` changes to read the task from a file, keeping what the tests mean by a task (a script to run):

```rust
    let body = if cfg!(windows) {
        "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"cmd.exe\"\n\n[task]\nargs = [\"/d\", \"/v:off\", \"/c\", \"powershell.exe\", \"-NoProfile\", \"-NonInteractive\", \"-Command\", \"-\", \"<%DISPATCH_TASK_FILE%\"]\ninput = \"file\"\n"
    } else {
        "id = \"shell\"\ndisplay_name = \"Shell\"\ncommand = \"sh\"\n\n[task]\nargs = [\"-c\", \"{task}\"]\n"
    };
```

`powershell -Command -` runs the script it reads from standard input: `echo delegated-42` prints, `sleep 30` sleeps and `ping -n 30 127.0.0.1` pings, so the existing tasks mean on Windows what they mean elsewhere. This is a test fixture choosing to run its task as a script; no shipped harness does. If the Windows job shows PowerShell mishandling a final line with no newline, append `\n` to the fixture's tasks rather than to the file Dispatch writes — the file holds the task exactly, which is the point of this task.

- [ ] **Step 8: See it fail**

```bash
cargo test -p dispatch-daemon --locked 2>&1 | tail -10
```

Expected: compile errors (`set_task_dir`; `task_launch` now returns `TaskRun`). The Windows capture test is ignored here and runs in CI.

- [ ] **Step 9: Write the task down, and refuse the unsafe form**

`crates/dispatch-os/src/paths.rs`:

```rust
/// Creates a new file only this user can read or write.
///
/// Refuses one that already exists: a name in a shared directory must not
/// be one somebody else prepared. On Windows the file takes its directory's
/// access list, and the directories Dispatch writes these in are the user's
/// own.
pub fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    options.open(path)
}

/// `path` as a shell's `<` needs it in the variable that names it.
///
/// Quoted on Windows, where `cmd.exe` expands the variable in place and a
/// space in the path would end the file name there; bare elsewhere, where
/// the shell quotes the expansion itself.
#[must_use]
pub fn redirect_operand(path: &Path) -> String {
    if cfg!(windows) {
        format!("\"{}\"", path.display())
    } else {
        path.display().to_string()
    }
}
```

`crates/dispatch-daemon/src/task_file.rs`:

```rust
//! A task, written where a one-shot run reads it from its standard input.

use std::io::Write;
use std::path::{Path, PathBuf};

use dispatch_core::RequestId;

/// A task's file, removed when this is dropped.
///
/// Held by the pane running the task: the file lives exactly as long as
/// something might still read it, and a daemon that stops leaves none
/// behind.
#[derive(Debug)]
pub struct TaskFile {
    path: PathBuf,
}

impl TaskFile {
    /// Writes `task` to a new file in `dir` that only this user can read.
    pub fn write(dir: &Path, request: RequestId, task: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;

        // Named before it exists, so a write that fails part-way still
        // removes what it created.
        let file = Self {
            path: dir.join(format!("dispatch-task-{request}.txt")),
        };
        let mut handle = dispatch_os::paths::create_private(&file.path)?;
        handle.write_all(task.as_bytes())?;
        handle.flush()?;

        Ok(file)
    }

    /// The value the child's environment carries in
    /// [`dispatch_config::TASK_FILE_ENV`].
    #[must_use]
    pub fn for_redirect(&self) -> String {
        dispatch_os::paths::redirect_operand(&self.path)
    }
}

impl Drop for TaskFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(%error, path = %self.path.display(), "failed to remove a task's file");
        }
    }
}
```

`crates/dispatch-daemon/Cargo.toml`: add `dispatch-core` if missing (it is present). `lib.rs`: `mod task_file;`.

`pane.rs`: `DaemonPane` gains

```rust
    /// The file its task was delivered in, while the process may still read
    /// it.
    pub task_file: Option<crate::task_file::TaskFile>,
```

and both `DaemonPane { … }` literals in `session.rs` set it (`None` in `spawn_pane`).

`session.rs`:
- `Daemon` gains `task_dir: PathBuf`, initialised to `std::env::temp_dir()`, and:

```rust
    /// Where task files are written: the system's temporary directory unless
    /// told otherwise.
    #[doc(hidden)]
    pub fn set_task_dir(&mut self, dir: PathBuf) {
        self.task_dir = dir;
    }
```

- `delegate_request`, before `refusal`:

```rust
        // A form that would put the task on cmd.exe's command line is refused
        // whatever the caps say: approving it would not make it safe.
        if let Some(reason) = self
            .harnesses
            .get(&harness)
            .and_then(|def| def.task_refusal_for(std::env::consts::OS))
        {
            tracing::info!(%parent, %harness, %reason, "refused an unsafe task form");
            self.resolve(request, caller, DelegateOutcome::Refused { reason });
            return;
        }
```

- `approve`: `launch` (Task 4's name) now holds an `Option<TaskRun>`; rename it `run`, keep the refusal check on `run.is_some()`, then:

```rust
        let Some(run) = run else {
            // `refusal` refuses a missing form first, so this cannot be
            // reached; kept as a refusal rather than a panic all the same.
            self.resolve(
                request,
                caller,
                DelegateOutcome::Refused {
                    reason: format!("harness {harness:?} has no [task] form"),
                },
            );
            return;
        };

        let id = PaneId::new();
        let mut launch = run.launch;
        for (key, value) in self.pane_env(id) {
            launch.env.entry(key).or_insert(value);
        }

        let task_file = match run.input {
            TaskInput::Argument => None,
            TaskInput::File => match TaskFile::write(&self.task_dir, request, task) {
                Ok(file) => {
                    launch
                        .env
                        .insert(dispatch_config::TASK_FILE_ENV.to_string(), file.for_redirect());
                    Some(file)
                }
                Err(error) => {
                    self.resolve(
                        request,
                        caller,
                        DelegateOutcome::Refused {
                            reason: format!("could not write the task down for {harness}: {error}"),
                        },
                    );
                    return;
                }
            },
        };
```

and the `DaemonPane` literal sets `task_file`. Imports: `use dispatch_config::TaskInput;` and `use crate::task_file::TaskFile;`.

- `pump_panes`, where an exit is first seen:

```rust
            if let RunState::Exited(code) = pane.session.state()
                && pane.status.is_live()
            {
                pane.status = PaneStatus::Exited(code);
                pane.exited_at = Some(Instant::now());
                // Read by now, and nothing will read it again.
                pane.task_file = None;
                exited.push((*id, code));
            }
```

- [ ] **Step 10: Run everything local, cross-check, commit, push, read the Windows log**

```bash
cargo test --workspace --locked
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings
git add crates/dispatch-config crates/dispatch-os crates/dispatch-daemon dispatchd/src/main.rs
git commit -m "fix: never put a task on cmd.exe's command line

The Windows task forms ran cmd.exe /c claude -p {task}, so & and % in a
task ran as commands. The task is now written to a file only the user can
read and redirected into the agent's standard input; its path reaches
cmd.exe through DISPATCH_TASK_FILE, already quoted. Unedited built-in
harness files are upgraded; an edited form that still puts the task on
cmd.exe's command line is refused on Windows with the fix spelled out.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
gh pr checks --watch
```

Expected on Windows CI: `a_task_reaches_a_cmd_wrapped_agent_exactly ... ok` (not ignored), every existing delegation test still passing through the file-fed fixture.

---

### Task 18: Say what the approval flow protects, close out the handoffs, and review the whole branch

The audit's first recommendation beyond its findings: make the trust model explicit. `DISPATCH_PANE` is attribution, roles decide delivery not privilege, and anything that can reach the socket can spawn panes. Approval is a cooperative workflow among processes of one user; it is not a sandbox. That belongs where a user deciding whether to run untrusted agents will find it.

**Files:**
- Create: `docs/security-model.md`
- Modify: `README.md` (link it)
- Modify: `docs/superpowers/federation-handoff.md` (parked item 1 is fixed)

**Interfaces:** none.

- [ ] **Step 1: Write `docs/security-model.md`**

```markdown
# What Dispatch's boundaries are, and are not

## The boundary is the operating-system user

A daemon runs as one user and serves that user alone. On Unix its socket is
mode 0600; on Windows its named pipe carries a DACL admitting that user and
nobody else, and refuses clients on other machines. Reaching another
machine goes through `ssh`, whose authentication is the boundary there.

Anything that can open the socket can do anything a client can: open
projects, start panes running any registered harness, type into any pane,
approve or deny any delegation. There is no second tier of client.

## Approval is a workflow, not a sandbox

When an agent asks for a subagent, the request is put to the user, and the
daemon enforces the caps (`max_depth`, `max_live_per_parent`) and the
deadline. That keeps a well-meaning agent from fanning out without anyone
noticing. It does not stop a *malicious* agent:

- `DISPATCH_PANE` says which pane a request comes from. It is attribution,
  not a credential: any process of the user's can set it, or connect to the
  socket directly and act as an interface, approving its own requests.
- A client's role (`interface` or `delegate`) decides what it is sent, not
  what it may do.
- Agents run as the user, with the user's files, keys and network.

If the agents themselves are not trusted, run them under an operating-system
boundary — a separate user, a container, a VM — and give that boundary its
own daemon. A role check or a token in the environment would not be one.

## What the daemon does protect against

A slow, stuck or broken client cannot impair the others: each client's
queue has a byte budget, a client that stops reading is hung up on and
replayed on reconnection, a client that never says hello or stops mid-frame
is hung up on, and at most 64 may connect. A pane that stops reading its
input is refused more input rather than stalling the daemon. On Windows a
task handed to an agent through `cmd.exe` travels on standard input, never
on a command line the shell would parse.
```

- [ ] **Step 2: Link it from the README**

After the README's "Building" section:

```markdown
## Security model

One daemon serves one operating-system user, and anything that can reach
its socket can do anything a client can. Delegation approval keeps
well-meaning agents in check; it is not a sandbox for untrusted ones. See
[docs/security-model.md](docs/security-model.md).
```

- [ ] **Step 3: Close the parked Windows item**

In `docs/superpowers/federation-handoff.md`, replace parked item 1 with:

```markdown
1. ~~**Windows `process::terminate_tree` only calls `TerminateProcess` on the
   one handle.**~~ Fixed on the `audit-remediation` branch (audit A03): panes
   and command transports are created suspended inside a Job Object, and
   `terminate_tree` ends the job.
```

- [ ] **Step 4: Commit and push**

```bash
git add docs/security-model.md README.md docs/superpowers/federation-handoff.md
git commit -m "docs: say what the approval flow protects and what it does not

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
git push
```

- [ ] **Step 5: Whole-branch review**

Dispatch a fresh reviewer (superpowers:requesting-code-review, most capable model) over `git diff bd49298...HEAD` with the audit handoff as the spec, asking specifically: does each finding's acceptance paragraph have a test that would fail without its fix; does any budget or deadline block the daemon's loop; can any `Closer::close` or `Wire::lost` path deadlock against the writer it is meant to unblock; is every `unsafe` block's SAFETY comment true. Fix what it confirms, one commit each.

- [ ] **Step 6: Final gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
~/.cargo/bin/cargo +stable clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked --target-dir target/windows-check -- -D warnings
~/.cargo/bin/cargo +1.89.0 build --workspace --all-targets --locked --target-dir target/msrv-1.89
target/tools/bin/cargo-audit audit --deny warnings
gh pr checks
```

Expected: every command clean; every CI job green on the PR.

- [ ] **Step 7: Update the PR description**

Replace the draft PR's body with a table mapping A01–A11 to the commit that fixes each and the test that pins it, the decisions listed at the top of this plan, and what remains out of scope (the audit's improvements 2–4: splitting `app.rs`, the parked federation UI items, fuzzing). Leave the PR in draft for the user.
