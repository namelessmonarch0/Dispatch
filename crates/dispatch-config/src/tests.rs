//! Tests for harness loading.

use super::*;

use crate::testing::TempDir;

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
            windows.command, def.launch.command,
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
fn harness_environment_reaches_the_launch() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "demo"
display_name = "Demo"
command = "demo"

[env]
DEMO_MODE = "on"
"#,
    )
    .expect("valid harness");

    assert_eq!(
        def.launch_for("linux")
            .env
            .get("DEMO_MODE")
            .map(String::as_str),
        Some("on"),
        "env declared on the harness must reach the spawned child"
    );
}

#[test]
fn a_platform_override_wins_over_the_harness_environment() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "demo"
display_name = "Demo"
command = "demo"

[env]
SHELL_KIND = "posix"

[platform.windows]
command = "cmd.exe"

[platform.windows.env]
SHELL_KIND = "windows"
"#,
    )
    .expect("valid harness");

    assert_eq!(
        def.launch_for("linux")
            .env
            .get("SHELL_KIND")
            .map(String::as_str),
        Some("posix")
    );
    assert_eq!(
        def.launch_for("windows")
            .env
            .get("SHELL_KIND")
            .map(String::as_str),
        Some("windows")
    );
}

#[test]
fn a_harness_exposes_its_core_identifier() {
    let def: HarnessDef =
        toml::from_str("id = \"demo\"\ndisplay_name = \"Demo\"\ncommand = \"demo\"\n")
            .expect("valid harness");
    assert_eq!(def.harness_id().as_str(), "demo");
}

#[test]
fn a_registered_harness_is_not_offered_again() {
    let dir = TempDir::new("discover");
    write_missing_built_ins(dir.path()).expect("writing succeeds");
    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("loading succeeds");

    let found = discover_unregistered(&registry, &[]);

    assert!(
        found.is_empty(),
        "everything shipped is already registered, got {found:?}"
    );
}

#[test]
fn registering_writes_the_built_in_definition() {
    let dir = TempDir::new("register");

    let path = register_harness(dir.path(), "claude").expect("writing succeeds");
    assert!(path.ends_with("claude.toml"));

    let def = HarnessRegistry::load_file(&path).expect("the written file parses");
    assert_eq!(def.display_name, "Claude Code");
}

#[test]
fn registering_something_unknown_writes_a_usable_definition() {
    let dir = TempDir::new("register-unknown");

    let path = register_harness(dir.path(), "somebot").expect("writing succeeds");
    let def = HarnessRegistry::load_file(&path).expect("the written file parses");

    assert_eq!(def.id, "somebot");
    assert_eq!(def.launch.command, "somebot");
    // Windows shims are the common case, so the template covers them.
    assert_eq!(def.launch_for("windows").command, "cmd.exe");
}

#[test]
fn registering_does_not_overwrite_an_existing_definition() {
    let dir = TempDir::new("register-twice");
    dir.write(
        "claude.toml",
        "id = \"claude\"\ndisplay_name = \"Mine\"\ncommand = \"mine\"\n",
    );

    register_harness(dir.path(), "claude").expect("writing succeeds");

    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("loading succeeds");
    assert_eq!(
        registry.get("claude").expect("registered").display_name,
        "Mine",
        "an existing definition must not be discarded"
    );
}

#[test]
fn which_finds_a_program_that_exists() {
    // Something guaranteed present on every platform this runs on.
    let name = if cfg!(windows) { "cmd" } else { "sh" };
    assert!(which(name).is_some(), "{name} should be on PATH");
}

#[test]
fn which_does_not_find_a_program_that_does_not_exist() {
    assert!(which("dispatch-definitely-not-installed").is_none());
}

#[test]
fn a_harness_can_declare_a_one_shot_task_form() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "claude"
display_name = "Claude Code"
command = "claude"

[task]
args = ["-p", "{task}"]
"#,
    )
    .expect("the definition parses");

    let launch = def
        .task_launch("write the tests")
        .expect("the harness declares a task form");

    assert_eq!(launch.command, "claude");
    assert_eq!(launch.args, vec!["-p", "write the tests"]);
}

#[test]
fn a_harness_without_a_task_form_cannot_be_delegated_to() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "agy"
display_name = "agy"
command = "agy"
"#,
    )
    .expect("the definition parses");

    assert!(
        def.task_launch("anything").is_none(),
        "a harness with no [task] form has no non-interactive shape to run"
    );
}

#[test]
fn a_task_is_one_argument_however_it_is_written() {
    // Substituted as an argv element, never interpolated into a shell string:
    // quotes, newlines and command substitution have to arrive as text.
    let def: HarnessDef = toml::from_str(
        r#"
id = "shell"
display_name = "Shell"
command = "sh"

[task]
args = ["-c", "{task}"]
"#,
    )
    .expect("the definition parses");

    let hostile = "say \"hi\"\nthen $(rm -rf /)";
    let launch = def.task_launch(hostile).expect("a task form exists");

    assert_eq!(launch.args.len(), 2);
    assert_eq!(launch.args[1], hostile);
}

#[test]
fn a_task_placeholder_inside_a_longer_argument_is_substituted() {
    let def: HarnessDef = toml::from_str(
        r#"
id = "codex"
display_name = "Codex"
command = "codex"

[task]
args = ["exec", "--prompt={task}"]
"#,
    )
    .expect("the definition parses");

    let launch = def.task_launch("build it").expect("a task form exists");
    assert_eq!(launch.args, vec!["exec", "--prompt=build it"]);
}

#[test]
fn the_built_in_agents_that_can_be_delegated_to_say_so() {
    // claude and codex have documented non-interactive forms. agy and opencode
    // do not ship one: a guess at their flags would run a process with flags
    // that mean something else.
    let dir = TempDir::new("built-in-task-forms");
    write_missing_built_ins(dir.path()).expect("the built-ins are written");
    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("they load");

    for id in ["claude", "codex"] {
        assert!(
            registry
                .get(id)
                .expect("the built-in exists")
                .task_launch("x")
                .is_some(),
            "{id} should declare a [task] form"
        );
    }

    for id in ["agy", "opencode"] {
        assert!(
            registry
                .get(id)
                .expect("the built-in exists")
                .task_launch("x")
                .is_none(),
            "{id} should not guess at a [task] form"
        );
    }
}

#[test]
fn a_windows_wrapper_survives_a_one_shot_run() {
    // claude installs on Windows as a .cmd shim that has to be run through
    // cmd.exe. A one-shot run reaches the agent the same way an interactive one
    // does, or it runs cmd.exe and never the agent.
    let dir = TempDir::new("windows-task-wrapper");
    write_missing_built_ins(dir.path()).expect("the built-ins are written");
    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("they load");

    let claude = registry.get("claude").expect("the built-in exists");
    let launch = claude
        .task_launch_for("windows", "write the tests")
        .expect("claude can be delegated to on Windows");

    assert_eq!(launch.command, "cmd.exe");
    assert_eq!(
        launch.args,
        vec!["/c", "claude", "-p", "write the tests"],
        "the shim wrapper has to stay in front of the one-shot flags"
    );
}

#[test]
fn a_platform_without_its_own_form_uses_the_default_one() {
    let dir = TempDir::new("platform-fallback");
    write_missing_built_ins(dir.path()).expect("the built-ins are written");
    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("they load");

    let launch = registry
        .get("claude")
        .expect("the built-in exists")
        .task_launch_for("linux", "write the tests")
        .expect("claude can be delegated to on Linux");

    assert_eq!(launch.command, "claude");
    assert_eq!(launch.args, vec!["-p", "write the tests"]);
}

#[test]
fn a_task_form_with_no_arguments_is_no_task_form() {
    // Nothing to carry the task in, so there is nothing to run.
    let def: HarnessDef = toml::from_str(
        r#"
id = "empty"
display_name = "Empty"
command = "empty"

[task]
args = []
"#,
    )
    .expect("the definition parses");

    assert!(def.task_launch("anything").is_none());
}

#[test]
fn every_built_in_ships_an_icon() {
    // The sidebar names a pane by the agent running in it, and four rows of
    // similar titles are told apart by the mark beside them.
    for built_in in defaults::BUILT_INS {
        let def: HarnessDef = toml::from_str(built_in.toml).expect("built-ins parse");
        assert_ne!(
            def.icon(),
            harness::DEFAULT_ICON,
            "{} falls back to the generic icon",
            built_in.id
        );
    }
}

#[test]
fn a_harness_with_no_icon_falls_back_to_a_generic_one() {
    // Icons are optional: a harness registered by hand is still listed.
    let def: HarnessDef =
        toml::from_str("id = \"x\"\ndisplay_name = \"X\"\ncommand = \"x\"").expect("it parses");

    assert_eq!(def.icon(), harness::DEFAULT_ICON);
}

#[test]
fn an_icon_is_one_column_wide() {
    // The sidebar reserves a single cell for it, so a two-cell glyph would
    // push every title one column right on that row alone.
    for built_in in defaults::BUILT_INS {
        let def: HarnessDef = toml::from_str(built_in.toml).expect("built-ins parse");
        assert_eq!(
            def.icon().chars().count(),
            1,
            "{} has a multi-character icon",
            built_in.id
        );
    }
}

#[test]
fn a_built_in_without_an_icon_still_gets_its_own() {
    // Dispatch never rewrites a harness file that already exists, so every
    // installation made before icons existed has four files with no `icon`
    // key. Falling back on the id keeps those looking right.
    let def: HarnessDef =
        toml::from_str("id = \"claude\"\ndisplay_name = \"Claude Code\"\ncommand = \"claude\"")
            .expect("it parses");

    let shipped: HarnessDef = toml::from_str(
        defaults::BUILT_INS
            .iter()
            .find(|b| b.id == "claude")
            .expect("claude ships")
            .toml,
    )
    .expect("built-ins parse");

    assert_eq!(def.icon(), shipped.icon());
}
