//! Pins the published JSON Schema. The schema is the contract agents write
//! decks against; any drift must be a deliberate commit, never a side effect.
//!
//! To accept an intentional change: UPDATE_GOLDEN=1 cargo test

use std::path::Path;

#[test]
fn schema_matches_golden() {
    let golden_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/debrief.schema.json");
    let current = debrief::schema_json() + "\n";

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden_path.parent().unwrap()).unwrap();
        std::fs::write(&golden_path, &current).unwrap();
        return;
    }

    let golden = std::fs::read_to_string(&golden_path)
        .expect("schema/debrief.schema.json missing — run UPDATE_GOLDEN=1 cargo test");
    assert_eq!(
        golden, current,
        "schema drifted from schema/debrief.schema.json — if intentional, run UPDATE_GOLDEN=1 cargo test and commit the diff"
    );
}
