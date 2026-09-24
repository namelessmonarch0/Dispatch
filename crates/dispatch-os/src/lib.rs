//! `dispatch-os`.
//!
//! All platform-specific behaviour lives here. No other crate in the
//! workspace carries a `#[cfg(windows)]`.

pub mod dll;
pub mod host;
pub mod ipc;
pub mod paths;
pub mod process;
pub mod pty;
pub mod signal;

/// Serialises the tests that move the process-wide configuration directory.
///
/// `DISPATCH_CONFIG_DIR` belongs to the process, not to a test, so a test that
/// redirects it changes what every other test resolves -- including between
/// two calls inside one assertion. Every test that sets or reads it holds this
/// first.
#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes [`ENV_LOCK`], ignoring a poisoning left by an unrelated failure.
#[cfg(test)]
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
