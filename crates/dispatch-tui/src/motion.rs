//! Time-based motion: a tween, and a store of running ones keyed by what they
//! animate.
//!
//! Everything that moves reads its progress from here at draw time, so one
//! clock drives every animation and nothing keeps state of its own between
//! frames.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// How often to draw while something is tweening: about thirty frames a
/// second, smooth for a 150 ms ease without redrawing at the terminal's
/// full rate.
pub const TWEEN_FRAME: Duration = Duration::from_millis(33);

/// How often to draw while a spinner is on screen: one braille frame each.
pub const SPIN_FRAME: Duration = Duration::from_millis(100);

/// One stretch of time, and how far through it a moment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tween {
    start: Instant,
    duration: Duration,
}

impl Tween {
    /// A tween starting at `start` and lasting `duration`.
    #[must_use]
    pub fn new(start: Instant, duration: Duration) -> Self {
        Self { start, duration }
    }

    /// How far through it `now` is, evenly: 0.0 at the start, 1.0 at and
    /// after the end.
    #[must_use]
    pub fn linear(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(self.start).as_secs_f32();
        (elapsed / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// How far through it `now` is, eased out: quick to start, gentle to
    /// land, which is how something moving under its own momentum reads.
    #[must_use]
    pub fn progress(&self, now: Instant) -> f32 {
        1.0 - (1.0 - self.linear(now)).powi(3)
    }

    /// Whether it has run its course.
    #[must_use]
    pub fn done(&self, now: Instant) -> bool {
        self.linear(now) >= 1.0
    }
}

/// Running tweens, one per key.
///
/// Starting a tween on a key that already has one replaces it; the caller
/// passes the value the old one had reached as the new one's `from`, so a
/// quick succession of changes continues smoothly rather than queuing or
/// snapping back.
#[derive(Debug)]
pub struct Animations<K> {
    enabled: bool,
    running: HashMap<K, (Tween, f32)>,
}

impl<K: Copy + Eq + Hash> Animations<K> {
    /// A store that animates when `enabled`, and otherwise starts nothing, so
    /// every change is shown at once.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            running: HashMap::new(),
        }
    }

    /// Turns motion on or off. Off, whatever runs stops where it is.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.running.clear();
        }
    }

    /// Whether motion is on.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Animates `key` from `from` (0.0-1.0, the value it shows now) to 1.0
    /// over `duration`.
    pub fn start(&mut self, key: K, now: Instant, duration: Duration, from: f32) {
        if self.enabled {
            self.running
                .insert(key, (Tween::new(now, duration), from.clamp(0.0, 1.0)));
        }
    }

    /// Stops animating `key`.
    pub fn stop(&mut self, key: K) {
        self.running.remove(&key);
    }

    /// Where `key` has got to, eased; `None` when nothing runs on it, which
    /// the caller reads as "at rest".
    #[must_use]
    pub fn value(&self, key: K, now: Instant) -> Option<f32> {
        self.running
            .get(&key)
            .map(|(tween, from)| from + (1.0 - from) * tween.progress(now))
    }

    /// How far through `key`'s tween `now` is, evenly, for an animation with
    /// a shape of its own.
    #[must_use]
    pub fn linear(&self, key: K, now: Instant) -> Option<f32> {
        self.running.get(&key).map(|(tween, _)| tween.linear(now))
    }

    /// Whether anything is still moving at `now`.
    #[must_use]
    pub fn active(&self, now: Instant) -> bool {
        self.running.values().any(|(tween, _)| !tween.done(now))
    }

    /// Whether the store holds nothing: no tween moving, and none finished
    /// but not yet swept.
    ///
    /// A finished tween still owes a frame: the one that sweeps it up and so
    /// draws what it animated at rest.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// Forgets the tweens that have finished by `now`, returning their keys.
    pub fn sweep(&mut self, now: Instant) -> Vec<K> {
        let finished: Vec<K> = self
            .running
            .iter()
            .filter(|(_, (tween, _))| tween.done(now))
            .map(|(key, _)| *key)
            .collect();
        for key in &finished {
            self.running.remove(key);
        }
        finished
    }
}

#[cfg(test)]
mod tests;
