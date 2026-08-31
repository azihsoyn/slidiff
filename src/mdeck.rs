//! Markdown deck authoring. Same model, same limits — a friendlier pen,
//! not a laxer format. Prose on a slide body is a parse error that points
//! the writer at `???` (the speaker-notes divider), never silently shown.
//!
//! ```markdown
//! # Deck title            ← first slide = cover
//! base: origin/develop
//!
//! One line stating what was done
//! - up to three bullets
//!
//! ???
//! speaker notes prose (markdown)
//!
//! ---
//!
//! ## The claim is the heading
//! @ src/foo.rs:22-27
//!
//! - 26: note hanging off line 26
//!
//! ---
//!
//! ## A risk reads the same
//! @ src/foo.rs:30-38 risk=medium
//!
//! ---
//!
//! ## Old vs new
//! @ src/foo.rs:22-27 before_after
//!
//! ---
//!
//! ## The map of the change
//! @map
//! - engine: src/engine/, src/api.rs
//! ```
//!
//! A slide with a heading and no `@` is a text slide (rendered like the
//! cover: headline plus bullets).

use crate::deck::{Anchor, Deck, Group, Note, Severity, Step};

/// Parse a markdown deck. Returns every problem at once, phrased so the
/// writer knows what to move or fix.
pub fn parse(text: &str) -> Result<Deck, Vec<String>> {
    let mut errors: Vec<String> = Vec::new();
    let slides = split_slides(text);
    if slides.is_empty() {
        return Err(vec!["empty file — a deck needs at least a cover slide".into()]);
    }

    let mut title = String::new();
    let mut base: Option<String> = None;
    let mut steps: Vec<Step> = Vec::new();

    for (i, slide) in slides.iter().enumerate() {
        let n = i + 1;
        let (body, speaker_notes) = split_notes(slide);
        let mut heading: Option<String> = None;
        let mut heading_depth: usize = 0;
        let mut anchor_line: Option<String> = None;
        let mut is_map = false;
        let mut list_items: Vec<String> = Vec::new();
        let mut paragraphs: Vec<String> = Vec::new();

        for raw in body.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((depth, rest)) = heading_text(line) {
                if heading.is_some() {
                    errors.push(format!(
                        "slide {n}: two headings — one claim per slide; move the rest below ???"
                    ));
                }
                heading = Some(rest.to_string());
                heading_depth = depth;
            } else if line == "@map" || line == "@ map" {
                is_map = true;
            } else if let Some(rest) = line.strip_prefix("@ ") {
                if anchor_line.is_some() {
                    errors.push(format!(
                        "slide {n}: two @ anchors — one excerpt per slide; split the slide"
                    ));
                }
                anchor_line = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))
            {
                list_items.push(rest.trim().to_string());
            } else if i == 0 && line.starts_with("base:") {
                base = Some(line["base:".len()..].trim().to_string());
            } else {
                paragraphs.push(line.to_string());
            }
        }

        if i == 0 {
            match heading {
                Some(h) => title = h,
                None => errors.push("slide 1: missing `# title` heading".into()),
            }
            if anchor_line.is_some() || is_map {
                errors.push(
                    "slide 1 is the cover — put @ anchors and @map on later slides".into(),
                );
            }
            let what = paragraphs.first().cloned().unwrap_or_default();
            if paragraphs.len() > 1 {
                errors.push(format!(
                    "slide 1: {} paragraphs — one line of what, the rest below ???",
                    paragraphs.len()
                ));
            }
            steps.push(Step::Cover {
                what,
                bullets: list_items,
                level: 1,
                speaker_notes,
            });
            continue;
        }

        if is_map {
            if anchor_line.is_some() {
                errors.push(format!("slide {n}: @map and @ anchor on the same slide"));
            }
            let mut groups = Vec::new();
            for item in &list_items {
                match item.split_once(':') {
                    Some((label, files)) => groups.push(Group {
                        label: label.trim().to_string(),
                        files: files
                            .split(',')
                            .map(|f| f.trim().to_string())
                            .filter(|f| !f.is_empty())
                            .collect(),
                    }),
                    None => errors.push(format!(
                        "slide {n}: map item \"{item}\" — write `label: path, path`"
                    )),
                }
            }
            steps.push(Step::Map {
                groups,
                speaker_notes,
            });
            continue;
        }

        let Some(anchor_line) = anchor_line else {
            // Headline slide: heading depth nests it — `##` opens a
            // section, `###` a subsection. Files make the third tier.
            let Some(heading) = heading else {
                errors.push(format!(
                    "slide {n}: no heading and no @ anchor — a slide is a claim or a headline"
                ));
                continue;
            };
            if !paragraphs.is_empty() {
                errors.push(format!(
                    "slide {n}: prose on the slide — move it below ??? into speaker notes"
                ));
            }
            if heading_depth > 3 {
                errors.push(format!(
                    "slide {n}: `{}` heading — three tiers is the max (## section, ### subsection, files)",
                    "#".repeat(heading_depth)
                ));
            }
            steps.push(Step::Cover {
                what: heading,
                bullets: list_items,
                level: if heading_depth >= 3 { 2 } else { 1 },
                speaker_notes,
            });
            continue;
        };

        // Excerpt slide: @ file:range [risk=<sev>|before_after]
        let mut parts = anchor_line.split_whitespace();
        let at: Option<Anchor> = match parts.next() {
            Some(spec) => match spec.parse() {
                Ok(a) => Some(a),
                Err(e) => {
                    errors.push(format!("slide {n}: @ {e}"));
                    None
                }
            },
            None => {
                errors.push(format!("slide {n}: @ with nothing after it"));
                None
            }
        };
        let mut severity: Option<Severity> = None;
        let mut before_after = false;
        for flag in parts {
            match flag {
                "before_after" => before_after = true,
                "risk=low" => severity = Some(Severity::Low),
                "risk=medium" => severity = Some(Severity::Medium),
                "risk=high" => severity = Some(Severity::High),
                other => errors.push(format!(
                    "slide {n}: unknown @ flag \"{other}\" — before_after or risk=low|medium|high"
                )),
            }
        }
        if !paragraphs.is_empty() {
            errors.push(format!(
                "slide {n}: prose on the slide — move it below ??? into speaker notes"
            ));
        }
        let mut notes: Vec<Note> = Vec::new();
        for item in &list_items {
            match parse_note(item) {
                Some(note) => notes.push(note),
                None => errors.push(format!(
                    "slide {n}: list item \"{item}\" — on an excerpt slide write `- <line>: note`"
                )),
            }
        }
        let claim = heading.unwrap_or_default();
        if claim.is_empty() && !before_after {
            errors.push(format!("slide {n}: missing `## claim` heading"));
        }
        let Some(at) = at else { continue };
        let step = if let Some(severity) = severity {
            Step::Risk {
                at,
                claim,
                severity,
                notes,
                speaker_notes,
            }
        } else if before_after {
            if !notes.is_empty() {
                errors.push(format!(
                    "slide {n}: line notes are not drawn on before_after slides"
                ));
            }
            Step::BeforeAfter {
                at,
                claim: (!claim.is_empty()).then_some(claim),
                speaker_notes,
            }
        } else {
            Step::Point {
                at,
                claim,
                notes,
                speaker_notes,
            }
        };
        steps.push(step);
    }

    let deck = Deck { title, base, steps };
    errors.extend(deck.validate());
    if errors.is_empty() {
        Ok(deck)
    } else {
        Err(errors)
    }
}

fn split_slides(text: &str) -> Vec<String> {
    let mut slides = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.trim() == "---" {
            slides.push(std::mem::take(&mut current));
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    slides.push(current);
    slides.retain(|s| !s.trim().is_empty());
    slides
}

/// Everything below a lone `???` line is the speaker notes (remark.js
/// convention).
fn split_notes(slide: &str) -> (String, Option<String>) {
    let mut body = String::new();
    let mut notes: Option<String> = None;
    for line in slide.lines() {
        if notes.is_none() && line.trim() == "???" {
            notes = Some(String::new());
            continue;
        }
        let target = notes.as_mut().unwrap_or(&mut body);
        target.push_str(line);
        target.push('\n');
    }
    let notes = notes.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    (body, notes)
}

fn heading_text(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    line[hashes..].strip_prefix(' ').map(|t| (hashes, t.trim()))
}

fn parse_note(item: &str) -> Option<Note> {
    let (line, text) = item.split_once(':')?;
    let line: u32 = line.trim().parse().ok()?;
    Some(Note {
        line,
        text: text.trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# My deck
base: main

What was done here
- first bullet
- second bullet

???
cover notes prose

---

## The lock ordering is the fix
@ src/session.rs:138-145

- 142: taken before the map read

---

## Callers may see a torn view
@ src/api.rs:30-36 risk=medium

---

## Old vs new
@ src/session.rs:138-145 before_after

---

## Where things landed
@map
- core: src/session/, src/api.rs
- tests: tests/

---

## A section headline

- just bullets here
";

    #[test]
    fn parses_all_step_types() {
        let deck = parse(SAMPLE).unwrap();
        assert_eq!(deck.title, "My deck");
        assert_eq!(deck.base.as_deref(), Some("main"));
        assert_eq!(deck.steps.len(), 6);
        assert_eq!(deck.steps[0].type_name(), "cover");
        assert_eq!(deck.steps[0].speaker_notes(), Some("cover notes prose"));
        assert_eq!(deck.steps[1].type_name(), "point");
        assert_eq!(deck.steps[1].notes().len(), 1);
        assert_eq!(deck.steps[1].notes()[0].line, 142);
        assert_eq!(deck.steps[2].type_name(), "risk");
        assert_eq!(deck.steps[3].type_name(), "before_after");
        assert_eq!(deck.steps[4].type_name(), "map");
        let Step::Map { groups, .. } = &deck.steps[4] else {
            panic!()
        };
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].files, vec!["src/session/", "src/api.rs"]);
        // Text slide renders as a cover-shaped step.
        assert_eq!(deck.steps[5].type_name(), "cover");
    }

    #[test]
    fn prose_on_a_slide_is_refused_toward_speaker_notes() {
        let text = "\
# t

w

---

## claim
@ a.rs:1-3

this prose does not belong here
";
        let errors = parse(text).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("move it below ???")),
            "{errors:?}"
        );
    }

    #[test]
    fn schema_limits_apply_to_markdown_too() {
        let long_claim = "c".repeat(90);
        let text = format!("# t\n\nw\n\n---\n\n## {long_claim}\n@ a.rs:1-3\n");
        let errors = parse(&text).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("limit is 80 — cut 10")),
            "{errors:?}"
        );
    }

    #[test]
    fn deck_length_is_unbounded_in_markdown() {
        let mut text = String::from("# t\n\nw\n");
        for i in 0..120 {
            text.push_str(&format!("\n---\n\n## slide {i}\n@ a.rs:1-3\n"));
        }
        let deck = parse(&text).unwrap();
        assert_eq!(deck.steps.len(), 121);
    }

    #[test]
    fn bad_anchor_and_unknown_flag_are_reported_with_slide_number() {
        let text = "\
# t

w

---

## c
@ a.rs risk=extreme
";
        let errors = parse(text).unwrap_err();
        assert!(errors.iter().any(|e| e.starts_with("slide 2: @")), "{errors:?}");
        assert!(
            errors.iter().any(|e| e.contains("unknown @ flag \"risk=extreme\"")),
            "{errors:?}"
        );
    }

    #[test]
    fn heading_depth_nests_sections_and_refuses_a_fourth_tier() {
        let text = "\
# t

w

---

## section

---

### subsection

---

## c
@ a.rs:1-3
";
        let deck = parse(text).unwrap();
        let Step::Cover { level, .. } = &deck.steps[1] else { panic!() };
        assert_eq!(*level, 1);
        let Step::Cover { level, .. } = &deck.steps[2] else { panic!() };
        assert_eq!(*level, 2);

        let too_deep = "# t\n\nw\n\n---\n\n#### nope\n";
        let errors = parse(too_deep).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("three tiers is the max")),
            "{errors:?}"
        );
    }

    #[test]
    fn question_marks_inside_notes_do_not_resplit() {
        let text = "\
# t

w

???
line one
??? not a divider mid-line
line three
";
        let deck = parse(text).unwrap();
        let notes = deck.steps[0].speaker_notes().unwrap();
        assert!(notes.contains("line three"), "{notes}");
    }
}
