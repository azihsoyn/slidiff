//! Deck format: what an agent writes, what the viewer reads.
//!
//! The format holds no code. Steps point at `file:line`; the viewer renders
//! the real hunk from the repository at display time. Length limits live in
//! the JSON Schema (maxLength / maxItems) so oversized prose is refused at
//! the format level, and again in [`Deck::validate`] with errors that say
//! exactly what to cut.

use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// A deck may not exceed this many steps. One claim per screen only works
/// if the whole thing stays readable in one sitting.
pub const MAX_STEPS: usize = 12;
/// A claim may not exceed this many characters (Unicode scalar values,
/// same unit as JSON Schema `maxLength`).
pub const MAX_CLAIM_CHARS: usize = 120;
/// A cover may not carry more bullets than this.
pub const MAX_BULLETS: usize = 3;
/// Deck title and cover `what` share the claim limit.
pub const MAX_TITLE_CHARS: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Deck {
    /// One line naming what this deck reports on.
    #[schemars(length(max = 120))]
    pub title: String,
    /// Git rev to diff against (e.g. "main", "HEAD~3"). Defaults to HEAD:
    /// uncommitted changes against the last commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The slides. At most 12 — the schema refuses more.
    #[schemars(length(min = 1, max = 12))]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// Title slide: what happened, in at most three bullets.
    Cover {
        /// One line stating what was done.
        #[schemars(length(max = 120))]
        what: String,
        /// Up to three supporting lines.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[schemars(length(max = 3), inner(length(max = 120)))]
        bullets: Vec<String>,
    },
    /// One claim about one place. The viewer shows the hunk at `at`.
    Point {
        at: Location,
        #[schemars(length(max = 120))]
        claim: String,
    },
    /// Old and new side by side for the hunk at `at`.
    BeforeAfter {
        at: Location,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 120))]
        claim: Option<String>,
    },
    /// The file as it is now, centered on `at` — for reading context,
    /// not the change itself.
    Zoom {
        at: Location,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 120))]
        claim: Option<String>,
    },
    /// Something that could go wrong, anchored to where it lives.
    Risk {
        at: Location,
        #[schemars(length(max = 120))]
        claim: String,
        severity: Severity,
    },
    /// Overview of every file the diff touches. Drawn from the repo;
    /// carries no content of its own.
    Map,
}

impl Step {
    pub fn type_name(&self) -> &'static str {
        match self {
            Step::Cover { .. } => "cover",
            Step::Point { .. } => "point",
            Step::BeforeAfter { .. } => "before_after",
            Step::Zoom { .. } => "zoom",
            Step::Risk { .. } => "risk",
            Step::Map => "map",
        }
    }

    pub fn location(&self) -> Option<&Location> {
        match self {
            Step::Point { at, .. }
            | Step::BeforeAfter { at, .. }
            | Step::Zoom { at, .. }
            | Step::Risk { at, .. } => Some(at),
            Step::Cover { .. } | Step::Map => None,
        }
    }

    pub fn claim(&self) -> Option<&str> {
        match self {
            Step::Point { claim, .. } | Step::Risk { claim, .. } => Some(claim),
            Step::BeforeAfter { claim, .. } | Step::Zoom { claim, .. } => claim.as_deref(),
            Step::Cover { .. } | Step::Map => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Low => write!(f, "low"),
            Severity::Medium => write!(f, "medium"),
            Severity::High => write!(f, "high"),
        }
    }
}

/// A place in the repository: `path/to/file.rs:42`. The line refers to the
/// new side of the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: String,
    pub line: u32,
}

impl FromStr for Location {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((file, line)) = s.rsplit_once(':') else {
            return Err(format!("\"{s}\" is missing \":<line>\" — write file:line"));
        };
        if file.is_empty() {
            return Err(format!("\"{s}\" has no file before the colon"));
        }
        let line: u32 = line
            .parse()
            .map_err(|_| format!("\"{s}\" — \"{line}\" is not a line number"))?;
        if line == 0 {
            return Err(format!("\"{s}\" — lines start at 1"));
        }
        Ok(Location {
            file: file.to_string(),
            line,
        })
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}

impl Serialize for Location {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Location {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Location {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Location".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^.+:[1-9][0-9]*$",
            "description": "A place in the repository, written file:line (line on the new side of the diff), e.g. \"src/main.rs:42\"."
        })
    }
}

impl Deck {
    /// Check everything serde cannot: character limits, step count, bullet
    /// count. Returns every violation at once, each phrased so the writer
    /// knows exactly what to cut.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        check_len("title", &self.title, MAX_TITLE_CHARS, &mut errors);

        if self.steps.is_empty() {
            errors.push("steps: empty — a deck needs at least one step".to_string());
        }
        if self.steps.len() > MAX_STEPS {
            errors.push(format!(
                "steps: {} steps, limit is {} — merge or cut {}",
                self.steps.len(),
                MAX_STEPS,
                self.steps.len() - MAX_STEPS
            ));
        }

        for (i, step) in self.steps.iter().enumerate() {
            let at = |field: &str| format!("steps[{i}] ({}).{field}", step.type_name());
            match step {
                Step::Cover { what, bullets } => {
                    check_len(&at("what"), what, MAX_CLAIM_CHARS, &mut errors);
                    if bullets.len() > MAX_BULLETS {
                        errors.push(format!(
                            "{}: {} bullets, limit is {} — cut {}",
                            at("bullets"),
                            bullets.len(),
                            MAX_BULLETS,
                            bullets.len() - MAX_BULLETS
                        ));
                    }
                    for (j, b) in bullets.iter().enumerate() {
                        check_len(
                            &format!("steps[{i}] (cover).bullets[{j}]"),
                            b,
                            MAX_CLAIM_CHARS,
                            &mut errors,
                        );
                    }
                }
                Step::Point { claim, .. } | Step::Risk { claim, .. } => {
                    check_len(&at("claim"), claim, MAX_CLAIM_CHARS, &mut errors);
                }
                Step::BeforeAfter { claim, .. } | Step::Zoom { claim, .. } => {
                    if let Some(claim) = claim {
                        check_len(&at("claim"), claim, MAX_CLAIM_CHARS, &mut errors);
                    }
                }
                Step::Map => {}
            }
        }

        errors
    }
}

fn check_len(field: &str, value: &str, max: usize, errors: &mut Vec<String>) {
    let n = value.chars().count();
    if n > max {
        errors.push(format!(
            "{field}: {n} chars, limit is {max} — cut {}",
            n - max
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_parses_file_and_line() {
        let loc: Location = "src/main.rs:42".parse().unwrap();
        assert_eq!(loc.file, "src/main.rs");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn location_allows_colons_in_path() {
        let loc: Location = "weird:name.rs:7".parse().unwrap();
        assert_eq!(loc.file, "weird:name.rs");
        assert_eq!(loc.line, 7);
    }

    #[test]
    fn location_rejects_missing_line() {
        assert!("src/main.rs".parse::<Location>().is_err());
        assert!("src/main.rs:".parse::<Location>().is_err());
        assert!("src/main.rs:0".parse::<Location>().is_err());
        assert!(":42".parse::<Location>().is_err());
    }

    #[test]
    fn validate_reports_how_much_to_cut() {
        let deck = Deck {
            title: "t".repeat(130),
            base: None,
            steps: vec![Step::Point {
                at: "a.rs:1".parse().unwrap(),
                claim: "c".repeat(125),
            }],
        };
        let errors = deck.validate();
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("cut 10"), "{}", errors[0]);
        assert!(errors[1].contains("cut 5"), "{}", errors[1]);
    }

    #[test]
    fn validate_counts_japanese_by_chars_not_bytes() {
        let deck = Deck {
            title: "あ".repeat(120),
            base: None,
            steps: vec![Step::Map],
        };
        assert!(deck.validate().is_empty());
    }

    #[test]
    fn validate_refuses_thirteenth_step() {
        let deck = Deck {
            title: "t".into(),
            base: None,
            steps: vec![Step::Map; 13],
        };
        let errors = deck.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("merge or cut 1"), "{}", errors[0]);
    }
}
