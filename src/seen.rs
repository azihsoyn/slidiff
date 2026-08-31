//! Local, per-worktree record of which changed lines the reader has
//! actually inspected. Stored under the repository's git dir — never
//! committed, never in the working tree.
//!
//! Keys are content-derived: a hunk is identified by a hash of its lines
//! (kinds and text, no line numbers), a changed line by its index among
//! the hunk's changed lines. Rebase or edit a hunk and its marks fall off
//! by construction — exactly what a re-review wants.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::diff::{FileDiff, Hunk, LineKind};

/// file path → hunk hash → indices (within the hunk's changed lines) seen.
type SeenMap = BTreeMap<String, BTreeMap<String, BTreeSet<usize>>>;

#[derive(Debug, Serialize, Deserialize, Default)]
struct SeenFile {
    seen: SeenMap,
}

pub struct SeenStore {
    path: Option<PathBuf>,
    map: SeenMap,
}

impl SeenStore {
    /// Load from `<git-dir>/debrief/seen.json`. A store without a path
    /// (e.g. in tests) works in memory and never saves.
    pub fn load(git_dir: Option<PathBuf>) -> SeenStore {
        let path = git_dir.map(|d| d.join("debrief/seen.json"));
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str::<SeenFile>(&text).ok())
            .map(|f| f.seen)
            .unwrap_or_default();
        SeenStore { path, map }
    }

    pub fn in_memory() -> SeenStore {
        SeenStore {
            path: None,
            map: SeenMap::new(),
        }
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&SeenFile {
            seen: self.map.clone(),
        }) {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn is_seen(&self, file: &str, hunk_hash: &str, idx: usize) -> bool {
        self.map
            .get(file)
            .and_then(|h| h.get(hunk_hash))
            .is_some_and(|s| s.contains(&idx))
    }

    pub fn toggle_line(&mut self, file: &str, hunk_hash: &str, idx: usize) {
        let set = self
            .map
            .entry(file.to_string())
            .or_default()
            .entry(hunk_hash.to_string())
            .or_default();
        if !set.insert(idx) {
            set.remove(&idx);
        }
        self.save();
    }

    /// Mark the whole hunk seen; if it already is, clear it.
    pub fn toggle_hunk(&mut self, file: &str, hunk_hash: &str, changed_total: usize) {
        let set = self
            .map
            .entry(file.to_string())
            .or_default()
            .entry(hunk_hash.to_string())
            .or_default();
        if set.len() == changed_total {
            set.clear();
        } else {
            *set = (0..changed_total).collect();
        }
        self.save();
    }

    /// (seen, total) changed lines for one file, counting only marks whose
    /// hunk hash still exists in the current diff — stale marks are dead.
    pub fn progress_for(&self, fd: &FileDiff) -> (usize, usize) {
        let mut seen = 0;
        let mut total = 0;
        let by_hash = self.map.get(fd.new_path.as_str());
        for hunk in &fd.hunks {
            let changed = changed_count(hunk);
            total += changed;
            if let Some(set) = by_hash.and_then(|m| m.get(&hunk_hash(hunk))) {
                seen += set.iter().filter(|&&i| i < changed).count();
            }
        }
        (seen, total)
    }
}

pub fn changed_count(hunk: &Hunk) -> usize {
    hunk.lines
        .iter()
        .filter(|l| l.kind != LineKind::Context)
        .count()
}

/// Deterministic content hash (FNV-1a, 64 bit) over the hunk's lines.
/// Line numbers are excluded on purpose: a hunk that merely moved keeps
/// its marks, a hunk that changed loses them.
pub fn hunk_hash(hunk: &Hunk) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    for line in &hunk.lines {
        eat(match line.kind {
            LineKind::Context => b" ",
            LineKind::Add => b"+",
            LineKind::Del => b"-",
        });
        eat(line.text.as_bytes());
        eat(b"\n");
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified;

    const SAMPLE: &str = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -1,3 +1,4 @@
 ctx
-old
+new
+more
 tail
";

    fn hunk() -> Hunk {
        parse_unified(SAMPLE)[0].hunks[0].clone()
    }

    #[test]
    fn hash_ignores_position_but_not_content() {
        let mut moved = hunk();
        moved.new_start = 500;
        moved.old_start = 499;
        assert_eq!(hunk_hash(&hunk()), hunk_hash(&moved));

        let mut edited = hunk();
        edited.lines[2].text = "different".into();
        assert_ne!(hunk_hash(&hunk()), hunk_hash(&edited));
    }

    #[test]
    fn toggle_line_and_hunk_and_progress() {
        let files = parse_unified(SAMPLE);
        let fd = &files[0];
        let h = hunk_hash(&fd.hunks[0]);
        let mut store = SeenStore::in_memory();

        assert_eq!(store.progress_for(fd), (0, 3));
        store.toggle_line("f.rs", &h, 0);
        assert!(store.is_seen("f.rs", &h, 0));
        assert_eq!(store.progress_for(fd), (1, 3));
        store.toggle_line("f.rs", &h, 0);
        assert_eq!(store.progress_for(fd), (0, 3));

        store.toggle_hunk("f.rs", &h, 3);
        assert_eq!(store.progress_for(fd), (3, 3));
        store.toggle_hunk("f.rs", &h, 3);
        assert_eq!(store.progress_for(fd), (0, 3));
    }

    #[test]
    fn stale_marks_do_not_count() {
        let files = parse_unified(SAMPLE);
        let fd = &files[0];
        let mut store = SeenStore::in_memory();
        store.toggle_line("f.rs", "dead_hash", 0);
        assert_eq!(store.progress_for(fd), (0, 3));
    }

    #[test]
    fn survives_a_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let files = parse_unified(SAMPLE);
        let h = hunk_hash(&files[0].hunks[0]);
        {
            let mut store = SeenStore::load(Some(dir.path().to_path_buf()));
            store.toggle_line("f.rs", &h, 1);
        }
        let store = SeenStore::load(Some(dir.path().to_path_buf()));
        assert!(store.is_seen("f.rs", &h, 1));
        assert_eq!(store.progress_for(&files[0]), (1, 3));
    }
}
