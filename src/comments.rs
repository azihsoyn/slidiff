//! Review comments: what the reader wants changed or explained, anchored
//! to changed lines the same content-addressed way as seen marks, and
//! bundled back to the agent with anchors it can open.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::diff::{FileDiff, LineKind};
use crate::seen::hunk_hash;

pub struct CommentStore(diffseen::Comments);

impl CommentStore {
    /// Persist under `<git-dir>/slidiff/comments.json`.
    pub fn load(git_dir: Option<PathBuf>) -> CommentStore {
        match git_dir {
            Some(dir) => CommentStore(diffseen::Comments::open(dir.join("slidiff/comments.json"))),
            None => CommentStore(diffseen::Comments::in_memory()),
        }
    }

    pub fn add(&mut self, file: &str, hunk_hash: &str, idx: usize, text: String) {
        self.0.add(file, hunk_hash, idx, text);
    }

    pub fn get(&self, file: &str, hunk_hash: &str, idx: usize) -> &[String] {
        self.0.get(file, hunk_hash, idx)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn count(&self) -> usize {
        self.0.iter().count()
    }

    /// The whole review as one markdown message: every comment with its
    /// anchor resolved against the current diff, the commented line
    /// quoted, and comments whose hunk has since changed listed as stale
    /// rather than dropped.
    pub fn bundle(&self, files: &[FileDiff]) -> String {
        let mut out = String::from("Review comments from a slidiff reading.\n");
        let mut stale: Vec<(&str, &str)> = Vec::new();
        let mut last_file = "";
        for (file, hash, idx, text) in self.0.iter() {
            match resolve(files, file, hash, idx) {
                Some((line_no, line_text, kind)) => {
                    if file != last_file {
                        let _ = write!(out, "\n## {file}\n");
                        last_file = file;
                    }
                    let sign = match kind {
                        LineKind::Add => '+',
                        LineKind::Del => '-',
                        LineKind::Context => ' ',
                    };
                    let _ = write!(
                        out,
                        "\n{file}:{line_no}\n```diff\n{sign}{line_text}\n```\n{text}\n"
                    );
                }
                None => stale.push((file, text)),
            }
        }
        if !stale.is_empty() {
            let _ = write!(
                out,
                "\n## stale (the hunk changed since these were written)\n"
            );
            for (file, text) in stale {
                let _ = writeln!(out, "- {file}: {text}");
            }
        }
        out
    }
}

/// (display line number, line text, kind) for a content-addressed anchor,
/// against the current diff. Deleted lines report their old-side number.
fn resolve<'a>(
    files: &'a [FileDiff],
    file: &str,
    hash: &str,
    idx: usize,
) -> Option<(u32, &'a str, LineKind)> {
    let fd = files.iter().find(|f| f.new_path == file)?;
    let hunk = fd.hunks.iter().find(|h| hunk_hash(h) == hash)?;
    let line = hunk
        .lines
        .iter()
        .filter(|l| l.kind != LineKind::Context)
        .nth(idx)?;
    let no = line.new_no.or(line.old_no)?;
    Some((no, line.text.as_str(), line.kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified;

    const SAMPLE: &str = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -10,3 +10,3 @@
 ctx
-old line
+new line
 tail
";

    #[test]
    fn bundle_resolves_anchors_and_reports_stale() {
        let files = parse_unified(SAMPLE);
        let h = hunk_hash(&files[0].hunks[0]);
        let mut store = CommentStore(diffseen::Comments::in_memory());
        store.add("f.rs", &h, 1, "why not keep the old call?".into());
        store.add("f.rs", "dead_hash", 0, "this hunk changed".into());

        let bundle = store.bundle(&files);
        assert!(bundle.contains("## f.rs"), "{bundle}");
        assert!(bundle.contains("f.rs:11"), "{bundle}");
        assert!(bundle.contains("+new line"), "{bundle}");
        assert!(bundle.contains("why not keep the old call?"), "{bundle}");
        assert!(bundle.contains("stale"), "{bundle}");
        assert!(bundle.contains("this hunk changed"), "{bundle}");
        assert_eq!(store.count(), 2);
    }
}
