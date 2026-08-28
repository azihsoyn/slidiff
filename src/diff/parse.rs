//! Parsing `git diff` unified output. No external diff library — the format
//! is stable and small, and owning the parser keeps the renderer honest.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    pub status: FileStatus,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Whatever git put after the second `@@` — usually the enclosing
    /// function.
    pub section: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Del,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line content without the +/-/space prefix.
    pub text: String,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
}

impl FileDiff {
    pub fn added(&self) -> usize {
        self.count(LineKind::Add)
    }

    pub fn deleted(&self) -> usize {
        self.count(LineKind::Del)
    }

    fn count(&self, kind: LineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == kind)
            .count()
    }
}

/// Parse the output of `git diff --no-color`. Unknown header lines are
/// skipped; a malformed input yields fewer files, never a panic.
pub fn parse_unified(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        let Some(paths) = line.strip_prefix("diff --git ") else {
            continue;
        };
        let (old_path, new_path) = split_git_paths(paths);
        let mut file = FileDiff {
            old_path,
            new_path,
            status: FileStatus::Modified,
            binary: false,
            hunks: Vec::new(),
        };

        // Header lines until the first hunk or the next file.
        while let Some(&header) = lines.peek() {
            if header.starts_with("diff --git ") || header.starts_with("@@ ") {
                break;
            }
            let header = lines.next().unwrap();
            if header.starts_with("new file mode") {
                file.status = FileStatus::Added;
            } else if header.starts_with("deleted file mode") {
                file.status = FileStatus::Deleted;
            } else if let Some(p) = header.strip_prefix("rename from ") {
                file.status = FileStatus::Renamed;
                file.old_path = p.to_string();
            } else if let Some(p) = header.strip_prefix("rename to ") {
                file.new_path = p.to_string();
            } else if header.starts_with("Binary files ") {
                file.binary = true;
            } else if let Some(p) = header.strip_prefix("--- a/") {
                file.old_path = p.to_string();
            } else if let Some(p) = header.strip_prefix("+++ b/") {
                file.new_path = p.to_string();
            }
        }

        // Hunks.
        while let Some(&hunk_line) = lines.peek() {
            let Some(mut hunk) = parse_hunk_header(hunk_line) else {
                break;
            };
            lines.next();
            let mut old_no = hunk.old_start;
            let mut new_no = hunk.new_start;
            while let Some(&body) = lines.peek() {
                let (kind, text) = match body.as_bytes().first() {
                    Some(b' ') => (LineKind::Context, &body[1..]),
                    Some(b'-') if !body.starts_with("--- ") => (LineKind::Del, &body[1..]),
                    Some(b'+') if !body.starts_with("+++ ") => (LineKind::Add, &body[1..]),
                    Some(b'\\') => {
                        // "\ No newline at end of file"
                        lines.next();
                        continue;
                    }
                    _ => break,
                };
                lines.next();
                let (o, n) = match kind {
                    LineKind::Context => {
                        let pair = (Some(old_no), Some(new_no));
                        old_no += 1;
                        new_no += 1;
                        pair
                    }
                    LineKind::Del => {
                        let pair = (Some(old_no), None);
                        old_no += 1;
                        pair
                    }
                    LineKind::Add => {
                        let pair = (None, Some(new_no));
                        new_no += 1;
                        pair
                    }
                };
                hunk.lines.push(DiffLine {
                    kind,
                    text: text.to_string(),
                    old_no: o,
                    new_no: n,
                });
            }
            file.hunks.push(hunk);
        }

        files.push(file);
    }

    files
}

/// `a/src/x.rs b/src/x.rs` → both sides. Paths with spaces stay intact as
/// long as both sides are equal length-wise ambiguous cases fall back to
/// the `---`/`+++` headers parsed later, which are authoritative anyway.
fn split_git_paths(s: &str) -> (String, String) {
    // Try the unambiguous case first: "a/X b/X" with identical X.
    if let Some(rest) = s.strip_prefix("a/") {
        // "X b/X" is 2n+3 bytes for an n-byte X.
        let n = rest.len().saturating_sub(3) / 2;
        if rest.len() == 2 * n + 3 && rest.is_char_boundary(n) {
            let (old, tail) = rest.split_at(n);
            if let Some(new) = tail.strip_prefix(" b/")
                && old == new {
                    return (old.to_string(), new.to_string());
                }
        }
    }
    // Renames: split at the last " b/".
    if let Some(pos) = s.rfind(" b/") {
        let old = s[..pos].strip_prefix("a/").unwrap_or(&s[..pos]);
        let new = &s[pos + 3..];
        return (old.to_string(), new.to_string());
    }
    (s.to_string(), s.to_string())
}

fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, section) = rest.split_once(" @@")?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        section: section.trim().to_string(),
        lines: Vec::new(),
    })
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,7 +10,8 @@ impl Session {
     fn close(&mut self) {
-        self.map.remove(&id);
+        let _guard = self.lock.lock();
+        self.map.remove(&id);
         self.notify();
     }
 }
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
\\ No newline at end of file
";

    #[test]
    fn parses_files_hunks_and_line_numbers() {
        let files = parse_unified(SAMPLE);
        assert_eq!(files.len(), 2);

        let f = &files[0];
        assert_eq!(f.new_path, "src/lib.rs");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.hunks.len(), 1);
        let h = &f.hunks[0];
        assert_eq!((h.old_start, h.old_count, h.new_start, h.new_count), (10, 7, 10, 8));
        assert_eq!(h.section, "impl Session {");
        assert_eq!(h.lines.len(), 7);
        assert_eq!(h.lines[1].kind, LineKind::Del);
        assert_eq!(h.lines[1].old_no, Some(11));
        assert_eq!(h.lines[1].new_no, None);
        assert_eq!(h.lines[2].kind, LineKind::Add);
        assert_eq!(h.lines[2].new_no, Some(11));
        assert_eq!(h.lines[3].kind, LineKind::Add);
        assert_eq!(h.lines[3].new_no, Some(12));
        // Context after the change is numbered on both sides.
        assert_eq!(h.lines[4].old_no, Some(12));
        assert_eq!(h.lines[4].new_no, Some(13));

        let f = &files[1];
        assert_eq!(f.status, FileStatus::Added);
        assert_eq!(f.added(), 2);
        assert_eq!(f.deleted(), 0);
    }

    #[test]
    fn parses_rename() {
        let text = "\
diff --git a/old_name.rs b/new_name.rs
similarity index 90%
rename from old_name.rs
rename to new_name.rs
index 1111111..2222222 100644
--- a/old_name.rs
+++ b/new_name.rs
@@ -1,2 +1,2 @@
-fn a() {}
+fn b() {}
 fn c() {}
";
        let files = parse_unified(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Renamed);
        assert_eq!(files[0].old_path, "old_name.rs");
        assert_eq!(files[0].new_path, "new_name.rs");
    }

    #[test]
    fn binary_file_has_no_hunks() {
        let text = "\
diff --git a/img.png b/img.png
index 1111111..2222222 100644
Binary files a/img.png and b/img.png differ
";
        let files = parse_unified(text);
        assert_eq!(files.len(), 1);
        assert!(files[0].binary);
        assert!(files[0].hunks.is_empty());
    }
}
