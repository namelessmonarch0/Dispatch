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

#[test]
fn a_remote_machines_projects_are_kept_apart_from_this_ones() {
    let dir = TempDir::new("projects-remote");

    remember(dir.path(), Path::new("/Users/me/code/thing")).expect("written");
    assert!(remember_on(dir.path(), "tower", Path::new("~/code/server")).expect("written"));

    assert_eq!(
        load(dir.path()).expect("it reads back"),
        [PathBuf::from("/Users/me/code/thing")],
        "this machine's list is untouched"
    );
    assert_eq!(
        load_on(dir.path(), "tower").expect("it reads back"),
        [PathBuf::from("~/code/server")]
    );
    assert!(
        load_on(dir.path(), "gpu")
            .expect("an unknown machine is not an error")
            .is_empty()
    );
}

#[test]
fn saving_this_machines_list_keeps_the_others() {
    // `save` replaces this machine's roots; it must not take every remote
    // machine's kept projects with it.
    let dir = TempDir::new("projects-save-keeps");
    remember_on(dir.path(), "tower", Path::new("~/a")).expect("written");

    save(dir.path(), &[PathBuf::from("/tmp/here")]).expect("written");

    assert_eq!(
        load_on(dir.path(), "tower").expect("it reads back"),
        [PathBuf::from("~/a")]
    );
}

#[test]
fn a_remote_project_can_be_forgotten() {
    let dir = TempDir::new("projects-remote-forget");
    remember_on(dir.path(), "tower", Path::new("~/a")).expect("written");
    remember_on(dir.path(), "tower", Path::new("~/b")).expect("written");

    assert!(forget_on(dir.path(), "tower", Path::new("~/a")).expect("written"));
    assert!(!forget_on(dir.path(), "tower", Path::new("~/a")).expect("nothing to do"));
    assert_eq!(
        load_on(dir.path(), "tower").expect("it reads back"),
        [PathBuf::from("~/b")]
    );
}

#[test]
fn forgetting_a_machine_drops_its_whole_list() {
    let dir = TempDir::new("projects-forget-machine");
    remember(dir.path(), Path::new("/tmp/here")).expect("written");
    remember_on(dir.path(), "tower", Path::new("~/a")).expect("written");

    assert!(forget_machine(dir.path(), "tower").expect("written"));
    assert!(!forget_machine(dir.path(), "tower").expect("nothing to do"));
    assert!(
        load_on(dir.path(), "tower")
            .expect("it reads back")
            .is_empty()
    );
    assert_eq!(
        load(dir.path()).expect("it reads back"),
        [PathBuf::from("/tmp/here")]
    );
}

#[test]
fn a_file_from_before_machines_still_loads() {
    let dir = TempDir::new("projects-old-shape");
    dir.write("projects.toml", "roots = [\"/tmp/old\"]\n");

    assert_eq!(
        load(dir.path()).expect("it reads"),
        [PathBuf::from("/tmp/old")]
    );
    assert!(load_on(dir.path(), "tower").expect("it reads").is_empty());
}
