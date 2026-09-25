//! Tests for the activity tracker.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dispatch_config::status::{StatusDef, StatusRules};
use dispatch_pty::Signals;

use super::*;

fn rules(text: &str) -> Arc<StatusRules> {
    let def: StatusDef = toml::from_str(text).expect("the rules parse");
    Arc::new(StatusRules::compile("test", &def))
}

fn none() -> Arc<StatusRules> {
    Arc::new(StatusRules::default())
}

const MS: fn(u64) -> Duration = Duration::from_millis;

#[test]
fn the_first_evaluation_reports_at_once() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());

    assert_eq!(tracker.evaluate(start, &[]), Some(Verdict::Idle));
    assert_eq!(
        tracker.evaluate(start + MS(300), &[]),
        None,
        "no change, no report"
    );
}

#[test]
fn output_makes_a_pane_working() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());
    tracker.evaluate(start, &[]);

    tracker.output(start + MS(10));
    assert_eq!(
        tracker.evaluate(start + MS(20), &[]),
        Some(Verdict::Working)
    );
}

#[test]
fn quiet_makes_it_idle_only_once_it_has_settled() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());
    tracker.output(start);
    assert_eq!(tracker.evaluate(start, &[]), Some(Verdict::Working));

    // A second of quiet makes the raw verdict idle...
    assert_eq!(tracker.evaluate(start + MS(1100), &[]), None);
    // ...which is reported only once it has held for the settling time.
    assert_eq!(tracker.evaluate(start + MS(1500), &[]), None);
    assert_eq!(tracker.evaluate(start + MS(1800), &[]), Some(Verdict::Idle));
}

#[test]
fn output_while_settling_keeps_it_working() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());
    tracker.output(start);
    tracker.evaluate(start, &[]);

    tracker.evaluate(start + MS(1100), &[]);
    tracker.output(start + MS(1300));
    assert_eq!(
        tracker.evaluate(start + MS(1900), &[]),
        None,
        "still working"
    );

    // The clock restarts from the last output.
    assert_eq!(tracker.evaluate(start + MS(2400), &[]), None);
    assert_eq!(tracker.evaluate(start + MS(3100), &[]), Some(Verdict::Idle));
}

#[test]
fn the_echo_of_input_is_not_activity() {
    let start = Instant::now();
    let mut tracker = Tracker::new(none());
    tracker.evaluate(start, &[]);

    tracker.input(start + MS(100));
    tracker.output(start + MS(140));
    assert_eq!(tracker.evaluate(start + MS(200), &[]), None, "still idle");

    tracker.output(start + MS(400));
    assert_eq!(
        tracker.evaluate(start + MS(410), &[]),
        Some(Verdict::Working),
        "output long after the keystroke is the program's own"
    );
}

#[test]
fn blocked_is_reported_at_once_and_left_at_once() {
    let start = Instant::now();
    let mut tracker = Tracker::new(rules(
        r#"
        [[rules]]
        state = "blocked"
        region = "screen"
        contains = ["proceed?"]
        "#,
    ));
    tracker.output(start);
    tracker.evaluate(start, &[]);

    let prompt = vec!["Do you want to proceed?".to_string()];
    assert_eq!(
        tracker.evaluate(start + MS(50), &prompt),
        Some(Verdict::Blocked)
    );
    assert_eq!(
        tracker.evaluate(start + MS(1500), &[]),
        Some(Verdict::Idle),
        "leaving blocked is not damped"
    );
}

#[test]
fn a_working_rule_holds_without_output() {
    let start = Instant::now();
    let mut tracker = Tracker::new(rules(
        r#"
        [[rules]]
        state = "working"
        region = "title"
        regex = ['^\x{280b} ']
        "#,
    ));
    tracker.signals(&Signals {
        title: Some("\u{280b} thinking".to_string()),
        ..Signals::default()
    });

    assert_eq!(tracker.evaluate(start, &[]), Some(Verdict::Working));
    assert_eq!(
        tracker.evaluate(start + MS(5000), &[]),
        None,
        "still working"
    );
}

#[test]
fn an_idle_rule_with_fresh_output_stays_working() {
    let start = Instant::now();
    let mut tracker = Tracker::new(rules(
        r#"
        [[rules]]
        state = "idle"
        region = "screen"
        contains = [">"]
        "#,
    ));
    tracker.output(start);

    assert_eq!(
        tracker.evaluate(start + MS(10), &[">".to_string()]),
        Some(Verdict::Working),
        "a prompt on screen does not outrank output arriving"
    );
}

#[test]
fn progress_and_title_are_kept_until_replaced() {
    let start = Instant::now();
    let mut tracker = Tracker::new(rules(
        r#"
        [[rules]]
        state = "idle"
        region = "progress"
        regex = ['^4;0']
        "#,
    ));
    tracker.signals(&Signals {
        progress: Some("4;0".to_string()),
        ..Signals::default()
    });
    tracker.signals(&Signals::default());

    assert_eq!(tracker.evaluate(start, &[]), Some(Verdict::Idle));
}
