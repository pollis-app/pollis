use rusqlite::Connection;
use crate::signal::group::SenderKeyState;
use crate::error::Result;

/// Load our SenderKeyState for a given channel from local DB.
/// channel_id can be a group channel id or dm_channel id.
/// sender_id is our own user_id.
pub fn load_sender_key(
    conn: &Connection,
    channel_id: &str,
    sender_id: &str,
) -> Result<Option<SenderKeyState>> {
    let result = conn.query_row(
        "SELECT chain_id, iteration, chain_key FROM group_sender_key
         WHERE group_id = ?1 AND sender_id = ?2",
        rusqlite::params![channel_id, sender_id],
        |row| {
            let chain_id: Vec<u8> = row.get(0)?;
            let iteration: u32 = row.get(1)?;
            let chain_key: Vec<u8> = row.get(2)?;
            Ok(SenderKeyState { chain_id, iteration, chain_key, skipped_keys: Default::default() })
        },
    );

    match result {
        Ok(state) => Ok(Some(state)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Save/update our SenderKeyState for a channel.
pub fn save_sender_key(
    conn: &Connection,
    channel_id: &str,
    sender_id: &str,
    state: &SenderKeyState,
) -> Result<()> {
    conn.execute(
        "INSERT INTO group_sender_key (group_id, sender_id, chain_id, iteration, chain_key, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
         ON CONFLICT(group_id, sender_id) DO UPDATE SET
             chain_id = excluded.chain_id,
             iteration = excluded.iteration,
             chain_key = excluded.chain_key,
             updated_at = excluded.updated_at",
        rusqlite::params![
            channel_id,
            sender_id,
            &state.chain_id,
            state.iteration,
            &state.chain_key,
        ],
    )?;
    Ok(())
}

/// Load a peer's SenderKeyState (received via distribution) for a channel.
pub fn load_peer_sender_key(
    conn: &Connection,
    channel_id: &str,
    sender_id: &str,
) -> Result<Option<SenderKeyState>> {
    // Peer keys are stored in the same table using a synthetic sender_id
    // prefixed with "peer:" to distinguish from own keys.
    let peer_key = format!("peer:{sender_id}");
    load_sender_key(conn, channel_id, &peer_key)
}

/// Save a peer's SenderKeyState for a channel.
pub fn save_peer_sender_key(
    conn: &Connection,
    channel_id: &str,
    sender_id: &str,
    state: &SenderKeyState,
) -> Result<()> {
    let peer_key = format!("peer:{sender_id}");
    save_sender_key(conn, channel_id, &peer_key, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::signal::group::SenderKeyState;

    const LOCAL_SCHEMA: &str = include_str!("../db/migrations/local_schema.sql");

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // Set up migrations table first
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
        ").unwrap();
        conn.execute_batch(LOCAL_SCHEMA).unwrap();
        conn
    }

    #[test]
    fn save_and_load_sender_key_roundtrip() {
        let conn = db();
        let state = SenderKeyState::new();
        let channel_id = "ch-test";
        let sender_id = "alice";

        save_sender_key(&conn, channel_id, sender_id, &state).expect("save should succeed");

        let loaded = load_sender_key(&conn, channel_id, sender_id)
            .expect("load should succeed")
            .expect("should find saved state");

        assert_eq!(state.chain_id, loaded.chain_id);
        assert_eq!(state.iteration, loaded.iteration);
        assert_eq!(state.chain_key, loaded.chain_key);
    }

    #[test]
    fn load_nonexistent_sender_key_returns_none() {
        let conn = db();
        let result = load_sender_key(&conn, "missing-channel", "missing-user")
            .expect("load should not error");
        assert!(result.is_none());
    }

    #[test]
    fn save_sender_key_updates_on_conflict() {
        let conn = db();
        let channel_id = "ch-test";
        let sender_id = "alice";

        let state1 = SenderKeyState::new();
        save_sender_key(&conn, channel_id, sender_id, &state1).expect("first save");

        let mut state2 = SenderKeyState::new();
        state2.iteration = 42;
        save_sender_key(&conn, channel_id, sender_id, &state2).expect("second save");

        let loaded = load_sender_key(&conn, channel_id, sender_id)
            .expect("load should succeed")
            .expect("should find updated state");

        assert_eq!(loaded.iteration, 42);
        assert_eq!(loaded.chain_id, state2.chain_id);
    }

    #[test]
    fn save_and_load_peer_sender_key_roundtrip() {
        let conn = db();
        let state = SenderKeyState::new();
        let channel_id = "ch-test";
        let peer_id = "bob";

        save_peer_sender_key(&conn, channel_id, peer_id, &state).expect("save peer key");

        let loaded = load_peer_sender_key(&conn, channel_id, peer_id)
            .expect("load should succeed")
            .expect("should find peer state");

        assert_eq!(state.chain_id, loaded.chain_id);
        assert_eq!(state.iteration, loaded.iteration);
        assert_eq!(state.chain_key, loaded.chain_key);
    }

    #[test]
    fn own_and_peer_keys_are_separate() {
        let conn = db();
        let channel_id = "ch-test";
        let user_id = "alice";

        let own_state = SenderKeyState::new();
        let peer_state = SenderKeyState::new();

        save_sender_key(&conn, channel_id, user_id, &own_state).expect("save own");
        save_peer_sender_key(&conn, channel_id, user_id, &peer_state).expect("save peer");

        let loaded_own = load_sender_key(&conn, channel_id, user_id)
            .expect("load own")
            .expect("own exists");

        let loaded_peer = load_peer_sender_key(&conn, channel_id, user_id)
            .expect("load peer")
            .expect("peer exists");

        assert_eq!(loaded_own.chain_id, own_state.chain_id);
        assert_eq!(loaded_peer.chain_id, peer_state.chain_id);
        // Confirm they are different (extremely unlikely they would match by chance)
        assert_ne!(loaded_own.chain_id, loaded_peer.chain_id);
    }
}
