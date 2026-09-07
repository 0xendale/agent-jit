#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Typed absence checks used after migration.

mod migration_support;

use agent_jit_domain::ids::TrajectoryId;
use agent_jit_store::{Store, StorePath};
use migration_support::create_v2;

#[test]
fn given_migrated_store_when_legacy_trajectory_is_queried_then_no_annotation_or_pointer_exists() {
    // Given
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("state.sqlite3");
    create_v2(&database);
    let path = StorePath::new(&database).unwrap();
    let mut store = Store::open(&path).unwrap();
    store.migrate().unwrap();
    let trajectory: TrajectoryId = "trj_01J0000000000000000000000A".parse().unwrap();

    // When
    let history = store.list_outcome_annotations(&trajectory).unwrap();

    // Then
    assert!(history.is_empty());
    assert!(
        store
            .current_outcome_annotation(&trajectory)
            .unwrap()
            .is_none()
    );
}
