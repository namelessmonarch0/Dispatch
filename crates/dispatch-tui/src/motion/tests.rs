//! Tests for tweens and the animation store.

use std::time::{Duration, Instant};

use super::*;

const MS: fn(u64) -> Duration = Duration::from_millis;

#[test]
fn a_tween_runs_from_zero_to_one_and_eases_out() {
    let start = Instant::now();
    let tween = Tween::new(start, MS(100));

    assert_eq!(tween.progress(start), 0.0);
    assert_eq!(tween.progress(start + MS(100)), 1.0);
    assert_eq!(tween.progress(start + MS(500)), 1.0, "held at the end");
    assert!(
        tween.progress(start + MS(50)) > tween.linear(start + MS(50)),
        "eased out: fast first"
    );
    assert!(!tween.done(start + MS(99)));
    assert!(tween.done(start + MS(100)));
}

#[test]
fn a_zero_length_tween_is_already_done() {
    let start = Instant::now();
    assert!(Tween::new(start, Duration::ZERO).done(start));
}

#[test]
fn a_value_runs_from_where_it_started_to_one() {
    let start = Instant::now();
    let mut animations = Animations::new(true);
    animations.start('a', start, MS(100), 0.5);

    assert_eq!(animations.value('a', start), Some(0.5));
    assert_eq!(animations.value('a', start + MS(100)), Some(1.0));
    assert_eq!(animations.value('b', start), None, "nothing runs on it");
}

#[test]
fn starting_again_replaces_the_running_one() {
    let start = Instant::now();
    let mut animations = Animations::new(true);
    animations.start('a', start, MS(100), 0.0);
    let halfway = animations.value('a', start + MS(50)).expect("running");

    animations.start('a', start + MS(50), MS(100), halfway);
    assert_eq!(
        animations.value('a', start + MS(50)),
        Some(halfway),
        "it continues from where it had got to"
    );
}

#[test]
fn finished_animations_are_swept_up() {
    let start = Instant::now();
    let mut animations = Animations::new(true);
    animations.start('a', start, MS(100), 0.0);
    animations.start('b', start, MS(300), 0.0);

    assert!(animations.active(start + MS(200)));
    assert_eq!(animations.sweep(start + MS(200)), vec!['a']);
    assert_eq!(animations.value('a', start + MS(200)), None);
    assert_eq!(animations.sweep(start + MS(400)), vec!['b']);
    assert!(!animations.active(start + MS(400)));
}

#[test]
fn with_motion_off_nothing_starts() {
    let start = Instant::now();
    let mut animations = Animations::new(false);
    animations.start('a', start, MS(100), 0.0);

    assert_eq!(animations.value('a', start), None);
    assert!(!animations.active(start));
}
