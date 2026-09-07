#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use rusqlite::{Connection, params};

pub const OUTCOME_JSON: &str = r#"{"schema":"agent_jit.outcome","version":1,"id":"out_01J0000000000000000000000A","provenance":{"produced_by":"agent-jit/0.1.0","source":"confirmed","recorded_at_unix_ms":1756000000000,"parents":[]},"body":{"trajectory_id":"trj_01J0000000000000000000000A","status":"solved","note":null,"metrics":{"tool_calls":3,"agent_turns":2,"estimated_input_tokens":100,"estimated_output_tokens":20,"duration_ms":42000},"confirmed_by_human":true}}"#;
pub const OUTCOME_DIGEST: &str = "84bcfbddde2d80e258a1baade4264103248ce1fbef01478ad6b2bf0e060ea77d";

pub fn create_v2(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(include_str!("../../migrations/0001_initial.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!("../../migrations/0002_trace_metrics.sql"))
        .unwrap();
    connection.execute_batch("PRAGMA user_version = 2").unwrap();
    connection
        .execute(
            "INSERT INTO repositories VALUES (?1,'/redacted/.git','identity','repo','{}','rd',1)",
            ["rep_01J0000000000000000000000A"],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sessions VALUES (?1,?2,'/redacted',?3,'ClaudeCode','2','model',1,2,'{}','sd',1)",
            params![
                "ses_01J0000000000000000000000A",
                "rep_01J0000000000000000000000A",
                "0".repeat(40)
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO trajectories VALUES (?1,?2,?3,?4,'intent','{}','td',1)",
            params![
                "trj_01J0000000000000000000000A",
                "ses_01J0000000000000000000000A",
                "rep_01J0000000000000000000000A",
                "0".repeat(40)
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO outcomes VALUES (?1,?2,'Solved',1,?3,?4,1)",
            params![
                "out_01J0000000000000000000000A",
                "trj_01J0000000000000000000000A",
                OUTCOME_JSON,
                OUTCOME_DIGEST
            ],
        )
        .unwrap();
    drop(connection);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
