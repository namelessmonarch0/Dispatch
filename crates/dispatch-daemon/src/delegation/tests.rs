//! Tests for the rules that refuse a request without asking anyone.

use super::*;

fn limits() -> DelegationLimits {
    DelegationLimits::default()
}

#[test]
fn a_request_within_the_caps_is_not_refused() {
    assert_eq!(refusal(0, 0, limits(), true, "claude"), None);
}

#[test]
fn a_subagent_cannot_delegate_at_the_default_depth() {
    let reason = refusal(1, 0, limits(), true, "claude").expect("depth 1 is the cap");
    assert!(
        reason.contains("depth"),
        "the reason should say what stopped it, got {reason:?}"
    );
}

#[test]
fn a_parent_is_capped_on_how_many_run_at_once() {
    let reason = refusal(0, 4, limits(), true, "claude").expect("four live is the cap");
    assert!(
        reason.contains('4'),
        "the reason should name the cap, got {reason:?}"
    );

    assert_eq!(refusal(0, 3, limits(), true, "claude"), None);
}

#[test]
fn a_harness_with_no_task_form_is_refused_by_name() {
    let reason = refusal(0, 0, limits(), false, "agy").expect("no task form");
    assert!(
        reason.contains("agy") && reason.contains("[task]"),
        "the reason should name the harness and what it lacks, got {reason:?}"
    );
}

#[test]
fn raised_caps_are_honoured() {
    let generous = DelegationLimits {
        max_depth: 2,
        max_live_per_parent: 8,
        request_timeout_secs: 60,
    };

    assert_eq!(refusal(1, 7, generous, true, "claude"), None);
    assert!(refusal(2, 0, generous, true, "claude").is_some());
}
