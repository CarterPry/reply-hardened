use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppResult;

/// A chat the user has permanently hidden. The name is captured at
/// mute time so the unmute UI can render without touching chat.db.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutedChat {
    pub chat_guid: String,
    pub name: String,
}

/// Tracks per-user state: message guids marked complete, snoozed
/// chats, and muted chats. Stored in a separate SQLite file (the
/// app's data dir) — `chat.db` itself is never written to.
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
            );
            CREATE TABLE IF NOT EXISTS snoozed (
                chat_guid TEXT PRIMARY KEY,
                snoozed_until INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS muted (
                chat_guid TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                muted_at INTEGER NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn mark_complete(&self, last_message_guid: &str) -> AppResult<()> {
        let now = now_unix();
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

    pub fn snooze_chat(&self, chat_guid: &str, until: i64) -> AppResult<()> {
        let conn = self.conn.lock().expect("state lock poisoned");
        conn.execute(
            "INSERT INTO snoozed (chat_guid, snoozed_until)
             VALUES (?1, ?2)
             ON CONFLICT(chat_guid) DO UPDATE SET snoozed_until = excluded.snoozed_until",
            params![chat_guid, until],
        )?;
        Ok(())
    }

    /// chat guid → snoozed-until unix timestamp. Expired rows are
    /// pruned on read so the table doesn't grow unbounded.
    pub fn snoozed_until(&self) -> AppResult<HashMap<String, i64>> {
        let now = now_unix();
        let conn = self.conn.lock().expect("state lock poisoned");
        conn.execute(
            "DELETE FROM snoozed WHERE snoozed_until <= ?1",
            params![now],
        )?;
        let mut stmt = conn.prepare("SELECT chat_guid, snoozed_until FROM snoozed")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let (guid, until) = r?;
            out.insert(guid, until);
        }
        Ok(out)
    }

    pub fn mute_chat(&self, chat_guid: &str, name: &str) -> AppResult<()> {
        let conn = self.conn.lock().expect("state lock poisoned");
        conn.execute(
            "INSERT INTO muted (chat_guid, name, muted_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(chat_guid) DO UPDATE SET name = excluded.name",
            params![chat_guid, name, now_unix()],
        )?;
        Ok(())
    }

    pub fn unmute_chat(&self, chat_guid: &str) -> AppResult<()> {
        let conn = self.conn.lock().expect("state lock poisoned");
        conn.execute("DELETE FROM muted WHERE chat_guid = ?1", params![chat_guid])?;
        Ok(())
    }

    pub fn muted_chats(&self) -> AppResult<Vec<MutedChat>> {
        let conn = self.conn.lock().expect("state lock poisoned");
        let mut stmt = conn.prepare("SELECT chat_guid, name FROM muted ORDER BY muted_at DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok(MutedChat {
                chat_guid: row.get(0)?,
                name: row.get(1)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
