use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use slidiff::deck::Deck;
use slidiff::schema_json;

const USAGE: &str = "\
slidiff — a deck an agent writes, a person reads in the terminal

usage:
  slidiff <deck.md|yaml>      view a deck (n/p step, Enter dive into full diff, q quit)
  slidiff check <deck.md|yaml>  validate a deck, exit 1 with what to fix
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
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let deck: Deck = match path.extension().and_then(|e| e.to_str()) {
        Some("json") => serde_json::from_str(&text)
            .with_context(|| format!("{} is not a valid deck", path.display()))?,
        Some("md" | "markdown") => slidiff::mdeck::parse(&text)
            .map_err(|errors| anyhow::anyhow!("{}", errors.join("\n")))?,
        _ => serde_yaml::from_str(&text)
            .with_context(|| format!("{} is not a valid deck", path.display()))?,
    };
    Ok(deck)
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
    slidiff::ui::run(deck, repo)?;
    Ok(ExitCode::SUCCESS)
}
