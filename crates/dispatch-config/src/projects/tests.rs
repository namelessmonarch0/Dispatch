//! Tests for the remembered project list.

use super::*;

use crate::testing::TempDir;

#[test]
fn nothing_is_remembered_before_anything_is_opened() {
    let dir = TempDir::new("projects-empty");

    let kept: Vec<PathBuf> = load(dir.path()).expect("an absent file is not an error");

    assert!(kept.is_empty());
}

#[test]
fn a_remembered_project_survives_a_restart() {
    let dir = TempDir::new("projects-roundtrip");

    assert!(remember(dir.path(), Path::new("/tmp/alpha")).expect("the directory is writable"));

    assert_eq!(
        load(dir.path()).expect("it reads back"),
        [PathBuf::from("/tmp/alpha")]
    );
}

#[test]
fn opening_the_same_project_again_changes_nothing() {
    // Every start opens whatever is kept, so rewriting the file each time
    // would churn it for no reason and reorder the list.
    let dir = TempDir::new("projects-again");

    remember(dir.path(), Path::new("/tmp/alpha")).expect("the directory is writable");
    remember(dir.path(), Path::new("/tmp/beta")).expect("the directory is writable");

    assert!(!remember(dir.path(), Path::new("/tmp/alpha")).expect("it reads back"));
    assert_eq!(
        load(dir.path()).expect("it reads back"),
        [PathBuf::from("/tmp/alpha"), PathBuf::from("/tmp/beta")],
        "the order they were first opened in"
    );
}

#[test]
fn a_forgotten_project_stays_forgotten() {
    let dir = TempDir::new("projects-forget");

    remember(dir.path(), Path::new("/tmp/alpha")).expect("the directory is writable");
    remember(dir.path(), Path::new("/tmp/beta")).expect("the directory is writable");

    assert!(forget(dir.path(), Path::new("/tmp/alpha")).expect("it is written"));
    assert_eq!(
        load(dir.path()).expect("it reads back"),
        [PathBuf::from("/tmp/beta")]
    );
}

#[test]
fn forgetting_something_that_was_never_kept_is_not_an_error() {
    let dir = TempDir::new("projects-forget-missing");

    assert!(!forget(dir.path(), Path::new("/tmp/nowhere")).expect("it is not an error"));
}
