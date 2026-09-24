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
        remember(Path::new(&dir), &PathBuf::from(format!("/tmp/{label}-{i}")))
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
