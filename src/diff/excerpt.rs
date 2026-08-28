//! Building the excerpt a slide shows: the chosen new-side line range,
//! diff-aware where the diff touches it, plain file content where it does
//! not, with deleted lines interleaved where they disappeared.

use super::parse::{FileDiff, Hunk, LineKind};
use super::words::{Segment, emphasize_hunk};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcerptRow {
    pub kind: LineKind,
    pub text: String,
    /// New-side line number; None for deleted rows.
    pub new_no: Option<u32>,
    /// Word-level emphasis, present when this row is part of a del/add pair.
    pub segments: Option<Vec<Segment>>,
}

/// The rows to display for new-side lines `start..=end`.
///
/// - Lines the diff touches come from the hunks, with word emphasis and
///   deleted lines shown where they vanished.
/// - Lines outside every hunk come from the worktree file.
/// - With neither a diff entry nor file content, the result is empty.
pub fn excerpt(
    fd: Option<&FileDiff>,
    file_lines: Option<&[String]>,
    start: u32,
    end: u32,
) -> Vec<ExcerptRow> {
    let mut end = end;
    if let Some(lines) = file_lines {
        end = end.min(lines.len() as u32);
    }
    if end < start {
        return Vec::new();
    }

    // From the hunks: rows keyed by new-side line, deletions keyed by the
    // new-side line they precede.
    let mut at_line: Vec<Option<ExcerptRow>> = vec![None; (end - start + 1) as usize];
    let mut dels_before: Vec<Vec<ExcerptRow>> = vec![Vec::new(); (end - start + 2) as usize];

    for hunk in fd.map(|f| f.hunks.as_slice()).unwrap_or_default() {
        if !overlaps(hunk, start, end) {
            continue;
        }
        let segs = emphasize_hunk(hunk);
        // Where a deletion "is" on the new side: after the previous line
        // that still exists there.
        let mut next_new = hunk.new_start;
        for (line, seg) in hunk.lines.iter().zip(segs) {
            match line.kind {
                LineKind::Context | LineKind::Add => {
                    let no = line.new_no.expect("context/add lines carry new_no");
                    next_new = no + 1;
                    if no >= start && no <= end {
                        at_line[(no - start) as usize] = Some(ExcerptRow {
                            kind: line.kind,
                            text: line.text.clone(),
                            new_no: Some(no),
                            segments: (line.kind == LineKind::Add).then_some(seg),
                        });
                    }
                }
                LineKind::Del => {
                    if next_new >= start && next_new <= end + 1 {
                        dels_before[(next_new - start) as usize].push(ExcerptRow {
                            kind: LineKind::Del,
                            text: line.text.clone(),
                            new_no: None,
                            segments: Some(seg),
                        });
                    }
                }
            }
        }
    }

    let mut rows = Vec::new();
    for no in start..=end {
        rows.append(&mut dels_before[(no - start) as usize]);
        match at_line[(no - start) as usize].take() {
            Some(row) => rows.push(row),
            None => {
                if let Some(text) = file_lines.and_then(|l| l.get((no - 1) as usize)) {
                    rows.push(ExcerptRow {
                        kind: LineKind::Context,
                        text: text.clone(),
                        new_no: Some(no),
                        segments: None,
                    });
                }
            }
        }
    }
    rows.append(&mut dels_before[(end - start + 1) as usize]);
    rows
}

fn overlaps(hunk: &Hunk, start: u32, end: u32) -> bool {
    let h_start = hunk.new_start;
    let h_end = hunk.new_start + hunk.new_count.max(1); // +1 so trailing dels count
    h_start <= end + 1 && h_end >= start
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse::parse_unified;

    const SAMPLE: &str = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -10,4 +10,4 @@
 ctx1
-old line
+new line
 ctx2
";

    fn file() -> Vec<String> {
        (1..=30).map(|i| format!("file line {i}")).collect()
    }

    #[test]
    fn merges_hunk_rows_with_file_gaps() {
        let files = parse_unified(SAMPLE);
        let rows = excerpt(Some(&files[0]), Some(&file()), 9, 13);
        let texts: Vec<(&str, LineKind)> =
            rows.iter().map(|r| (r.text.as_str(), r.kind)).collect();
        assert_eq!(
            texts,
            vec![
                ("file line 9", LineKind::Context),
                ("ctx1", LineKind::Context),
                ("old line", LineKind::Del),
                ("new line", LineKind::Add),
                ("ctx2", LineKind::Context),
                ("file line 13", LineKind::Context),
            ]
        );
        // The del row sits exactly before the add that replaced it.
        assert!(rows[2].new_no.is_none());
        assert_eq!(rows[3].new_no, Some(11));
        assert!(rows[3].segments.is_some());
    }

    #[test]
    fn plain_file_when_diff_misses_range() {
        let files = parse_unified(SAMPLE);
        let rows = excerpt(Some(&files[0]), Some(&file()), 20, 22);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.kind == LineKind::Context));
    }

    #[test]
    fn no_sources_yields_empty() {
        assert!(excerpt(None, None, 1, 5).is_empty());
    }

    #[test]
    fn clamps_to_file_length() {
        let rows = excerpt(None, Some(&file()), 28, 99);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.last().unwrap().new_no, Some(30));
    }
}
