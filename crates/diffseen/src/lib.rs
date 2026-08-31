//! A local, content-addressed record of which changed lines of a diff a
//! human has actually reviewed.
//!
//! Nothing here knows about git, patches, or any particular diff parser.
//! A hunk is whatever you hash with [`hunk_hash`] — a sequence of
//! ([`Mark`], text) lines — and a changed line is addressed by its index
//! among the hunk's non-context lines. Because keys derive from content
//! and not position, marks survive hunks that merely move, and fall off
//! automatically when a hunk's content changes: exactly the invalidation
//! a re-review wants.
//!
//! The store persists as pretty JSON at a path the caller chooses (for a
//! git workflow, somewhere under the repository's git dir keeps it local
//! and uncommitted). A store without a path works in memory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The role of one diff line, as far as hashing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Context,
    Add,
    Del,
}

/// file path → hunk hash → indices (within the hunk's changed lines) seen.
type SeenMap = BTreeMap<String, BTreeMap<String, BTreeSet<usize>>>;

#[derive(Debug, Serialize, Deserialize, Default)]
struct SeenFile {
    seen: SeenMap,
}

pub struct Store {
    path: Option<PathBuf>,
    map: SeenMap,
}

impl Store {
    /// Open (or create on first save) the store at `path`.
    pub fn open(path: PathBuf) -> Store {
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<SeenFile>(&text).ok())
            .map(|f| f.seen)
            .unwrap_or_default();
        Store {
            path: Some(path),
            map,
        }
    }

    /// A store that never persists.
    pub fn in_memory() -> Store {
        Store {
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
        let set = self.entry(file, hunk_hash);
        if !set.insert(idx) {
            set.remove(&idx);
        }
        self.save();
    }

    /// Set one line's mark explicitly.
    pub fn set_line(&mut self, file: &str, hunk_hash: &str, idx: usize, seen: bool) {
        let set = self.entry(file, hunk_hash);
        if seen {
            set.insert(idx);
        } else {
            set.remove(&idx);
        }
        self.save();
    }

    /// Mark the whole hunk seen or unseen.
    pub fn set_hunk(&mut self, file: &str, hunk_hash: &str, changed_total: usize, seen: bool) {
        let set = self.entry(file, hunk_hash);
        if seen {
            *set = (0..changed_total).collect();
        } else {
            set.clear();
        }
        self.save();
    }

    /// Mark the whole hunk seen; if it already is, clear it.
    pub fn toggle_hunk(&mut self, file: &str, hunk_hash: &str, changed_total: usize) {
        let already = self.seen_count(file, hunk_hash, changed_total) == changed_total;
        self.set_hunk(file, hunk_hash, changed_total, !already);
    }

    /// How many of the hunk's `changed_total` lines are marked. Stale
    /// indices beyond the current total do not count.
    pub fn seen_count(&self, file: &str, hunk_hash: &str, changed_total: usize) -> usize {
        self.map
            .get(file)
            .and_then(|h| h.get(hunk_hash))
            .map(|s| s.iter().filter(|&&i| i < changed_total).count())
            .unwrap_or(0)
    }

    /// (seen, total) across a file's hunks, given as (hash, changed_total)
    /// pairs from the *current* diff — marks for vanished hunks are dead.
    pub fn progress<'a>(
        &self,
        file: &str,
        hunks: impl IntoIterator<Item = (&'a str, usize)>,
    ) -> (usize, usize) {
        let mut seen = 0;
        let mut total = 0;
        for (hash, changed) in hunks {
            seen += self.seen_count(file, hash, changed);
            total += changed;
        }
        (seen, total)
    }

    fn entry(&mut self, file: &str, hunk_hash: &str) -> &mut BTreeSet<usize> {
        self.map
            .entry(file.to_string())
            .or_default()
            .entry(hunk_hash.to_string())
            .or_default()
    }
}

/// Deterministic content hash (FNV-1a, 64 bit) over a hunk's lines.
/// Positions are excluded on purpose: a hunk that merely moved keeps its
/// marks, a hunk that changed loses them.
pub fn hunk_hash<'a>(lines: impl IntoIterator<Item = (Mark, &'a str)>) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    for (mark, text) in lines {
        eat(match mark {
            Mark::Context => b" ",
            Mark::Add => b"+",
            Mark::Del => b"-",
        });
        eat(text.as_bytes());
        eat(b"\n");
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hash() -> String {
        hunk_hash([
            (Mark::Context, "ctx"),
            (Mark::Del, "old"),
            (Mark::Add, "new"),
            (Mark::Add, "more"),
            (Mark::Context, "tail"),
        ])
    }

    #[test]
    fn hash_depends_on_content_not_position() {
        assert_eq!(sample_hash(), sample_hash());
        let edited = hunk_hash([(Mark::Context, "ctx"), (Mark::Add, "different")]);
        assert_ne!(sample_hash(), edited);
        // Kind matters even for identical text.
        assert_ne!(
            hunk_hash([(Mark::Add, "x")]),
            hunk_hash([(Mark::Del, "x")])
        );
    }

    #[test]
    fn toggle_set_and_progress() {
        let h = sample_hash();
        let mut store = Store::in_memory();
        assert_eq!(store.progress("f", [(h.as_str(), 3)]), (0, 3));

        store.toggle_line("f", &h, 0);
        assert!(store.is_seen("f", &h, 0));
        assert_eq!(store.progress("f", [(h.as_str(), 3)]), (1, 3));

        store.toggle_hunk("f", &h, 3);
        assert_eq!(store.progress("f", [(h.as_str(), 3)]), (3, 3));
        store.toggle_hunk("f", &h, 3);
        assert_eq!(store.progress("f", [(h.as_str(), 3)]), (0, 3));

        store.set_hunk("f", &h, 3, true);
        assert_eq!(store.seen_count("f", &h, 3), 3);
    }

    #[test]
    fn stale_marks_do_not_count() {
        let mut store = Store::in_memory();
        store.toggle_line("f", "dead_hash", 0);
        assert_eq!(store.progress("f", [(sample_hash().as_str(), 3)]), (0, 3));
        // Indices beyond the current changed_total are dead too.
        let h = sample_hash();
        store.toggle_line("f", &h, 99);
        assert_eq!(store.seen_count("f", &h, 3), 0);
    }

    #[test]
    fn survives_a_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seen.json");
        let h = sample_hash();
        {
            let mut store = Store::open(path.clone());
            store.toggle_line("f", &h, 1);
        }
        let store = Store::open(path);
        assert!(store.is_seen("f", &h, 1));
    }
}
