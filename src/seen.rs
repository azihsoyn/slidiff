//! debrief's adapter over the [`diffseen`] crate: maps our parsed diff
//! types onto its content-addressed store. The store itself — keying,
//! persistence, invalidation-by-content — lives in `crates/diffseen` and
//! knows nothing about this tool.

use std::path::PathBuf;

use diffseen::{Mark, Store};

use crate::diff::{FileDiff, Hunk, LineKind};

pub struct SeenStore(Store);

impl SeenStore {
    /// Persist under `<git-dir>/debrief/seen.json` — local, uncommitted.
    pub fn load(git_dir: Option<PathBuf>) -> SeenStore {
        match git_dir {
            Some(dir) => SeenStore(Store::open(dir.join("debrief/seen.json"))),
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

    /// Mark every hunk of the file seen; if the file already is fully
    /// seen, clear it. One keystroke for twenty near-identical jsonnet
    /// changes.
    pub fn toggle_file(&mut self, fd: &FileDiff) {
        let (seen, total) = self.progress_for(fd);
        let make_seen = !(total > 0 && seen == total);
        for hunk in &fd.hunks {
            self.0.set_hunk(
                &fd.new_path,
                &hunk_hash(hunk),
                changed_count(hunk),
                make_seen,
            );
        }
    }

    /// (seen, total) changed lines for one file, counting only marks whose
    /// hunk hash still exists in the current diff.
    pub fn progress_for(&self, fd: &FileDiff) -> (usize, usize) {
        let hunks: Vec<(String, usize)> = fd
            .hunks
            .iter()
            .map(|h| (hunk_hash(h), changed_count(h)))
            .collect();
        self.0.progress(
            &fd.new_path,
            hunks.iter().map(|(h, c)| (h.as_str(), *c)),
        )
    }
}

pub fn changed_count(hunk: &Hunk) -> usize {
    hunk.lines
        .iter()
        .filter(|l| l.kind != LineKind::Context)
        .count()
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
