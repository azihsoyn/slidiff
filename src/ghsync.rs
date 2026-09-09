//! GitHub PR "Viewed" ⇔ local seen sync, spoken through `gh` so
//! authentication stays gh's problem. Everything here blocks on network
//! calls — callers that own a UI run it off-thread.

use std::collections::HashSet;

use anyhow::{Context, Result};

pub struct Pr {
    pub id: String,
    pub number: u64,
    pub owner: String,
    pub name: String,
    /// Paths the PR touches, per GitHub.
    pub files: HashSet<String>,
}

/// The PR for the current branch (or an explicit number/URL/branch).
pub fn pr_info(pr: Option<&str>) -> Result<Pr> {
    let mut view = std::process::Command::new("gh");
    view.args(["pr", "view"]);
    if let Some(pr) = pr {
        view.arg(pr);
    }
    view.args(["--json", "id,number,url,files"]);
    let out = view.output().context("cannot run gh — is it installed?")?;
    if !out.status.success() {
        anyhow::bail!(
            "gh pr view failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let info: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let url = info["url"].as_str().context("PR has no url")?;
    // https://github.com/OWNER/REPO/pull/N
    let parts: Vec<&str> = url.trim_start_matches("https://").split('/').collect();
    Ok(Pr {
        id: info["id"].as_str().context("PR has no id")?.to_string(),
        number: info["number"].as_u64().unwrap_or(0),
        owner: parts.get(1).context("bad PR url")?.to_string(),
        name: parts.get(2).context("bad PR url")?.to_string(),
        files: info["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| f["path"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The set of paths the viewer already checked off on GitHub.
pub fn viewed_paths(pr: &Pr) -> Result<HashSet<String>> {
    let out = std::process::Command::new("gh")
        .args([
            "api", "graphql", "--paginate",
            "-f",
            "query=query($owner:String!,$name:String!,$number:Int!,$endCursor:String){repository(owner:$owner,name:$name){pullRequest(number:$number){files(first:100,after:$endCursor){nodes{path viewerViewedState}pageInfo{hasNextPage endCursor}}}}}",
            "-f",
        ])
        .arg(format!("owner={}", pr.owner))
        .arg("-f")
        .arg(format!("name={}", pr.name))
        .arg("-F")
        .arg(format!("number={}", pr.number))
        .arg("--jq")
        .arg(".data.repository.pullRequest.files.nodes[] | select(.viewerViewedState==\"VIEWED\") | .path")
        .output()
        .context("cannot run gh")?;
    if !out.status.success() {
        anyhow::bail!(
            "gh api graphql failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(std::str::from_utf8(&out.stdout)?
        .lines()
        .map(String::from)
        .collect())
}

/// Check one path's Viewed box on the PR. Best-effort.
pub fn mark_viewed(pr_id: &str, path: &str) -> bool {
    std::process::Command::new("gh")
        .args([
            "api",
            "graphql",
            "-f",
            "query=mutation($pr:ID!,$path:String!){markFileAsViewed(input:{pullRequestId:$pr,path:$path}){clientMutationId}}",
            "-f",
        ])
        .arg(format!("pr={pr_id}"))
        .arg("-f")
        .arg(format!("path={path}"))
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub struct SyncReport {
    pub number: u64,
    /// Viewed on GitHub after the push — the set to import locally.
    pub viewed: HashSet<String>,
    pub pushed: usize,
    pub push_failed: usize,
}

/// One blocking round-trip: read the PR's Viewed set, check off the
/// locally-done paths GitHub doesn't have yet, and hand back the final
/// set for import. An empty `push_done` makes this a pure pull.
pub fn sync(push_done: &[String]) -> Result<SyncReport> {
    let pr = pr_info(None)?;
    let mut viewed = viewed_paths(&pr)?;
    let mut pushed = 0;
    let mut push_failed = 0;
    for path in push_done {
        if !pr.files.contains(path) || viewed.contains(path) {
            continue;
        }
        if mark_viewed(&pr.id, path) {
            viewed.insert(path.clone());
            pushed += 1;
        } else {
            push_failed += 1;
        }
    }
    Ok(SyncReport {
        number: pr.number,
        viewed,
        pushed,
        push_failed,
    })
}
