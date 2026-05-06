use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppResult;

/// Tracks which message guids the user has marked complete. Stored in
/// a separate SQLite file (the app's data dir) — `chat.db` itself is
/// never written to.
pub struct StateStore {
    conn: Mutex<Connection>,
}

impl StateStore {
    pub fn open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS completed (
                last_message_guid TEXT PRIMARY KEY,
                completed_at INTEGER NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn mark_complete(&self, last_message_guid: &str) -> AppResult<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let conn = self.conn.lock().expect("state lock poisoned");
        conn.execute(
            "INSERT INTO completed (last_message_guid, completed_at)
             VALUES (?1, ?2)
             ON CONFLICT(last_message_guid) DO UPDATE SET completed_at = excluded.completed_at",
            params![last_message_guid, now],
        )?;
        Ok(())
    }

    pub fn completed_guids(&self) -> AppResult<HashSet<String>> {
        let conn = self.conn.lock().expect("state lock poisoned");
        let mut stmt = conn.prepare("SELECT last_message_guid FROM completed")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for r in rows {
            out.insert(r?);
        }
        Ok(out)
    }
}
