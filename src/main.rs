use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use slidiff::deck::Deck;
use slidiff::schema_json;

const USAGE: &str = "\
slidiff — a deck an agent writes, a person reads in the terminal

usage:
  slidiff <deck.md|yaml>      view a deck (press ? inside for the keymap)
  slidiff check <deck.md|yaml>  validate a deck, exit 1 with what to fix
  slidiff comments [deck]     print the review comments as markdown
                              (anchors resolved; [deck] supplies the diff base)
  slidiff viewed [deck] [--pr N] [--dry-run]
                              mark files that are fully seen locally (and
                              carry no flags) as Viewed on the GitHub PR
  slidiff viewed --pull ...   the other direction: files marked Viewed on
                              the PR become fully seen locally
  slidiff schema              print the deck JSON Schema
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some("schema") => cmd_schema(),
        Some("comments") => cmd_comments(args.get(1).map(Path::new)),
        Some("viewed") => cmd_viewed(&args[1..]),
        Some("check") => match args.get(1) {
            Some(path) => cmd_check(Path::new(path)),
            None => {
                eprint!("check needs a deck file\n\n{USAGE}");
                Ok(ExitCode::FAILURE)
            }
        },
        Some(path) => cmd_view(Path::new(path)),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("slidiff: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_schema() -> Result<ExitCode> {
    println!("{}", schema_json());
    Ok(ExitCode::SUCCESS)
}

fn load_deck(path: &Path) -> Result<Deck> {
    slidiff::load_deck(path)
}

fn cmd_check(path: &Path) -> Result<ExitCode> {
    let deck = load_deck(path)?;
    let errors = deck.validate();
    if errors.is_empty() {
        println!(
            "ok: {} step{}",
            deck.steps.len(),
            if deck.steps.len() == 1 { "" } else { "s" }
        );
        return Ok(ExitCode::SUCCESS);
    }
    for error in &errors {
        eprintln!("{error}");
    }
    Ok(ExitCode::FAILURE)
}

/// The review comments as one markdown bundle — what an agent reads to
/// pick up the reader's feedback without the TUI.
fn cmd_comments(deck_path: Option<&Path>) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read current dir")?;
    let repo = slidiff::diff::Repo::discover(&cwd)?;
    let base = match deck_path {
        Some(p) => load_deck(p)?.base,
        None => None,
    };
    let files = slidiff::diff::load_diff(&repo, base.as_deref())?;
    let comments = slidiff::comments::CommentStore::load(repo.git_dir());
    if comments.is_empty() {
        println!("no comments");
        return Ok(ExitCode::SUCCESS);
    }
    print!("{}", comments.bundle(&files));
    Ok(ExitCode::SUCCESS)
}

/// Mirror the local review state to GitHub: every file whose changed
/// lines are all seen (and unflagged) gets the PR's Viewed checkbox,
/// via `gh` so authentication stays gh's problem.
fn cmd_viewed(args: &[String]) -> Result<ExitCode> {
    let mut deck_path: Option<&Path> = None;
    let mut pr: Option<&str> = None;
    let mut dry_run = false;
    let mut pull = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--pull" => pull = true,
            "--pr" => pr = Some(it.next().context("--pr needs a number")?.as_str()),
            other => deck_path = Some(Path::new(other)),
        }
    }

    let cwd = std::env::current_dir().context("cannot read current dir")?;
    let repo = slidiff::diff::Repo::discover(&cwd)?;
    let base = match deck_path {
        Some(p) => load_deck(p)?.base,
        None => None,
    };
    let files = slidiff::diff::load_diff(&repo, base.as_deref())?;
    let seen = slidiff::seen::SeenStore::load(repo.git_dir());

    // The PR's identity and file list, through gh.
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
    let pr_info: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let pr_id = pr_info["id"].as_str().context("PR has no id")?;
    let pr_number = pr_info["number"].as_u64().unwrap_or(0);

    if pull {
        return pull_viewed(&pr_info, &files, &mut slidiff::seen::SeenStore::load(repo.git_dir()), dry_run);
    }
    let pr_files: std::collections::HashSet<&str> = pr_info["files"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f["path"].as_str()).collect())
        .unwrap_or_default();

    let done: Vec<&str> = files
        .iter()
        .filter(|fd| pr_files.contains(fd.new_path.as_str()))
        .filter(|fd| seen.file_is_done(fd))
        .map(|fd| fd.new_path.as_str())
        .collect();

    if done.is_empty() {
        println!("no fully seen, unflagged files intersect PR #{pr_number}");
        return Ok(ExitCode::SUCCESS);
    }
    if dry_run {
        println!("would mark viewed on PR #{pr_number}:");
        for path in &done {
            println!("  {path}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut marked = 0;
    for path in &done {
        let status = std::process::Command::new("gh")
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
            .status();
        match status {
            Ok(s) if s.success() => {
                marked += 1;
                println!("viewed: {path}");
            }
            _ => eprintln!("failed to mark: {path}"),
        }
    }
    println!("marked {marked}/{} file(s) viewed on PR #{pr_number}", done.len());
    Ok(ExitCode::SUCCESS)
}

/// GitHub → local: every PR file the viewer marked Viewed becomes fully
/// seen here. One-way — an unviewed checkbox never erases line-level
/// progress, and flags/comments stay untouched.
fn pull_viewed(
    pr_info: &serde_json::Value,
    files: &[slidiff::diff::FileDiff],
    seen: &mut slidiff::seen::SeenStore,
    dry_run: bool,
) -> Result<ExitCode> {
    let pr_number = pr_info["number"].as_u64().unwrap_or(0);
    let url = pr_info["url"].as_str().context("PR has no url")?;
    // https://github.com/OWNER/REPO/pull/N
    let parts: Vec<&str> = url.trim_start_matches("https://").split('/').collect();
    let (owner, name) = (
        parts.get(1).context("bad PR url")?,
        parts.get(2).context("bad PR url")?,
    );
    let out = std::process::Command::new("gh")
        .args([
            "api", "graphql", "--paginate",
            "-f",
            "query=query($owner:String!,$name:String!,$number:Int!,$endCursor:String){repository(owner:$owner,name:$name){pullRequest(number:$number){files(first:100,after:$endCursor){nodes{path viewerViewedState}pageInfo{hasNextPage endCursor}}}}}",
            "-f",
        ])
        .arg(format!("owner={owner}"))
        .arg("-f")
        .arg(format!("name={name}"))
        .arg("-F")
        .arg(format!("number={pr_number}"))
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
    let viewed: std::collections::HashSet<&str> =
        std::str::from_utf8(&out.stdout)?.lines().collect();

    let mut imported = 0;
    let mut already = 0;
    for fd in files {
        if !viewed.contains(fd.new_path.as_str()) {
            continue;
        }
        let (s, t) = seen.progress_for(fd);
        if t == 0 || s == t {
            already += 1;
            continue;
        }
        if dry_run {
            println!("would mark seen locally: {} ({} lines)", fd.new_path, t - s);
        } else {
            seen.mark_file_seen(fd);
            println!("seen: {} ({} lines imported)", fd.new_path, t - s);
        }
        imported += 1;
    }
    println!(
        "PR #{pr_number}: {} viewed on GitHub · {imported} imported{} · {already} already seen",
        viewed.len(),
        if dry_run { " (dry run)" } else { "" },
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_view(path: &Path) -> Result<ExitCode> {
    let deck = load_deck(path)?;
    let errors = deck.validate();
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("{error}");
        }
        bail!("deck does not validate — fix it or run `slidiff check`");
    }
    let cwd = std::env::current_dir().context("cannot read current dir")?;
    let repo = slidiff::diff::Repo::discover(&cwd)?;
    let deck_key = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();
    slidiff::ui::run(deck, repo, Some(deck_key))?;
    Ok(ExitCode::SUCCESS)
}
