//! Remember where the reader left off, per deck. A tiny map from the
//! deck's canonical path to a slide index, kept next to the seen record
//! under the repository's git dir — local, never committed.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn file_path(git_dir: &Option<PathBuf>) -> Option<PathBuf> {
    git_dir.as_ref().map(|d| d.join("slidiff/resume.json"))
}

fn read(git_dir: &Option<PathBuf>) -> BTreeMap<String, usize> {
    file_path(git_dir)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The slide the reader was on last time this deck was open.
pub fn load(git_dir: &Option<PathBuf>, deck_key: &str) -> Option<usize> {
    read(git_dir).get(deck_key).copied()
}

pub fn save(git_dir: &Option<PathBuf>, deck_key: &str, step: usize) {
    let Some(path) = file_path(git_dir) else { return };
    let mut map = read(git_dir);
    map.insert(deck_key.to_string(), step);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_per_deck() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = Some(dir.path().to_path_buf());
        assert_eq!(load(&git_dir, "/tmp/a.md"), None);
        save(&git_dir, "/tmp/a.md", 7);
        save(&git_dir, "/tmp/b.md", 2);
        assert_eq!(load(&git_dir, "/tmp/a.md"), Some(7));
        assert_eq!(load(&git_dir, "/tmp/b.md"), Some(2));
        save(&git_dir, "/tmp/a.md", 9);
        assert_eq!(load(&git_dir, "/tmp/a.md"), Some(9));
    }

    #[test]
    fn no_git_dir_is_a_quiet_noop() {
        save(&None, "/tmp/a.md", 3);
        assert_eq!(load(&None, "/tmp/a.md"), None);
    }
}
