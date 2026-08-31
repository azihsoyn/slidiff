//! Running git. The only process this tool ever spawns.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::parse::{FileDiff, parse_unified};

/// A repository the viewer reads from. `root` is the worktree top level.
#[derive(Debug, Clone)]
pub struct Repo {
    pub root: PathBuf,
}

impl Repo {
    /// Discover the repository containing `dir`.
    pub fn discover(dir: &Path) -> Result<Repo> {
        let out = git_in(dir, &["rev-parse", "--show-toplevel"])?;
        Ok(Repo {
            root: PathBuf::from(out.trim()),
        })
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        git_in(&self.root, args)
    }

    /// The actual git dir (resolves the `.git` file of a linked worktree).
    pub fn git_dir(&self) -> Option<PathBuf> {
        let out = self.git(&["rev-parse", "--absolute-git-dir"]).ok()?;
        Some(PathBuf::from(out.trim()))
    }

    /// Resolve what the deck's `base` means as a concrete diff base:
    /// the merge-base of `base` and HEAD. Diffing from there shows the
    /// work done since branching plus anything uncommitted, and never
    /// the reversal of commits the base branch gained meanwhile.
    pub fn resolve_base(&self, base: &str) -> Result<String> {
        let sha = self
            .git(&["merge-base", base, "HEAD"])
            .with_context(|| format!("cannot resolve base \"{base}\""))?;
        Ok(sha.trim().to_string())
    }

    /// The full worktree diff against `base_sha`, parsed.
    pub fn diff(&self, base_sha: &str) -> Result<Vec<FileDiff>> {
        let out = self.git(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            "-U3",
            base_sha,
        ])?;
        Ok(parse_unified(&out))
    }

    /// Current contents of a worktree file, split into lines.
    pub fn read_file(&self, path: &str) -> Result<Vec<String>> {
        let full = self.root.join(path);
        let text = std::fs::read_to_string(&full)
            .with_context(|| format!("cannot read {}", full.display()))?;
        Ok(text.lines().map(str::to_string).collect())
    }
}

fn git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("cannot run git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("git output is not utf-8")
}

/// Load the deck's diff: resolve `base` (HEAD when unset) and parse the
/// worktree diff against it.
pub fn load_diff(repo: &Repo, base: Option<&str>) -> Result<Vec<FileDiff>> {
    let base = base.unwrap_or("HEAD");
    let sha = repo.resolve_base(base)?;
    repo.diff(&sha)
}
