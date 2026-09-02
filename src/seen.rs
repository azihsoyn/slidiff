//! slidiff's adapter over the [`diffseen`] crate: maps our parsed diff
//! types onto its content-addressed store. The store itself — keying,
//! persistence, invalidation-by-content — lives in `crates/diffseen` and
//! knows nothing about this tool.

use std::path::PathBuf;

use diffseen::{Mark, Store};

use crate::diff::{FileDiff, Hunk, LineKind};

pub struct SeenStore(Store);

impl SeenStore {
    /// Persist under `<git-dir>/slidiff/seen.json` — local, uncommitted.
    pub fn load(git_dir: Option<PathBuf>) -> SeenStore {
        match git_dir {
            Some(dir) => SeenStore(Store::open(dir.join("slidiff/seen.json"))),
            None => SeenStore(Store::in_memory()),
        }
    }

    pub fn in_memory() -> SeenStore {
        SeenStore(Store::in_memory())
    }

    pub fn is_seen(&self, file: &str, hunk_hash: &str, idx: usize) -> bool {
        self.0.is_seen(file, hunk_hash, idx)
    }

    pub fn toggle_line(&mut self, file: &str, hunk_hash: &str, idx: usize) {
        self.0.toggle_line(file, hunk_hash, idx);
    }

    pub fn toggle_hunk(&mut self, file: &str, hunk_hash: &str, changed_total: usize) {
        self.0.toggle_hunk(file, hunk_hash, changed_total);
    }

    pub fn is_flagged(&self, file: &str, hunk_hash: &str, idx: usize) -> bool {
        self.0.is_flagged(file, hunk_hash, idx)
    }

    pub fn toggle_flag(&mut self, file: &str, hunk_hash: &str, idx: usize) {
        self.0.toggle_flag(file, hunk_hash, idx);
    }

    pub fn set_flag(&mut self, file: &str, hunk_hash: &str, idx: usize) {
        self.0.set_flag(file, hunk_hash, idx);
    }

    pub fn flag_count_cached(&self, file: &str, hunks: &[(String, usize)]) -> usize {
        self.0.flag_count(file, hunks)
    }

    /// Mark every hunk of the file seen; if the file already is fully
    /// seen, clear it. One keystroke for twenty near-identical jsonnet
    /// changes.
    pub fn toggle_file(&mut self, fd: &FileDiff) {
        let (seen, total) = self.progress_for(fd);
        let make_seen = !(total > 0 && seen == total);
        for (key, hunk) in hunk_keys(fd).iter().zip(&fd.hunks) {
            self.0
                .set_hunk(&fd.new_path, key, changed_count(hunk), make_seen);
        }
    }

    /// Toggle a specific set of changed lines — e.g. the ones a slide's
    /// excerpt shows. Completes first: if any are unseen, mark them all;
    /// only a fully seen set toggles off. Returns the new state.
    pub fn toggle_lines(&mut self, file: &str, lines: &[(String, usize)]) -> bool {
        let all_seen = lines
            .iter()
            .all(|(hash, idx)| self.0.is_seen(file, hash, *idx));
        for (hash, idx) in lines {
            self.0.set_line(file, hash, *idx, !all_seen);
        }
        !all_seen
    }

    /// [`Self::progress_for`] against precomputed (hash, changed) pairs,
    /// so hot paths never rehash the diff.
    pub fn progress_cached(&self, file: &str, hunks: &[(String, usize)]) -> (usize, usize) {
        self.0
            .progress(file, hunks.iter().map(|(h, c)| (h.as_str(), *c)))
    }

    /// (seen, total) changed lines for one file, counting only marks whose
    /// hunk hash still exists in the current diff.
    pub fn progress_for(&self, fd: &FileDiff) -> (usize, usize) {
        let keys = hunk_keys(fd);
        self.0.progress(
            &fd.new_path,
            keys.iter()
                .map(String::as_str)
                .zip(fd.hunks.iter().map(changed_count)),
        )
    }
}

pub fn changed_count(hunk: &Hunk) -> usize {
    hunk.lines
        .iter()
        .filter(|l| l.kind != LineKind::Context)
        .count()
}

/// Content keys for a file's hunks. Identical hunks in one file would
/// alias — marking one would mark them all — so the nth duplicate gets a
/// `#n` suffix. Unique hunks (the normal case) keep the bare hash, which
/// also keeps existing marks valid.
pub fn hunk_keys(fd: &FileDiff) -> Vec<String> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    fd.hunks
        .iter()
        .map(|h| {
            let raw = hunk_hash(h);
            let n = counts.entry(raw.clone()).or_insert(0);
            let key = if *n == 0 {
                raw.clone()
            } else {
                format!("{raw}#{n}")
            };
            *n += 1;
            key
        })
        .collect()
}

pub fn hunk_hash(hunk: &Hunk) -> String {
    diffseen::hunk_hash(hunk.lines.iter().map(|l| {
        (
            match l.kind {
                LineKind::Context => Mark::Context,
                LineKind::Add => Mark::Add,
                LineKind::Del => Mark::Del,
            },
            l.text.as_str(),
        )
    }))
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

    #[test]
    fn hash_ignores_position_but_not_content() {
        let files = parse_unified(SAMPLE);
        let mut moved = files[0].hunks[0].clone();
        moved.new_start = 500;
        moved.old_start = 499;
        assert_eq!(hunk_hash(&files[0].hunks[0]), hunk_hash(&moved));

        let mut edited = files[0].hunks[0].clone();
        edited.lines[2].text = "different".into();
        assert_ne!(hunk_hash(&files[0].hunks[0]), hunk_hash(&edited));
    }

    #[test]
    fn file_toggle_flips_between_all_and_none() {
        let files = parse_unified(SAMPLE);
        let fd = &files[0];
        let mut store = SeenStore::in_memory();

        assert_eq!(store.progress_for(fd), (0, 3));
        store.toggle_file(fd);
        assert_eq!(store.progress_for(fd), (3, 3));
        store.toggle_file(fd);
        assert_eq!(store.progress_for(fd), (0, 3));

        // Partially seen → toggle completes it first.
        store.toggle_line("f.rs", &hunk_hash(&fd.hunks[0]), 0);
        store.toggle_file(fd);
        assert_eq!(store.progress_for(fd), (3, 3));
    }

    #[test]
    fn identical_hunks_get_distinct_keys() {
        let two = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -1,2 +1,3 @@
 ctx
+same line
 tail
@@ -10,2 +11,3 @@
 ctx
+same line
 tail
";
        let files = parse_unified(two);
        let keys = hunk_keys(&files[0]);
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1], "duplicate hunks must not alias");
        assert!(keys[1].ends_with("#1"), "{keys:?}");

        // Marking one leaves the twin untouched.
        let mut store = SeenStore::in_memory();
        store.toggle_line("f.rs", &keys[0], 0);
        assert!(store.is_seen("f.rs", &keys[0], 0));
        assert!(!store.is_seen("f.rs", &keys[1], 0));
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
