//! Against a real repository: base resolution and hunk lookup, the path a
//! deck's `at: file:line` travels before anything is drawn.

use std::path::Path;
use std::process::Command;

use slidiff::diff::{FileStatus, Repo, file_diff, hunk_at, load_diff};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn write(dir: &Path, file: &str, content: &str) {
    std::fs::write(dir.join(file), content).unwrap();
}

#[test]
fn diff_against_moved_base_shows_only_own_work() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git(dir, &["init", "-q", "-b", "main"]);
    write(dir, "a.txt", "one\ntwo\nthree\n");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "base"]);

    // Branch off here, but let main move ahead first; its commit must not
    // appear reversed in the deck's diff.
    git(dir, &["branch", "-q", "work"]);
    write(dir, "main_only.txt", "elsewhere\n");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "main moves"]);

    // On work: change a line, commit, and leave an uncommitted addition.
    git(dir, &["checkout", "-q", "work"]);
    write(dir, "a.txt", "one\nTWO\nthree\n");
    git(dir, &["commit", "-q", "-am", "change two"]);
    write(dir, "b.txt", "fresh\n");
    git(dir, &["add", "b.txt"]);

    let repo = Repo::discover(dir).unwrap();
    let files = load_diff(&repo, Some("main")).unwrap();

    let paths: Vec<&str> = files.iter().map(|f| f.new_path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "b.txt"], "main's own commit must not leak in");
    assert_eq!(files[1].status, FileStatus::Added);

    // a.txt line 2 (new side) lands in the only hunk, exactly.
    let fd = file_diff(&files, "a.txt").unwrap();
    let (hunk, exact) = hunk_at(fd, 2).unwrap();
    assert!(exact);
    assert!(hunk.lines.iter().any(|l| l.text == "TWO"));

    // A line outside any hunk still finds the nearest one.
    let (_, exact) = hunk_at(fd, 3000).unwrap();
    assert!(!exact);
}

#[test]
fn default_base_is_head_worktree_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    git(dir, &["init", "-q", "-b", "main"]);
    write(dir, "a.txt", "one\n");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "base"]);
    write(dir, "a.txt", "one\nuncommitted\n");

    let repo = Repo::discover(dir).unwrap();
    let files = load_diff(&repo, None).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].added(), 1);
    assert_eq!(files[0].deleted(), 0);
}
