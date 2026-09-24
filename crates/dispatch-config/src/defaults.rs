//! The harnesses Dispatch ships with.
//!
//! Written to the harnesses directory on first run, then owned by the user.
//! Dispatch rewrites a file that already exists only while it is still exactly
//! as an earlier Dispatch wrote it, so local edits survive upgrades and fixes
//! to untouched files still arrive.

/// A built-in harness: its file stem and TOML body.
pub struct BuiltIn {
    /// File stem, without the `.toml` extension.
    pub id: &'static str,
    /// File contents.
    pub toml: &'static str,
    /// Every body an earlier Dispatch wrote for this file.
    ///
    /// A file still exactly one of these was never edited, so it is
    /// replaced with the current body: an unsafe form must not live on in
    /// every installation made before it was fixed. A file that differs
    /// from all of them is the user's, and is left alone.
    pub superseded: &'static [&'static str],
}

/// Every harness written on first run.
pub const BUILT_INS: &[BuiltIn] = &[
    BuiltIn {
        id: "claude",
        toml: include_str!("../harnesses/claude.toml"),
        superseded: &[include_str!("../harnesses/superseded/claude-1.toml")],
    },
    BuiltIn {
        id: "codex",
        toml: include_str!("../harnesses/codex.toml"),
        superseded: &[include_str!("../harnesses/superseded/codex-1.toml")],
    },
    BuiltIn {
        id: "agy",
        toml: include_str!("../harnesses/agy.toml"),
        superseded: &[],
    },
    BuiltIn {
        id: "opencode",
        toml: include_str!("../harnesses/opencode.toml"),
        superseded: &[],
    },
];
