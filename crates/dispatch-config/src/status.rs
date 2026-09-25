//! Rules that read what a pane is doing off its screen.
//!
//! An agent's terminal says whether it is busy — a spinner in the title, an
//! "esc to interrupt" footer — and whether it is waiting on the user — a
//! permission prompt. Each harness carries rules that recognise those, in its
//! file's `[status]` section or, when it has none, the built-in set for its
//! id. The approach, and the built-in rules, follow herdr's.

use regex::Regex;
use serde::{Deserialize, Serialize};

mod builtin;

/// What a rule says a pane is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleState {
    /// Busy.
    Working,
    /// Waiting for the next thing to do.
    Idle,
    /// Waiting on a decision only the user can make.
    Blocked,
}

/// One rule, as a harness file writes it.
///
/// `state` and `region` are strings rather than enums so a value this build
/// does not know skips the one rule instead of failing the whole file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleDef {
    /// `working`, `idle` or `blocked`.
    pub state: String,
    /// `title`, `progress`, `bottom:N` or `screen`.
    pub region: String,
    /// Every one must appear, case-insensitively.
    pub contains: Vec<String>,
    /// At least one must appear, case-insensitively.
    pub any: Vec<String>,
    /// None may appear, case-insensitively.
    pub not: Vec<String>,
    /// At least one must match some line of the region.
    pub regex: Vec<String>,
    /// Higher is tried first; ties keep file order.
    pub priority: i32,
}

/// A harness file's `[status]` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusDef {
    /// The rules, in file order.
    pub rules: Vec<RuleDef>,
}

/// Where on the terminal a rule looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Title,
    Progress,
    Bottom(usize),
    Screen,
}

impl Region {
    fn parse(text: &str) -> Option<Region> {
        match text {
            "title" => Some(Region::Title),
            "progress" => Some(Region::Progress),
            "screen" => Some(Region::Screen),
            _ => text
                .strip_prefix("bottom:")
                .and_then(|count| count.parse().ok())
                .filter(|count| *count > 0)
                .map(Region::Bottom),
        }
    }
}

/// A rule ready to match.
#[derive(Debug, Clone)]
struct Rule {
    state: RuleState,
    region: Region,
    contains: Vec<String>,
    any: Vec<String>,
    not: Vec<String>,
    regex: Vec<Regex>,
    priority: i32,
}

/// What rules are matched against.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusInput<'a> {
    /// The raw title the program last set.
    pub title: &'a str,
    /// The last `OSC 9;4` payload, after `9;`.
    pub progress: &'a str,
    /// The live screen, top to bottom.
    pub screen: &'a [String],
}

/// A harness's rules, compiled, highest priority first.
#[derive(Debug, Clone, Default)]
pub struct StatusRules {
    rules: Vec<Rule>,
}

impl StatusRules {
    /// Compiles `def`'s rules for harness `id`.
    ///
    /// A rule that cannot be used — a regex that does not compile, a state or
    /// region this build does not know, or no positive condition at all — is
    /// logged and left out. The harness still loads with the rest: one bad
    /// pattern should cost that pattern, not the agent's whole status.
    #[must_use]
    pub fn compile(id: &str, def: &StatusDef) -> StatusRules {
        let mut rules: Vec<Rule> = def
            .rules
            .iter()
            .enumerate()
            .filter_map(|(index, rule)| match compile_rule(rule) {
                Ok(rule) => Some(rule),
                Err(reason) => {
                    tracing::warn!(harness = id, rule = index, %reason, "skipping a status rule");
                    None
                }
            })
            .collect();

        // Stable, so equal priorities keep the order the file gave them.
        rules.sort_by_key(|rule| std::cmp::Reverse(rule.priority));
        StatusRules { rules }
    }

    /// The rules harness `id` gets: its file's own `[status]` when it has
    /// one, else the built-in set for its id, else none.
    ///
    /// An empty `[status]` counts as having one: it is how a user says "go by
    /// activity alone" for an agent whose built-ins misread it.
    #[must_use]
    pub fn for_harness(id: &str, own: Option<&StatusDef>) -> StatusRules {
        if let Some(own) = own {
            return StatusRules::compile(id, own);
        }

        #[derive(Deserialize)]
        struct File {
            status: StatusDef,
        }

        builtin::builtin(id)
            .and_then(|text| match toml::from_str::<File>(text) {
                Ok(file) => Some(StatusRules::compile(id, &file.status)),
                Err(error) => {
                    tracing::error!(harness = id, %error, "built-in status rules do not parse");
                    None
                }
            })
            .unwrap_or_default()
    }

    /// Whether there are no rules, so activity alone decides.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The state the first matching rule names, trying the highest priority
    /// first; `None` when nothing matches.
    #[must_use]
    pub fn evaluate(&self, input: &StatusInput<'_>) -> Option<RuleState> {
        self.rules
            .iter()
            .find(|rule| rule.matches(input))
            .map(|rule| rule.state)
    }
}

impl Rule {
    fn matches(&self, input: &StatusInput<'_>) -> bool {
        let lines = region_lines(self.region, input);
        let text = lines.join("\n").to_lowercase();

        self.contains
            .iter()
            .all(|needle| text.contains(needle.as_str()))
            && (self.any.is_empty() || self.any.iter().any(|needle| text.contains(needle.as_str())))
            && !self.not.iter().any(|needle| text.contains(needle.as_str()))
            && (self.regex.is_empty()
                || self
                    .regex
                    .iter()
                    .any(|pattern| lines.iter().any(|line| pattern.is_match(line))))
    }
}

/// The lines a region covers.
fn region_lines<'a>(region: Region, input: &StatusInput<'a>) -> Vec<&'a str> {
    match region {
        Region::Title => vec![input.title],
        Region::Progress => vec![input.progress],
        Region::Screen => input.screen.iter().map(String::as_str).collect(),
        Region::Bottom(count) => {
            let mut kept: Vec<&str> = input
                .screen
                .iter()
                .rev()
                .map(String::as_str)
                .filter(|line| !line.trim().is_empty())
                .take(count)
                .collect();
            kept.reverse();
            kept
        }
    }
}

/// Turns one rule as written into one ready to match, or says why not.
fn compile_rule(def: &RuleDef) -> Result<Rule, String> {
    let state = match def.state.as_str() {
        "working" => RuleState::Working,
        "idle" => RuleState::Idle,
        "blocked" => RuleState::Blocked,
        other => return Err(format!("unknown state {other:?}")),
    };
    let region =
        Region::parse(&def.region).ok_or_else(|| format!("unknown region {:?}", def.region))?;

    let lower = |values: &[String]| -> Vec<String> {
        values
            .iter()
            .filter(|value| !value.is_empty())
            .map(|value| value.to_lowercase())
            .collect()
    };
    let contains = lower(&def.contains);
    let any = lower(&def.any);
    let not = lower(&def.not);
    let regex = def
        .regex
        .iter()
        .map(|pattern| Regex::new(pattern).map_err(|error| format!("bad regex: {error}")))
        .collect::<Result<Vec<_>, _>>()?;

    if contains.is_empty() && any.is_empty() && regex.is_empty() {
        return Err("no condition to match on".to_string());
    }

    Ok(Rule {
        state,
        region,
        contains,
        any,
        not,
        regex,
        priority: def.priority,
    })
}

#[cfg(test)]
mod tests;
