//! Checked logical size accounting for session-owned evidence.

use agent_jit_domain::canonical::canonical_string_of;
use agent_jit_domain::envelope::Envelope;
use agent_jit_domain::ids::SessionId;
use agent_jit_domain::trace::Event;
use rusqlite::Connection;

use crate::StoreError;

pub(crate) fn logical_bytes(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<u64, StoreError> {
    let mut statement = connection.prepare(
        "SELECT record_json FROM (\
         SELECT record_json FROM sessions WHERE session_id=?1 UNION ALL \
         SELECT record_json FROM trajectories WHERE session_id=?1 UNION ALL \
         SELECT o.record_json FROM outcomes o JOIN trajectories t USING(trajectory_id) \
           WHERE t.session_id=?1 UNION ALL \
         SELECT m.record_json FROM trace_metrics m JOIN trajectories t USING(trajectory_id) \
           WHERE t.session_id=?1 UNION ALL \
         SELECT a.record_json FROM outcome_annotations a \
           JOIN trajectories t USING(trajectory_id) WHERE t.session_id=?1)",
    )?;
    let rows = statement.query_map([session_id.to_string()], |row| row.get::<_, String>(0))?;
    let mut total = 0_u64;
    for row in rows {
        let json = row?;
        total = total
            .checked_add(canonical_len(&json)?)
            .ok_or_else(size_overflow)?;
    }
    total = total
        .checked_add(event_bytes(connection, session_id)?)
        .ok_or_else(size_overflow)?;
    Ok(total)
}

fn event_bytes(connection: &Connection, session_id: &SessionId) -> Result<u64, StoreError> {
    let mut statement = connection.prepare(
        "SELECT record_json,payload_bytes FROM events WHERE session_id=?1 ORDER BY event_id",
    )?;
    let rows = statement.query_map([session_id.to_string()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut total = 0_u64;
    for row in rows {
        let (json, indexed_payload_bytes) = row?;
        let event: Envelope<Event> = serde_json::from_str(&json).map_err(size_error)?;
        let indexed_payload_bytes = u64::try_from(indexed_payload_bytes).map_err(size_error)?;
        if event.body().session_id != *session_id
            || event.body().payload_bytes != indexed_payload_bytes
        {
            return Err(size_error(
                "event index does not match authoritative record",
            ));
        }
        total = total
            .checked_add(canonical_len(&json)?)
            .and_then(|value| value.checked_add(event.body().payload_bytes))
            .ok_or_else(size_overflow)?;
    }
    Ok(total)
}

fn canonical_len(json: &str) -> Result<u64, StoreError> {
    let value = serde_json::from_str::<serde_json::Value>(json).map_err(size_error)?;
    let canonical = canonical_string_of(&value).map_err(size_error)?;
    u64::try_from(canonical.len()).map_err(size_error)
}

pub(crate) fn total_bytes(connection: &Connection) -> Result<u64, StoreError> {
    let mut statement =
        connection.prepare("SELECT session_id FROM sessions ORDER BY session_id")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut total = 0_u64;
    for row in rows {
        let value = row?;
        let session_id = value.parse().map_err(size_error)?;
        total = total
            .checked_add(logical_bytes(connection, &session_id)?)
            .ok_or_else(size_overflow)?;
    }
    Ok(total)
}

pub(crate) fn size_overflow() -> StoreError {
    StoreError::RetentionSizeUnavailable {
        reason: "negative or overflowing logical byte count".to_owned(),
    }
}

fn size_error(error: impl std::fmt::Display) -> StoreError {
    StoreError::RetentionSizeUnavailable {
        reason: error.to_string(),
    }
}
