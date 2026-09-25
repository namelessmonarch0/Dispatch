//! The rules Dispatch ships for the agents it knows.
//!
//! Adapted from herdr's detection manifests
//! (<https://github.com/ogulcancelik/herdr>, Apache-2.0), simplified to the
//! regions and conditions Dispatch's rules have. Written as TOML — the same
//! format a harness file's `[status]` section uses — so any of these can be
//! copied into that file and edited when an agent's interface moves on.

/// The built-in `[status]` section for harness `id`, as TOML.
pub(super) fn builtin(id: &str) -> Option<&'static str> {
    match id {
        "claude" => Some(CLAUDE),
        "codex" => Some(CODEX),
        "opencode" => Some(OPENCODE),
        "agy" => Some(AGY),
        _ => None,
    }
}

const CLAUDE: &str = r#"
# The title's first glyph spins while Claude Code works: braille through
# 2.1.227, half circles since.
[[status.rules]]
state = "working"
region = "title"
regex = ['^[\x{2800}-\x{28FF}\x{25D0}-\x{25D3}] ']
priority = 1100

# A permission prompt.
[[status.rules]]
state = "blocked"
region = "bottom:15"
contains = ["do you want to proceed?"]
regex = ['(?i)^\s*❯?\s*1\.\s*yes\b']
priority = 990

# A form waiting on a choice.
[[status.rules]]
state = "blocked"
region = "bottom:15"
contains = ["esc to cancel"]
any = ["enter to confirm", "enter to select"]
priority = 980

# The live turn's footer.
[[status.rules]]
state = "working"
region = "bottom:12"
contains = ["esc to interrupt"]
priority = 970

# The live turn's activity line: a star glyph, a verb, an ellipsis.
[[status.rules]]
state = "working"
region = "bottom:12"
regex = ['^\s*[\x{002A}\x{00B7}\x{2722}\x{2733}\x{2736}\x{273B}\x{273D}]\s+\S.*…(?:\s+\(\d+[smh]|\s*$)']
priority = 965

# At rest the title carries a still mark, and progress is cleared.
[[status.rules]]
state = "idle"
region = "title"
regex = ['^\x{2733} ']
priority = 250

[[status.rules]]
state = "idle"
region = "progress"
regex = ['^4;0']
priority = 250
"#;

const CODEX: &str = r#"
[[status.rules]]
state = "blocked"
region = "title"
contains = ["action required"]
priority = 1100

# A braille spinner glyph standing on its own in the title.
[[status.rules]]
state = "working"
region = "title"
regex = ['(?:^| )[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏](?: |$)']
priority = 1050

[[status.rules]]
state = "blocked"
region = "screen"
contains = ["do you trust the contents of this directory?"]
priority = 950

[[status.rules]]
state = "blocked"
region = "bottom:20"
any = [
  "press enter to confirm or esc to cancel",
  "enter to submit answer",
  "enter to submit all",
  "allow command?",
]
priority = 900

[[status.rules]]
state = "blocked"
region = "bottom:20"
any = ["[y/n]", "yes (y)"]
priority = 600

# The running turn's timer.
[[status.rules]]
state = "working"
region = "bottom:12"
regex = ['\((?:[0-9]+[hm] )*[0-9]+s • [^)]*to interrupt\)']
priority = 500
"#;

const OPENCODE: &str = r#"
[[status.rules]]
state = "blocked"
region = "screen"
any = ["△ permission required"]
priority = 300

[[status.rules]]
state = "blocked"
region = "screen"
contains = ["esc dismiss"]
any = ["enter confirm", "enter submit", "enter toggle"]
priority = 290

[[status.rules]]
state = "working"
region = "screen"
any = ["esc to interrupt", "ctrl+c to interrupt", "esc interrupt"]
priority = 110

# The progress bar under a running turn.
[[status.rules]]
state = "working"
region = "screen"
regex = ['(■|⬝){4,}']
priority = 100
"#;

const AGY: &str = r#"
[[status.rules]]
state = "blocked"
region = "screen"
contains = ["requesting permission for:"]
any = ["do you want to proceed?", "edit command"]
priority = 300

# A braille spinner before an "-ing" word.
[[status.rules]]
state = "working"
region = "screen"
regex = ['^\s*[\x{2800}-\x{28FF}]+\s+\p{Alphabetic}+\w*ing\b']
priority = 100

[[status.rules]]
state = "working"
region = "bottom:5"
regex = ['(?i)·\s*[1-9][0-9]*\s+task']
priority = 90
"#;
