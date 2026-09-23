//! Tests for the machine registry.

use super::*;

use crate::testing::TempDir;

#[test]
fn nothing_is_registered_before_anything_is_added() {
    let dir = TempDir::new("machines-absent");
    assert!(
        load(dir.path())
            .expect("an absent file is not an error")
            .is_empty()
    );
}

#[test]
fn an_empty_file_is_no_machines() {
    // A user who created the file and then deleted every entry: that is no
    // machines, not a broken configuration.
    let dir = TempDir::new("machines-empty");
    dir.write("machines.toml", "");
    assert!(load(dir.path()).expect("an empty file parses").is_empty());
}

#[test]
fn a_machine_survives_a_restart_with_and_without_a_command() {
    let dir = TempDir::new("machines-roundtrip");
    let plain = Machine::new("tower", "me@tower");
    let custom = Machine {
        name: "gpu".into(),
        target: "gpu-box".into(),
        command: Some(Command {
            program: "/Applications/My Tools/tunnel".into(),
            args: vec!["gpu-box".into(), "dispatchd".into(), "--stdio".into()],
        }),
    };

    save(dir.path(), &[plain.clone(), custom.clone()]).expect("the directory is writable");

    assert_eq!(load(dir.path()).expect("it reads back"), [plain, custom]);
}

#[test]
fn the_default_command_is_ssh_that_never_prompts() {
    // `BatchMode` is what keeps ssh from asking for a password on the
    // terminal the interface is drawn on.
    let (program, args) = Machine::new("tower", "me@tower").dial();

    assert_eq!(program, "ssh");
    assert_eq!(
        args,
        [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "me@tower",
            "dispatchd",
            "--stdio"
        ]
        .map(OsString::from)
    );
}

#[test]
fn a_command_replaces_the_default_whole() {
    let machine = Machine {
        name: "gpu".into(),
        target: "gpu-box".into(),
        command: Some(Command {
            program: "/Applications/My Tools/tunnel".into(),
            args: vec!["--stdio".into()],
        }),
    };

    let (program, args) = machine.dial();

    assert_eq!(
        program, "/Applications/My Tools/tunnel",
        "one program, space and all"
    );
    assert_eq!(args, [OsString::from("--stdio")]);
}

#[test]
fn a_default_name_comes_from_the_targets_host() {
    assert_eq!(default_name("tower").as_deref(), Some("tower"));
    assert_eq!(default_name("me@tower.lan").as_deref(), Some("tower"));
    assert_eq!(
        default_name("ssh://me@tower:2222").as_deref(),
        Some("tower")
    );
    assert_eq!(default_name("gpu-box").as_deref(), Some("gpu-box"));
    assert_eq!(
        default_name("192.168.1.5").as_deref(),
        Some("192-168-1-5"),
        "an address keeps all four parts rather than becoming `192`"
    );
    assert_eq!(
        default_name("me@").as_deref(),
        None,
        "nothing to name it after"
    );
}

#[test]
fn a_name_is_letters_digits_dashes_and_underscores() {
    assert!(valid_name("tower"));
    assert!(valid_name("gpu_box-2"));
    assert!(!valid_name(""));
    assert!(!valid_name("my tower"));
    assert!(!valid_name("tower.lan"));
}

#[test]
fn adding_refuses_a_name_already_taken() {
    let dir = TempDir::new("machines-duplicate");
    add(dir.path(), Machine::new("tower", "me@tower"), "laptop").expect("the first is added");

    let error = add(dir.path(), Machine::new("tower", "other@tower"), "laptop")
        .expect_err("the name is taken");

    assert!(error.to_string().contains("already registered"), "{error}");
    assert_eq!(load(dir.path()).expect("it reads back").len(), 1);
}

#[test]
fn adding_refuses_this_machines_own_name() {
    // The local daemon's row already carries the hostname.
    let dir = TempDir::new("machines-self");

    let error = add(dir.path(), Machine::new("laptop", "laptop"), "Laptop.local")
        .expect_err("that is this machine");

    assert!(error.to_string().contains("this machine"), "{error}");
}

#[test]
fn adding_refuses_a_name_that_is_not_one() {
    let dir = TempDir::new("machines-invalid");

    let error = add(dir.path(), Machine::new("my tower", "tower"), "laptop")
        .expect_err("a space is not allowed");

    assert!(error.to_string().contains("not a machine name"), "{error}");
}

#[test]
fn a_target_is_something_ssh_reads_as_a_host() {
    assert!(valid_target("tower"));
    assert!(valid_target("me@tower.lan"));
    assert!(valid_target("ssh://me@tower:2222"));
    assert!(!valid_target(""));
    // ssh would read these as options: `ProxyCommand` runs a local command.
    assert!(!valid_target("-oProxyCommand=touch /tmp/x"));
    assert!(!valid_target("-p2222"));
    assert!(!valid_target("my tower"));
    assert!(!valid_target("tower\t"));
}

#[test]
fn adding_refuses_a_target_ssh_would_read_as_an_option() {
    let dir = TempDir::new("machines-target");

    let error = add(
        dir.path(),
        Machine::new("evil", "-oProxyCommand=x"),
        "laptop",
    )
    .expect_err("a leading dash is an ssh option");

    assert!(error.to_string().contains("not an ssh target"), "{error}");
    assert!(load(dir.path()).expect("it reads").is_empty());
}

#[test]
fn removing_answers_whether_it_was_there() {
    let dir = TempDir::new("machines-remove");
    add(dir.path(), Machine::new("tower", "me@tower"), "laptop").expect("added");

    assert!(remove(dir.path(), "tower").expect("written"));
    assert!(!remove(dir.path(), "tower").expect("nothing to write"));
    assert!(load(dir.path()).expect("it reads back").is_empty());
}
