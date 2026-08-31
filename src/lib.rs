pub mod deck;
pub mod diff;
pub mod highlight;
pub mod md;
pub mod mdeck;
pub mod ui;

/// The deck JSON Schema, pretty-printed. Published via `debrief schema` and
/// pinned by the golden test — the schema is the contract agents write to.
pub fn schema_json() -> String {
    let schema = schemars::schema_for!(deck::Deck);
    serde_json::to_string_pretty(&schema.to_value()).expect("schema serializes")
}
