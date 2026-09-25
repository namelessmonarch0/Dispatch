//! Tests for the top-level configuration file.

use super::*;

use crate::testing::TempDir;

#[test]
fn a_missing_file_means_the_defaults() {
    // A fresh install has no config.toml, and must behave like a configured one
    // that changed nothing.
    let dir = TempDir::new("missing");
    let config =
        Config::load(&dir.path().join("config.toml")).expect("an absent file is not an error");

    assert_eq!(config, Config::default());
    assert_eq!(config.delegation.max_depth, 1);
    assert_eq!(config.delegation.max_live_per_parent, 4);
    assert_eq!(config.delegation.request_timeout_secs, 600);
}

#[test]
fn the_caps_can_be_raised_deliberately() {
    let dir = TempDir::new("raised");
    let path = dir.config(
        r#"
[delegation]
max_depth = 2
max_live_per_parent = 8
request_timeout_secs = 60
"#,
    );

    let config = Config::load(&path).expect("the file parses");
    assert_eq!(config.delegation.max_depth, 2);
    assert_eq!(config.delegation.max_live_per_parent, 8);
    assert_eq!(config.delegation.request_timeout_secs, 60);
}

#[test]
fn a_partly_written_section_keeps_the_other_defaults() {
    let dir = TempDir::new("partial");
    let path = dir.config("[delegation]\nmax_depth = 2\n");

    let config = Config::load(&path).expect("the file parses");
    assert_eq!(config.delegation.max_depth, 2);
    assert_eq!(
        config.delegation.max_live_per_parent, 4,
        "an unmentioned cap keeps its default"
    );
}

#[test]
fn a_key_this_build_does_not_know_is_kept_and_reported() {
    // A newer daemon's key must not stop an older one starting, and a typo must
    // not be silent.
    let dir = TempDir::new("unknown");
    let path = dir.config(
        r#"
[delegation]
max_depth = 1
max_liv_per_parent = 9
"#,
    );

    let loaded = Config::load_reporting(&path).expect("the file parses");
    assert_eq!(loaded.config.delegation.max_live_per_parent, 4);
    assert_eq!(
        loaded.unknown,
        vec!["delegation.max_liv_per_parent".to_string()],
        "the key is named so a typo can be found"
    );
}

#[test]
fn a_broken_file_names_itself() {
    let dir = TempDir::new("broken");
    let path = dir.config("[delegation\nmax_depth = 1\n");

    let error = Config::load(&path).expect_err("invalid TOML is an error");
    assert!(
        error.to_string().contains("config.toml"),
        "the user has to be told which file to fix, got {error}"
    );
}

#[test]
fn motion_is_on_unless_turned_off() {
    assert!(Config::default().interface.motion);

    let config: Config = toml::from_str("[interface]\nmotion = false\n").expect("parses");
    assert!(!config.interface.motion);
}

#[test]
fn the_interface_section_is_not_reported_unknown() {
    let raw: toml::Table = toml::from_str("[interface]\nmotion = false\n").expect("parses");
    assert!(unknown_keys(&raw).is_empty());

    let raw: toml::Table = toml::from_str("[interface]\nsparkles = true\n").expect("parses");
    assert_eq!(unknown_keys(&raw), vec!["interface.sparkles".to_string()]);
}
