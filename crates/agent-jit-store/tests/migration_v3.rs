#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Executable v2-to-v3 legacy compatibility test.

mod migration_support;

use agent_jit_domain::ids::OutcomeId;
use agent_jit_store::{CURRENT_SCHEMA_VERSION, Store, StorePath};
use migration_support::{OUTCOME_DIGEST, OUTCOME_JSON, create_v2};
use rusqlite::Connection;

#[test]
fn given_v2_outcome_when_migrated_then_v1_record_loads_without_byte_or_digest_changes() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    create_v2(&path);

    // When
    let mut store = Store::open(&StorePath::new(&path).unwrap()).unwrap();
    let report = store.migrate().unwrap();

    // Then
    assert_eq!(CURRENT_SCHEMA_VERSION, 3);
    assert_eq!((report.from_version, report.to_version), (2, 3));
    let id: OutcomeId = "out_01J0000000000000000000000A".parse().unwrap();
    let loaded = store.get_outcome(&id).unwrap().unwrap();
    assert_eq!(serde_json::to_string(&loaded).unwrap(), OUTCOME_JSON);
    assert_eq!(loaded.digest().unwrap().to_string(), OUTCOME_DIGEST);
    drop(store);

    let connection = Connection::open(path).unwrap();
    let stored: (String, String) = connection
        .query_row(
            "SELECT record_json,digest FROM outcomes WHERE outcome_id=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (OUTCOME_JSON.to_owned(), OUTCOME_DIGEST.to_owned()));
    let trajectory: (String, String) = connection
        .query_row(
            "SELECT record_json,digest FROM trajectories WHERE trajectory_id=?1",
            ["trj_01J0000000000000000000000A"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(trajectory, ("{}".to_owned(), "td".to_owned()));
    let annotations: i64 = connection
        .query_row("SELECT COUNT(*) FROM outcome_annotations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let pointers: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM outcome_annotation_current",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((annotations, pointers), (0, 0));
}
