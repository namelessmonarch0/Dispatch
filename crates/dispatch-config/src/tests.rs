//! Tests for harness loading.

use super::*;

/// A temporary directory that cleans itself up.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        // Counter keeps parallel tests in the same process from colliding.
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let path = std::env::temp_dir().join(format!(
            "dispatch-config-{}-{label}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir is writable");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, contents).expect("temp dir is writable");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn every_built_in_parses() {
    for built_in in defaults::BUILT_INS {
        let def: HarnessDef = toml::from_str(built_in.toml)
            .unwrap_or_else(|e| panic!("built-in harness {} is invalid: {e}", built_in.id));
        assert_eq!(def.id, built_in.id, "id must match the file stem");
        assert!(
            !def.launch.command.is_empty(),
            "{} has no command",
            built_in.id
        );
        assert!(
            !def.display_name.is_empty(),
            "{} has no display name",
            built_in.id
        );
    }
}

#[test]
fn the_four_specified_harnesses_ship_by_default() {
    let ids: Vec<_> = defaults::BUILT_INS.iter().map(|b| b.id).collect();
    assert_eq!(ids, vec!["claude", "codex", "agy", "opencode"]);
}

#[test]
fn every_built_in_has_a_windows_launch_override() {
    // These agents install on Windows as .cmd shims, which CreateProcess
    // cannot execute directly. Without an override every pane fails to spawn
    // there, and the failure looks like a missing binary.
    for built_in in defaults::BUILT_INS {
        let def: HarnessDef = toml::from_str(built_in.toml).expect("built-ins parse");
        let windows = def.launch_for("windows");
        assert_ne!(
            windows, &def.launch,
            "{} has no windows override",
            built_in.id
        );
        assert_eq!(windows.command, "cmd.exe", "{}", built_in.id);
        assert_eq!(
            windows.args.first().map(String::as_str),
            Some("/c"),
            "{}",
            built_in.id
        );
    }
}

#[test]
fn an_unlisted_platform_falls_back_to_the_default_launch() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "demo"
display_name = "Demo"
command = "demo"
args = ["--tui"]
"#,
    )
    .expect("valid harness");

    assert_eq!(def.launch_for("linux").command, "demo");
    assert_eq!(def.launch_for("macos").args, vec!["--tui"]);
    assert_eq!(def.launch_for("windows").command, "demo");
}

#[test]
fn first_run_writes_the_built_ins() {
    let dir = TempDir::new("first-run");

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");
    assert_eq!(written.len(), defaults::BUILT_INS.len());

    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("loading succeeds");
    assert_eq!(registry.len(), defaults::BUILT_INS.len());
    assert!(registry.get("claude").is_some());
}

#[test]
fn a_second_run_does_not_overwrite_user_edits() {
    let dir = TempDir::new("no-clobber");
    write_missing_built_ins(dir.path()).expect("writing succeeds");

    // Stand in for a user editing the shipped file.
    dir.write(
        "claude.toml",
        r#"
id = "claude"
display_name = "My Claude"
command = "/usr/local/bin/claude"
"#,
    );

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");
    assert!(written.is_empty(), "nothing should be rewritten");

    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("loading succeeds");
    let claude = registry.get("claude").expect("claude is registered");
    assert_eq!(claude.display_name, "My Claude");
    assert_eq!(claude.launch.command, "/usr/local/bin/claude");
}

#[test]
fn a_missing_directory_yields_an_empty_registry() {
    let dir = TempDir::new("missing");
    let registry =
        HarnessRegistry::load_from_dir(&dir.path().join("nope")).expect("absence is not an error");
    assert!(registry.is_empty());
}

#[test]
fn non_toml_files_are_ignored() {
    let dir = TempDir::new("ignore");
    write_missing_built_ins(dir.path()).expect("writing succeeds");
    dir.write("notes.md", "not a harness");
    dir.write("claude.toml.bak", "id = broken");

    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("loading succeeds");
    assert_eq!(registry.len(), defaults::BUILT_INS.len());
}

#[test]
fn a_malformed_file_is_reported_with_its_path() {
    let dir = TempDir::new("malformed");
    dir.write("broken.toml", "id = \"broken\"\ncommand =");

    let error = HarnessRegistry::load_from_dir(dir.path()).expect_err("parsing should fail");

    match error {
        ConfigError::Toml { path, .. } => {
            assert!(path.ends_with("broken.toml"), "got {}", path.display());
        }
        other => panic!("expected a Toml error naming the file, got {other:?}"),
    }
}

#[test]
fn a_file_missing_a_required_field_is_reported() {
    let dir = TempDir::new("incomplete");
    dir.write(
        "incomplete.toml",
        "id = \"incomplete\"\ndisplay_name = \"X\"\n",
    );

    let error = HarnessRegistry::load_from_dir(dir.path()).expect_err("command is required");
    assert!(matches!(error, ConfigError::Toml { .. }));
}

#[test]
fn an_id_that_disagrees_with_the_file_name_is_rejected() {
    // The file stem is how the picker refers to a harness, so a mismatch would
    // make it unreachable under its own name.
    let dir = TempDir::new("mismatch");
    dir.write(
        "renamed.toml",
        "id = \"original\"\ndisplay_name = \"X\"\ncommand = \"x\"\n",
    );

    let error = HarnessRegistry::load_from_dir(dir.path()).expect_err("mismatch should fail");

    match error {
        ConfigError::IdMismatch {
            declared, expected, ..
        } => {
            assert_eq!(declared, "original");
            assert_eq!(expected, "renamed");
        }
        other => panic!("expected IdMismatch, got {other:?}"),
    }
}

#[test]
fn settings_parse_into_their_kinds() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "demo"
display_name = "Demo"
command = "demo"

[[settings]]
key = "model"
label = "Model"
kind = "choice"
options = ["fast", "slow"]
default = "fast"

[[settings]]
key = "note"
label = "Note"
kind = "text"

[[settings]]
key = "verbose"
label = "Verbose"
kind = "bool"
default = true
"#,
    )
    .expect("valid harness");

    assert_eq!(def.settings.len(), 3);
    assert!(matches!(
        &def.settings[0].kind,
        SettingKind::Choice { options, default }
            if options.len() == 2 && default.as_deref() == Some("fast")
    ));
    assert!(matches!(
        &def.settings[1].kind,
        SettingKind::Text { default: None }
    ));
    assert!(matches!(
        &def.settings[2].kind,
        SettingKind::Bool {
            default: Some(true)
        }
    ));
}

#[test]
fn a_harness_exposes_its_core_identifier() {
    let def: HarnessDef =
        toml::from_str("id = \"demo\"\ndisplay_name = \"Demo\"\ncommand = \"demo\"\n")
            .expect("valid harness");
    assert_eq!(def.harness_id().as_str(), "demo");
}
