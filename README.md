# deck

A slide deck an agent writes and a person reads in the terminal, one claim to a
screen.

A deck holds no code. Every step points at `file:line`, and the viewer draws the
real hunk out of the repository at display time, with word-level emphasis — so
what is on screen is always what is in the tree, never a copy that has drifted.

Length is refused by the schema rather than asked for: a claim is at most 120
characters and a deck at most 12 steps. Prose that will not fit does not get
written.

Working name. The session building this picks the real one.
