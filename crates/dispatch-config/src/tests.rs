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

    let run = def
        .task_launch("write the tests")
        .expect("the harness declares a task form");

    assert_eq!(run.launch.command, "claude");
    assert_eq!(run.launch.args, vec!["-p", "write the tests"]);
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
    let run = def.task_launch(hostile).expect("a task form exists");

    assert_eq!(run.launch.args.len(), 2);
    assert_eq!(run.launch.args[1], hostile);
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

    let run = def.task_launch("build it").expect("a task form exists");
    assert_eq!(run.launch.args, vec!["exec", "--prompt=build it"]);
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
fn a_windows_task_reaches_the_agent_on_standard_input() {
    // claude installs on Windows as a .cmd shim that only cmd.exe can run,
    // and cmd.exe reads its whole command line as shell syntax. The task
    // goes to a file, which cmd.exe redirects into `claude -p`; the command
    // line carries nothing of it.
    let dir = TempDir::new("windows-task-wrapper");
    write_missing_built_ins(dir.path()).expect("the built-ins are written");
    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("they load");

    let hostile = "x & echo DISPATCH_AUDIT_MARKER | %PATH% \"quoted\"\nsecond line";
    for (id, expected) in [
        (
            "claude",
            vec![
                "/d",
                "/v:off",
                "/c",
                "claude",
                "-p",
                "<%DISPATCH_TASK_FILE%",
            ],
        ),
        (
            "codex",
            vec![
                "/d",
                "/v:off",
                "/c",
                "codex",
                "exec",
                "-",
                "<%DISPATCH_TASK_FILE%",
            ],
        ),
    ] {
        let run = registry
            .get(id)
            .expect("the built-in exists")
            .task_launch_for("windows", hostile)
            .expect("it can be delegated to on Windows");

        assert_eq!(run.launch.command, "cmd.exe", "{id}");
        assert_eq!(run.launch.args, expected, "{id}");
        assert_eq!(run.input, TaskInput::File, "{id}");
        assert!(
            !run.launch.args.iter().any(|arg| arg.contains("MARKER")),
            "{id}: the task reached the command line"
        );
    }
}

#[test]
fn a_task_form_that_puts_the_task_on_cmds_command_line_is_refused_on_windows() {
    // Exactly what every earlier Dispatch wrote. A user who never edited it
    // gets the new one written over it; one who did is told what to change.
    let old: HarnessDef =
        toml::from_str(include_str!("../harnesses/superseded/claude-5.toml")).expect("it parses");

    let reason = old
        .task_refusal_for("windows")
        .expect("the old Windows form is refused");
    assert!(
        reason.contains("claude.toml") && reason.contains("DISPATCH_TASK_FILE"),
        "the refusal names the file and the fix: {reason}"
    );

    assert_eq!(
        old.task_refusal_for("linux"),
        None,
        "no shell is involved there"
    );

    let current: HarnessDef =
        toml::from_str(defaults::BUILT_INS[0].toml).expect("the built-in parses");
    assert_eq!(current.task_refusal_for("windows"), None);
}

#[test]
fn a_refusal_spells_out_the_lines_that_fix_it() {
    // An edited file is the one that does not show how, and a harness of the
    // user's own may have no Windows table at all: the message has to carry
    // the fix itself.
    let def: HarnessDef = toml::from_str(
        "id = \"mine\"\ndisplay_name = \"Mine\"\ncommand = \"cmd.exe\"\n\n\
         [task]\nargs = [\"/c\", \"agent\", \"{task}\"]\n",
    )
    .expect("the definition parses");

    let reason = def
        .task_refusal_for("windows")
        .expect("the form is refused");

    for line in [
        "mine.toml",
        "[task.platform.windows]",
        "input = \"file\"",
        "args = [..., \"<%DISPATCH_TASK_FILE%\"]",
    ] {
        assert!(reason.contains(line), "{line:?} is missing from: {reason}");
    }
    assert!(
        !reason.contains("shows how"),
        "the refusal points at a file that may not show anything: {reason}"
    );
}

#[test]
fn an_unedited_built_in_from_an_older_release_is_upgraded() {
    let dir = TempDir::new("upgrade-unedited");
    dir.write(
        "claude.toml",
        include_str!("../harnesses/superseded/claude-5.toml"),
    );

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");

    assert!(
        written.contains(&"claude"),
        "the old file was replaced: {written:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("claude.toml")).expect("it reads"),
        defaults::BUILT_INS[0].toml
    );
}

#[test]
fn every_body_an_earlier_dispatch_wrote_is_upgraded() {
    // Byte for byte what each release wrote, oldest first: an installation
    // made at any of them and never edited has one of these, and the unsafe
    // Windows form in all but the first.
    let written_before: [(&str, [&str; 5]); 2] = [
        (
            "claude",
            [
                include_str!("../harnesses/superseded/claude-1.toml"),
                include_str!("../harnesses/superseded/claude-2.toml"),
                include_str!("../harnesses/superseded/claude-3.toml"),
                include_str!("../harnesses/superseded/claude-4.toml"),
                include_str!("../harnesses/superseded/claude-5.toml"),
            ],
        ),
        (
            "codex",
            [
                include_str!("../harnesses/superseded/codex-1.toml"),
                include_str!("../harnesses/superseded/codex-2.toml"),
                include_str!("../harnesses/superseded/codex-3.toml"),
                include_str!("../harnesses/superseded/codex-4.toml"),
                include_str!("../harnesses/superseded/codex-5.toml"),
            ],
        ),
    ];

    for (id, bodies) in written_before {
        let current = defaults::BUILT_INS
            .iter()
            .find(|b| b.id == id)
            .expect("it ships")
            .toml;
        for (release, body) in bodies.iter().enumerate() {
            let dir = TempDir::new("upgrade-every-body");
            dir.write(&format!("{id}.toml"), body);

            let written = write_missing_built_ins(dir.path()).expect("writing succeeds");

            assert!(
                written.contains(&id),
                "{id}-{} was not recognised: {written:?}",
                release + 1
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join(format!("{id}.toml"))).expect("it reads"),
                current,
                "{id}-{}",
                release + 1
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn an_upgrade_replaces_the_file_rather_than_rewriting_it() {
    // A reader -- another Dispatch starting -- must see the old body or the
    // new, never part of either. A second name for the old file shows which
    // happened: a file replaced leaves it holding the old body, and one
    // rewritten in place changes under it.
    let dir = TempDir::new("upgrade-replaces");
    let old = include_str!("../harnesses/superseded/claude-5.toml");
    dir.write("claude.toml", old);
    std::fs::hard_link(
        dir.path().join("claude.toml"),
        dir.path().join("claude.old"),
    )
    .expect("the file system links");

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");

    assert!(written.contains(&"claude"), "{written:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("claude.old")).expect("it reads"),
        old,
        "the old file was written over rather than replaced"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("claude.toml")).expect("it reads"),
        defaults::BUILT_INS[0].toml
    );
}

#[test]
#[cfg(unix)]
fn an_upgrade_through_a_link_changes_the_file_it_names() {
    // Harness files kept elsewhere -- in a dotfiles repository, say -- and
    // linked in: the link is the user's arrangement, and stays.
    let dir = TempDir::new("upgrade-through-link");
    let kept = TempDir::new("upgrade-link-target");
    kept.write(
        "claude.toml",
        include_str!("../harnesses/superseded/claude-5.toml"),
    );
    std::os::unix::fs::symlink(
        kept.path().join("claude.toml"),
        dir.path().join("claude.toml"),
    )
    .expect("the file system links");

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");

    assert!(written.contains(&"claude"), "{written:?}");
    assert!(
        std::fs::symlink_metadata(dir.path().join("claude.toml"))
            .expect("the link is there")
            .file_type()
            .is_symlink(),
        "the link was replaced by a file"
    );
    assert_eq!(
        std::fs::read_to_string(kept.path().join("claude.toml")).expect("it reads"),
        defaults::BUILT_INS[0].toml,
        "the file the link names was not upgraded"
    );
}

#[test]
#[cfg(unix)]
fn an_upgrade_that_cannot_be_written_leaves_everything_else_working() {
    // The old form stays refused on Windows, so a file that cannot be
    // upgraded is safe to leave; stopping Dispatch from starting over it is
    // not.
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("upgrade-read-only");
    let old = include_str!("../harnesses/superseded/claude-5.toml");
    dir.write("claude.toml", old);
    write_missing_built_ins(dir.path()).expect("the others are written first");
    dir.write("claude.toml", old);

    let read_only = |mode| {
        std::fs::set_permissions(
            dir.path().join("claude.toml"),
            std::fs::Permissions::from_mode(mode & 0o666),
        )
        .expect("permissions change");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode))
            .expect("permissions change");
    };
    read_only(0o555);
    if std::fs::write(dir.path().join("probe"), "").is_ok() {
        // Permissions stop nobody running as root.
        read_only(0o755);
        eprintln!("skipped: permissions do not stop this user writing");
        return;
    }

    let result = write_missing_built_ins(dir.path());
    let after = std::fs::read_to_string(dir.path().join("claude.toml"));
    read_only(0o755);

    assert_eq!(
        result.expect("a failed upgrade is not an error"),
        Vec::<&str>::new(),
        "nothing was written"
    );
    assert_eq!(after.expect("it reads"), old, "the file is as it was");
}

#[test]
fn an_older_built_in_checked_out_with_crlf_is_still_recognised() {
    let dir = TempDir::new("upgrade-crlf");
    // Made LF first: a Windows checkout already has CRLF in what
    // `include_str!` reads, and doubling its carriage returns would make a
    // file no checkout ever wrote.
    let crlf = include_str!("../harnesses/superseded/codex-5.toml")
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    dir.write("codex.toml", &crlf);

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");
    assert!(written.contains(&"codex"), "{written:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("codex.toml")).expect("it reads"),
        defaults::BUILT_INS
            .iter()
            .find(|b| b.id == "codex")
            .expect("codex ships")
            .toml,
        "the file holds the current body, not the old one or a mix"
    );
}

#[test]
fn an_edited_built_in_from_an_older_release_is_left_alone() {
    let dir = TempDir::new("upgrade-edited");
    let edited = format!(
        "{}\n# mine\n",
        include_str!("../harnesses/superseded/claude-5.toml")
    );
    dir.write("claude.toml", &edited);

    let written = write_missing_built_ins(dir.path()).expect("writing succeeds");

    assert!(!written.contains(&"claude"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("claude.toml")).expect("it reads"),
        edited
    );
}

#[test]
fn a_command_windows_finds_as_a_batch_file_puts_the_task_on_cmds_command_line_too() {
    // `claude` written bare is `claude.cmd` once Windows looks for it, and
    // a batch file runs through cmd.exe: the file found decides, not the
    // name written.
    let def: HarnessDef = toml::from_str(
        "id = \"bare\"\ndisplay_name = \"Bare\"\ncommand = \"claude\"\n\n\
         [task]\nargs = [\"-p\", \"{task}\"]\n",
    )
    .expect("the definition parses");
    let on_path = |dir: &TempDir, path_key: &str| {
        let mut launch = def.launch_for("windows");
        launch
            .env
            .insert(path_key.into(), dir.path().display().to_string());
        launch
            .env
            .insert("PATHEXT".into(), ".COM;.EXE;.BAT;.CMD".into());
        launch
    };

    // Named in PATHEXT's own case, so a case-sensitive file system finds
    // them as Windows would.
    let shim = TempDir::new("batch-on-path");
    shim.write("claude.CMD", "");
    // Keyed as Windows keys it: case aside, `Path` is `PATH`.
    let reason = def
        .task_refusal_as("windows", &on_path(&shim, "Path"))
        .expect("a batch file found on PATH is refused");
    assert!(
        reason.to_lowercase().contains("claude.cmd") && reason.contains("bare.toml"),
        "the refusal says what was found and which file to fix: {reason}"
    );

    let executable = TempDir::new("exe-on-path");
    executable.write("claude.EXE", "");
    assert_eq!(
        def.task_refusal_as("windows", &on_path(&executable, "PATH")),
        None,
        "an executable is started directly, and its arguments reach it whole"
    );
    assert_eq!(
        def.task_refusal_as("linux", &on_path(&shim, "PATH")),
        None,
        "a .cmd file is nothing special where there is no cmd.exe"
    );
}

#[test]
fn a_file_form_that_also_names_the_task_is_refused_everywhere() {
    // A file form fills in nothing, so `{task}` would reach the agent as
    // those six characters: a form that cannot mean what it says.
    let def: HarnessDef = toml::from_str(
        "id = \"mixed\"\ndisplay_name = \"Mixed\"\ncommand = \"agent\"\n\n\
         [task]\nargs = [\"-p\", \"{task}\"]\ninput = \"file\"\n",
    )
    .expect("the definition parses");

    for os in ["linux", "macos", "windows"] {
        let reason = def
            .task_refusal_for(os)
            .unwrap_or_else(|| panic!("the form is refused on {os}"));
        for needed in [
            "mixed.toml",
            "{task}",
            "%DISPATCH_TASK_FILE%",
            "$DISPATCH_TASK_FILE",
        ] {
            assert!(
                reason.contains(needed),
                "{os}: {needed:?} is missing from: {reason}"
            );
        }
    }
}

#[test]
fn a_batch_file_named_as_the_command_puts_the_task_on_cmds_command_line_too() {
    // Windows runs a .cmd or .bat through cmd.exe whatever starts it, so a
    // task in its arguments is parsed exactly as it would be after `/c`.
    let form = |command: &str| -> HarnessDef {
        toml::from_str(&format!(
            "id = \"shim\"\ndisplay_name = \"Shim\"\ncommand = '{command}'\n\n\
             [task]\nargs = [\"-p\", \"{{task}}\"]\n"
        ))
        .expect("the definition parses")
    };

    for command in ["claude.cmd", r"C:\tools\RUN.BAT", "cmd"] {
        assert!(
            form(command).task_refusal_for("windows").is_some(),
            "{command} runs through cmd.exe"
        );
    }
    assert_eq!(
        form("claude.exe").task_refusal_for("windows"),
        None,
        "an executable is started directly, and its arguments reach it whole"
    );
}

#[test]
fn a_platform_without_its_own_form_uses_the_default_one() {
    let dir = TempDir::new("platform-fallback");
    write_missing_built_ins(dir.path()).expect("the built-ins are written");
    let registry = HarnessRegistry::load_from_dir(dir.path()).expect("they load");

    let run = registry
        .get("claude")
        .expect("the built-in exists")
        .task_launch_for("linux", "write the tests")
        .expect("claude can be delegated to on Linux");

    assert_eq!(run.launch.command, "claude");
    assert_eq!(run.launch.args, vec!["-p", "write the tests"]);
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
    // An installation made before icons existed keeps files with no `icon`
    // key wherever they are not upgraded: agy and opencode, and any the user
    // edited. Falling back on the id keeps those looking right.
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

#[test]
fn each_built_in_wears_the_mark_its_agent_is_known_by() {
    // Pinned by codepoint: these are the glyphs the user picked out of their
    // Nerd Font, and a silent change to one is a row that stops being
    // recognisable at a glance.
    let expected = [
        ("claude", '\u{ec82}'),
        ("codex", '\u{ec81}'),
        ("agy", '\u{e7f0}'),
        ("opencode", '\u{f121}'),
    ];

    for (id, glyph) in expected {
        let built_in = defaults::BUILT_INS
            .iter()
            .find(|b| b.id == id)
            .unwrap_or_else(|| panic!("{id} ships"));
        let def: HarnessDef = toml::from_str(built_in.toml).expect("built-ins parse");

        assert_eq!(def.icon(), glyph.to_string(), "{id}'s icon in its file");

        // And the same mark for an installation made before icons existed,
        // whose file has no `icon` key to read.
        let bare: HarnessDef = toml::from_str(&format!(
            "id = \"{id}\"\ndisplay_name = \"x\"\ncommand = \"x\""
        ))
        .expect("it parses");
        assert_eq!(bare.icon(), glyph.to_string(), "{id}'s fallback");
    }
}
