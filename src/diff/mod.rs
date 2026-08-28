//! Everything about showing the real change: run git, parse unified diffs,
//! find the hunk a `file:line` points at, emphasize changed words.
//!
//! This layer knows nothing about the TUI. It turns a repository plus a
//! location into data the viewer can draw.

mod git;
mod parse;
mod words;

pub use git::{Repo, load_diff};
pub use parse::{DiffLine, FileDiff, FileStatus, Hunk, LineKind, parse_unified};
pub use words::{Segment, emphasize_hunk};

/// Find the file diff for `file`, if the diff touches it.
pub fn file_diff<'a>(files: &'a [FileDiff], file: &str) -> Option<&'a FileDiff> {
    files
        .iter()
        .find(|f| f.new_path == file || f.old_path == file)
}

/// The hunk whose new side contains `line`, or failing that the hunk
/// nearest to it. `bool` is true when the hit is exact.
pub fn hunk_at(fd: &FileDiff, line: u32) -> Option<(&Hunk, bool)> {
    let exact = fd
        .hunks
        .iter()
        .find(|h| line >= h.new_start && line < h.new_start + h.new_count.max(1));
    if let Some(h) = exact {
        return Some((h, true));
    }
    fd.hunks
        .iter()
        .min_by_key(|h| {
            let start = h.new_start;
            let end = h.new_start + h.new_count.max(1) - 1;
            if line < start {
                start - line
            } else {
                line - end
            }
        })
        .map(|h| (h, false))
}
