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
