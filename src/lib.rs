pub mod comments;
pub mod deck;
pub mod diff;
pub mod highlight;
pub mod md;
pub mod mdeck;
pub mod resume;
pub mod seen;
pub mod ui;

/// The deck JSON Schema, pretty-printed. Published via `slidiff schema` and
/// pinned by the golden test — the schema is the contract agents write to.
pub fn schema_json() -> String {
    let schema = schemars::schema_for!(deck::Deck);
    serde_json::to_string_pretty(&schema.to_value()).expect("schema serializes")
}

/// Load a deck from md / yaml / json by extension. Shared by the CLI and
/// the viewer's watch reload.
pub fn load_deck(path: &std::path::Path) -> anyhow::Result<deck::Deck> {
    use anyhow::Context;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let deck: deck::Deck = match path.extension().and_then(|e| e.to_str()) {
        Some("json") => serde_json::from_str(&text)
            .with_context(|| format!("{} is not a valid deck", path.display()))?,
        Some("md" | "markdown") => mdeck::parse(&text)
            .map_err(|errors| anyhow::anyhow!("{}", errors.join("\n")))?,
        _ => serde_yaml::from_str(&text)
            .with_context(|| format!("{} is not a valid deck", path.display()))?,
    };
    Ok(deck)
}
