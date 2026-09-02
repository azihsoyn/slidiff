# slidiff: slides that point at diffs
base: da4587f

An agent writes the deck; you read it in the terminal, one claim per screen

- Slides hold no code — they point at file:line ranges
- The viewer draws the real hunks from your repo, live
- Review progress travels back: seen marks, flags, comments

???
This demo deck describes the slidiff repository itself. The `base:` above is the repo's first commit, so the entire implementation renders as a diff. Every excerpt you see is drawn live from the tree — nothing on a slide can drift from the code.

---

## The map of the change
@map

- schema & format: src/deck.rs, src/mdeck.rs, src/md.rs, schema/
- diff engine: src/diff/, src/highlight.rs
- viewer: src/ui.rs
- review state: crates/diffseen/, src/seen.rs, src/comments.rs, src/resume.rs

---

## 1. The format refuses prose

---

## Every limit that keeps a slide readable is enforced, not requested
@ src/deck.rs:21-33

- 22: a claim is one line, 80 chars
- 33: an excerpt shows at most 16 lines

???
`slidiff check` rejects an oversized deck with instructions — "claim: 95 chars, limit is 80 — cut 15". The rewrite happens on the writing side, not in the reader's patience. Deck length itself is unbounded: how many slides a change needs is the change's business.

---

## Validation errors tell the writer exactly what to do
@ src/deck.rs:338-346

- 342: pointing at a whole block is refused

---

## 2. Review progress is content-addressed

---

## A seen mark survives a moved hunk and dies with a changed one
@ crates/diffseen/src/lib.rs:298-311

- 301: hashed from content, never line numbers

???
Marks live in `.git/slidiff/`, local and uncommitted. Because keys derive from hunk content, a force-push invalidates exactly the hunks that changed — a re-review starts with only the real deltas unread. The `diffseen` crate carries this store on its own: seen marks, flags, and anchored comments.

---

## Comments travel back with anchors the agent can open
@ src/comments.rs:42-56

- 42: every anchor resolves to a current line

???
`c` comments a line in the dive; `C` bundles the whole review — anchors resolved, lines quoted — and pipes it to whatever `SLIDIFF_ASK_CMD` points at: an agent's terminal pane, or the clipboard. `slidiff comments` prints the same bundle for an agent without the TUI.

---

## The viewer keeps up while the agent keeps working
@ src/ui.rs:424-436 risk=low

- 428: deck and diff reload themselves

???
Every two idle seconds the viewer stats the deck file and fingerprints the diff. An edited deck reloads in place; a changed diff swaps in. Nothing to babysit — and thanks to content addressing, your review progress survives both.

---

## Try it

- cargo install slidiff
- slidiff schema · write a deck · slidiff check
- press ? inside the viewer for everything else
