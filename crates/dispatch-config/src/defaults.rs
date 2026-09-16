//! The harnesses Dispatch ships with.
//!
//! Written to the harnesses directory on first run, then owned by the user.
//! Dispatch never rewrites a file that already exists, so local edits survive
//! upgrades.

/// A built-in harness: its file stem and TOML body.
pub struct BuiltIn {
    /// File stem, without the `.toml` extension.
    pub id: &'static str,
    /// File contents.
    pub toml: &'static str,
}

/// Every harness written on first run.
pub const BUILT_INS: &[BuiltIn] = &[
    BuiltIn {
        id: "claude",
        toml: include_str!("../harnesses/claude.toml"),
    },
    BuiltIn {
        id: "codex",
        toml: include_str!("../harnesses/codex.toml"),
    },
    BuiltIn {
        id: "agy",
        toml: include_str!("../harnesses/agy.toml"),
    },
    BuiltIn {
        id: "opencode",
        toml: include_str!("../harnesses/opencode.toml"),
    },
];
