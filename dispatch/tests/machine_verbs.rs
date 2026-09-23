//! `dispatch machine`, run as a user runs it.
//!
//! No ssh: a command override after `--` is how a test reaches a machine,
//! the same way `dispatch/tests/end_to_end.rs` reaches one.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Output;

/// A directory of this test's own, removed afterwards.
///
/// Short labels: a daemon's socket lives in here, and a Unix socket address
/// is limited to about a hundred bytes.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("dispatch-mv-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir is writable");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // The bridge started a daemon here; it outlives the bridge on purpose.
        if let Ok(pid) = std::fs::read_to_string(self.0.join("dispatchd.pid")) {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", pid.trim()])
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dispatchd_binary() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_dispatch"));
    path.set_file_name("dispatchd");
    path
}

/// Runs `dispatch machine …` against `config`.
fn machine(config: &Path, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_dispatch"))
        .arg("machine")
        .args(args)
        .env(dispatch_os::paths::CONFIG_DIR_ENV, config)
        .output()
        .expect("the dispatch binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_machine_that_answers_is_added_listed_and_removed() {
    let config = Scratch::new("ok-cfg");
    let there = Scratch::new("ok-far");
    let dispatchd = dispatchd_binary();
    let endpoint = there.0.join("dispatchd.sock");

    let added = machine(
        &config.0,
        &[
            "add",
            "far",
            "--name",
            "far",
            "--",
            dispatchd.to_str().expect("a UTF-8 path"),
            "--stdio",
            "--endpoint",
            endpoint.to_str().expect("a UTF-8 path"),
        ],
    );
    assert!(added.status.success(), "{}", stderr(&added));
    assert!(stdout(&added).contains("added far"), "{}", stdout(&added));

    let listed = machine(&config.0, &["list"]);
    assert!(stdout(&listed).contains("far"), "{}", stdout(&listed));

    let removed = machine(&config.0, &["remove", "far"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert!(
        stdout(&removed).contains("left running"),
        "{}",
        stdout(&removed)
    );

    let listed = machine(&config.0, &["list"]);
    assert!(
        stdout(&listed).contains("no machines"),
        "{}",
        stdout(&listed)
    );
}

#[test]
fn a_machine_that_cannot_be_reached_is_not_added() {
    let config = Scratch::new("bad-cfg");

    let added = machine(
        &config.0,
        &["add", "gone", "--", "/nonexistent/dispatchd", "--stdio"],
    );

    assert_eq!(added.status.code(), Some(1));
    assert!(
        stderr(&added).contains("/nonexistent/dispatchd"),
        "{}",
        stderr(&added)
    );
    assert!(
        !config.0.join("machines.toml").exists(),
        "nothing is saved for a machine that did not answer"
    );
}

#[test]
fn a_name_already_taken_is_refused_before_dialling() {
    let config = Scratch::new("dup-cfg");

    let first = machine(&config.0, &["add", "z", "--no-check", "--", "true"]);
    assert!(first.status.success(), "{}", stderr(&first));

    let second = machine(&config.0, &["add", "z", "--", "/nonexistent/dispatchd"]);
    assert_eq!(second.status.code(), Some(1));
    assert!(
        stderr(&second).contains("already registered"),
        "refused on the name, before any dial: {}",
        stderr(&second)
    );
}
