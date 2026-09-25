//! The rules Dispatch ships for the agents it knows.

/// The built-in `[status]` section for harness `id`, as TOML.
pub(super) fn builtin(_id: &str) -> Option<&'static str> {
    None
}
