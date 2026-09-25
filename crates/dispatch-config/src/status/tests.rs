//! Tests for status rules.

use super::*;

/// Rules as a harness file would write them.
fn rules(text: &str) -> StatusRules {
    #[derive(serde::Deserialize)]
    struct File {
        status: StatusDef,
    }

    let file: File = toml::from_str(text).expect("the test's TOML parses");
    StatusRules::compile("test", &file.status)
}

fn lines(text: &[&str]) -> Vec<String> {
    text.iter().map(|line| (*line).to_string()).collect()
}

fn on_screen(rules: &StatusRules, screen: &[&str]) -> Option<RuleState> {
    let screen = lines(screen);
    rules.evaluate(&StatusInput {
        screen: &screen,
        ..StatusInput::default()
    })
}

#[test]
fn contains_matches_regardless_of_case() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "blocked"
        region = "screen"
        contains = ["do you want to proceed?"]
        "#,
    );

    assert_eq!(
        on_screen(&rules, &["  Do you want to PROCEED?"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(on_screen(&rules, &["nothing here"]), None);
}

#[test]
fn every_contains_must_appear_and_one_any() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "blocked"
        region = "screen"
        contains = ["esc to cancel"]
        any = ["enter to confirm", "enter to select"]
        "#,
    );

    assert_eq!(
        on_screen(&rules, &["Pick one", "enter to select · esc to cancel"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        on_screen(&rules, &["esc to cancel"]),
        None,
        "no `any` string appeared"
    );
}

#[test]
fn not_vetoes_a_match() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "working"
        region = "screen"
        contains = ["thinking"]
        not = ["waiting for permission"]
        "#,
    );

    assert_eq!(on_screen(&rules, &["thinking…"]), Some(RuleState::Working));
    assert_eq!(
        on_screen(&rules, &["thinking…", "Waiting for permission"]),
        None
    );
}

#[test]
fn a_regex_is_tested_against_each_line() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "blocked"
        region = "screen"
        regex = ['^\s*❯?\s*1\.\s*Yes\b']
        "#,
    );

    assert_eq!(
        on_screen(
            &rules,
            &["Do you want to proceed?", " ❯ 1. Yes", "   2. No"]
        ),
        Some(RuleState::Blocked),
        "`^` anchors at the start of the second line, not of the screen"
    );
}

#[test]
fn bottom_reads_only_the_last_non_blank_lines() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "working"
        region = "bottom:2"
        contains = ["esc to interrupt"]
        "#,
    );

    assert_eq!(
        on_screen(&rules, &["esc to interrupt", "a", "", "b", ""]),
        None,
        "the phrase is above the last two non-blank lines"
    );
    assert_eq!(
        on_screen(&rules, &["a", "esc to interrupt", "", "b", ""]),
        Some(RuleState::Working)
    );
}

#[test]
fn the_title_and_progress_regions_read_their_signal() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "working"
        region = "title"
        regex = ['^[\x{2800}-\x{28FF}] ']

        [[status.rules]]
        state = "idle"
        region = "progress"
        regex = ['^4;0']
        "#,
    );

    let working = rules.evaluate(&StatusInput {
        title: "\u{280b} Refactor",
        ..StatusInput::default()
    });
    let idle = rules.evaluate(&StatusInput {
        progress: "4;0",
        ..StatusInput::default()
    });

    assert_eq!(working, Some(RuleState::Working));
    assert_eq!(idle, Some(RuleState::Idle));
}

#[test]
fn the_highest_priority_match_decides_and_ties_keep_file_order() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "idle"
        region = "screen"
        contains = ["x"]
        priority = 10

        [[status.rules]]
        state = "blocked"
        region = "screen"
        contains = ["x"]
        priority = 20

        [[status.rules]]
        state = "working"
        region = "screen"
        contains = ["x"]
        priority = 20
        "#,
    );

    assert_eq!(on_screen(&rules, &["x"]), Some(RuleState::Blocked));
}

#[test]
fn a_rule_that_cannot_be_used_is_skipped_and_the_rest_still_work() {
    let rules = rules(
        r#"
        [[status.rules]]
        state = "blocked"
        region = "screen"
        regex = ['(unclosed']

        [[status.rules]]
        state = "sleeping"
        region = "screen"
        contains = ["x"]

        [[status.rules]]
        state = "working"
        region = "sideways"
        contains = ["x"]

        [[status.rules]]
        state = "working"
        region = "screen"
        not = ["y"]

        [[status.rules]]
        state = "working"
        region = "screen"
        contains = ["x"]
        "#,
    );

    assert_eq!(on_screen(&rules, &["x"]), Some(RuleState::Working));
}

#[test]
fn no_rules_match_nothing() {
    assert!(StatusRules::default().is_empty());
    assert_eq!(on_screen(&StatusRules::default(), &["anything"]), None);
}

#[test]
fn a_harness_file_carries_a_status_section() {
    let def: crate::HarnessDef = toml::from_str(
        r#"
        id = "custom"
        display_name = "Custom"
        command = "custom"

        [[status.rules]]
        state = "working"
        region = "screen"
        contains = ["busy"]
        "#,
    )
    .expect("the harness parses");

    let rules = StatusRules::for_harness(&def.id, def.status.as_ref());
    assert_eq!(on_screen(&rules, &["busy"]), Some(RuleState::Working));
}

#[test]
fn an_empty_status_section_means_activity_only() {
    let own = StatusDef::default();

    assert!(StatusRules::for_harness("claude", Some(&own)).is_empty());
}

#[test]
fn an_unknown_harness_with_no_section_has_no_rules() {
    assert!(StatusRules::for_harness("never-heard-of-it", None).is_empty());
}

fn builtin(id: &str) -> StatusRules {
    let rules = StatusRules::for_harness(id, None);
    assert!(!rules.is_empty(), "{id} has built-in rules");
    rules
}

fn verdict(rules: &StatusRules, title: &str, progress: &str, screen: &[&str]) -> Option<RuleState> {
    let screen = lines(screen);
    rules.evaluate(&StatusInput {
        title,
        progress,
        screen: &screen,
    })
}

#[test]
fn claude_is_working_while_its_title_spins() {
    let claude = builtin("claude");

    assert_eq!(
        verdict(&claude, "\u{2802} Refactor the sidebar", "", &["> "]),
        Some(RuleState::Working)
    );
    assert_eq!(
        verdict(&claude, "\u{25d0} Refactor the sidebar", "", &["> "]),
        Some(RuleState::Working),
        "the newer half-circle spinner too"
    );
}

#[test]
fn claude_is_working_while_its_turn_footer_shows() {
    assert_eq!(
        verdict(
            &builtin("claude"),
            "",
            "",
            &[
                "✻ Thinking… (12s · ↑ 1.2k tokens)",
                "",
                "⏵⏵ accept edits on · esc to interrupt"
            ]
        ),
        Some(RuleState::Working)
    );
}

#[test]
fn claude_is_blocked_on_a_permission_prompt() {
    assert_eq!(
        verdict(
            &builtin("claude"),
            "\u{2733} Claude Code",
            "",
            &[
                "Bash command",
                "  rm -rf target",
                "Do you want to proceed?",
                "❯ 1. Yes",
                "  2. No, and tell Claude what to do differently (esc)",
            ]
        ),
        Some(RuleState::Blocked)
    );
}

#[test]
fn claude_is_blocked_on_a_choice_form() {
    assert_eq!(
        verdict(
            &builtin("claude"),
            "",
            "",
            &[
                "Which approach?",
                "❯ 1. Fast",
                "  2. Thorough",
                "Enter to select · ↑/↓ to navigate · Esc to cancel"
            ]
        ),
        Some(RuleState::Blocked)
    );
}

#[test]
fn claude_is_idle_at_rest() {
    let claude = builtin("claude");

    assert_eq!(
        verdict(
            &claude,
            "\u{2733} Claude Code",
            "",
            &["╭────╮", "│ >  │", "╰────╯"]
        ),
        Some(RuleState::Idle)
    );
    assert_eq!(verdict(&claude, "", "4;0", &[">"]), Some(RuleState::Idle));
}

#[test]
fn codex_reads_its_title() {
    let codex = builtin("codex");

    assert_eq!(
        verdict(&codex, "\u{280b} dispatch", "", &[]),
        Some(RuleState::Working)
    );
    assert_eq!(
        verdict(&codex, "Action Required", "", &[]),
        Some(RuleState::Blocked)
    );
}

#[test]
fn codex_is_blocked_on_its_prompts() {
    let codex = builtin("codex");

    assert_eq!(
        verdict(&codex, "", "", &["Run `cargo test`? [y/n]"]),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        verdict(
            &codex,
            "",
            "",
            &[
                "Allow command?",
                "  cargo build",
                "Press enter to confirm or esc to cancel"
            ]
        ),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        verdict(
            &codex,
            "",
            "",
            &[
                "> You are in /home/me/app",
                "Do you trust the contents of this directory?"
            ]
        ),
        Some(RuleState::Blocked)
    );
}

#[test]
fn codex_is_working_while_its_timer_runs() {
    assert_eq!(
        verdict(
            &builtin("codex"),
            "",
            "",
            &["• Working (12s • esc to interrupt)"]
        ),
        Some(RuleState::Working)
    );
}

#[test]
fn opencode_reads_its_screen() {
    let opencode = builtin("opencode");

    assert_eq!(
        verdict(
            &opencode,
            "",
            "",
            &["△ Permission required", "  edit src/main.rs"]
        ),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        verdict(&opencode, "", "", &["Building… esc to interrupt"]),
        Some(RuleState::Working)
    );
    assert_eq!(
        verdict(&opencode, "", "", &["■■■■⬝⬝⬝⬝"]),
        Some(RuleState::Working)
    );
}

#[test]
fn agy_reads_its_screen() {
    let agy = builtin("agy");

    assert_eq!(
        verdict(
            &agy,
            "",
            "",
            &[
                "Agent is requesting permission for:",
                "  npm install",
                "Do you want to proceed?"
            ]
        ),
        Some(RuleState::Blocked)
    );
    assert_eq!(
        verdict(&agy, "", "", &["⠙ Searching the codebase"]),
        Some(RuleState::Working)
    );
}

#[test]
fn a_harness_files_own_section_replaces_the_built_ins() {
    let own: StatusDef = toml::from_str(
        r#"
        [[rules]]
        state = "working"
        region = "screen"
        contains = ["my marker"]
        "#,
    )
    .expect("the section parses");
    let rules = StatusRules::for_harness("claude", Some(&own));

    assert_eq!(
        verdict(&rules, "\u{2733} Claude Code", "", &[]),
        None,
        "the built-in idle rule is gone"
    );
    assert_eq!(
        verdict(&rules, "", "", &["my marker"]),
        Some(RuleState::Working)
    );
}

#[test]
fn the_registry_hands_out_each_harnesss_rules() {
    let def: crate::HarnessDef = toml::from_str(
        r#"
        id = "custom"
        display_name = "Custom"
        command = "custom"

        [[status.rules]]
        state = "blocked"
        region = "screen"
        contains = ["stop"]
        "#,
    )
    .expect("the harness parses");
    let registry: crate::HarnessRegistry = [def].into_iter().collect();

    let screen = lines(&["stop"]);
    let input = StatusInput {
        screen: &screen,
        ..StatusInput::default()
    };
    assert_eq!(
        registry.status_rules("custom").evaluate(&input),
        Some(RuleState::Blocked)
    );
    assert!(registry.status_rules("unknown").is_empty());
}
