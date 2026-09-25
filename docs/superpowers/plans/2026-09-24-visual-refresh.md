# Visual Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Dispatch herdr's look — square edges, faded secondary text, tinted selection, the name in the top-left, a sidebar split per machine — and show which git branch every project and pane is on.

**Architecture:** Branch detection lives in `dispatch-os` (read the foreground process's working directory, read `.git/HEAD` from files) and runs wherever a pane's process runs: the daemon for its panes, the client for panes it runs itself. Two additive protocol messages carry it. Colours are a small `Theme` in `dispatch-tui`, mixed from the terminal's own colours queried once at startup. The sidebar is rebuilt around one layout function — sections per machine, branch group rows, per-machine scroll — that both drawing and click handling walk.

**Tech Stack:** Rust 2024, ratatui 0.29, crossterm 0.28, libc (Unix), `unicode-width` 0.2 (already a `dispatch-tui` dependency).

**Spec:** `docs/superpowers/specs/2026-09-24-visual-refresh-design.md`

## Global Constraints

- `#[cfg]` on a platform lives only in `dispatch-os` (`crates/dispatch-os/src/lib.rs:4`). Tests may carry `#[cfg(...)]`.
- CI runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` on `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-gnu`. Every task leaves all three green.
- Every `unsafe` block carries a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks` is on).
- `dispatch_proto::VERSION` stays `1.1`. New variants land in an existing `Unknown` for older peers.
- No `git` subprocess anywhere. Branches are read from files.
- No new crate dependencies.
- Sidebar width `34`. Branch recheck interval `2 s` (`dispatch_os::git::RECHECK`). Colour query timeout `1 s`.
- Fallback palette: background `#16161e`, foreground `#c8c8d8`, accent `#b4a0f0`. Mixes: `faded` = foreground 45% toward background; `tint` = background 10% toward foreground; `tab` = background 30% toward accent.
- Comments explain *why*, in the voice of the surrounding code; match its density. Commit messages follow the repo's `type(scope): lowercase summary` style and end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **A colour reply split across two reads** — the terminal's answer arrives in pieces; the colour must still be recognised once the rest arrives, and a half-reply must not panic or end the wait early. Test in Task 7.
2. **A branch name far longer than the sidebar** (`feature/an-extremely-long-branch-name`) — cut with `…` inside the frame, never drawn over the border. Test in Task 10.
3. **Several machines in a very short sidebar** (three machines, five rows) — no panic, the bottom border intact, dividers that do not fit simply not drawn. Test in Task 11.
4. **A terminal smaller than the sidebar** (a few columns, one or two rows) — drawing never panics. Test in Task 8.
5. **A pane whose process has just exited** — it keeps the branch it last had rather than jumping back to the project's group in the moment its process can no longer be read. Test in Task 5.

---

### Task 1: Read a directory's branch from its repository's files

**Files:**
- Create: `crates/dispatch-os/src/git.rs`
- Modify: `crates/dispatch-os/src/lib.rs` (module list)

**Interfaces:**
- Produces: `dispatch_os::git::head(dir: &std::path::Path) -> Option<String>` — `Some("main")`, `Some("feat/x")`, `Some("@3d2d929")` for a detached head, `None` outside a repository or on any read failure. `dispatch_os::git::RECHECK: std::time::Duration` (2 s).

- [ ] **Step 1: Write the failing tests**

Create `crates/dispatch-os/src/git.rs` with only the tests and a stub, so they compile and fail:

```rust
//! Which branch a directory is on, read from the repository's own files.
//!
//! Read rather than asked of `git`: a remote machine need not have `git` on
//! its PATH for Dispatch to run there, and a subprocess per pane every few
//! seconds is a cost that grows with every pane opened.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often a pane's or a project's branch is looked at again.
///
/// A `git switch` shows in the sidebar within this long. Each look is a few
/// small file reads, so the bound is on how stale a row may be, not on cost.
pub const RECHECK: Duration = Duration::from_secs(2);

/// The checked-out branch of the repository containing `dir`.
#[must_use]
pub fn head(_dir: &Path) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory under the system's temporary one, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);

            let path = std::env::temp_dir().join(format!(
                "dispatch-os-git-{}-{label}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("temp dir is writable");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Makes `dir` a repository whose `HEAD` reads `head`.
    fn repository(dir: &Path, head: &str) {
        std::fs::create_dir_all(dir.join(".git")).expect("temp dir is writable");
        std::fs::write(dir.join(".git").join("HEAD"), head).expect("temp dir is writable");
    }

    #[test]
    fn a_branch_is_named_without_its_refs_prefix() {
        let dir = Scratch::new("branch");
        repository(&dir.0, "ref: refs/heads/main\n");

        assert_eq!(head(&dir.0).as_deref(), Some("main"));
    }

    #[test]
    fn a_branch_with_slashes_keeps_them() {
        let dir = Scratch::new("slashes");
        repository(&dir.0, "ref: refs/heads/feat/usage-charts\n");

        assert_eq!(head(&dir.0).as_deref(), Some("feat/usage-charts"));
    }

    #[test]
    fn a_detached_head_is_its_short_commit() {
        let dir = Scratch::new("detached");
        repository(&dir.0, "3d2d929fbf8f191cbfeef851927c9f1730b6b5d8\n");

        assert_eq!(head(&dir.0).as_deref(), Some("@3d2d929"));
    }

    #[test]
    fn a_subdirectory_finds_the_repository_above_it() {
        let dir = Scratch::new("nested");
        repository(&dir.0, "ref: refs/heads/main\n");
        let deep = dir.0.join("src").join("module");
        std::fs::create_dir_all(&deep).expect("temp dir is writable");

        assert_eq!(head(&deep).as_deref(), Some("main"));
    }

    #[test]
    fn a_worktree_follows_its_gitdir_file() {
        // `git worktree add` leaves a `.git` *file* naming a directory inside
        // the main repository, and that directory holds the worktree's HEAD.
        let dir = Scratch::new("worktree");
        let real = dir.0.join("main").join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&real).expect("temp dir is writable");
        std::fs::write(real.join("HEAD"), "ref: refs/heads/feat/wt\n").expect("writable");

        let worktree = dir.0.join("wt");
        std::fs::create_dir_all(&worktree).expect("temp dir is writable");
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", real.display()),
        )
        .expect("writable");

        assert_eq!(head(&worktree).as_deref(), Some("feat/wt"));
    }

    #[test]
    fn a_relative_gitdir_is_read_from_the_file_that_names_it() {
        // `git worktree add --relative-paths` writes the path relative to the
        // worktree, not to wherever Dispatch happens to be running.
        let dir = Scratch::new("relative");
        let real = dir.0.join("main").join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&real).expect("temp dir is writable");
        std::fs::write(real.join("HEAD"), "ref: refs/heads/feat/rel\n").expect("writable");

        let worktree = dir.0.join("wt");
        std::fs::create_dir_all(&worktree).expect("temp dir is writable");
        std::fs::write(worktree.join(".git"), "gitdir: ../main/.git/worktrees/wt\n")
            .expect("writable");

        assert_eq!(head(&worktree).as_deref(), Some("feat/rel"));
    }

    #[test]
    fn a_directory_outside_any_repository_has_no_branch() {
        let dir = Scratch::new("plain");

        assert_eq!(head(&dir.0), None);
    }

    #[test]
    fn a_repository_with_no_head_has_no_branch() {
        let dir = Scratch::new("headless");
        std::fs::create_dir_all(dir.0.join(".git")).expect("temp dir is writable");

        assert_eq!(head(&dir.0), None);
    }

    #[test]
    fn a_ref_that_is_not_a_branch_has_no_branch() {
        let dir = Scratch::new("tag");
        repository(&dir.0, "ref: refs/tags/v1.0\n");

        assert_eq!(head(&dir.0), None);
    }

    #[test]
    fn a_malformed_head_has_no_branch() {
        let dir = Scratch::new("garbage");
        repository(&dir.0, "not a head at all\n");

        assert_eq!(head(&dir.0), None);
    }
}
```

In `crates/dispatch-os/src/lib.rs`, add the module in alphabetical order:

```rust
pub mod dll;
pub mod git;
pub mod host;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-os git::`
Expected: the six tests expecting `Some(..)` FAIL with `left: None`; the four `None` tests pass.

- [ ] **Step 3: Implement `head`**

Replace the stub in `git.rs` with:

```rust
/// How many hex digits of a detached commit are shown.
const SHORT: usize = 7;

/// The checked-out branch of the repository containing `dir`.
///
/// `@` and the first seven hex digits of the commit for a detached `HEAD`;
/// `None` outside any repository, or when the repository cannot be read.
#[must_use]
pub fn head(dir: &Path) -> Option<String> {
    let git_dir = git_dir(dir)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    parse_head(&head)
}

/// The git directory of the repository containing `dir`, walking up.
///
/// A `.git` directory is the git directory. A `.git` file belongs to a
/// worktree or a submodule and names the real one, relative to the file's own
/// directory when it is not absolute.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir
        .ancestors()
        .map(|candidate| candidate.join(".git"))
        .find(|candidate| candidate.exists())?;

    if dot_git.is_dir() {
        return Some(dot_git);
    }

    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();

    Some(dot_git.parent()?.join(target))
}

/// What a `HEAD` file says, as the sidebar should show it.
fn parse_head(head: &str) -> Option<String> {
    let head = head.trim();

    if let Some(reference) = head.strip_prefix("ref:") {
        return reference
            .trim()
            .strip_prefix("refs/heads/")
            .filter(|name| !name.is_empty())
            .map(str::to_string);
    }

    // SHA-1 or SHA-256: a detached head names a commit, not a branch.
    let is_commit =
        matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit());

    is_commit.then(|| format!("@{}", &head[..SHORT]))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-os git::`
Expected: 10 passed.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy -p dispatch-os --all-targets -- -D warnings && cargo fmt --all --check`
Expected: no warnings, no diff.

```bash
git add crates/dispatch-os/src/git.rs crates/dispatch-os/src/lib.rs
git commit -m "feat(os): read a directory's branch from its repository's files

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Find the directory a pane's foreground program is working in

**Files:**
- Modify: `crates/dispatch-os/src/process.rs` (new public fn after `terminate_tree`, new `cwd` modules at the end, before the existing test module)

**Interfaces:**
- Produces: `dispatch_os::process::working_dir(pid: u32) -> Option<std::path::PathBuf>` — the working directory of the foreground process group of the terminal `pid` runs in, falling back to `pid`'s own; `None` when neither can be read, and always on Windows.

- [ ] **Step 1: Write the failing tests and the public stub**

Add after `terminate_tree` in `process.rs`:

```rust
/// The working directory of whatever is in the foreground of the terminal
/// `pid` runs in.
///
/// For a shell running `cd wt && claude` that is Claude in `wt`, not the
/// shell: the foreground program is the one the user is looking at. Falls
/// back to `pid`'s own directory when the terminal's foreground cannot be
/// read, and is `None` where neither can -- a process that has exited, one
/// owned by another user, or a platform with no way to ask.
#[must_use]
pub fn working_dir(pid: u32) -> Option<std::path::PathBuf> {
    cwd::working_dir(pid)
}
```

Add at the end of `process.rs`, before `#[cfg(all(test, unix))] mod tests`:

```rust
#[cfg(target_os = "linux")]
mod cwd {
    use std::path::PathBuf;

    pub(super) fn working_dir(_pid: u32) -> Option<PathBuf> {
        None
    }

    /// Field 8 of a `/proc/<pid>/stat` line: the foreground process group of
    /// the process's terminal.
    pub(super) fn tpgid(_stat: &str) -> Option<u32> {
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_foreground_group_is_read_past_a_name_with_spaces_and_parentheses() {
            // The command name is free text in parentheses, so counting
            // fields from the start of the line breaks on the first name
            // with a space in it.
            let stat = "4242 (a (weird) name) S 1 4242 4242 34817 5151 4194560 0 0";

            assert_eq!(tpgid(stat), Some(5151));
        }

        #[test]
        fn a_process_with_no_terminal_has_no_foreground_group() {
            let stat = "4242 (daemon) S 1 4242 4242 0 -1 4194560 0 0";

            assert_eq!(tpgid(stat), None);
        }

        #[test]
        fn a_process_with_no_terminal_reports_its_own_directory() {
            use std::os::unix::process::CommandExt;

            let dir = std::env::temp_dir().join(format!("dispatch-os-cwd-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir is writable");
            let dir = dir.canonicalize().expect("the temp dir resolves");

            let mut command = std::process::Command::new("sleep");
            command.arg("30").current_dir(&dir);
            // SAFETY: setsid is async-signal-safe and the closure allocates
            // nothing. It detaches the child from the test runner's terminal,
            // whose foreground job would otherwise be the answer.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().expect("sleep starts");

            let found = working_dir(child.id());

            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);

            assert_eq!(found, Some(dir));
        }

        #[test]
        fn a_process_that_does_not_exist_has_no_directory() {
            assert_eq!(working_dir(u32::MAX), None);
        }
    }
}

#[cfg(target_os = "macos")]
mod cwd {
    pub(super) fn working_dir(_pid: u32) -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod cwd {
    /// Windows has no terminal foreground to ask about, and nothing else
    /// Dispatch runs on is supported.
    pub(super) fn working_dir(_pid: u32) -> Option<std::path::PathBuf> {
        None
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-os cwd::`
Expected: `the_foreground_group_is_read_past...` and `a_process_with_no_terminal_reports_its_own_directory` FAIL; the two `None` tests pass.

- [ ] **Step 3: Implement Linux and macOS**

Replace the Linux module's two stub functions with:

```rust
    pub(super) fn working_dir(pid: u32) -> Option<PathBuf> {
        foreground(pid)
            .and_then(|leader| std::fs::read_link(format!("/proc/{leader}/cwd")).ok())
            .or_else(|| std::fs::read_link(format!("/proc/{pid}/cwd")).ok())
    }

    /// The foreground process group of the terminal `pid` runs in.
    fn foreground(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        tpgid(&stat)
    }

    /// Field 8 of a `/proc/<pid>/stat` line: the foreground process group of
    /// the process's terminal.
    ///
    /// The command name is field 2, in parentheses, and may itself hold
    /// spaces or parentheses, so fields are counted from the last `)` rather
    /// than from the start of the line.
    pub(super) fn tpgid(stat: &str) -> Option<u32> {
        let rest = &stat[stat.rfind(')')? + 1..];

        // After the name: state, ppid, pgrp, session, tty_nr, tpgid.
        let field: i64 = rest.split_whitespace().nth(5)?.parse().ok()?;

        // -1 is "no controlling terminal".
        u32::try_from(field).ok().filter(|group| *group > 0)
    }
```

Replace the macOS module with:

```rust
#[cfg(target_os = "macos")]
mod cwd {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    pub(super) fn working_dir(pid: u32) -> Option<PathBuf> {
        foreground(pid)
            .and_then(directory_of)
            .or_else(|| directory_of(pid))
    }

    /// The foreground process group of the terminal `pid` runs in.
    fn foreground(pid: u32) -> Option<u32> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;

        // SAFETY: `proc_bsdinfo` is plain data, and all zeroes is a valid
        // value of it.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };

        // SAFETY: the buffer is `info`, owned by this frame, and `size` is
        // its exact length.
        let read = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };

        (read == size && info.e_tpgid > 0).then_some(info.e_tpgid)
    }

    /// `pid`'s own working directory.
    fn directory_of(pid: u32) -> Option<PathBuf> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;

        // SAFETY: `proc_vnodepathinfo` is plain data, and all zeroes is a
        // valid value of it.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };

        // SAFETY: the buffer is `info`, owned by this frame, and `size` is
        // its exact length.
        let read = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size)
        };
        if read != size {
            return None;
        }

        // `vip_path` is a MAXPATHLEN buffer, which libc spells as 32 rows of
        // 32 to stay within what an old compiler could derive traits for.
        let bytes: Vec<u8> = info
            .pvi_cdir
            .vip_path
            .as_flattened()
            .iter()
            .map(|&byte| byte as u8)
            .collect();
        let path = CStr::from_bytes_until_nul(&bytes).ok()?;

        (!path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path.to_bytes())))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-os cwd::`
Expected: 4 passed.

- [ ] **Step 5: Check the macOS and Windows code compiles**

This machine is Linux; CI builds the other two. To catch a mistake before pushing, when the targets are installed (`rustup target list --installed`):

Run: `cargo check -p dispatch-os --target aarch64-apple-darwin && cargo check -p dispatch-os --target x86_64-pc-windows-gnu`
Expected: both finish without errors. If a target is not installed, run `rustup target add aarch64-apple-darwin x86_64-pc-windows-gnu` first (only `dispatch-os` is checked, which needs no C toolchain for either). If adding targets is not wanted, say so in the task report — CI will be the first check of the macOS module.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy -p dispatch-os --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-os/src/process.rs
git commit -m "feat(os): find the directory a terminal's foreground program is in

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Panes and projects carry a branch

**Files:**
- Modify: `crates/dispatch-core/src/pane.rs` (`Pane` struct and `Pane::new`)
- Modify: `crates/dispatch-core/src/project.rs` (`Project` struct, `Project::new`, new `with_branch`)
- Modify: `crates/dispatch-core/src/state.rs` (two setters after `set_pane_title`, tests)

**Interfaces:**
- Produces: `Pane::branch: Option<String>`, `Project::branch: Option<String>` (both `#[serde(default)]`), `Project::with_branch(self, Option<String>) -> Project`, `AppState::set_pane_branch(&mut self, PaneId, Option<String>) -> Result<bool, StateError>`, `AppState::set_project_branch(&mut self, ProjectId, Option<String>) -> Result<bool, StateError>` — `Ok(true)` when the value changed.

- [ ] **Step 1: Write the failing tests**

Add to the test module at the bottom of `crates/dispatch-core/src/state.rs`:

```rust
    #[test]
    fn a_panes_branch_is_recorded_and_says_whether_it_moved() {
        let mut state = AppState::new();
        let project = state.add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let pane = state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");

        assert_eq!(
            state.pane(pane).and_then(|pane| pane.branch.clone()),
            None,
            "a new pane has not been looked at yet"
        );
        assert_eq!(state.set_pane_branch(pane, Some("main".into())), Ok(true));
        assert_eq!(
            state.set_pane_branch(pane, Some("main".into())),
            Ok(false),
            "the same branch again is no change, so nothing to redraw"
        );
        assert_eq!(
            state.pane(pane).and_then(|pane| pane.branch.as_deref()),
            Some("main")
        );
    }

    #[test]
    fn a_branch_for_an_unknown_pane_is_refused() {
        let mut state = AppState::new();
        let missing = PaneId::new();

        assert_eq!(
            state.set_pane_branch(missing, None),
            Err(StateError::NoSuchPane(missing))
        );
    }

    #[test]
    fn a_projects_branch_is_recorded_and_says_whether_it_moved() {
        let mut state = AppState::new();
        let project = state.add_project(
            Project::new("/tmp/one", ProjectSource::GitRepo { remote: None })
                .with_branch(Some("main".into())),
        );

        assert_eq!(state.projects()[0].branch.as_deref(), Some("main"));
        assert_eq!(
            state.set_project_branch(project, Some("feat/tabs".into())),
            Ok(true)
        );
        assert_eq!(
            state.set_project_branch(project, Some("feat/tabs".into())),
            Ok(false)
        );
        assert_eq!(state.projects()[0].branch.as_deref(), Some("feat/tabs"));
    }

    #[test]
    fn a_branch_for_an_unknown_project_is_refused() {
        let mut state = AppState::new();
        let missing = ProjectId::new();

        assert_eq!(
            state.set_project_branch(missing, None),
            Err(StateError::NoSuchProject(missing))
        );
    }
```

If the test module does not already see `ProjectSource`, add `use crate::project::ProjectSource;` beside its `use super::*;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-core branch`
Expected: compile errors — no field `branch`, no method `with_branch`, `set_pane_branch`, `set_project_branch`.

- [ ] **Step 3: Add the fields and setters**

In `pane.rs`, add to `Pane` after `closed`:

```rust
    /// The git branch the pane's foreground program is working on, while it
    /// is inside a repository.
    ///
    /// Reported by whichever machine runs the pane; `None` from a daemon too
    /// old to say.
    #[serde(default)]
    pub branch: Option<String>,
```

and `branch: None,` at the end of the struct literal in `Pane::new`.

In `project.rs`, add to `Project` after `source`:

```rust
    /// The branch the root has checked out, when it is a repository.
    ///
    /// Reported by the machine the project is on; `None` from a daemon too
    /// old to say.
    #[serde(default)]
    pub branch: Option<String>,
```

`branch: None,` at the end of the literal in `Project::new`, and after `with_device`:

```rust
    /// Records the branch the root has checked out.
    #[must_use]
    pub fn with_branch(mut self, branch: Option<String>) -> Self {
        self.branch = branch;
        self
    }
```

In `state.rs`, after `set_pane_title`:

```rust
    /// Records which branch a pane is working on.
    ///
    /// Returns whether that changed anything: the caller looks every few
    /// seconds, and should redraw only when a row actually moved.
    pub fn set_pane_branch(
        &mut self,
        id: PaneId,
        branch: Option<String>,
    ) -> Result<bool, StateError> {
        let pane = self
            .panes
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(StateError::NoSuchPane(id))?;

        if pane.branch == branch {
            return Ok(false);
        }
        pane.branch = branch;
        Ok(true)
    }

    /// Records which branch a project's root has checked out.
    ///
    /// Returns whether that changed anything.
    pub fn set_project_branch(
        &mut self,
        id: ProjectId,
        branch: Option<String>,
    ) -> Result<bool, StateError> {
        let project = self
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(StateError::NoSuchProject(id))?;

        if project.branch == branch {
            return Ok(false);
        }
        project.branch = branch;
        Ok(true)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-core`
Expected: all pass, including the four new tests.

- [ ] **Step 5: Build everything and commit**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: builds — nothing outside `dispatch-core` builds a `Pane` or `Project` with a struct literal.

```bash
git add crates/dispatch-core/src
git commit -m "feat(core): panes and projects carry the branch they are on

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Carry branch changes over the protocol, and apply them

**Files:**
- Modify: `crates/dispatch-proto/src/message.rs` (`PaneUpdate`, `ServerMessage`, new `ProjectUpdate`)
- Modify: `crates/dispatch-proto/src/lib.rs` (re-export)
- Modify: `crates/dispatch-proto/src/message/tests.rs`
- Modify: `dispatch/src/app.rs` (`apply_from`, imports, tests)

**Interfaces:**
- Consumes: `AppState::set_pane_branch`, `AppState::set_project_branch` (Task 3).
- Produces: `PaneUpdate::Branch { branch: Option<String> }`, `ServerMessage::ProjectChanged { project: ProjectId, update: ProjectUpdate }`, `dispatch_proto::ProjectUpdate::{Branch { branch: Option<String> }, Unknown}`.

- [ ] **Step 1: Write the failing protocol tests**

Append to `crates/dispatch-proto/src/message/tests.rs`:

```rust
#[test]
fn branch_changes_round_trip() {
    for branch in [Some("feat/tabs".to_string()), None] {
        let pane = ServerMessage::PaneChanged {
            pane: PaneId::new(),
            update: PaneUpdate::Branch {
                branch: branch.clone(),
            },
        };
        assert_eq!(round_trip(&pane), pane);

        let project = ServerMessage::ProjectChanged {
            project: ProjectId::new(),
            update: ProjectUpdate::Branch { branch },
        };
        assert_eq!(round_trip(&project), project);
    }
}

#[test]
fn an_unknown_project_update_is_skipped_rather_than_fatal() {
    // The same reason `PaneUpdate` has an `Unknown`: it travels inside a
    // message, so a variant this build lacks would fail the whole frame.
    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum FutureProjectUpdate {
        Remote { url: String },
    }

    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum FutureServerMessage {
        ProjectChanged {
            project: ProjectId,
            update: FutureProjectUpdate,
        },
    }

    let project = ProjectId::new();
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &FutureServerMessage::ProjectChanged {
            project,
            update: FutureProjectUpdate::Remote {
                url: "git@example.com:x.git".into(),
            },
        },
    )
    .expect("writing succeeds");

    let read: ServerMessage =
        Frame::read(&mut buf.as_slice()).expect("an unknown update must not fail the frame");

    assert_eq!(
        read,
        ServerMessage::ProjectChanged {
            project,
            update: ProjectUpdate::Unknown,
        }
    );
}

#[test]
fn a_project_from_an_older_daemon_has_no_branch() {
    // An older daemon's `Project` has no `branch` field at all.
    #[derive(serde::Serialize)]
    struct OlderProject {
        id: ProjectId,
        name: &'static str,
        root: &'static str,
        source: dispatch_core::ProjectSource,
    }

    #[derive(serde::Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OlderServerMessage {
        ProjectOpened { project: OlderProject },
    }

    let id = ProjectId::new();
    let mut buf = Vec::new();
    Frame::write(
        &mut buf,
        &OlderServerMessage::ProjectOpened {
            project: OlderProject {
                id,
                name: "app",
                root: "/home/me/app",
                source: dispatch_core::ProjectSource::LocalDir,
            },
        },
    )
    .expect("writing succeeds");

    let read: ServerMessage = Frame::read(&mut buf.as_slice()).expect("reading succeeds");
    let ServerMessage::ProjectOpened { project } = read else {
        panic!("expected a project, got {read:?}");
    };

    assert_eq!(project.id, id);
    assert_eq!(project.branch, None);
}
```

If `ProjectId` is not in scope through `use super::*;`, add `use dispatch_core::ProjectId;` at the top of the test file.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-proto`
Expected: compile errors — no variant `PaneUpdate::Branch`, `ServerMessage::ProjectChanged`, type `ProjectUpdate`.

- [ ] **Step 3: Add the variants**

In `message.rs`, add to `PaneUpdate` before `Unknown`:

```rust
    /// The branch the pane is working on changed.
    ///
    /// `None` once the pane's program is outside any repository.
    Branch {
        /// The branch, or `@` and a short commit when the head is detached.
        branch: Option<String>,
    },
```

Add to `ServerMessage` before `Unknown`:

```rust
    /// Something about a project changed after it was opened.
    ProjectChanged {
        /// Which project.
        project: ProjectId,
        /// What changed.
        update: ProjectUpdate,
    },
```

After the `PaneUpdate` enum:

```rust
/// A change to a project after it was opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProjectUpdate {
    /// The project root's checked-out branch changed.
    Branch {
        /// The branch, or `@` and a short commit when the head is detached.
        branch: Option<String>,
    },
    /// A change this build does not know.
    ///
    /// Travels inside [`ServerMessage::ProjectChanged`], so it needs the same
    /// landing place [`PaneUpdate::Unknown`] gives a pane's.
    #[serde(other)]
    Unknown,
}
```

In `lib.rs`, add `ProjectUpdate` to the `pub use message::{...}` list.

- [ ] **Step 4: Run the protocol tests to verify they pass**

Run: `cargo test -p dispatch-proto`
Expected: all pass. `cargo build --workspace` now fails in `dispatch/src/app.rs` with non-exhaustive matches — that is the next step.

- [ ] **Step 5: Write the failing client tests**

In `dispatch/src/app.rs`'s test module, next to the other `attached_app` tests:

```rust
    #[test]
    fn a_daemon_saying_a_pane_moved_branch_is_recorded() {
        let (mut app, project, daemon, _sent) = attached_app();
        let pane = spawn_several(&mut app, &daemon, project, 1)[0];

        daemon
            .send(ServerMessage::PaneChanged {
                pane,
                update: PaneUpdate::Branch {
                    branch: Some("feat/tabs".into()),
                },
            })
            .expect("the app is listening");

        assert!(app.poll_daemon(), "a branch change is worth a redraw");
        assert_eq!(
            app.state.pane(pane).and_then(|pane| pane.branch.as_deref()),
            Some("feat/tabs")
        );
    }

    #[test]
    fn a_daemon_saying_a_project_moved_branch_is_recorded() {
        let (mut app, project, daemon, _sent) = attached_app();

        daemon
            .send(ServerMessage::ProjectChanged {
                project,
                update: ProjectUpdate::Branch {
                    branch: Some("main".into()),
                },
            })
            .expect("the app is listening");

        assert!(app.poll_daemon());
        assert_eq!(
            app.state
                .projects()
                .iter()
                .find(|candidate| candidate.id == project)
                .and_then(|project| project.branch.as_deref()),
            Some("main")
        );
    }
```

- [ ] **Step 6: Apply the messages in `apply_from`**

Extend the import: `use dispatch_proto::{ClientMessage, DelegateOutcome, PaneUpdate, ProjectUpdate, ServerMessage};`

In `apply_from`'s `ServerMessage::PaneChanged { pane, update } => match update {` add before `PaneUpdate::Unknown`:

```rust
                PaneUpdate::Branch { branch } => {
                    self.state.set_pane_branch(pane, branch).unwrap_or(false)
                }
```

After the `ServerMessage::ProjectClosed { project } => { ... }` arm:

```rust
            ServerMessage::ProjectChanged { project, update } => match update {
                ProjectUpdate::Branch { branch } => {
                    self.state.set_project_branch(project, branch).unwrap_or(false)
                }
                // A newer daemon's change this build has no name for: ignored,
                // as the protocol promises, rather than failing the frame.
                ProjectUpdate::Unknown => false,
            },
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p dispatch moved_branch`
Expected: 2 passed.

- [ ] **Step 8: Build, lint and commit**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings`
If `dispatch-daemon` fails to compile on a non-exhaustive match over `PaneUpdate` or `ServerMessage`, add the new variants there as ignored (`=> {}`), the way that match already treats `Unknown`.

```bash
git add crates/dispatch-proto dispatch/src/app.rs crates/dispatch-daemon
git commit -m "feat(proto): tell clients when a pane or project changes branch

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The daemon watches its panes' and projects' branches

**Files:**
- Modify: `crates/dispatch-daemon/src/pane.rs` (`DaemonPane::branch`)
- Modify: `crates/dispatch-daemon/src/session.rs` (fields, `open_project`, `pump_panes`, subscribe replay, both `DaemonPane { .. }` literals)
- Modify: `crates/dispatch-daemon/src/session/tests.rs`
- Modify: `docs/superpowers/specs/2026-09-24-visual-refresh-design.md` (one sentence)

**Interfaces:**
- Consumes: `dispatch_os::git::{head, RECHECK}` (Task 1), `dispatch_os::process::working_dir` (Task 2), `Project::with_branch` (Task 3), `PaneUpdate::Branch`, `ServerMessage::ProjectChanged`, `ProjectUpdate::Branch` (Task 4).
- Produces: `Daemon::branch_every: Duration` (private; tests set it to `Duration::ZERO`).

- [ ] **Step 1: Write the failing tests**

Append to `crates/dispatch-daemon/src/session/tests.rs`:

```rust
/// Makes `dir` a repository on `branch`, as far as reading `HEAD` goes.
fn check_out(dir: &Path, branch: &str) {
    std::fs::create_dir_all(dir.join(".git")).expect("temp dir is writable");
    std::fs::write(
        dir.join(".git").join("HEAD"),
        format!("ref: refs/heads/{branch}\n"),
    )
    .expect("temp dir is writable");
}

/// A daemon with one project in a repository on `main`, looking at branches
/// on every tick rather than every two seconds.
fn daemon_in_a_repository(label: &str) -> (Daemon, ProjectId, TempDir) {
    let dir = TempDir::new(label);
    check_out(&dir.0, "main");
    let registry = harnesses(&dir.0.join("harnesses"));

    let mut daemon = Daemon::new(registry, "test-device");
    daemon.branch_every = Duration::ZERO;
    let root = dispatch_os::paths::resolve(&dir.0).expect("the temp dir resolves");
    let project = daemon.open_project(root);

    (daemon, project, dir)
}

/// The pane a `PaneSpawned` among `messages` announced.
fn spawned_pane(messages: &[ServerMessage]) -> Option<PaneId> {
    messages.iter().find_map(|message| match message {
        ServerMessage::PaneSpawned { pane, .. } => Some(*pane),
        _ => None,
    })
}

/// The branch last reported for `pane`, if any report was seen.
fn last_branch(messages: &[ServerMessage], pane: PaneId) -> Option<Option<String>> {
    messages.iter().rev().find_map(|message| match message {
        ServerMessage::PaneChanged {
            pane: changed,
            update: PaneUpdate::Branch { branch },
        } if *changed == pane => Some(branch.clone()),
        _ => None,
    })
}

#[test]
fn a_project_in_a_repository_is_announced_with_its_branch() {
    let (mut daemon, project, _dir) = daemon_in_a_repository("branch-open");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);

    let announced = drain(&inbox)
        .into_iter()
        .find_map(|message| match message {
            ServerMessage::ProjectOpened { project: opened } if opened.id == project => {
                Some(opened)
            }
            _ => None,
        })
        .expect("the project is announced on subscribe");

    assert_eq!(announced.branch.as_deref(), Some("main"));
}

#[test]
fn switching_a_projects_branch_is_announced() {
    let (mut daemon, project, dir) = daemon_in_a_repository("branch-switch");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    check_out(&dir.0, "feat/tabs");

    wait_for(&mut daemon, &inbox, |messages| {
        messages.iter().any(|message| {
            matches!(
                message,
                ServerMessage::ProjectChanged {
                    project: changed,
                    update: ProjectUpdate::Branch { branch: Some(branch) },
                } if *changed == project && branch == "feat/tabs"
            )
        })
    });
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_pane_reports_the_branch_of_the_directory_its_shell_is_in() {
    let (mut daemon, project, dir) = daemon_in_a_repository("branch-pane");
    check_out(&dir.0.join("wt"), "feat/wt");

    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );

    let seen = wait_for(&mut daemon, &inbox, |messages| {
        spawned_pane(messages).and_then(|pane| last_branch(messages, pane))
            == Some(Some("main".into()))
    });
    let pane = spawned_pane(&seen).expect("the pane was announced");

    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane,
            bytes: b"cd wt\n".to_vec(),
        },
    );
    wait_for(&mut daemon, &inbox, |messages| {
        last_branch(messages, pane) == Some(Some("feat/wt".into()))
    });

    // A client attaching now is told where the pane is, rather than left to
    // wait for it to move again.
    let late = daemon.attach_for_test(2);
    daemon.request_for_test(2, hello());
    daemon.request_for_test(2, ClientMessage::Subscribe);
    assert_eq!(
        last_branch(&drain(&late), pane),
        Some(Some("feat/wt".into()))
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn an_exited_pane_keeps_the_branch_it_last_had() {
    // Between its process exiting and the exit being noticed, a pane's
    // directory cannot be read. Reporting that as "no branch" would bounce
    // the finished pane out of its group for no reason.
    let (mut daemon, project, _dir) = daemon_in_a_repository("branch-exit");
    let inbox = daemon.attach_for_test(1);
    daemon.request_for_test(1, hello());
    daemon.request_for_test(1, ClientMessage::Subscribe);
    let _ = drain(&inbox);

    daemon.request_for_test(
        1,
        ClientMessage::SpawnPane {
            project,
            harness: "shell".into(),
            size: (80, 24),
        },
    );
    let seen = wait_for(&mut daemon, &inbox, |messages| {
        spawned_pane(messages).and_then(|pane| last_branch(messages, pane))
            == Some(Some("main".into()))
    });
    let pane = spawned_pane(&seen).expect("the pane was announced");

    daemon.request_for_test(
        1,
        ClientMessage::WritePane {
            pane,
            bytes: b"exit\n".to_vec(),
        },
    );
    let seen = wait_for(&mut daemon, &inbox, |messages| {
        messages.iter().any(|message| {
            matches!(
                message,
                ServerMessage::PaneChanged {
                    pane: changed,
                    update: PaneUpdate::Status { status: PaneStatus::Exited(_) },
                } if *changed == pane
            )
        })
    });

    let mut after = Vec::new();
    for _ in 0..20 {
        daemon.tick();
        after.extend(drain(&inbox));
        std::thread::sleep(Duration::from_millis(10));
    }
    let all: Vec<ServerMessage> = seen.into_iter().chain(after).collect();

    assert_eq!(
        last_branch(&all, pane),
        Some(Some("main".into())),
        "the last word on the pane's branch is still main: {all:#?}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-daemon branch`
Expected: compile error — no field `branch_every` on `Daemon`.

- [ ] **Step 3: Implement**

In `pane.rs`, add to `DaemonPane` after `exited_at`:

```rust
    /// The branch last reported to clients, so a look that finds the same
    /// one says nothing.
    pub branch: Option<String>,
```

and `branch: None,` in both `DaemonPane { .. }` literals in `session.rs`.

In `session.rs`, extend the imports:

```rust
use dispatch_proto::{
    ClientMessage, DelegateOutcome, Frame, FrameError, PaneUpdate, ProjectUpdate, ProtocolError,
    Role, ServerMessage,
};
```

Add to `Daemon`:

```rust
    /// When branches were last looked at.
    branches_checked: Option<Instant>,
    /// How often they are looked at: [`dispatch_os::git::RECHECK`], or every
    /// tick in a test that cannot wait two seconds per step.
    branch_every: Duration,
```

and in `with_limits`: `branches_checked: None, branch_every: dispatch_os::git::RECHECK,`.

In `open_project`, replace the two lines building the project:

```rust
        let branch = dispatch_os::git::head(&root);
        let project = Project::new(root, source).with_branch(branch);
```

At the end of `pump_panes`, after `self.expire_requests();`, add `self.refresh_branches();` and the method after `pump_panes`:

```rust
    /// Looks again at which branch every live pane and every project is on,
    /// at most every `branch_every`, and tells clients about what moved.
    ///
    /// A pane whose directory cannot be read keeps the branch it had: that is
    /// a process between exiting and being reaped, or one this user may not
    /// look into, and neither has moved anywhere.
    fn refresh_branches(&mut self) {
        if self
            .branches_checked
            .is_some_and(|checked| checked.elapsed() < self.branch_every)
        {
            return;
        }
        self.branches_checked = Some(Instant::now());

        let mut messages = Vec::new();

        for (id, pane) in &mut self.panes {
            if !pane.status.is_live() {
                continue;
            }
            let Some(dir) = pane.session.pid().and_then(dispatch_os::process::working_dir)
            else {
                continue;
            };

            let branch = dispatch_os::git::head(&dir);
            if branch != pane.branch {
                pane.branch.clone_from(&branch);
                messages.push(ServerMessage::PaneChanged {
                    pane: *id,
                    update: PaneUpdate::Branch { branch },
                });
            }
        }

        for project in self.projects.values_mut() {
            let branch = dispatch_os::git::head(&project.root);
            if branch != project.branch {
                project.branch.clone_from(&branch);
                messages.push(ServerMessage::ProjectChanged {
                    project: project.id,
                    update: ProjectUpdate::Branch { branch },
                });
            }
        }

        for message in messages {
            self.broadcast(message);
        }
    }
```

In the subscribe replay, inside `for pane in self.panes.values() {`, directly after the `existing.push(ServerMessage::PaneSpawned { .. });`:

```rust
                    // Not part of `PaneSpawned`, so said straight after it:
                    // a client attaching now should not have to wait for the
                    // pane to move before it can be grouped.
                    if let Some(branch) = &pane.branch {
                        existing.push(ServerMessage::PaneChanged {
                            pane: pane.id,
                            update: PaneUpdate::Branch {
                                branch: Some(branch.clone()),
                            },
                        });
                    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-daemon`
Expected: all pass, including the four new tests.

- [ ] **Step 5: Record the refinement in the spec**

In the spec's "Who looks, and how often" section, replace the final paragraph's last sentence (starting "Any failure") with:

```markdown
A pane whose directory cannot be read — a process between exiting and
being reaped, or one owned by another user — keeps the branch it last had
rather than being reported as on none. A directory that can be read but is
outside any repository is `None`. Nothing here is ever a message on screen.
```

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-daemon docs/superpowers/specs/2026-09-24-visual-refresh-design.md
git commit -m "feat(daemon): watch which branch each pane and project is on

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: A standalone client watches its own panes' branches

**Files:**
- Modify: `dispatch/src/app.rs` (`App` fields and `App::new`, `add_project`, `poll_panes`, new `refresh_local_branches`, tests)

**Interfaces:**
- Consumes: `dispatch_os::git::{head, RECHECK}`, `dispatch_os::process::working_dir`, `AppState::set_pane_branch`, `AppState::set_project_branch`, `Project::with_branch`.
- Produces: `App::branch_every: Duration` (private; tests set it to `Duration::ZERO`).

- [ ] **Step 1: Write the failing test**

In `app.rs`'s test module, beside `attaching_takes_down_the_agents_this_process_started`:

```rust
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_standalone_client_watches_its_own_panes_and_projects_branches() {
        let def = dispatch_config::HarnessDef {
            id: "shell".to_string(),
            display_name: "Shell".to_string(),
            launch: Launch {
                command: "sh".to_string(),
                args: Vec::new(),
                env: Default::default(),
            },
            ..Default::default()
        };

        let dir = scratch("branch-local");
        std::fs::create_dir_all(dir.join(".git")).expect("temp dir is writable");
        std::fs::write(dir.join(".git").join("HEAD"), "ref: refs/heads/main\n")
            .expect("temp dir is writable");

        let mut app = App::new([def].into_iter().collect());
        app.branch_every = Duration::ZERO;
        app.add_project(dir.clone());
        let project = app.state.projects()[0].id;
        assert_eq!(
            app.state.projects()[0].branch.as_deref(),
            Some("main"),
            "a project knows its branch as soon as it is opened"
        );

        let _ = app.state.select_project(project);
        app.spawn_pane("shell", Size::new(80, 24))
            .expect("a shell starts");
        let pane = app.state.focused_pane().expect("the new pane is focused");

        let deadline = Instant::now() + Duration::from_secs(10);
        while app.state.pane(pane).and_then(|pane| pane.branch.clone()).is_none() {
            assert!(Instant::now() < deadline, "the pane never learned its branch");
            app.poll_panes();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            app.state.pane(pane).and_then(|pane| pane.branch.as_deref()),
            Some("main")
        );

        std::fs::write(dir.join(".git").join("HEAD"), "ref: refs/heads/feat/x\n")
            .expect("temp dir is writable");
        while app.state.projects()[0].branch.as_deref() != Some("feat/x") {
            assert!(Instant::now() < deadline, "the project never moved branch");
            app.poll_panes();
            std::thread::sleep(Duration::from_millis(20));
        }

        app.close_focused();
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p dispatch watches_its_own`
Expected: compile error — no field `branch_every`.

- [ ] **Step 3: Implement**

Add to `App` (beside `status`):

```rust
    /// When this client last looked at its own panes' and projects' branches.
    branches_checked: Option<Instant>,
    /// How often it looks: [`dispatch_os::git::RECHECK`], or every poll in a
    /// test that cannot wait two seconds per step.
    branch_every: Duration,
```

and in `App::new`: `branches_checked: None, branch_every: dispatch_os::git::RECHECK,`.

In `add_project`, replace the final `self.state.add_project(Project::new(root, source).with_device(device));` with:

```rust
        let branch = dispatch_os::git::head(&root);
        self.state.add_project(
            Project::new(root, source)
                .with_branch(branch)
                .with_device(device),
        );
```

At the end of `poll_panes`, before `changed` is returned:

```rust
        if self.refresh_local_branches() {
            changed = true;
        }
```

After `poll_panes`:

```rust
    /// Looks again at which branch this process's own panes and projects are
    /// on, at most every `branch_every`. Returns whether any row moved.
    ///
    /// Standalone only: attached, every pane and project is a daemon's, and
    /// the daemon looks from the machine they are on. A pane whose directory
    /// cannot be read keeps the branch it had, as the daemon's do.
    fn refresh_local_branches(&mut self) -> bool {
        let Some(local) = self.local else {
            return false;
        };
        if self
            .branches_checked
            .is_some_and(|checked| checked.elapsed() < self.branch_every)
        {
            return false;
        }
        self.branches_checked = Some(Instant::now());

        let mut changed = false;

        let dirs: Vec<(PaneId, PathBuf)> = self
            .panes
            .iter()
            .filter_map(|(id, pane)| match &pane.backend {
                Backend::Local(session) => session
                    .pid()
                    .and_then(dispatch_os::process::working_dir)
                    .map(|dir| (*id, dir)),
                Backend::Remote(_) => None,
            })
            .collect();
        for (id, dir) in dirs {
            let branch = dispatch_os::git::head(&dir);
            changed |= self.state.set_pane_branch(id, branch).unwrap_or(false);
        }

        let roots: Vec<(ProjectId, PathBuf)> = self
            .state
            .projects()
            .iter()
            .filter(|project| project.device == local)
            .map(|project| (project.id, project.root.clone()))
            .collect();
        for (id, root) in roots {
            let branch = dispatch_os::git::head(&root);
            changed |= self.state.set_project_branch(id, branch).unwrap_or(false);
        }

        changed
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch`
Expected: all pass.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add dispatch/src/app.rs
git commit -m "feat(dispatch): watch branches of the panes a standalone client runs

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: The theme, and reading the terminal's colour replies

**Files:**
- Create: `crates/dispatch-tui/src/theme.rs`
- Create: `crates/dispatch-tui/src/theme/tests.rs`
- Modify: `crates/dispatch-tui/src/lib.rs`

**Interfaces:**
- Produces (`dispatch_tui::theme`): `Rgb(u8, u8, u8)` with `mix(self, Rgb, f32) -> Rgb`; `Palette { background, foreground, accent: Rgb }` with `Palette::FALLBACK`; `Depth::{TrueColor, Indexed}` with `Depth::from_colorterm(Option<&str>) -> Depth`; `Theme { faded, tint, tab, accent: ratatui::style::Color }` with `Theme::new(Palette, Depth)`, `Theme::fallback()`, `Default`; `nearest_indexed(Rgb) -> u8`; `QUERY: &[u8]`; `Replies { foreground, background, accent: Option<Rgb>, done: bool }` with `Replies::parse(&[u8]) -> Replies` and `Replies::palette(&self) -> Palette`. `dispatch_tui::Theme` re-exported.

- [ ] **Step 1: Write the failing tests**

Create `crates/dispatch-tui/src/theme/tests.rs`:

```rust
//! Tests for the theme.

use super::*;

#[test]
fn mixing_moves_one_colour_toward_another() {
    let black = Rgb(0, 0, 0);
    let white = Rgb(255, 255, 255);

    assert_eq!(black.mix(white, 0.0), black);
    assert_eq!(black.mix(white, 1.0), white);
    assert_eq!(black.mix(white, 0.5), Rgb(128, 128, 128));
}

#[test]
fn every_role_is_mixed_from_the_palette() {
    let palette = Palette::FALLBACK;
    let theme = Theme::new(palette, Depth::TrueColor);
    let rgb = |c: Rgb| Color::Rgb(c.0, c.1, c.2);

    assert_eq!(
        theme.faded,
        rgb(palette.foreground.mix(palette.background, 0.45))
    );
    assert_eq!(
        theme.tint,
        rgb(palette.background.mix(palette.foreground, 0.10))
    );
    assert_eq!(theme.tab, rgb(palette.background.mix(palette.accent, 0.30)));
    assert_eq!(theme.accent, rgb(palette.accent));
}

#[test]
fn a_terminal_without_24_bit_colour_gets_palette_indices() {
    let theme = Theme::new(Palette::FALLBACK, Depth::Indexed);

    for colour in [theme.faded, theme.tint, theme.tab, theme.accent] {
        assert!(matches!(colour, Color::Indexed(_)), "{colour:?}");
    }
}

#[test]
fn colorterm_says_whether_24_bit_colour_is_there() {
    assert_eq!(Depth::from_colorterm(Some("truecolor")), Depth::TrueColor);
    assert_eq!(Depth::from_colorterm(Some("24bit")), Depth::TrueColor);
    assert_eq!(Depth::from_colorterm(Some("")), Depth::Indexed);
    assert_eq!(Depth::from_colorterm(None), Depth::Indexed);
}

#[test]
fn the_nearest_palette_entry_is_found_in_the_cube_or_the_grey_ramp() {
    assert_eq!(nearest_indexed(Rgb(0, 0, 0)), 16);
    assert_eq!(nearest_indexed(Rgb(255, 255, 255)), 231);
    assert_eq!(nearest_indexed(Rgb(255, 0, 0)), 196);
    assert_eq!(nearest_indexed(Rgb(128, 128, 128)), 244);
}

#[test]
fn replies_are_read_in_every_width_and_both_terminators() {
    let replies = Replies::parse(
        b"\x1b]10;rgb:c8c8/c8c8/d8d8\x07\
          \x1b]11;rgb:16/16/1e\x1b\\\
          \x1b]4;5;rgb:b/a/f\x07",
    );

    assert_eq!(replies.foreground, Some(Rgb(0xc8, 0xc8, 0xd8)));
    assert_eq!(replies.background, Some(Rgb(0x16, 0x16, 0x1e)));
    assert_eq!(replies.accent, Some(Rgb(0xbb, 0xaa, 0xff)));
    assert!(!replies.done, "no device attributes yet");
}

#[test]
fn an_rgba_reply_is_read_for_its_colour() {
    let replies = Replies::parse(b"\x1b]11;rgba:1616/1616/1e1e/ffff\x07");

    assert_eq!(replies.background, Some(Rgb(0x16, 0x16, 0x1e)));
}

#[test]
fn the_device_attributes_reply_ends_the_wait() {
    assert!(Replies::parse(b"\x1b[?62;22c").done);
    assert!(
        !Replies::parse(b"\x1b[12;40R").done,
        "another report is not the one that ends it"
    );
}

#[test]
fn keystrokes_and_other_sequences_between_replies_are_skipped() {
    let replies = Replies::parse(b"x\x1b[A\x1b]11;rgb:0000/0000/0000\x07q\x1b[?1;2c");

    assert_eq!(replies.background, Some(Rgb(0, 0, 0)));
    assert!(replies.done);
}

#[test]
fn a_reply_split_across_reads_is_read_once_it_is_whole() {
    let whole: &[u8] = b"\x1b]11;rgb:1616/1616/1e1e\x07\x1b[?62c";
    let first = &whole[..12];

    let partial = Replies::parse(first);
    assert_eq!(partial.background, None, "half a reply is not a colour");
    assert!(!partial.done, "and does not end the wait");

    let complete = Replies::parse(whole);
    assert_eq!(complete.background, Some(Rgb(0x16, 0x16, 0x1e)));
    assert!(complete.done);
}

#[test]
fn a_colour_not_given_is_taken_from_the_fallback() {
    let palette = Replies::parse(b"\x1b]11;rgb:ffff/ffff/ffff\x07").palette();

    assert_eq!(palette.background, Rgb(255, 255, 255));
    assert_eq!(palette.foreground, Palette::FALLBACK.foreground);
    assert_eq!(palette.accent, Palette::FALLBACK.accent);
}

#[test]
fn the_query_asks_for_all_three_colours_then_device_attributes() {
    assert_eq!(
        QUERY,
        b"\x1b]10;?\x07\x1b]11;?\x07\x1b]4;5;?\x07\x1b[c".as_slice()
    );
}
```

Create `crates/dispatch-tui/src/theme.rs` with the module doc and `#[cfg(test)] mod tests;` only, and add to `lib.rs`:

```rust
pub mod theme;
```

and `pub use theme::Theme;` beside the other re-exports.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui theme::`
Expected: compile errors — `Rgb`, `Palette`, `Theme`, … not found.

- [ ] **Step 3: Implement the module**

`crates/dispatch-tui/src/theme.rs`:

```rust
//! Colours for Dispatch's own chrome, mixed from the terminal's.
//!
//! Dispatch draws around the agents rather than over them, so its colours
//! should sit with whatever theme the terminal already has. It asks the
//! terminal for its background, foreground and one accent at startup and
//! mixes everything else from those three; a terminal that does not answer
//! gets a built-in dark palette in the same spirit.

use ratatui::style::Color;

/// A colour as three 8-bit channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `self` moved `amount` of the way toward `other`, from 0.0 to 1.0.
    #[must_use]
    pub fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let channel = |from: u8, to: u8| {
            let from = f32::from(from);
            let to = f32::from(to);
            (from + (to - from) * amount).round().clamp(0.0, 255.0) as u8
        };

        Rgb(
            channel(self.0, other.0),
            channel(self.1, other.1),
            channel(self.2, other.2),
        )
    }
}

/// The three colours everything else is mixed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The terminal's background.
    pub background: Rgb,
    /// The terminal's default text colour.
    pub foreground: Rgb,
    /// Palette slot 5, magenta in most themes: the one colour Dispatch
    /// uses to say "this has the keyboard".
    pub accent: Rgb,
}

impl Palette {
    /// The built-in palette, for a terminal that does not say what its own is.
    pub const FALLBACK: Palette = Palette {
        background: Rgb(0x16, 0x16, 0x1e),
        foreground: Rgb(0xc8, 0xc8, 0xd8),
        accent: Rgb(0xb4, 0xa0, 0xf0),
    };
}

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Any 24-bit colour.
    TrueColor,
    /// The xterm 256-colour palette.
    ///
    /// The safe assumption: a terminal without 24-bit colour misreads an RGB
    /// escape rather than approximating it.
    Indexed,
}

impl Depth {
    /// Read from `COLORTERM`, the one variable terminals use to say so.
    #[must_use]
    pub fn from_colorterm(value: Option<&str>) -> Depth {
        match value {
            Some("truecolor" | "24bit") => Depth::TrueColor,
            _ => Depth::Indexed,
        }
    }
}

/// What Dispatch's chrome is drawn in.
///
/// Ordinary text is not here: it stays the terminal's own foreground. Nor are
/// the state glyphs' colours, which are ANSI and so already the theme's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Secondary text: branches, the app name, inactive tabs, unfocused
    /// borders, the status row.
    pub faded: Color,
    /// The row behind a selected project or a focused pane.
    pub tint: Color,
    /// The active tab's background.
    pub tab: Color,
    /// The focused pane's border.
    pub accent: Color,
}

impl Theme {
    /// Mixes a theme from `palette`, at the depth the terminal can show.
    #[must_use]
    pub fn new(palette: Palette, depth: Depth) -> Theme {
        let colour = |rgb: Rgb| match depth {
            Depth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
            Depth::Indexed => Color::Indexed(nearest_indexed(rgb)),
        };

        Theme {
            faded: colour(palette.foreground.mix(palette.background, 0.45)),
            tint: colour(palette.background.mix(palette.foreground, 0.10)),
            tab: colour(palette.background.mix(palette.accent, 0.30)),
            accent: colour(palette.accent),
        }
    }

    /// The built-in palette at full depth.
    ///
    /// What every test draws with, so no assertion depends on the terminal
    /// running it.
    #[must_use]
    pub fn fallback() -> Theme {
        Theme::new(Palette::FALLBACK, Depth::TrueColor)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::fallback()
    }
}

/// The xterm-256 entry nearest `rgb`, from the colour cube or the grey ramp.
///
/// The first sixteen entries are left out: they are the terminal's own theme
/// colours, which are exactly what cannot be known here.
#[must_use]
pub fn nearest_indexed(rgb: Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    let level = |channel: u8| {
        (0..LEVELS.len())
            .min_by_key(|&index| (i32::from(LEVELS[index]) - i32::from(channel)).abs())
            .unwrap_or(0)
    };
    let (r, g, b) = (level(rgb.0), level(rgb.1), level(rgb.2));
    let cube = Rgb(LEVELS[r], LEVELS[g], LEVELS[b]);
    let cube_index = 16 + 36 * r + 6 * g + b;

    // The ramp runs 8, 18, …, 238 in 24 steps.
    let average = (u32::from(rgb.0) + u32::from(rgb.1) + u32::from(rgb.2)) / 3;
    let step = ((average.saturating_sub(8) + 5) / 10).min(23);
    let grey_value = u8::try_from(8 + 10 * step).unwrap_or(u8::MAX);
    let grey = Rgb(grey_value, grey_value, grey_value);
    let grey_index = 232 + step as usize;

    let index = if distance(rgb, grey) < distance(rgb, cube) {
        grey_index
    } else {
        cube_index
    };
    u8::try_from(index).unwrap_or(u8::MAX)
}

/// Squared distance between two colours.
fn distance(a: Rgb, b: Rgb) -> u32 {
    let d = |x: u8, y: u8| {
        let delta = i32::from(x) - i32::from(y);
        delta.unsigned_abs() * delta.unsigned_abs()
    };
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// What is asked of the terminal at startup, as one write.
///
/// Foreground, background and palette slot 5, then primary device
/// attributes. Every terminal answers the last, and answers in order, so its
/// reply arriving means every colour reply that is coming has come.
pub const QUERY: &[u8] = b"\x1b]10;?\x07\x1b]11;?\x07\x1b]4;5;?\x07\x1b[c";

/// What the terminal has answered so far.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Replies {
    /// The default text colour, when it said.
    pub foreground: Option<Rgb>,
    /// The background, when it said.
    pub background: Option<Rgb>,
    /// Palette slot 5, when it said.
    pub accent: Option<Rgb>,
    /// Whether the device-attributes reply has arrived, which ends the wait.
    pub done: bool,
}

impl Replies {
    /// Reads every complete reply in `bytes`.
    ///
    /// Given the whole of what has been read each time rather than the last
    /// chunk: a reply can be split across reads, and re-reading a few dozen
    /// bytes is simpler than carrying half a reply over. An incomplete
    /// sequence at the end is left for the next call.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Replies {
        let mut replies = Replies::default();
        let mut index = 0;

        while index + 1 < bytes.len() {
            if bytes[index] != 0x1b {
                index += 1;
                continue;
            }

            match bytes[index + 1] {
                b']' => {
                    let start = index + 2;
                    let Some((end, next)) = osc_end(bytes, start) else {
                        break;
                    };
                    replies.take_colour(&bytes[start..end]);
                    index = next;
                }
                b'[' => {
                    // Parameters, then one final byte from 0x40 to 0x7e.
                    let start = index + 2;
                    let Some(length) = bytes[start..]
                        .iter()
                        .position(|byte| (0x40..=0x7e).contains(byte))
                    else {
                        break;
                    };
                    let end = start + length;
                    if bytes[end] == b'c' && bytes.get(start) == Some(&b'?') {
                        replies.done = true;
                    }
                    index = end + 1;
                }
                _ => index += 1,
            }
        }

        replies
    }

    /// The palette these replies describe, with the fallback's colour
    /// wherever the terminal said nothing.
    #[must_use]
    pub fn palette(&self) -> Palette {
        Palette {
            background: self.background.unwrap_or(Palette::FALLBACK.background),
            foreground: self.foreground.unwrap_or(Palette::FALLBACK.foreground),
            accent: self.accent.unwrap_or(Palette::FALLBACK.accent),
        }
    }

    /// Records a colour reply's body: `10;rgb:…`, `11;rgb:…` or `4;5;rgb:…`.
    fn take_colour(&mut self, body: &[u8]) {
        let Ok(body) = std::str::from_utf8(body) else {
            return;
        };

        if let Some(value) = body.strip_prefix("10;") {
            self.foreground = parse_rgb(value).or(self.foreground);
        } else if let Some(value) = body.strip_prefix("11;") {
            self.background = parse_rgb(value).or(self.background);
        } else if let Some(value) = body.strip_prefix("4;5;") {
            self.accent = parse_rgb(value).or(self.accent);
        }
    }
}

/// Where an operating-system command starting at `start` ends: the index of
/// its terminator, and the index just past it. `BEL` and `ESC \` both end one.
fn osc_end(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut index = start;

    while index < bytes.len() {
        match bytes[index] {
            0x07 => return Some((index, index + 1)),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some((index, index + 2)),
            _ => index += 1,
        }
    }

    None
}

/// `rgb:R/G/B` or `rgba:R/G/B/A`, each channel one to four hex digits,
/// scaled to eight bits.
fn parse_rgb(value: &str) -> Option<Rgb> {
    let channels = value
        .strip_prefix("rgb:")
        .or_else(|| value.strip_prefix("rgba:"))?;
    let mut parts = channels.split('/');

    let mut channel = || -> Option<u8> {
        let hex = parts.next()?;
        if hex.is_empty() || hex.len() > 4 {
            return None;
        }
        let raw = u32::from_str_radix(hex, 16).ok()?;
        let max = (1_u32 << (4 * hex.len())) - 1;
        u8::try_from((raw * 255 + max / 2) / max).ok()
    };

    Some(Rgb(channel()?, channel()?, channel()?))
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-tui theme::`
Expected: 12 passed.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy -p dispatch-tui --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui/src/theme.rs crates/dispatch-tui/src/theme crates/dispatch-tui/src/lib.rs
git commit -m "feat(tui): a theme mixed from the terminal's own colours

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: The chrome — ask the terminal, a top row, square borders, tinted tabs

**Files:**
- Create: `crates/dispatch-os/src/tty.rs`
- Modify: `crates/dispatch-os/src/lib.rs`
- Modify: `dispatch/src/terminal.rs` (query in `acquire`, `theme()` accessor)
- Modify: `dispatch/src/main.rs` (hand the theme to the app)
- Modify: `dispatch/src/app.rs` (`theme` field, `set_theme`, `draw`, `draw_name`, `draw_tabs`, `draw_status`, `pane_block`, `interior`, `draw_panes`, `draw_overlay`, `Overlay::set_border`, approval widget, imports, tests)
- Modify: `dispatch/src/approval.rs` (`border` field)
- Modify: `dispatch/src/add_machine.rs` (`prompt_mut`)
- Modify: `crates/dispatch-tui/src/picker.rs`, `prompt.rs`, `browser.rs` (`border` field, `set_border`)
- Modify: `docs/superpowers/specs/2026-09-24-visual-refresh-design.md` (timeout)

**Interfaces:**
- Consumes: `dispatch_tui::theme::{Theme, Replies, Depth, QUERY}` (Task 7).
- Produces: `dispatch_os::tty::ask(query: &[u8], timeout: Duration, done: impl FnMut(&[u8]) -> bool) -> Vec<u8>`; `TerminalGuard::theme(&self) -> Theme`; `App::set_theme(&mut self, Theme)`; `App::theme: Theme` (private, read by Task 9's sidebar call); `Picker::set_border`, `Prompt::set_border`, `Browser::set_border` (`&mut self, Style`); `Approval::border: Style`; `AddMachine::prompt_mut(&mut self) -> &mut Prompt`. Layout after this task: row 0 is the top row, the sidebar frame starts at row 1, the status row is the last row and no longer overlaps the sidebar.

- [ ] **Step 1: Write the failing app tests**

In `app.rs`'s test module:

```rust
    #[test]
    fn the_top_row_carries_the_name_and_the_tabs() {
        let mut app = App::new(HarnessRegistry::default());
        let project = app
            .state
            .add_project(Project::new("/tmp/one", ProjectSource::LocalDir));
        let pane = app
            .state
            .spawn_pane(project, HarnessId::new("claude"))
            .expect("the project exists");
        app.state
            .set_pane_title(pane, "refactor")
            .expect("the pane exists");

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("the frame is drawn");

        let text = rendered_text(&terminal);
        let top = text.lines().next().expect("the frame has rows");
        let over_sidebar: String = top.chars().take(sidebar::WIDTH as usize).collect();
        let over_panes: String = top.chars().skip(sidebar::WIDTH as usize).collect();

        assert_eq!(over_sidebar.trim(), APP_NAME, "{top:?}");
        assert!(
            over_panes.contains("1 refactor"),
            "a lone tab is still drawn, named for its pane: {top:?}"
        );
    }

    #[test]
    fn the_active_tab_is_tinted_rather_than_inverted() {
        let (mut app, project, daemon, _sent) = attached_app();
        spawn_several(&mut app, &daemon, project, 5);

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("the frame is drawn");
        let buf = terminal.backend().buffer();
        let theme = Theme::fallback();

        let top: Vec<&ratatui::buffer::Cell> = (sidebar::WIDTH..buf.area.width)
            .filter_map(|x| buf.cell((x, 0)))
            .collect();
        let active = top
            .iter()
            .find(|cell| cell.symbol() == "2")
            .expect("the focused pane's tab is drawn");
        let inactive = top
            .iter()
            .find(|cell| cell.symbol() == "1")
            .expect("the other tab is drawn");

        assert_eq!(active.bg, theme.tab, "the fifth pane is focused, on tab 2");
        assert!(!active.modifier.contains(Modifier::REVERSED));
        assert_eq!(inactive.fg, theme.faded);
        assert_eq!(inactive.bg, Color::Reset);
    }

    #[test]
    fn pane_corners_are_square() {
        let (mut app, project, daemon, _sent) = attached_app();
        spawn_several(&mut app, &daemon, project, 2);

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
            .expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("the frame is drawn");
        let text = rendered_text(&terminal);

        assert!(!text.contains('╭') && !text.contains('╯'), "{text}");
        let panes: String = text
            .lines()
            .map(|line| line.chars().skip(sidebar::WIDTH as usize).collect::<String>())
            .collect();
        assert!(panes.contains('┌') && panes.contains('┘'), "{text}");
    }

    #[test]
    fn a_terminal_smaller_than_the_sidebar_still_draws() {
        let (mut app, project, daemon, _sent) = attached_app();
        spawn_several(&mut app, &daemon, project, 3);

        for (width, height) in [(1, 1), (5, 2), (10, 3), (20, 4), (40, 2)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                    .expect("a test backend can be created");
            terminal
                .draw(|frame| app.draw(frame))
                .expect("drawing into a tiny terminal does not fail");
        }
    }
```

Update the existing sidebar click tests for the new top row — every click moves down one row:
- `a_click_on_a_project_row_selects_it_and_folds_its_panes`: both `click(&mut app, 1, 1)` → `click(&mut app, 1, 2)`, and its comment to "The first row inside the sidebar's frame — below the top row and the frame's own edge — is the first project."
- `a_click_on_a_panes_twisty_folds_its_children_without_focusing_it`: `click(&mut app, 3, 2)` → `click(&mut app, 3, 3)`.
- `a_click_on_the_rest_of_a_pane_row_still_focuses_it`: `click(&mut app, 8, 2)` → `click(&mut app, 8, 3)`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch -- top_row active_tab corners smaller_than click_on`
Expected: compile errors (`APP_NAME`, `Theme` not in scope), then after Step 3 the behaviour tests fail until Step 5.

- [ ] **Step 3: Ask the terminal**

Create `crates/dispatch-os/src/tty.rs`:

```rust
//! Asking the terminal a question before anything else reads its answers.

use std::time::Duration;

/// Writes `query` to the terminal and collects what it answers, until `done`
/// says the answer is complete or `timeout` passes.
///
/// Reads standard input directly, so it must run before anything else is
/// reading it: an event loop already running would take the answers for
/// keystrokes. Sends nothing and returns nothing when standard input is not a
/// terminal, and on Windows, which has no way to wait on it with a deadline.
pub fn ask(query: &[u8], timeout: Duration, done: impl FnMut(&[u8]) -> bool) -> Vec<u8> {
    imp::ask(query, timeout, done)
}

#[cfg(unix)]
mod imp {
    use std::io::{IsTerminal, Write};
    use std::time::{Duration, Instant};

    pub(super) fn ask(
        query: &[u8],
        timeout: Duration,
        mut done: impl FnMut(&[u8]) -> bool,
    ) -> Vec<u8> {
        let mut answer = Vec::new();
        if !std::io::stdin().is_terminal() {
            return answer;
        }

        let mut out = std::io::stdout();
        if out.write_all(query).and_then(|()| out.flush()).is_err() {
            return answer;
        }

        let deadline = Instant::now() + timeout;
        let mut chunk = [0_u8; 256];

        while !done(&answer) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }

            let mut ready = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            let wait = libc::c_int::try_from(left.as_millis())
                .unwrap_or(libc::c_int::MAX)
                .max(1);

            // SAFETY: one pollfd, owned by this frame, and a count of one.
            let polled = unsafe { libc::poll(&raw mut ready, 1, wait) };
            if polled < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if polled == 0 {
                break;
            }

            // Read from the descriptor rather than through `Stdin`, whose
            // buffer would keep whatever it read past the answer from the
            // event loop that reads next.
            //
            // SAFETY: the buffer is `chunk`, owned by this frame, and the
            // length passed is its own.
            let read = unsafe {
                libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), chunk.len())
            };
            let Ok(read) = usize::try_from(read) else {
                break;
            };
            if read == 0 {
                break;
            }
            answer.extend_from_slice(&chunk[..read]);
        }

        answer
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    pub(super) fn ask(
        _query: &[u8],
        _timeout: Duration,
        _done: impl FnMut(&[u8]) -> bool,
    ) -> Vec<u8> {
        Vec::new()
    }
}
```

Add `pub mod tty;` to `crates/dispatch-os/src/lib.rs` after `pub mod signal;`.

In `dispatch/src/terminal.rs`, add imports and a constant:

```rust
use std::time::Duration;

use dispatch_tui::theme::{self, Depth, Replies, Theme};

/// How long the terminal is given to say what its colours are.
///
/// An upper bound rather than a wait: the device-attributes reply ends it as
/// soon as the terminal has answered, which locally takes a millisecond or
/// two. Long enough for an SSH round trip, because an answer that arrives
/// after the event loop has started is read as keystrokes.
const ASK_TIMEOUT: Duration = Duration::from_secs(1);
```

Add `theme: Theme` to `TerminalGuard`, and in `acquire`, straight after `enable_raw_mode()`:

```rust
        // Raw, so the answers are neither echoed nor held back for a newline;
        // before the alternate screen and the event loop, so nothing else is
        // reading yet.
        let theme = ask_for_theme();
```

returning `Ok(Self { terminal, theme })`, with:

```rust
    /// The colours Dispatch draws its chrome in, mixed from what the terminal
    /// said its own are.
    pub fn theme(&self) -> Theme {
        self.theme
    }
```

and the free function:

```rust
/// Asks the terminal for its colours and mixes Dispatch's from them.
fn ask_for_theme() -> Theme {
    let answer = dispatch_os::tty::ask(theme::QUERY, ASK_TIMEOUT, |bytes| {
        Replies::parse(bytes).done
    });
    let depth = Depth::from_colorterm(std::env::var("COLORTERM").ok().as_deref());

    Theme::new(Replies::parse(&answer).palette(), depth)
}
```

In `dispatch/src/main.rs`, after `let mut guard = TerminalGuard::acquire()?;`:

```rust
    app.set_theme(guard.theme());
```

In the spec, "Asking the terminal": replace "and reads replies for at most 200 ms" with "and reads replies for at most one second", and after "…rather than waiting out the timeout on a terminal that ignores OSC queries." add: "The bound is generous because it is only reached when something is wrong: a reply that arrived after the event loop started would be read as keystrokes." In the failure table, "200 ms timeout" becomes "One-second timeout".

- [ ] **Step 4: Border style on the overlays**

In each of `picker.rs`, `prompt.rs`, `browser.rs`: add a field

```rust
    /// The frame's colour, set by whoever draws the overlay so it matches
    /// the rest of the interface.
    border: Style,
```

initialised to `Style::default().fg(Color::Cyan)` in every constructor (`Picker::new`, `Prompt::new`, `Browser::new`, and any other function in those files that builds `Self { .. }`), a setter

```rust
    /// Draws the frame in `style`.
    pub fn set_border(&mut self, style: Style) {
        self.border = style;
    }
```

and in each `Widget` impl replace `.border_style(Style::default().fg(Color::Cyan))` on the frame's `Block` with `.border_style(self.border)`. Leave every other use of `Color::Cyan` (the selection bar, the cursor) as it is.

In `dispatch/src/add_machine.rs`, beside `prompt()`:

```rust
    /// The prompt on screen, for the caller to restyle before drawing it.
    pub fn prompt_mut(&mut self) -> &mut Prompt {
        &mut self.prompt
    }
```

In `dispatch/src/approval.rs`, add to `Approval`:

```rust
    /// The frame's colour.
    pub border: Style,
```

use it in `render` (`.border_style(self.border)` on the block), and add `border: Style::default(),` to the `approval()` helper in its tests.

- [ ] **Step 5: The app draws with the theme**

In `app.rs`:

Imports: add `use dispatch_tui::Theme;` and `use ratatui::text::{Line, Span};`; remove `BorderType` from the `ratatui::widgets` import.

Constants, beside `PANES_PER_TAB`:

```rust
/// The program's name as the top-left corner spells it, letter-spaced the
/// way a label rather than a heading is.
const APP_NAME: &str = "D I S P A T C H";

/// How much of a pane's title a tab shows.
const TAB_TITLE: usize = 16;
```

Replace `pane_block`:

```rust
/// The border drawn around one pane.
///
/// Square, like every other edge in the interface: faded when unfocused, and
/// in the accent on the pane that has the keyboard, with its title in bold.
fn pane_block(focused: bool, theme: &Theme) -> Block<'static> {
    let (colour, title) = if focused {
        (theme.accent, Style::default().add_modifier(Modifier::BOLD))
    } else {
        (theme.faded, Style::default())
    };

    Block::bordered()
        .border_style(Style::default().fg(colour))
        .title_style(title)
}
```

`interior` becomes `Block::bordered().inner(frame)`, and `draw_panes` calls `pane_block(is_focused, &self.theme)`.

Add to `App`: `theme: Theme,` (initialised `theme: Theme::fallback(),` in `App::new`) and:

```rust
    /// Draws the interface in `theme` from the next frame on.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }
```

Replace the start of `draw`, up to and including the tab-row block, with:

```rust
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();

        // One row across the top for the name and the tabs, one along the
        // bottom for status, and everything between for the sidebar and the
        // panes.
        let top = Rect::new(area.x, area.y, area.width, area.height.min(1));
        let body = Rect::new(
            area.x,
            area.y + top.height,
            area.width,
            area.height.saturating_sub(top.height + 1),
        );

        let sidebar_width = sidebar::WIDTH.min(area.width);
        let sidebar_area = Rect::new(body.x, body.y, sidebar_width, body.height);
        let panes_area = Rect::new(
            body.x + sidebar_width,
            body.y,
            body.width.saturating_sub(sidebar_width),
            body.height,
        );

        frame.render_widget(
            Sidebar::new(&self.state).with_harnesses(&self.harnesses),
            sidebar_area,
        );
        self.sidebar_area = sidebar_area;

        self.draw_name(frame, Rect::new(top.x, top.y, sidebar_width, top.height));
        self.draw_tabs(
            frame,
            Rect::new(panes_area.x, top.y, panes_area.width, top.height),
        );
```

keeping the rest of `draw` (frames, layout, `draw_panes`, `draw_status`, `draw_overlay`) as it is.

Add `draw_name` before `draw_tabs`:

```rust
    /// Writes the program's name in the top row, over the sidebar's column.
    fn draw_name(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.height == 0 || area.width < 2 {
            return;
        }

        // One column in, where the sidebar's own text starts below it.
        let row = Rect::new(area.x + 1, area.y, area.width - 1, 1);
        Paragraph::new(APP_NAME)
            .style(Style::default().fg(self.theme.faded))
            .render(row, frame.buffer_mut());
    }
```

Replace `draw_tabs`:

```rust
    /// Draws the row of tabs above the grid.
    ///
    /// Each is its number and its first pane's title. The one on screen sits
    /// on a tint rather than being inverted: it should read as the one you
    /// are in, not as a warning.
    fn draw_tabs(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.height == 0 {
            return;
        }

        let current = self.current_tab();
        let tileable = self.tileable();
        let mut spans = Vec::new();

        for index in 0..self.tab_count() {
            let title = tileable
                .chunks(PANES_PER_TAB)
                .nth(index)
                .and_then(<[PaneId]>::first)
                .and_then(|id| self.state.pane(*id))
                .map(|pane| clip(&pane.title, TAB_TITLE));

            let label = match title {
                Some(title) => format!(" {} {title} ", index + 1),
                None => format!(" {} ", index + 1),
            };
            let style = if index == current {
                Style::default()
                    .bg(self.theme.tab)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.faded)
            };

            if index > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(label, style));
        }

        Paragraph::new(Line::from(spans)).render(area, frame.buffer_mut());
    }
```

and the helper beside `strip_mark`:

```rust
/// `text` cut to `width` characters, marking the cut with an ellipsis.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}
```

In `draw_status`, change the style block to:

```rust
        let style = if self.router.is_armed() {
            Style::default()
                .bg(self.theme.tab)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.theme.faded)
        };
```

Add to `impl Overlay`:

```rust
    /// Draws the overlay's frame in `style`.
    fn set_border(&mut self, style: Style) {
        match self {
            Overlay::Harness(picker)
            | Overlay::Project(picker)
            | Overlay::Register(picker)
            | Overlay::Machine(picker) => picker.set_border(style),
            Overlay::Browse(browser) => browser.set_border(style),
            Overlay::OpenOn { prompt, .. } => prompt.set_border(style),
            Overlay::AddMachine(add) => add.prompt_mut().set_border(style),
            // Built fresh each frame, with the theme's border already on it.
            Overlay::Approval { .. } => {}
        }
    }
```

At the start of `draw_overlay`, before `let Some(overlay) = &self.overlay`:

```rust
        let border = Style::default().fg(self.theme.faded);
        if let Some(overlay) = &mut self.overlay {
            overlay.set_border(border);
        }
```

In `approval_widget`, add `border: Style::default().fg(self.theme.faded),` to the `Approval { .. }` literal.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: all pass. If a test elsewhere in `app.rs` asserts on a row number counted from the top of the frame, it is now one row lower — shift it by one and say so in a comment the way the click tests do.

- [ ] **Step 7: See it**

Run: `cargo build -p dispatch && (sleep 2; printf '\x01q') | timeout 5 script -qfc "stty cols 120 rows 30; ./target/debug/dispatch" /dev/null | cat -v | head -c 4000`
Expected: the start of the output holds the query `^[]10;?^G^[]11;?^G^[]4;5;?^G^[[c`, and the drawn text contains `D I S P A T C H` and `┌ Projects`. The terminal behaves normally afterwards.

- [ ] **Step 8: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-os crates/dispatch-tui dispatch docs/superpowers/specs/2026-09-24-visual-refresh-design.md
git commit -m "feat(dispatch): name in the corner, square borders, tinted tabs in the terminal's colours

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Sidebar rows that do not collide

**Files:**
- Modify: `crates/dispatch-tui/src/sidebar.rs`
- Modify: `crates/dispatch-tui/src/sidebar/tests.rs`
- Modify: `dispatch/src/app.rs` (pass the theme; one click test)

**Interfaces:**
- Consumes: `Theme` (Task 7), `App::theme` (Task 8).
- Produces: `sidebar::WIDTH == 34`; `Sidebar::with_theme(self, Theme) -> Self`. Column layout from a row's start: twisty `+0`, icon `+2`, text `+4` (`NAME`). A top-level pane's row starts 4 in from its project's; a subagent's 2 further. The state glyph sits two columns in from the frame's right edge. `OPEN_FOLDER` and `FOCUS` are gone; `SHUT_FOLDER` is the plain-directory mark.

- [ ] **Step 1: Update and add the tests**

In `sidebar/tests.rs`, replace these tests wholesale:

```rust
#[test]
fn the_focused_pane_is_tinted() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");
    spawn(&mut state, alpha, "codex");

    // Spawning focuses the new pane, so codex is focused.
    let buf = render(&state, WIDTH, 10);
    let tint = Theme::fallback().tint;

    assert_ne!(
        buf.cell((LEFT + 8, TOP + 1)).expect("cell exists").bg,
        tint,
        "claude is not focused"
    );
    for x in LEFT + 4..WIDTH - 1 {
        assert_eq!(
            buf.cell((x, TOP + 2)).expect("cell exists").bg,
            tint,
            "column {x} of the focused row is tinted"
        );
    }
    assert!(!row_text(&buf, TOP + 2).contains('▌'), "and not marked with a bar");
}

#[test]
fn the_selected_projects_whole_row_is_tinted() {
    // Emphasis on the name alone is easy to miss in a list of directory names
    // that already look alike. The tint runs the width of the list so the eye
    // finds it without reading.
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);
    let tint = Theme::fallback().tint;

    for x in LEFT..WIDTH - 1 {
        let cell = buf.cell((x, TOP)).expect("cell exists");
        assert_eq!(cell.bg, tint, "column {x} of the selected row is tinted");
        assert!(!cell.modifier.contains(Modifier::REVERSED), "and not inverted");
    }

    assert_ne!(
        buf.cell((LEFT, TOP + 1)).expect("cell exists").bg,
        tint,
        "an unselected project carries no tint"
    );
}

#[test]
fn the_frame_is_not_painted_by_the_highlight() {
    let (state, _, _) = state();
    let buf = render(&state, WIDTH, 6);

    assert_ne!(
        buf.cell((0, TOP)).expect("cell exists").bg,
        Theme::fallback().tint,
        "the tint stops at the frame"
    );
}

#[test]
fn every_icon_has_a_blank_column_after_it() {
    // A Nerd Font glyph is routinely drawn wider than its cell; with nothing
    // after it, it runs into the next glyph or the first letter of the name.
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let buf = render(&state, WIDTH, 6);
    let blank = |x: u16, y: u16| buf.cell((x, y)).expect("cell exists").symbol() == " ";

    // The project: twisty, blank, folder, blank, name.
    assert!(blank(LEFT + 1, TOP) && blank(LEFT + 3, TOP));
    // The pane: twisty, blank, harness icon, blank, title.
    assert!(blank(LEFT + 5, TOP + 1) && blank(LEFT + 7, TOP + 1));
}

#[test]
fn the_state_glyph_keeps_a_blank_between_it_and_the_frame() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        buf.cell((WIDTH - 2, TOP + 1)).expect("cell exists").symbol(),
        " "
    );
    assert_eq!(
        buf.cell((WIDTH - 1, TOP + 1)).expect("cell exists").symbol(),
        "│"
    );
}

#[test]
fn a_wide_name_is_cut_by_the_columns_it_takes() {
    // Four CJK characters take eight columns; counting them as four would
    // let the row run past the frame.
    let mut state = AppState::new();
    state.add_project(
        Project::new("/tmp/x", ProjectSource::LocalDir).with_name("日本語のプロジェクト名前"),
    );

    let buf = render(&state, 20, 5);
    let line = row_text(&buf, TOP);

    assert!(line.ends_with('│'), "the frame is not overwritten: {line:?}");
    assert!(line.contains('…'), "the cut is visible: {line:?}");
}

#[test]
fn a_plain_directory_is_a_folder_and_a_repository_is_marked_as_one() {
    let mut state = AppState::new();
    state.add_project(Project::new("/tmp/plain", ProjectSource::LocalDir));
    state.add_project(Project::new(
        "/tmp/repo",
        ProjectSource::GitRepo { remote: None },
    ));

    let buf = render(&state, WIDTH, 6);

    assert_eq!(
        buf.cell((LEFT + 2, TOP)).expect("cell exists").symbol(),
        SHUT_FOLDER
    );
    assert_eq!(
        buf.cell((LEFT + 2, TOP + 1)).expect("cell exists").symbol(),
        REPOSITORY
    );
}
```

Delete `the_focused_pane_is_marked`, `the_selected_projects_whole_row_is_highlighted`, `the_focus_marker_is_not_a_twisty`, `a_project_folder_is_open_while_its_panes_are_shown`, `a_project_with_nothing_in_it_is_a_shut_folder` and `a_repository_carries_a_git_mark_beside_its_folder` (each is replaced above).

Change these in place:
- `a_pane_with_children_carries_a_twisty_and_one_without_does_not`: `LEFT + 2, TOP + 1` → `LEFT + 4, TOP + 1`.
- `a_click_on_a_panes_twisty_toggles_it_rather_than_focusing_it`: `LEFT + 2` → `LEFT + 4`, `LEFT + 3` → `LEFT + 5`; comment "The twisty sits in the row's first column, four in from the list's edge."
- `a_click_on_a_childless_panes_twisty_column_focuses_it` and `a_tombstones_twisty_still_toggles`: `LEFT + 2` → `LEFT + 4`.
- `a_click_on_a_closed_panes_tombstone_finds_nothing`: `LEFT + 4` → `LEFT + 8` (column 4 is now its twisty, which still folds).
- `state_cell`: `buf.area.width - 2` → `buf.area.width - 3`, doc "Two columns in from the frame, where a pane's state is drawn."
- `a_tombstone_says_it_is_closed`: expected colour `Color::DarkGray` → `Theme::fallback().faded`.
- `a_pane_is_marked_with_the_icon_of_its_harness` and `a_pane_whose_harness_is_unregistered_is_marked_generically`: `LEFT + 2 + 2` → `LEFT + 4 + 2`, comment "Two columns in from the row's own start, past its twisty and a blank."

Add `use crate::theme::Theme;` at the top of the test file.

In `dispatch/src/app.rs`, `a_click_on_a_panes_twisty_folds_its_children_without_focusing_it`: `click(&mut app, 3, 3)` → `click(&mut app, 5, 3)`, comment "A pane row starts four columns inside the frame, and its twisty is the first of them."

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui sidebar::`
Expected: the new and changed tests fail (columns, tint), others pass.

- [ ] **Step 3: Implement**

In `sidebar.rs`:

- `WIDTH`:

```rust
/// Width the sidebar asks for.
///
/// Every icon is followed by a blank column, because a Nerd Font glyph drawn
/// wider than its cell otherwise runs into whatever is next to it. Those gaps
/// cost two columns, and the sidebar is two wider so titles keep their room.
pub const WIDTH: u16 = 34;
```

- Delete `OPEN_FOLDER`, `FOCUS` and `folder_icon`. Change `SHUT_FOLDER`'s doc to "A plain directory's mark." and `source_icon` to:

```rust
/// A project's mark: git for a repository, a folder for a plain directory.
///
/// One mark rather than a folder with a git mark beside it: the twisty
/// already says whether the folder is open, and a second glyph there was one
/// more thing to collide.
fn source_icon(source: &ProjectSource) -> &'static str {
    match source {
        ProjectSource::LocalDir => SHUT_FOLDER,
        ProjectSource::GitRepo { .. } => REPOSITORY,
    }
}
```

- `NAME` doc: "How far a row's text sits from its start: twisty, blank, icon, blank."

- Imports: add `use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};` and `use crate::theme::Theme;`.

- `Sidebar` gains `theme: Theme` (`Theme::fallback()` in `new`) and:

```rust
    /// Draws in `theme` rather than the built-in one.
    #[must_use]
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }
```

- Replace `write` and `truncate`, and add `fill`:

```rust
/// Writes `text` at `(x, y)`, clipped to `area`, and returns the next column.
///
/// Measured in display columns rather than characters, so a wide character
/// takes the two cells it is drawn in rather than pushing the rest of the row
/// out of line.
fn write(buf: &mut Buffer, area: Rect, x: u16, y: u16, text: &str, style: Style) -> u16 {
    let right = area.x + area.width;
    if x >= right || y < area.y || y >= area.y + area.height {
        return x;
    }

    buf.set_stringn(x, y, text, usize::from(right - x), style).0
}

/// Paints the row from `x` to the list's right edge in `style`, under
/// whatever is written on it afterwards.
fn fill(buf: &mut Buffer, area: Rect, x: u16, y: u16, style: Style) {
    for column in x..area.x + area.width {
        if let Some(cell) = buf.cell_mut((column, y)) {
            cell.set_style(style);
        }
    }
}

/// Truncates `text` to `width` display columns, marking the cut with an
/// ellipsis.
///
/// Project names come from directory names and are routinely longer than the
/// sidebar.
fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }

    let mut kept = String::new();
    let mut used = 0;
    for character in text.chars() {
        let columns = character.width().unwrap_or(0);
        if used + columns > width - 1 {
            break;
        }
        kept.push(character);
        used += columns;
    }
    kept.push('…');
    kept
}
```

- `state_glyph` takes `theme: &Theme`; the tombstone and `Exited(0)` arms use `Style::default().fg(theme.faded)` in place of `Color::DarkGray`.

- In `rows`, top-level panes are pushed at `step + 4` and children at `step + 6`; update the comment on indentation accordingly.

- Replace `render_project`, `render_device` and `render_pane`'s bodies:

```rust
    fn render_project(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        id: ProjectId,
        indent: u16,
        selected: Option<ProjectId>,
    ) {
        let project = self
            .state
            .projects()
            .iter()
            .find(|p| p.id == id)
            .expect("the caller iterates over registered projects");

        let x = area.x + indent;
        let is_selected = selected == Some(id);
        let style = if is_selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        // The tint runs the full width of the list rather than the width of
        // the name: a highlight that stops where a short name does reads as
        // part of the name. It starts at the row's own indent, so a project
        // under a machine does not paint over that machine's row.
        if is_selected {
            fill(buf, area, x, y, Style::default().bg(self.theme.tint));
        }

        let has_panes = self
            .state
            .panes_for(id)
            .iter()
            .any(|pane| pane.parent.is_none());
        let collapsed = self.state.is_project_collapsed(id);

        write(buf, area, x, y, twisty(has_panes, collapsed), style);
        write(buf, area, x + 2, y, source_icon(&project.source), style);

        let name_x = x + NAME;
        let room = (area.x + area.width).saturating_sub(name_x) as usize;
        write(buf, area, name_x, y, &truncate(&project.name, room), style);
    }
```

In `render_device`, the unreachable style becomes `Style::default().fg(self.theme.faded)`, the `MACHINE` mark is written at `area.x + 2`, and the name room is measured with `.width()` (`suffix.width()` in place of `suffix.chars().count()`).

```rust
    fn render_pane(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        pane: &Pane,
        focused: Option<PaneId>,
        indent: u16,
    ) {
        // A tombstone has no process behind it, so it can never be the row
        // the user is focused on.
        let is_focused = !pane.closed && focused == Some(pane.id);
        let style = if is_focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else if pane.closed {
            Style::default().fg(self.theme.faded)
        } else {
            Style::default()
        };

        let x = area.x + indent;
        if is_focused {
            fill(buf, area, x, y, Style::default().bg(self.theme.tint));
        }

        let has_children = !self.state.children_of(pane.id).is_empty();
        let twisty = twisty(has_children, self.state.is_pane_collapsed(pane.id));

        write(buf, area, x, y, twisty, style);
        write(buf, area, x + 2, y, self.icon(pane), style);

        // Two columns in from the frame, so the blank beside it keeps a glyph
        // drawn wider than its cell off the border. The title stops a blank
        // short of it.
        let (glyph, glyph_style) = state_glyph(pane, &self.theme);
        let state_x = (area.x + area.width).saturating_sub(2);

        let title_x = x + NAME;
        let room = state_x.saturating_sub(title_x + 1) as usize;
        write(buf, area, title_x, y, &truncate(&pane.title, room), style);

        write(buf, area, state_x, y, glyph, glyph_style);
    }
```

- In `render`, draw the frame in `faded`: `block().border_style(Style::default().fg(self.theme.faded)).render(area, buf);`

In `dispatch/src/app.rs`, the sidebar call in `draw` becomes:

```rust
        frame.render_widget(
            Sidebar::new(&self.state)
                .with_harnesses(&self.harnesses)
                .with_theme(self.theme),
            sidebar_area,
        );
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: all pass. `crates/dispatch-tui/src/browser.rs` still imports `REPOSITORY` and `SHUT_FOLDER`, which both remain.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui/src/sidebar.rs crates/dispatch-tui/src/sidebar dispatch/src/app.rs
git commit -m "fix(tui): give every sidebar icon room, and tint the selection

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Branch rows and branch groups in the sidebar

**Files:**
- Modify: `crates/dispatch-tui/src/sidebar.rs` (`Row`, `rows`, `render`, `hit_test`, new `render_branch`)
- Modify: `crates/dispatch-tui/src/sidebar/tests.rs`
- Modify: `docs/superpowers/specs/2026-09-24-visual-refresh-design.md` (folding)

**Interfaces:**
- Consumes: `Project::branch`, `Pane::branch`, `Project::with_branch` (Task 3); `Theme::faded`.
- Produces: `Row<'a>` gains `Branch(ProjectId, &'a str, u16)`; `rows(state: &AppState) -> Vec<Row<'_>>`. A click on a branch row answers `Hit::Project`.

- [ ] **Step 1: Write the failing tests**

Append to `sidebar/tests.rs`:

```rust
/// A repository on `main`, selected, with nothing in it yet.
fn repository() -> (AppState, ProjectId) {
    let mut state = AppState::new();
    let project = state.add_project(
        Project::new("/tmp/repo", ProjectSource::GitRepo { remote: None })
            .with_branch(Some("main".into())),
    );
    (state, project)
}

/// A pane of `harness` in `project`, on `branch`, titled `title`.
fn pane_on(
    state: &mut AppState,
    project: ProjectId,
    title: &str,
    branch: Option<&str>,
) -> PaneId {
    let pane = spawn(state, project, "claude");
    state.set_pane_title(pane, title).expect("the pane exists");
    state
        .set_pane_branch(pane, branch.map(str::to_string))
        .expect("the pane exists");
    pane
}

#[test]
fn a_projects_branch_is_drawn_faded_beneath_its_name() {
    let (state, _) = repository();
    let buf = render(&state, WIDTH, 6);

    let line = row_text(&buf, TOP + 1);
    assert_eq!(column_of(&line, "main"), usize::from(LEFT + NAME), "{line:?}");
    assert_eq!(
        buf.cell((LEFT + NAME, TOP + 1)).expect("cell exists").fg,
        Theme::fallback().faded
    );
}

#[test]
fn panes_on_the_projects_branch_or_none_sit_beneath_it() {
    let (mut state, project) = repository();
    pane_on(&mut state, project, "on-main", Some("main"));
    pane_on(&mut state, project, "unknown", None);

    let lines = render_lines(&state, WIDTH, 8);

    assert!(lines[TOP as usize + 1].contains("main"), "{lines:#?}");
    assert!(lines[TOP as usize + 2].contains("on-main"), "{lines:#?}");
    assert!(lines[TOP as usize + 3].contains("unknown"), "{lines:#?}");
}

#[test]
fn panes_on_another_branch_are_gathered_under_it() {
    let (mut state, project) = repository();
    pane_on(&mut state, project, "tabs-one", Some("feat/tabs"));
    pane_on(&mut state, project, "on-main", Some("main"));
    pane_on(&mut state, project, "tabs-two", Some("feat/tabs"));

    let lines = render_lines(&state, WIDTH, 10);
    let at = |text: &str| {
        lines
            .iter()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("{text:?} is drawn: {lines:#?}"))
    };

    assert!(at("main") < at("on-main"));
    assert!(at("on-main") < at("feat/tabs"), "{lines:#?}");
    assert_eq!(at("tabs-one"), at("feat/tabs") + 1, "{lines:#?}");
    assert_eq!(at("tabs-two"), at("feat/tabs") + 2, "{lines:#?}");
    assert_eq!(
        column_of(&lines[at("feat/tabs")], "feat/tabs"),
        usize::from(LEFT + NAME),
        "every branch line sits where the project's does"
    );
}

#[test]
fn a_subagent_stays_under_its_parent_whatever_its_branch() {
    let (mut state, project) = repository();
    let parent = pane_on(&mut state, project, "parent", Some("main"));
    let mut child = Pane::new(project, HarnessId::new("claude"));
    child.parent = Some(parent);
    child.title = "child".into();
    child.branch = Some("feat/elsewhere".into());
    state.adopt_pane(child).expect("the project exists");

    let lines = render_lines(&state, WIDTH, 8);
    let parent_row = lines.iter().position(|l| l.contains("parent")).expect("drawn");

    assert!(lines[parent_row + 1].contains("child"), "{lines:#?}");
    assert!(
        !lines.iter().any(|line| line.contains("feat/elsewhere")),
        "no group is opened for a subagent: {lines:#?}"
    );
}

#[test]
fn a_project_without_a_branch_draws_no_branch_row() {
    let (mut state, alpha, _) = state();
    spawn(&mut state, alpha, "claude");

    let lines = render_lines(&state, WIDTH, 6);

    assert!(lines[TOP as usize + 1].contains("claude"), "{lines:#?}");
}

#[test]
fn a_folded_project_keeps_its_own_branch_and_hides_the_rest() {
    let (mut state, project) = repository();
    pane_on(&mut state, project, "on-main", Some("main"));
    pane_on(&mut state, project, "tabs", Some("feat/tabs"));

    state.toggle_project_collapsed(project);
    let text = render_lines(&state, WIDTH, 8).join("\n");

    assert!(text.contains("main"), "{text}");
    assert!(!text.contains("on-main") && !text.contains("feat/tabs"), "{text}");
}

#[test]
fn a_click_on_a_branch_row_is_a_click_on_its_project() {
    let (state, project) = repository();
    let area = Rect::new(0, 0, WIDTH, 6);

    assert_eq!(
        hit_test(&state, area, LEFT + NAME, TOP + 1),
        Some(Hit::Project(project))
    );
}

#[test]
fn a_long_branch_name_is_cut_short_inside_the_frame() {
    let mut state = AppState::new();
    state.add_project(
        Project::new("/tmp/repo", ProjectSource::GitRepo { remote: None })
            .with_branch(Some("feature/an-extremely-long-branch-name-for-testing".into())),
    );

    let buf = render(&state, WIDTH, 5);
    let line = row_text(&buf, TOP + 1);

    assert!(line.ends_with('│'), "the frame is not overwritten: {line:?}");
    assert!(line.contains('…'), "the cut is visible: {line:?}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui sidebar::`
Expected: the new tests fail — no branch rows are drawn.

- [ ] **Step 3: Implement**

`Row` becomes:

```rust
/// One line of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row<'a> {
    /// A machine, when there is more than one.
    Device(DeviceId),
    /// A project heading, drawn `indent` columns in from the list's edge.
    Project(ProjectId, u16),
    /// A branch within a project: the project's own, or one some of its panes
    /// are working on. Drawn `indent` columns in, like its project.
    Branch(ProjectId, &'a str, u16),
    /// A pane, drawn `indent` columns in from the list's edge.
    Pane(PaneId, u16),
}
```

Replace the project loop inside `rows` (the `for project in state.projects() { .. }` block) with:

```rust
        for project in state.projects() {
            if device.is_some_and(|device| project.device != device) {
                continue;
            }

            rows.push(Row::Project(project.id, step));

            let collapsed = state.is_project_collapsed(project.id);
            let top: Vec<&Pane> = state
                .panes_for(project.id)
                .into_iter()
                .filter(|pane| pane.parent.is_none())
                .collect();

            let Some(own) = project.branch.as_deref() else {
                if !collapsed {
                    for pane in &top {
                        push_pane(state, &mut rows, pane, step + 4);
                    }
                }
                continue;
            };

            // The project's own branch stays when the project is folded: it
            // is part of what the project is, not one of the things folding
            // hides.
            rows.push(Row::Branch(project.id, own, step));
            if collapsed {
                continue;
            }

            // A pane whose branch is not known yet is shown with the project
            // rather than held back until it is.
            for pane in top
                .iter()
                .filter(|pane| pane.branch.as_deref().is_none_or(|branch| branch == own))
            {
                push_pane(state, &mut rows, pane, step + 4);
            }

            // Every other branch, in the order its first pane was opened.
            let mut others: Vec<&str> = Vec::new();
            for pane in &top {
                if let Some(branch) = pane.branch.as_deref()
                    && branch != own
                    && !others.contains(&branch)
                {
                    others.push(branch);
                }
            }

            for branch in others {
                rows.push(Row::Branch(project.id, branch, step));
                for pane in top
                    .iter()
                    .filter(|pane| pane.branch.as_deref() == Some(branch))
                {
                    push_pane(state, &mut rows, pane, step + 4);
                }
            }
        }
```

with the signature `fn rows(state: &AppState) -> Vec<Row<'_>>` and, after it:

```rust
/// A top-level pane's row, and its subagents' beneath it unless it is
/// folded.
///
/// The subagents follow their parent whatever branch they are on: the tree
/// is what the sidebar is, and grouping is for the panes at its top.
fn push_pane<'a>(state: &'a AppState, rows: &mut Vec<Row<'a>>, pane: &Pane, indent: u16) {
    rows.push(Row::Pane(pane.id, indent));

    if state.is_pane_collapsed(pane.id) {
        return;
    }
    for child in state.children_of(pane.id) {
        rows.push(Row::Pane(child.id, indent + 2));
    }
}
```

In `render`'s `match row`, add:

```rust
                Row::Branch(_, branch, indent) => self.render_branch(buf, area, y, branch, indent),
```

and the method:

```rust
    /// Draws a branch line, faded, where its project's name starts.
    fn render_branch(&self, buf: &mut Buffer, area: Rect, y: u16, branch: &str, indent: u16) {
        let x = area.x + indent + NAME;
        let room = (area.x + area.width).saturating_sub(x) as usize;

        write(
            buf,
            area,
            x,
            y,
            &truncate(branch, room),
            Style::default().fg(self.theme.faded),
        );
    }
```

In `hit_test`'s match, add `Row::Branch(id, _, _) => Some(Hit::Project(id)),`.

In the spec's "Branch groups" section, after the numbered list, add: "Folding a project hides its panes and every other branch's line, but keeps its own branch line: that line is part of what the project is."

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-tui && cargo test -p dispatch`
Expected: all pass.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui/src/sidebar.rs crates/dispatch-tui/src/sidebar docs/superpowers/specs/2026-09-24-visual-refresh-design.md
git commit -m "feat(tui): show each project's branch and group panes by theirs

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: A sidebar section per machine

**Files:**
- Modify: `crates/dispatch-tui/src/sidebar.rs` (replace `Row::Device`, `rows`, `render`, `hit_test`; add `Scroll`, `Section`, `sections`, `section`, `section_heights`, `open_panes`, `divider`, `render_label`, `hit_row`, `with_scroll`)
- Modify: `crates/dispatch-tui/src/sidebar/tests.rs`
- Modify: `dispatch/src/app.rs` (`sidebar_scroll` field; pass it to `Sidebar` and `hit_test`)

**Interfaces:**
- Produces: `pub type Scroll = HashMap<DeviceId, u16>`; `Sidebar::with_scroll(self, &'a Scroll) -> Self`; `hit_test(state: &AppState, area: Rect, scroll: &Scroll, x: u16, y: u16) -> Option<Hit>`; private `sections(state, area, scroll) -> Vec<Section<'_>>` with `Section { key: DeviceId, device: Option<DeviceId>, header: u16, body: Rect, rows: Vec<Row<'_>>, offset: usize }` and `Section::visible()`, `Section::below()`; private `section_heights(weights: &[u32], folded: &[bool], height: u16) -> Vec<u16>`. `Row` is now `Project(ProjectId)`, `Branch(ProjectId, &str)`, `Pane(PaneId, u16)`. `App::sidebar_scroll: sidebar::Scroll` exists (always empty until Task 12).

- [ ] **Step 1: Write the failing tests**

In `sidebar/tests.rs`, add a helper and replace every `hit_test(&state, area, ` with `hit(&state, area, ` (e.g. `sed -i 's/hit_test(&state, area, /hit(\&state, area, /g' crates/dispatch-tui/src/sidebar/tests.rs`):

```rust
/// What a click at `(x, y)` finds, with nothing scrolled.
fn hit(state: &AppState, area: Rect, x: u16, y: u16) -> Option<Hit> {
    hit_test(state, area, &Scroll::new(), x, y)
}
```

Delete `several_machines_each_get_a_row_above_their_projects`, `a_collapsed_device_hides_its_projects` and `a_click_on_a_device_row_finds_the_device`, and add:

```rust
#[test]
fn several_machines_each_get_a_section_named_on_the_line_above_it() {
    // Ten rows: frame 0 and 9, one divider, seven shared between two equal
    // weights — two each, then three split 2/1 by largest remainder.
    let (state, _, _) = fleet();
    let lines = render_lines(&state, WIDTH, 10);

    assert!(lines[0].starts_with('┌') && lines[0].contains("laptop"), "{lines:#?}");
    assert!(lines[1].contains("alpha"), "{lines:#?}");
    assert!(
        lines[5].starts_with('├') && lines[5].contains("tower") && lines[5].ends_with('┤'),
        "{lines:#?}"
    );
    assert!(lines[6].contains("beta"), "{lines:#?}");
    assert!(lines[9].starts_with('└'), "{lines:#?}");
}

#[test]
fn a_machine_with_more_open_panes_gets_more_of_the_height() {
    let (mut state, laptop, _) = fleet();
    let alpha = state
        .projects()
        .iter()
        .find(|project| project.device == laptop)
        .expect("the laptop has a project")
        .id;
    for _ in 0..3 {
        spawn(&mut state, alpha, "claude");
    }

    // Twenty rows: eighteen inside, one divider, seventeen shared by weights
    // four and one — two each, then thirteen split 10.4/2.6, the spare row
    // going to the larger remainder: twelve and five.
    let lines = render_lines(&state, WIDTH, 20);
    let divider = lines
        .iter()
        .position(|line| line.contains("tower"))
        .expect("the second machine is named");

    assert_eq!(divider, 13, "{lines:#?}");
}

#[test]
fn a_folded_machine_keeps_only_the_line_naming_it() {
    let (mut state, laptop, _) = fleet();

    state.toggle_device_collapsed(laptop);
    let lines = render_lines(&state, WIDTH, 10);
    let text = lines.join("\n");

    assert!(!text.contains("alpha"), "{text}");
    assert!(lines[0].contains("laptop"), "the machine stays: {text}");
    assert!(lines[1].contains("tower"), "its section is only that line: {text}");
    assert!(text.contains("beta"), "the other machine is unaffected: {text}");
}

#[test]
fn a_click_on_a_machines_name_finds_the_machine() {
    let (state, laptop, tower) = fleet();
    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit(&state, area, 3, 0), Some(Hit::Device(laptop)));
    assert_eq!(hit(&state, area, 3, 5), Some(Hit::Device(tower)));
}

#[test]
fn one_machines_top_border_is_not_a_control() {
    let (state, _, _) = state();
    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit(&state, area, 3, 0), None);
}

#[test]
fn a_click_in_the_second_section_finds_its_rows() {
    let (state, _, _) = fleet();
    let beta = state
        .projects()
        .iter()
        .find(|project| project.name == "beta")
        .expect("beta is registered")
        .id;
    let area = Rect::new(0, 0, WIDTH, 10);

    assert_eq!(hit(&state, area, LEFT + 4, 6), Some(Hit::Project(beta)));
}

#[test]
fn several_machines_in_a_very_short_sidebar_still_draw_the_frame() {
    let mut state = AppState::new();
    for name in ["one", "two", "three"] {
        let device = state.add_device(Device::new(name));
        state.add_project(
            Project::new(format!("/tmp/{name}"), ProjectSource::LocalDir).with_device(device),
        );
    }

    for height in 2..6 {
        let lines = render_lines(&state, WIDTH, height);
        let last = &lines[usize::from(height) - 1];
        assert!(last.starts_with('└'), "height {height}: {lines:#?}");
    }
}

#[test]
fn heights_are_shared_by_weight_after_two_rows_each() {
    assert_eq!(section_heights(&[1, 1], &[false, false], 10), vec![5, 5]);
    assert_eq!(section_heights(&[3, 1], &[false, false], 12), vec![8, 4]);
    assert_eq!(section_heights(&[1, 1, 1], &[false; 3], 10), vec![4, 3, 3]);
}

#[test]
fn a_folded_section_gets_no_height() {
    assert_eq!(section_heights(&[1, 1], &[false, true], 10), vec![10, 0]);
    assert_eq!(section_heights(&[1, 1], &[true, true], 10), vec![0, 0]);
}

#[test]
fn too_little_height_is_handed_out_a_row_at_a_time_in_order() {
    assert_eq!(section_heights(&[1, 1], &[false, false], 3), vec![2, 1]);
    assert_eq!(section_heights(&[1, 1], &[false, false], 1), vec![1, 0]);
    assert_eq!(section_heights(&[1, 1], &[false, false], 0), vec![0, 0]);
}
```

Update in place:
- `an_unreachable_device_says_so`, `an_unreachable_device_with_a_long_name_still_says_so`, `a_pending_device_with_no_projects_still_draws_a_row` — they search for the name's line and still pass; rename the last to `a_pending_device_with_no_projects_still_gets_a_section`.

In `dispatch/src/app.rs`: add `sidebar_scroll: sidebar::Scroll,` to `App` (initialised `sidebar::Scroll::new()`), pass `.with_scroll(&self.sidebar_scroll)` on the `Sidebar` in `draw`, and give `hit_test` its new argument: `sidebar::hit_test(&self.state, self.sidebar_area, &self.sidebar_scroll, mouse.column, mouse.row)`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui sidebar::`
Expected: compile errors — `Scroll`, `section_heights`, the new `hit_test` signature.

- [ ] **Step 3: Implement**

In `sidebar.rs`, add `use std::collections::HashMap;` and replace the module's middle — `Row`, `rows`, `push_pane`, the `Widget` impl, `hit_test`, and `render_device` — with the following. `render_project` and `render_branch` lose their `indent` parameter (a project row always starts at the body's left edge now): replace `let x = area.x + indent;` with `let x = area.x;` and `area.x + indent + NAME` with `area.x + NAME`. The doc comment at the top of the module changes its second paragraph to: "With more than one machine, the list is split into a section per machine, each headed by the machine's name and scrolled on its own."

```rust
/// How far each machine's section is scrolled, in rows.
///
/// A machine missing from the map is at its top. Keyed by machine even when
/// the sidebar is one undivided list, so one code path serves both; that
/// list is keyed by its lone machine, or by the nil id before any machine has
/// registered.
pub type Scroll = HashMap<DeviceId, u16>;

/// One line of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row<'a> {
    /// A project heading.
    Project(ProjectId),
    /// A branch within a project: the project's own, or one some of its
    /// panes are working on.
    Branch(ProjectId, &'a str),
    /// A pane, drawn `indent` columns in from the list's edge.
    Pane(PaneId, u16),
}

/// Where a top-level pane's row starts: under its project's name.
const PANE: u16 = 4;

/// One machine's share of the sidebar.
struct Section<'a> {
    /// Which offset in [`Scroll`] this section reads.
    key: DeviceId,
    /// The machine named on the section's header, when the sidebar is split.
    device: Option<DeviceId>,
    /// The line naming it: the top border for the first, a divider after.
    header: u16,
    /// Where its rows are drawn.
    body: Rect,
    /// Every row it has, drawn or not.
    rows: Vec<Row<'a>>,
    /// How many of `rows` are scrolled away above the body.
    offset: usize,
}

impl<'a> Section<'a> {
    /// The rows actually drawn, with the line each is drawn on.
    fn visible(&self) -> impl Iterator<Item = (u16, &Row<'a>)> {
        (self.body.y..self.body.y + self.body.height).zip(self.rows.iter().skip(self.offset))
    }

    /// How many rows are hidden below the body.
    fn below(&self) -> usize {
        self.rows
            .len()
            .saturating_sub(self.offset + usize::from(self.body.height))
    }
}

/// Every section the sidebar at `area` is split into, top to bottom.
///
/// Rendering, hit testing and scrolling all walk this, so a click can only
/// ever land on a row that is actually drawn — which is what makes folding
/// and scrolling safe: a hidden row is missing from all of them at once.
fn sections<'a>(state: &'a AppState, area: Rect, scroll: &Scroll) -> Vec<Section<'a>> {
    let inner = inner(area);
    let devices = state.devices();

    // One machine is one list, with no name on it: a line naming this
    // machine would say what the user already knows.
    if devices.len() <= 1 {
        let key = devices.first().map_or_else(DeviceId::nil, |device| device.id);
        return vec![section(key, None, area.y, inner, rows(state, None), scroll)];
    }

    let dividers = u16::try_from(devices.len() - 1).unwrap_or(u16::MAX);
    let weights: Vec<u32> = devices
        .iter()
        .map(|device| open_panes(state, device.id).saturating_add(1))
        .collect();
    let folded: Vec<bool> = devices
        .iter()
        .map(|device| state.is_device_collapsed(device.id))
        .collect();
    let heights = section_heights(&weights, &folded, inner.height.saturating_sub(dividers));

    let mut sections = Vec::new();
    let mut y = inner.y;

    for (index, (device, height)) in devices.iter().zip(heights).enumerate() {
        let header = if index == 0 {
            area.y
        } else {
            // A divider that would land on the bottom border has no room.
            if y >= inner.y + inner.height {
                break;
            }
            y += 1;
            y - 1
        };

        let body = Rect::new(inner.x, y, inner.width, height);
        y += height;

        let rows = if folded[index] {
            Vec::new()
        } else {
            rows(state, Some(device.id))
        };
        sections.push(section(device.id, Some(device.id), header, body, rows, scroll));
    }

    sections
}

/// A section, its offset brought back inside its rows.
fn section<'a>(
    key: DeviceId,
    device: Option<DeviceId>,
    header: u16,
    body: Rect,
    rows: Vec<Row<'a>>,
    scroll: &Scroll,
) -> Section<'a> {
    let furthest = rows.len().saturating_sub(usize::from(body.height));
    let offset = usize::from(scroll.get(&key).copied().unwrap_or(0)).min(furthest);

    Section {
        key,
        device,
        header,
        body,
        rows,
        offset,
    }
}

/// How many panes `device` has open at the top level: what its section's
/// share of the height is weighed by.
fn open_panes(state: &AppState, device: DeviceId) -> u32 {
    let count = state
        .projects()
        .iter()
        .filter(|project| project.device == device)
        .flat_map(|project| state.panes_for(project.id))
        .filter(|pane| pane.parent.is_none() && !pane.closed)
        .count();

    u32::try_from(count).unwrap_or(u32::MAX)
}

/// How many rows each section's body gets, out of `height`.
///
/// Folded sections get none. Every other gets two when there is room for
/// that, one when there is not, and otherwise one each to as many as fit, in
/// order. What is left is shared in proportion to `weights`, the rows lost
/// to rounding going to the largest remainders, the higher section first on
/// a tie. Always sums to `height` when anything is unfolded, so the sections
/// fill the frame rather than leaving the blank at the bottom.
fn section_heights(weights: &[u32], folded: &[bool], height: u16) -> Vec<u16> {
    let mut heights = vec![0_u16; weights.len()];
    let open: Vec<usize> = (0..weights.len())
        .filter(|&index| !folded.get(index).copied().unwrap_or(false))
        .collect();
    if open.is_empty() {
        return heights;
    }

    let count = u16::try_from(open.len()).unwrap_or(u16::MAX);
    let floor = if height >= count.saturating_mul(2) {
        2
    } else if height >= count {
        1
    } else {
        0
    };

    if floor == 0 {
        for &index in open.iter().take(usize::from(height)) {
            heights[index] = 1;
        }
        return heights;
    }

    for &index in &open {
        heights[index] = floor;
    }

    let spare = u64::from(height - floor * count);
    let total: u64 = open
        .iter()
        .map(|&index| u64::from(weights[index].max(1)))
        .sum();

    let mut given = 0;
    let mut remainders = Vec::with_capacity(open.len());
    for &index in &open {
        let share = spare * u64::from(weights[index].max(1));
        let whole = share / total;
        heights[index] += u16::try_from(whole).unwrap_or(u16::MAX);
        given += whole;
        remainders.push((share % total, index));
    }

    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let leftover = usize::try_from(spare - given).unwrap_or(0);
    for &(_, index) in remainders.iter().take(leftover) {
        heights[index] += 1;
    }

    heights
}

/// Every row of one machine's projects, or of every project when `device` is
/// `None`, top to bottom.
///
/// One level of children, deliberately: at the default `max_depth` of 1 a
/// subagent cannot delegate, so one level is the whole tree. Raise that cap
/// and a subagent's own subagent is tracked in state — the daemon owns it,
/// `^a s` can focus it, and closing its ancestors will not delete it — but it
/// has no row here. Drawing an arbitrary depth in a column this narrow needs
/// a shape nobody has designed yet, so the honest thing is to say where the
/// drawing stops rather than imply it does not.
fn rows(state: &AppState, device: Option<DeviceId>) -> Vec<Row<'_>> {
    let mut rows = Vec::new();

    for project in state.projects() {
        if device.is_some_and(|device| project.device != device) {
            continue;
        }

        rows.push(Row::Project(project.id));

        let collapsed = state.is_project_collapsed(project.id);
        let top: Vec<&Pane> = state
            .panes_for(project.id)
            .into_iter()
            .filter(|pane| pane.parent.is_none())
            .collect();

        let Some(own) = project.branch.as_deref() else {
            if !collapsed {
                for pane in &top {
                    push_pane(state, &mut rows, pane, PANE);
                }
            }
            continue;
        };

        // The project's own branch stays when the project is folded: it is
        // part of what the project is, not one of the things folding hides.
        rows.push(Row::Branch(project.id, own));
        if collapsed {
            continue;
        }

        // A pane whose branch is not known yet is shown with the project
        // rather than held back until it is.
        for pane in top
            .iter()
            .filter(|pane| pane.branch.as_deref().is_none_or(|branch| branch == own))
        {
            push_pane(state, &mut rows, pane, PANE);
        }

        // Every other branch, in the order its first pane was opened.
        let mut others: Vec<&str> = Vec::new();
        for pane in &top {
            if let Some(branch) = pane.branch.as_deref()
                && branch != own
                && !others.contains(&branch)
            {
                others.push(branch);
            }
        }

        for branch in others {
            rows.push(Row::Branch(project.id, branch));
            for pane in top
                .iter()
                .filter(|pane| pane.branch.as_deref() == Some(branch))
            {
                push_pane(state, &mut rows, pane, PANE);
            }
        }
    }

    rows
}

/// A top-level pane's row, and its subagents' beneath it unless it is
/// folded.
///
/// The subagents follow their parent whatever branch they are on: the tree
/// is what the sidebar is, and grouping is for the panes at its top.
fn push_pane<'a>(state: &'a AppState, rows: &mut Vec<Row<'a>>, pane: &Pane, indent: u16) {
    rows.push(Row::Pane(pane.id, indent));

    if state.is_pane_collapsed(pane.id) {
        return;
    }
    for child in state.children_of(pane.id) {
        rows.push(Row::Pane(child.id, indent + 2));
    }
}

/// Draws the line between two machines' sections across the frame.
fn divider(buf: &mut Buffer, area: Rect, y: u16, style: Style) {
    let right = area.x + area.width - 1;

    for x in area.x..=right {
        let symbol = if x == area.x {
            "├"
        } else if x == right {
            "┤"
        } else {
            "─"
        };
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(symbol);
            cell.set_style(style);
        }
    }
}

impl Widget for Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let edge = Style::default().fg(self.theme.faded);
        Block::bordered().border_style(edge).render(area, buf);

        let empty = Scroll::new();
        let sections = sections(self.state, area, self.scroll.unwrap_or(&empty));
        let selected = self.state.selected_project();
        let focused = self.state.focused_pane();

        for (index, section) in sections.iter().enumerate() {
            if index > 0 {
                divider(buf, area, section.header, edge);
            }
            self.render_label(buf, area, section);

            for (y, row) in section.visible() {
                match *row {
                    Row::Project(id) => self.render_project(buf, section.body, y, id, selected),
                    Row::Branch(_, branch) => self.render_branch(buf, section.body, y, branch),
                    Row::Pane(id, indent) => {
                        let pane = self
                            .state
                            .pane(id)
                            .expect("every row names a pane that is still in state");
                        self.render_pane(buf, section.body, y, pane, focused, indent);
                    }
                }
            }
        }
    }
}

/// What, if anything, sits at `(x, y)` in a sidebar drawn at `area`,
/// scrolled by `scroll`.
///
/// The sidebar has no keyboard focus of its own, so a pointer is the only way
/// to pick one row out of the list; this is what lets a click reach a pane
/// the tiled grid does not currently show. A closed pane's tombstone row
/// answers `Hit::Twisty` over its twisty and nothing elsewhere: there is no
/// process left to focus, but the children it is still holding can be folded
/// away. A machine's name line answers for the machine along its whole
/// length, frame included — which, for the first machine, is the top border.
#[must_use]
pub fn hit_test(state: &AppState, area: Rect, scroll: &Scroll, x: u16, y: u16) -> Option<Hit> {
    if x < area.x || x >= area.x + area.width || y < area.y || y >= area.y + area.height {
        return None;
    }

    let inner = inner(area);

    for section in sections(state, area, scroll) {
        if let Some(device) = section.device
            && y == section.header
        {
            return Some(Hit::Device(device));
        }

        if x < inner.x || x >= inner.x + inner.width {
            continue;
        }

        if let Some((_, row)) = section.visible().find(|(row_y, _)| *row_y == y) {
            return hit_row(state, section.body, row, x);
        }
    }

    None
}

/// What a click at column `x` on `row` means.
fn hit_row(state: &AppState, body: Rect, row: &Row<'_>, x: u16) -> Option<Hit> {
    match *row {
        Row::Project(id) | Row::Branch(id, _) => Some(Hit::Project(id)),
        Row::Pane(id, indent) => {
            let pane = state.pane(id)?;

            // A blank twisty column is not a control, so a click there falls
            // through to the row it is part of.
            if x == body.x + indent && !state.children_of(id).is_empty() {
                return Some(Hit::Twisty(id));
            }

            (!pane.closed).then_some(Hit::Pane(id))
        }
    }
}
```

`Sidebar` gains `scroll: Option<&'a Scroll>` (`None` in `new`) and:

```rust
    /// Draws each machine's section scrolled as `scroll` says.
    #[must_use]
    pub fn with_scroll(mut self, scroll: &'a Scroll) -> Self {
        self.scroll = Some(scroll);
        self
    }
```

Replace `render_device` with `render_label`:

```rust
    /// Writes a section's name onto the line that heads it.
    ///
    /// Dim and labelled when the machine's connection is down: its agents are
    /// still running, so the section stays, but a name that looks live while
    /// nothing can reach it is worse than no name.
    fn render_label(&self, buf: &mut Buffer, area: Rect, section: &Section<'_>) {
        // Inside the corners, with a blank either side, the way a frame's own
        // title sits.
        let line = Rect::new(area.x + 1, section.header, area.width.saturating_sub(2), 1);
        let room = usize::from(line.width.saturating_sub(2));

        let Some(id) = section.device else {
            write(buf, line, line.x, line.y, TITLE, Style::default());
            return;
        };
        let Some(device) = self.state.device(id) else {
            return;
        };

        let (name, style) = if device.reachable {
            (
                truncate(&device.name, room),
                Style::default().add_modifier(Modifier::BOLD),
            )
        } else {
            // The name gives way rather than the word this line exists to
            // show: a real hostname is routinely long enough to push it off.
            let suffix = format!(" — {UNREACHABLE}");
            let name_room = room.saturating_sub(suffix.width());
            (
                format!("{}{suffix}", truncate(&device.name, name_room)),
                Style::default().fg(self.theme.faded),
            )
        };

        write(buf, line, line.x, line.y, &format!(" {name} "), style);
    }
```

Delete `MACHINE`'s use (the machine glyph is no longer drawn; delete the constant too) and `block()` if nothing else calls it — `inner` becomes `Block::bordered().inner(area)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dispatch-tui && cargo test -p dispatch`
Expected: all pass. An app test that looked for a machine's *row* (e.g. `a_reattached_daemon_that_renamed_itself_updates_its_row`) finds the name on its section's line instead; if it asserted a column, the name now starts at column 2.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui/src/sidebar.rs crates/dispatch-tui/src/sidebar dispatch/src/app.rs
git commit -m "feat(tui): split the sidebar into a section per machine

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Scroll each section, count what is hidden, and follow the focus

**Files:**
- Modify: `crates/dispatch-tui/src/sidebar.rs` (`Anchor`, `Row::is`, `settle`, `section_at`, counts in `render` and `render_label`)
- Modify: `crates/dispatch-tui/src/sidebar/tests.rs`
- Modify: `dispatch/src/app.rs` (`anchored` field, `settle` before drawing, wheel over the sidebar, test)

**Interfaces:**
- Consumes: `Scroll`, `sections`, `Section` (Task 11).
- Produces: `pub enum Anchor { Pane(PaneId), Project(ProjectId) }`; `pub fn settle(state: &AppState, area: Rect, scroll: &mut Scroll, anchor: Option<Anchor>)`; `pub fn section_at(state: &AppState, area: Rect, scroll: &Scroll, x: u16, y: u16) -> Option<DeviceId>`.

- [ ] **Step 1: Write the failing tests**

Append to `sidebar/tests.rs`:

```rust
/// One machine with `count` projects named p0, p1, ….
fn many(count: usize) -> AppState {
    let mut state = AppState::new();
    for index in 0..count {
        state.add_project(Project::new(
            format!("/tmp/p{index}"),
            ProjectSource::LocalDir,
        ));
    }
    state
}

fn render_scrolled(state: &AppState, scroll: &Scroll, width: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    Sidebar::new(state).with_scroll(scroll).render(area, &mut buf);
    (0..height).map(|y| row_text(&buf, y)).collect()
}

#[test]
fn a_scrolled_section_starts_at_its_offset() {
    let state = many(20);
    let scroll = Scroll::from([(DeviceId::nil(), 5)]);

    let lines = render_scrolled(&state, &scroll, WIDTH, 8);

    assert!(lines[TOP as usize].contains(" p5 "), "{lines:#?}");
}

#[test]
fn an_offset_past_the_end_is_brought_back() {
    let state = many(20);
    let mut scroll = Scroll::from([(DeviceId::nil(), 99)]);

    settle(&state, Rect::new(0, 0, WIDTH, 8), &mut scroll, None);

    // Six rows inside the frame show the last six of twenty.
    assert_eq!(scroll[&DeviceId::nil()], 14);
}

#[test]
fn settling_on_a_pane_scrolls_just_far_enough_to_show_it() {
    let mut state = many(20);
    let p15 = state.projects()[15].id;
    let pane = spawn(&mut state, p15, "claude");
    let mut scroll = Scroll::new();

    settle(
        &state,
        Rect::new(0, 0, WIDTH, 8),
        &mut scroll,
        Some(Anchor::Pane(pane)),
    );

    // The pane is row 16; showing it as the last of six rows starts at 11.
    assert_eq!(scroll[&DeviceId::nil()], 11);
    let lines = render_scrolled(&state, &scroll, WIDTH, 8);
    assert!(lines[TOP as usize + 5].contains("claude"), "{lines:#?}");
}

#[test]
fn settling_without_an_anchor_leaves_the_wheels_scroll_alone() {
    let state = many(20);
    let mut scroll = Scroll::from([(DeviceId::nil(), 3)]);

    settle(&state, Rect::new(0, 0, WIDTH, 8), &mut scroll, None);

    assert_eq!(scroll[&DeviceId::nil()], 3);
}

#[test]
fn rows_hidden_below_are_counted_on_the_line_after_the_section() {
    let state = many(20);
    let lines = render_scrolled(&state, &Scroll::new(), WIDTH, 8);

    assert!(lines[7].contains("↓ 14"), "{lines:#?}");
}

#[test]
fn rows_hidden_above_are_counted_beside_the_sections_name() {
    let state = many(20);
    let scroll = Scroll::from([(DeviceId::nil(), 5)]);

    let lines = render_scrolled(&state, &scroll, WIDTH, 8);

    assert!(lines[0].contains("Projects ↑ 5"), "{lines:#?}");
}

#[test]
fn a_divider_carries_a_count_for_each_section_it_separates() {
    let mut state = AppState::new();
    let laptop = state.add_device(Device::new("laptop"));
    let tower = state.add_device(Device::new("a-tower-with-a-very-long-hostname"));
    for index in 0..10 {
        state.add_project(
            Project::new(format!("/tmp/l{index}"), ProjectSource::LocalDir).with_device(laptop),
        );
        state.add_project(
            Project::new(format!("/tmp/t{index}"), ProjectSource::LocalDir).with_device(tower),
        );
    }
    let scroll = Scroll::from([(tower, 2)]);

    // Twelve rows: ten inside, one divider, nine shared five and four.
    let lines = render_scrolled(&state, &scroll, WIDTH, 12);
    let divider = &lines[6];

    assert!(divider.contains("↑ 2"), "the tower's own count: {divider:?}");
    assert!(divider.contains("↓ 5"), "the laptop's count: {divider:?}");
    assert!(divider.ends_with('┤'), "{divider:?}");
}

#[test]
fn a_click_in_a_scrolled_section_finds_the_row_drawn_there() {
    let state = many(20);
    let p5 = state.projects()[5].id;
    let scroll = Scroll::from([(DeviceId::nil(), 5)]);

    assert_eq!(
        hit_test(&state, Rect::new(0, 0, WIDTH, 8), &scroll, LEFT + 4, TOP),
        Some(Hit::Project(p5))
    );
}

#[test]
fn the_wheel_finds_the_section_under_it() {
    let (state, laptop, tower) = fleet();
    let area = Rect::new(0, 0, WIDTH, 10);
    let scroll = Scroll::new();

    assert_eq!(section_at(&state, area, &scroll, 5, 1), Some(laptop));
    assert_eq!(section_at(&state, area, &scroll, 5, 6), Some(tower));
    assert_eq!(section_at(&state, area, &scroll, 5, 5), None, "a divider");
    assert_eq!(section_at(&state, area, &scroll, 0, 1), None, "the frame");
}
```

In `dispatch/src/app.rs`'s test module:

```rust
    #[test]
    fn the_wheel_over_the_sidebar_scrolls_it_rather_than_a_pane() {
        let mut app = App::new(HarnessRegistry::default());
        for index in 0..40 {
            app.state.add_project(Project::new(
                format!("/tmp/p{index}"),
                ProjectSource::LocalDir,
            ));
        }

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20))
            .expect("a test backend can be created");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("the frame is drawn");

        let wheel = Event::Mouse(dispatch_tui::input::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        app.handle(&wheel, Size::new(100, 20))
            .expect("the wheel is handled");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("the frame is drawn");

        // Row 0 is the top row and row 1 the sidebar's frame, so row 2 is the
        // first project shown — p1 now, and not scrolled back to the
        // selected p0 by the redraw.
        let text = rendered_text(&terminal);
        let first = sidebar_column(text.lines().nth(2).expect("the frame has rows"));
        assert!(first.contains(" p1 "), "{text}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dispatch-tui sidebar:: && cargo test -p dispatch wheel_over`
Expected: compile errors — `settle`, `Anchor`, `section_at` not found.

- [ ] **Step 3: Implement the sidebar side**

In `sidebar.rs`:

```rust
/// A row the sidebar should keep in view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// The focused pane's row.
    Pane(PaneId),
    /// The selected project's row, when no pane is focused.
    Project(ProjectId),
}

impl Row<'_> {
    /// Whether this is the row `anchor` names.
    fn is(&self, anchor: Anchor) -> bool {
        match (*self, anchor) {
            (Row::Pane(id, _), Anchor::Pane(wanted)) => id == wanted,
            (Row::Project(id), Anchor::Project(wanted)) => id == wanted,
            _ => false,
        }
    }
}

/// Brings every section's scroll back inside its rows and, when `anchor` is
/// given, scrolls the section holding that row just far enough to show it.
///
/// The caller passes an anchor only when the focus or the selection has
/// moved: anchoring every frame would undo a wheel scroll as fast as it
/// happened.
pub fn settle(state: &AppState, area: Rect, scroll: &mut Scroll, anchor: Option<Anchor>) {
    let found: Vec<(DeviceId, usize, Option<usize>, usize)> = sections(state, area, scroll)
        .iter()
        .map(|section| {
            let at = anchor.and_then(|anchor| section.rows.iter().position(|row| row.is(anchor)));
            (section.key, section.offset, at, usize::from(section.body.height))
        })
        .collect();

    for (key, offset, at, height) in found {
        let offset = match at {
            Some(at) if height > 0 && at < offset => at,
            Some(at) if height > 0 && at >= offset + height => at + 1 - height,
            _ => offset,
        };
        scroll.insert(key, u16::try_from(offset).unwrap_or(u16::MAX));
    }
}

/// Which machine's section holds `(x, y)`, for the wheel to scroll.
///
/// Only a section's rows count: the frame and the lines naming machines
/// belong to no one section's scroll.
#[must_use]
pub fn section_at(state: &AppState, area: Rect, scroll: &Scroll, x: u16, y: u16) -> Option<DeviceId> {
    let inner = inner(area);
    if x < inner.x || x >= inner.x + inner.width {
        return None;
    }

    sections(state, area, scroll)
        .into_iter()
        .find(|section| y >= section.body.y && y < section.body.y + section.body.height)
        .map(|section| section.key)
}

/// The text counting rows hidden below `section`, for the line after it.
fn below_label(section: &Section<'_>) -> Option<String> {
    let below = section.below();
    (below > 0).then(|| format!(" ↓ {below} "))
}
```

In `render`, after the loop over sections:

```rust
        // Counted on the line after each section — the next one's name line,
        // or the bottom border — right-aligned, one dash in from the corner.
        for section in &sections {
            if let Some(label) = below_label(section) {
                let y = section.body.y + section.body.height;
                let width = u16::try_from(label.width()).unwrap_or(u16::MAX);
                let x = (area.x + area.width).saturating_sub(2 + width);
                write(buf, area, x, y, &label, edge);
            }
        }
```

In `render`, the `self.render_label(buf, area, section);` call becomes:

```rust
            // The section above's `↓` count is drawn at the right end of this
            // same line, so the name leaves it room.
            let reserve = index
                .checked_sub(1)
                .and_then(|previous| below_label(&sections[previous]))
                .map_or(0, |label| label.width() + 1);
            self.render_label(buf, area, section, reserve);
```

Replace the constant `TITLE` (`" Projects "`) with:

```rust
/// What the sidebar is called when it is one list.
const TITLE: &str = "Projects";
```

and `render_label` with:

```rust
    /// Writes a section's name onto the line that heads it, followed by how
    /// many of its rows are scrolled away above, leaving `reserve` columns
    /// free at the right for the count of the section above it.
    ///
    /// Dim and labelled when the machine's connection is down: its agents are
    /// still running, so the section stays, but a name that looks live while
    /// nothing can reach it is worse than no name.
    fn render_label(&self, buf: &mut Buffer, area: Rect, section: &Section<'_>, reserve: usize) {
        // Inside the corners, with a blank either side, the way a frame's own
        // title sits.
        let line = Rect::new(area.x + 1, section.header, area.width.saturating_sub(2), 1);
        let up = if section.offset > 0 {
            format!(" ↑ {}", section.offset)
        } else {
            String::new()
        };
        let room = usize::from(line.width.saturating_sub(2)).saturating_sub(up.width() + reserve);

        let Some(id) = section.device else {
            write(buf, line, line.x, line.y, &format!(" {TITLE}{up} "), Style::default());
            return;
        };
        let Some(device) = self.state.device(id) else {
            return;
        };

        let (name, style) = if device.reachable {
            (
                truncate(&device.name, room),
                Style::default().add_modifier(Modifier::BOLD),
            )
        } else {
            // The name gives way rather than the word this line exists to
            // show: a real hostname is routinely long enough to push it off.
            let suffix = format!(" — {UNREACHABLE}");
            let name_room = room.saturating_sub(suffix.width());
            (
                format!("{}{suffix}", truncate(&device.name, name_room)),
                Style::default().fg(self.theme.faded),
            )
        };

        write(buf, line, line.x, line.y, &format!(" {name}{up} "), style);
    }
```

- [ ] **Step 4: Implement the app side**

Add to `App`: `anchored: Option<sidebar::Anchor>,` (initialised `None`). In `draw`, before rendering the sidebar:

```rust
        // Scrolled to the focus only when the focus has moved, so the wheel's
        // scroll survives every frame drawn in between.
        let anchor = self
            .state
            .focused_pane()
            .map(sidebar::Anchor::Pane)
            .or_else(|| self.state.selected_project().map(sidebar::Anchor::Project));
        let moved = anchor != self.anchored;
        sidebar::settle(
            &self.state,
            sidebar_area,
            &mut self.sidebar_scroll,
            if moved { anchor } else { None },
        );
        self.anchored = anchor;
```

In `handle`, after the sidebar click block and before `let layout = …`:

```rust
        // The wheel over the sidebar scrolls the section under it; the grid
        // never sees it, since no pane is under the pointer.
        if let Event::Mouse(mouse) = event
            && matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            )
            && let Some(device) = sidebar::section_at(
                &self.state,
                self.sidebar_area,
                &self.sidebar_scroll,
                mouse.column,
                mouse.row,
            )
        {
            let offset = self.sidebar_scroll.entry(device).or_insert(0);
            *offset = if mouse.kind == MouseEventKind::ScrollUp {
                offset.saturating_sub(1)
            } else {
                offset.saturating_add(1)
            };
            return Ok(());
        }
```

(`settle` on the next frame brings an offset past the end back.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`

```bash
git add crates/dispatch-tui/src/sidebar.rs crates/dispatch-tui/src/sidebar dispatch/src/app.rs
git commit -m "feat(tui): scroll each machine's section, count what it hides, follow the focus

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Verify the whole branch

**Files:** none new; fixes only where a check below fails.

- [ ] **Step 1: Everything CI runs, on this machine**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --no-fail-fast`
Expected: no diff, no warnings, every test passes. Report the pass count.

- [ ] **Step 2: The other two targets' platform code**

Run (when the targets are installed): `cargo clippy -p dispatch-os --all-targets --target aarch64-apple-darwin -- -D warnings && cargo clippy -p dispatch-os --all-targets --target x86_64-pc-windows-gnu -- -D warnings`
Expected: clean. Only `dispatch-os` carries platform code in this branch; the rest is checked by CI on push.

- [ ] **Step 3: Look at it**

From the repository root (itself a git repository, on `ui/visual-refresh`):

Run: `cargo build -p dispatch && (sleep 2; printf '\x01q') | timeout 6 script -qfc "stty cols 120 rows 30; ./target/debug/dispatch" /dev/null > "$TMPDIR/look.txt"; python3 -c "import re,sys; s=open(sys.argv[1],errors='replace').read(); s=re.sub(r'\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\x1b.',' ',s); print(' '.join(s.split())[:2000])" "$TMPDIR/look.txt"`

Expected, in the text: `D I S P A T C H`, `Projects`, `Dispatch`, and `ui/visual-refresh` (the project's branch line). No `╭`.

- [ ] **Step 4: The spec still describes what was built**

Read `docs/superpowers/specs/2026-09-24-visual-refresh-design.md` against the code. The three refinements made on the way — the one-second query bound (Task 8), the folded project keeping its own branch line (Task 10), an unreadable pane keeping its branch (Task 5) — are already in it. Fix any other place where it and the code disagree, in whichever is wrong.

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix(dispatch): what verifying the visual refresh turned up

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

(Skip if nothing changed.)
