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
