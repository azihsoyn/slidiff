# debrief

A slide deck an agent writes and a person reads in the terminal, one claim to a
screen.

A deck holds no code. Every step points at `file:line`, and the viewer draws the
real hunk out of the repository at display time, with word-level emphasis — so
what is on screen is always what is in the tree, never a copy that has drifted.

Length is refused by the schema rather than asked for: a claim is at most 120
characters and a deck at most 12 steps. Prose that will not fit does not get
written.

## Use

```
debrief report.yaml        # view: n next, p prev, Enter dive into the diff, q quit
debrief check report.yaml  # validate; exit 1 with exactly what to cut
debrief schema             # the JSON Schema agents write decks against
```

## A deck

```yaml
title: "Fix race in session cleanup"
base: main            # optional; defaults to HEAD (uncommitted work).
                      # Resolved via merge-base, so a moved base never shows reversed.
steps:
  - type: cover
    what: "One line stating what was done"
    bullets: ["up to", "three", "lines"]
  - type: map          # every touched file, +/- counts, drawn from the repo
  - type: point
    at: "src/session.rs:142"
    claim: "The lock is now taken before the map read"
  - type: before_after # old and new side by side for the hunk at `at`
    at: "src/session.rs:142"
  - type: zoom         # the file as it is now, centered on `at`
    at: "src/session.rs:120"
  - type: risk
    at: "src/api.rs:33"
    claim: "Callers holding the old iterator see a torn view"
    severity: medium   # low | medium | high
```

`examples/self.yaml` is this repository reporting on its own first branch.

## For agents

Ask for `debrief schema`, write YAML or JSON, run `debrief check`. Validation
errors are phrased as instructions ("claim: 145 chars, limit is 120 — cut 25");
oversized decks do not pass, so the rewrite happens on the writing side, not in
the reader's patience.

## Build

Rust; `cargo build --release`. Renders diffs with its own parser and word-level
LCS emphasis — `git` is the only external tool it runs.
