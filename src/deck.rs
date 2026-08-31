//! Deck format: what an agent writes, what the viewer reads.
//!
//! The format holds no code. Steps point at a `file:start-end` range; the
//! viewer renders those lines from the repository at display time. Every
//! limit that keeps a slide readable lives in the JSON Schema (maxLength /
//! maxItems) and again in [`Deck::validate`] with errors that say exactly
//! what to cut: text lengths, step count, and — the one that matters most —
//! the excerpt span. A slide can show at most [`MAX_SPAN_LINES`] lines of
//! code, so pointing at a whole block instead of the lines that matter is
//! refused, not discouraged.

use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

// Deck length is deliberately unbounded: per-slide limits keep each
// screen readable, and deck length scales with the change it reports.
// The filmstrip and the coverage meter keep a long deck navigable.
/// A claim is one short line above the code, not a paragraph.
pub const MAX_CLAIM_CHARS: usize = 80;
/// A note hangs off a single line; it must fit next to code.
pub const MAX_NOTE_CHARS: usize = 48;
/// Notes per step. More than this stops being pointing and starts being prose.
pub const MAX_NOTES: usize = 3;
/// A cover may not carry more bullets than this.
pub const MAX_BULLETS: usize = 3;
/// Bullets are shorter than claims.
pub const MAX_BULLET_CHARS: usize = 60;
/// An excerpt shows at most this many lines. Choosing which lines matter
/// is the writer's job; the format refuses a whole block.
pub const MAX_SPAN_LINES: u32 = 16;
/// When `at` names a single line, the viewer shows this many lines around it.
pub const DEFAULT_CONTEXT: u32 = 4;
/// Map groups: the summary of touched files, written by the agent.
pub const MAX_GROUPS: usize = 6;
pub const MAX_GROUP_LABEL_CHARS: usize = 24;
/// Speaker notes: the one place prose is allowed. Off-slide, reader-opt-in.
pub const MAX_SPEAKER_NOTES_CHARS: usize = 600;
/// Deck title shares the claim limit.
pub const MAX_TITLE_CHARS: usize = 80;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Deck {
    /// One line naming what this deck reports on.
    #[schemars(length(max = 80))]
    pub title: String,
    /// Git rev to diff against (e.g. "main", "HEAD~3"). Defaults to HEAD:
    /// uncommitted changes against the last commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The slides. As many as the change needs — length limits live on
    /// each slide, not on the deck.
    #[schemars(length(min = 1))]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// Title slide: what happened, in at most three bullets.
    Cover {
        /// One line stating what was done.
        #[schemars(length(max = 80))]
        what: String,
        /// Up to three supporting lines.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[schemars(length(max = 3), inner(length(max = 60)))]
        bullets: Vec<String>,
        /// Longer prose for the reader who wants the detail. Shown below
        /// the slide, never on it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 600))]
        speaker_notes: Option<String>,
    },
    /// One claim about one excerpt. The viewer shows the chosen lines —
    /// diff-aware when they changed, plain code when they did not — with
    /// notes hanging off individual lines.
    Point {
        at: Anchor,
        #[schemars(length(max = 80))]
        claim: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[schemars(length(max = 3))]
        notes: Vec<Note>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 600))]
        speaker_notes: Option<String>,
    },
    /// Old and new side by side for the excerpt at `at`.
    BeforeAfter {
        at: Anchor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 80))]
        claim: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 600))]
        speaker_notes: Option<String>,
    },
    /// A point that could go wrong, with a severity.
    Risk {
        at: Anchor,
        #[schemars(length(max = 80))]
        claim: String,
        severity: Severity,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[schemars(length(max = 3))]
        notes: Vec<Note>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 600))]
        speaker_notes: Option<String>,
    },
    /// The touched files, summarized by the writer into named groups.
    /// Files not covered by any group aggregate into an automatic rest row.
    /// Counts are drawn from the repo at display time.
    Map {
        #[schemars(length(min = 1, max = 6))]
        groups: Vec<Group>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 600))]
        speaker_notes: Option<String>,
    },
}

/// One line of explanation attached to one line of code.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Note {
    /// Line number (new side) the note hangs off. Must fall inside the
    /// step's range.
    pub line: u32,
    #[schemars(length(max = 48))]
    pub text: String,
}

/// A named group of files for the map step.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Group {
    #[schemars(length(max = 24))]
    pub label: String,
    /// Repo-relative paths, as they appear in the diff.
    #[schemars(length(min = 1))]
    pub files: Vec<String>,
}

impl Step {
    pub fn type_name(&self) -> &'static str {
        match self {
            Step::Cover { .. } => "cover",
            Step::Point { .. } => "point",
            Step::BeforeAfter { .. } => "before_after",
            Step::Risk { .. } => "risk",
            Step::Map { .. } => "map",
        }
    }

    pub fn anchor(&self) -> Option<&Anchor> {
        match self {
            Step::Point { at, .. } | Step::BeforeAfter { at, .. } | Step::Risk { at, .. } => {
                Some(at)
            }
            Step::Cover { .. } | Step::Map { .. } => None,
        }
    }

    pub fn notes(&self) -> &[Note] {
        match self {
            Step::Point { notes, .. } | Step::Risk { notes, .. } => notes,
            _ => &[],
        }
    }

    pub fn speaker_notes(&self) -> Option<&str> {
        match self {
            Step::Cover { speaker_notes, .. }
            | Step::Point { speaker_notes, .. }
            | Step::BeforeAfter { speaker_notes, .. }
            | Step::Risk { speaker_notes, .. }
            | Step::Map { speaker_notes, .. } => speaker_notes.as_deref(),
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

/// A place in the repository: `path/to/file.rs:42` (one line, shown with
/// [`DEFAULT_CONTEXT`] lines around it) or `path/to/file.rs:42-57` (an
/// explicit range, at most [`MAX_SPAN_LINES`] lines). Lines refer to the
/// new side of the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub file: String,
    pub start: u32,
    /// Explicit range end, inclusive. None for a single-line anchor.
    pub end: Option<u32>,
}

impl Anchor {
    /// The line range to display, before clamping to the file. Single-line
    /// anchors expand by [`DEFAULT_CONTEXT`] on both sides.
    pub fn range(&self) -> (u32, u32) {
        match self.end {
            Some(end) => (self.start, end),
            None => (
                self.start.saturating_sub(DEFAULT_CONTEXT).max(1),
                self.start + DEFAULT_CONTEXT,
            ),
        }
    }

    /// The line a note or highlight anchors to.
    pub fn focus(&self) -> u32 {
        self.start
    }
}

impl FromStr for Anchor {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((file, lines)) = s.rsplit_once(':') else {
            return Err(format!(
                "\"{s}\" is missing \":<line>\" — write file:line or file:start-end"
            ));
        };
        if file.is_empty() {
            return Err(format!("\"{s}\" has no file before the colon"));
        }
        let parse_no = |part: &str| -> Result<u32, String> {
            let n: u32 = part
                .parse()
                .map_err(|_| format!("\"{s}\" — \"{part}\" is not a line number"))?;
            if n == 0 {
                return Err(format!("\"{s}\" — lines start at 1"));
            }
            Ok(n)
        };
        let (start, end) = match lines.split_once('-') {
            Some((a, b)) => (parse_no(a)?, Some(parse_no(b)?)),
            None => (parse_no(lines)?, None),
        };
        if let Some(end) = end
            && end < start {
                return Err(format!("\"{s}\" — range end {end} is before start {start}"));
            }
        Ok(Anchor {
            file: file.to_string(),
            start,
            end,
        })
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.end {
            Some(end) => write!(f, "{}:{}-{}", self.file, self.start, end),
            None => write!(f, "{}:{}", self.file, self.start),
        }
    }
}

impl Serialize for Anchor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Anchor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Anchor {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Anchor".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^.+:[1-9][0-9]*(-[1-9][0-9]*)?$",
            "description": "An excerpt in the repository: \"src/main.rs:42\" (one line plus context) or \"src/main.rs:42-57\" (explicit range, at most 16 lines). Lines are on the new side of the diff."
        })
    }
}

impl Deck {
    /// Check everything serde cannot: character limits, step count, span,
    /// note anchoring. Returns every violation at once, each phrased so the
    /// writer knows exactly what to cut.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        check_len("title", &self.title, MAX_TITLE_CHARS, &mut errors);

        if self.steps.is_empty() {
            errors.push("steps: empty — a deck needs at least one step".to_string());
        }

        for (i, step) in self.steps.iter().enumerate() {
            let at = |field: &str| format!("steps[{i}] ({}).{field}", step.type_name());
            if let Some(sn) = step.speaker_notes() {
                check_len(&at("speaker_notes"), sn, MAX_SPEAKER_NOTES_CHARS, &mut errors);
            }
            if let Some(anchor) = step.anchor() {
                if let Some(end) = anchor.end {
                    let span = end - anchor.start + 1;
                    if span > MAX_SPAN_LINES {
                        errors.push(format!(
                            "{}: spans {span} lines, limit is {MAX_SPAN_LINES} — narrow to the lines that matter",
                            at("at"),
                        ));
                    }
                }
                let (lo, hi) = anchor.range();
                for (j, note) in step.notes().iter().enumerate() {
                    if note.line < lo || note.line > hi {
                        errors.push(format!(
                            "{}: line {} is outside the range {lo}-{hi} shown by `at`",
                            at(&format!("notes[{j}]")),
                            note.line,
                        ));
                    }
                    check_len(
                        &at(&format!("notes[{j}].text")),
                        &note.text,
                        MAX_NOTE_CHARS,
                        &mut errors,
                    );
                }
                if step.notes().len() > MAX_NOTES {
                    errors.push(format!(
                        "{}: {} notes, limit is {MAX_NOTES}",
                        at("notes"),
                        step.notes().len(),
                    ));
                }
            }
            match step {
                Step::Cover { what, bullets, .. } => {
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
                            &at(&format!("bullets[{j}]")),
                            b,
                            MAX_BULLET_CHARS,
                            &mut errors,
                        );
                    }
                }
                Step::Point { claim, .. } | Step::Risk { claim, .. } => {
                    check_len(&at("claim"), claim, MAX_CLAIM_CHARS, &mut errors);
                }
                Step::BeforeAfter { claim, .. } => {
                    if let Some(claim) = claim {
                        check_len(&at("claim"), claim, MAX_CLAIM_CHARS, &mut errors);
                    }
                }
                Step::Map { groups, .. } => {
                    if groups.is_empty() {
                        errors.push(format!(
                            "{}: empty — a map is the writer's summary; name at least one group",
                            at("groups"),
                        ));
                    }
                    if groups.len() > MAX_GROUPS {
                        errors.push(format!(
                            "{}: {} groups, limit is {MAX_GROUPS} — merge some",
                            at("groups"),
                            groups.len(),
                        ));
                    }
                    for (j, g) in groups.iter().enumerate() {
                        check_len(
                            &at(&format!("groups[{j}].label")),
                            &g.label,
                            MAX_GROUP_LABEL_CHARS,
                            &mut errors,
                        );
                        if g.files.is_empty() {
                            errors.push(format!(
                                "{}: no files — every group must cover something",
                                at(&format!("groups[{j}]")),
                            ));
                        }
                    }
                }
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
    fn anchor_parses_single_line_and_range() {
        let a: Anchor = "src/main.rs:42".parse().unwrap();
        assert_eq!((a.file.as_str(), a.start, a.end), ("src/main.rs", 42, None));
        assert_eq!(a.range(), (38, 46));

        let a: Anchor = "src/main.rs:42-57".parse().unwrap();
        assert_eq!(a.end, Some(57));
        assert_eq!(a.range(), (42, 57));
    }

    #[test]
    fn anchor_rejects_bad_forms() {
        assert!("src/main.rs".parse::<Anchor>().is_err());
        assert!("src/main.rs:0".parse::<Anchor>().is_err());
        assert!("src/main.rs:9-3".parse::<Anchor>().is_err());
        assert!(":42".parse::<Anchor>().is_err());
    }

    #[test]
    fn validate_refuses_wide_span() {
        let deck = Deck {
            title: "t".into(),
            base: None,
            steps: vec![Step::Point {
                at: "a.rs:10-40".parse().unwrap(),
                claim: "c".into(),
                notes: vec![],
                speaker_notes: None,
            }],
        };
        let errors = deck.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("spans 31 lines"), "{}", errors[0]);
    }

    #[test]
    fn validate_pins_notes_inside_range() {
        let deck = Deck {
            title: "t".into(),
            base: None,
            steps: vec![Step::Point {
                at: "a.rs:10-14".parse().unwrap(),
                claim: "c".into(),
                notes: vec![Note {
                    line: 20,
                    text: "n".into(),
                }],
                speaker_notes: None,
            }],
        };
        let errors = deck.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("outside the range 10-14"), "{}", errors[0]);
    }

    #[test]
    fn validate_reports_how_much_to_cut() {
        let deck = Deck {
            title: "t".repeat(90),
            base: None,
            steps: vec![Step::Point {
                at: "a.rs:1".parse().unwrap(),
                claim: "c".repeat(85),
                notes: vec![],
                speaker_notes: None,
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
            title: "あ".repeat(80),
            base: None,
            steps: vec![Step::Cover {
                what: "w".into(),
                bullets: vec![],
                speaker_notes: None,
            }],
        };
        assert!(deck.validate().is_empty());
    }

    #[test]
    fn deck_length_is_unbounded_but_empty_map_is_refused() {
        let step = Step::Map { groups: vec![], speaker_notes: None };
        let deck = Deck {
            title: "t".into(),
            base: None,
            steps: vec![step; 200],
        };
        let errors = deck.validate();
        assert!(
            !errors.iter().any(|e| e.contains("steps:")),
            "no per-deck length error expected: {errors:?}"
        );
        assert!(errors.iter().any(|e| e.contains("name at least one group")));
    }
}
