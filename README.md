# debrief

A slide deck an agent writes and a person reads in the terminal, one claim to a
screen.

A deck holds no code. Every step points at a `file:start-end` range, and the
viewer draws those lines out of the repository at display time — syntax
highlighted, diff-aware with word-level emphasis, context dimmed — so what is
on screen is always what is in the tree, never a copy that has drifted.

Selection is forced, not requested. A claim is one line (≤80 chars), an excerpt
is at most 16 lines, an explanation is a ≤48-char note attached to the exact
line it is about. Pointing at a whole block, or writing a paragraph, does not
fit the format and does not get written. The one place prose is allowed is
speaker notes (≤600 chars per step): shown below the slide, never on it,
cycled with `s` (panel → popup → hidden). A deck holds up to 40 steps — each
slide stays one claim, but deck length scales with the change, and the status
bar always reports how much of the diff the deck actually covers.

## Use

```
debrief report.yaml        # view: n next, p prev, Enter dive into the diff, q quit
                           # mouse: click the outline to jump, wheel to move
debrief check report.yaml  # validate; exit 1 with exactly what to cut
debrief schema             # the JSON Schema agents write decks against
```

## A deck, in markdown

Slides split on `---`; the first slide is the cover. A heading is the
claim, `@` points at the code, list items `- 142: …` hang notes off lines,
and everything below a lone `???` is speaker notes. Prose anywhere else on
a slide is a parse error — the format stays as strict as the YAML.

```markdown
# Fix race in session cleanup
base: main

One line stating what was done
- up to three bullets

???
Longer prose for the interested reader (markdown works here).

---

## The lock is now taken before the map read
@ src/session.rs:138-150

- 142: this ordering is the whole fix

---

## Callers holding the old iterator see a torn view
@ src/api.rs:30-38 risk=medium

---

## Old vs new
@ src/session.rs:138-150 before_after

---

## Where things landed
@map
- core: src/session/, src/api.rs

---

## A section headline

- a slide with no @ is a headline slide
```

## The same deck, in YAML

```yaml
title: "Fix race in session cleanup"
base: main            # optional; defaults to HEAD (uncommitted work).
                      # Resolved via merge-base, so a moved base never shows reversed.
steps:
  - type: cover
    what: "One line stating what was done"
    bullets: ["up to", "three", "lines"]
  - type: map          # the writer's summary of touched files; counts drawn from the repo
    groups:
      - label: "core"
        files: ["src/session/"]     # trailing slash = directory prefix
      - label: "api"
        files: ["src/api.rs"]
  - type: point
    at: "src/session.rs:138-150"    # ≤16 lines; or file.rs:142 for line ± context
    claim: "The lock is now taken before the map read"
    notes:
      - line: 142
        text: "this ordering is the whole fix"
    speaker_notes: "Longer prose for the interested reader goes here, off-slide."
  - type: before_after # old and new side by side for the same range
    at: "src/session.rs:138-150"
  - type: risk
    at: "src/api.rs:30-38"
    claim: "Callers holding the old iterator see a torn view"
    severity: medium   # low | medium | high
```

`examples/self.yaml` is this repository reporting on its own first branch.

## For agents

Ask for `debrief schema`, write YAML or JSON, run `debrief check`. Validation
errors are phrased as instructions ("claim: 95 chars, limit is 80 — cut 15";
"spans 31 lines, limit is 16 — narrow to the lines that matter"); oversized
decks do not pass, so the rewrite happens on the writing side, not in the
reader's patience.

## Build

Rust; `cargo build --release`. Renders diffs with its own parser, word-level
LCS emphasis, and its own minimal syntax highlighting — `git` is the only
external tool it runs.
