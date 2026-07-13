use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config;
use crate::error::AppResult;

/// A chat the user has permanently hidden. The name is captured at
/// mute time so the unmute UI can render without touching chat.db.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutedChat {
    pub chat_guid: String,
    pub name: String,
}

/// One "mark done" target: the chat plus the unix-seconds date of its
/// most recent real message at the moment the user clicked done. The
/// thread stays hidden until a real message with a LATER date arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneItem {
    pub chat_guid: String,
    pub message_date: i64,
}

/// What `undo_last` reversed, so the UI can describe it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoOutcome {
    pub action: String,
    pub count: usize,
}

/// Undo payload rows, one per affected chat. `prev` restores the state
/// the chat had before the action (None = no prior row).
#[derive(Serialize, Deserialize)]
struct DoneUndo {
    chat_guid: String,
    prev: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct SnoozeUndo {
    chat_guid: String,
    prev: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct MuteUndo {
    chat_guid: String,
}

/// Keep this many undoable actions. Old entries beyond the window are
/// pruned on every write; ⌘Z walks the log newest-first.
const ACTION_LOG_CAP: i64 = 50;

/// Tracks per-user state: chats marked done, snoozed chats, muted
/// chats, and the undo log. Stored in a separate SQLite file (the
/// app's data dir) - `chat.db` itself is never written to.
///
/// Done-state is keyed by `chat.guid` plus the date of the last real
/// message, NOT by message guid: message-guid keying silently
/// invalidated every marker whenever an app update changed how the
/// "last message" is computed, resurfacing already-triaged threads.
pub struct StateStore {
    conn: Mutex<Connection>,
}

impl StateStore {
    pub fn open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS done (
                chat_guid TEXT PRIMARY KEY,
                message_date INTEGER NOT NULL,
                done_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS snoozed (
                chat_guid TEXT PRIMARY KEY,
                snoozed_until INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS muted (
                chat_guid TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                muted_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS action_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                action TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Mark chats done in one transaction and log ONE undo entry for
    /// the whole batch - a bulk action reverses with a single ⌘Z.
    pub fn mark_done(&self, items: &[DoneItem]) -> AppResult<()> {
        if items.is_empty() {
            return Ok(());
        }
        let now = now_unix();
        let mut conn = self.conn.lock().expect("state lock poisoned");
        let tx = conn.transaction()?;
        let mut undo = Vec::with_capacity(items.len());
        for item in items {
            let prev: Option<i64> = tx
                .query_row(
                    "SELECT message_date FROM done WHERE chat_guid = ?1",
                    params![item.chat_guid],
                    |row| row.get(0),
                )
                .optional()?;
            tx.execute(
                "INSERT INTO done (chat_guid, message_date, done_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_guid) DO UPDATE SET
                     message_date = excluded.message_date,
                     done_at = excluded.done_at",
                params![item.chat_guid, item.message_date, now],
            )?;
            undo.push(DoneUndo {
                chat_guid: item.chat_guid.clone(),
                prev,
            });
        }
        log_action(&tx, "done", &serde_json::to_string(&undo)?, now)?;
        tx.commit()?;
        Ok(())
    }

    /// chat guid → done message date (unix seconds). Rows older than
    /// the stale cutoff are pruned - those threads can't surface anyway.
    pub fn done_map(&self) -> AppResult<HashMap<String, i64>> {
        let cutoff = now_unix() - config::STALE_AFTER_SECS;
        let conn = self.conn.lock().expect("state lock poisoned");
        conn.execute("DELETE FROM done WHERE message_date < ?1", params![cutoff])?;
        let mut stmt = conn.prepare("SELECT chat_guid, message_date FROM done")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let (guid, date) = r?;
            out.insert(guid, date);
        }
        Ok(out)
    }

    /// Snooze chats until `until` (unix seconds); one undo entry.
    pub fn snooze_chats(&self, chat_guids: &[String], until: i64) -> AppResult<()> {
        if chat_guids.is_empty() {
            return Ok(());
        }
        let now = now_unix();
        let mut conn = self.conn.lock().expect("state lock poisoned");
        let tx = conn.transaction()?;
        let mut undo = Vec::with_capacity(chat_guids.len());
        for guid in chat_guids {
            let prev: Option<i64> = tx
                .query_row(
                    "SELECT snoozed_until FROM snoozed WHERE chat_guid = ?1",
                    params![guid],
                    |row| row.get(0),
                )
                .optional()?;
            tx.execute(
                "INSERT INTO snoozed (chat_guid, snoozed_until)
                 VALUES (?1, ?2)
                 ON CONFLICT(chat_guid) DO UPDATE SET snoozed_until = excluded.snoozed_until",
                params![guid, until],
            )?;
            undo.push(SnoozeUndo {
                chat_guid: guid.clone(),
                prev,
            });
        }
        log_action(&tx, "snooze", &serde_json::to_string(&undo)?, now)?;
        tx.commit()?;
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

    /// Mute chats permanently; one undo entry for the batch.
    pub fn mute_chats(&self, chats: &[MutedChat]) -> AppResult<()> {
        if chats.is_empty() {
            return Ok(());
        }
        let now = now_unix();
        let mut conn = self.conn.lock().expect("state lock poisoned");
        let tx = conn.transaction()?;
        let mut undo = Vec::with_capacity(chats.len());
        for chat in chats {
            tx.execute(
                "INSERT INTO muted (chat_guid, name, muted_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_guid) DO UPDATE SET name = excluded.name",
                params![chat.chat_guid, chat.name, now],
            )?;
            undo.push(MuteUndo {
                chat_guid: chat.chat_guid.clone(),
            });
        }
        log_action(&tx, "mute", &serde_json::to_string(&undo)?, now)?;
        tx.commit()?;
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

    /// Reverse the most recent logged action (done / snooze / mute) and
    /// pop it from the log. Returns None when the log is empty. The log
    /// is persisted, so undo reaches actions from previous sessions.
    pub fn undo_last(&self) -> AppResult<Option<UndoOutcome>> {
        let mut conn = self.conn.lock().expect("state lock poisoned");
        let tx = conn.transaction()?;
        let last: Option<(i64, String, String)> = tx
            .query_row(
                "SELECT id, action, payload FROM action_log ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((id, action, payload)) = last else {
            return Ok(None);
        };
        let count = match action.as_str() {
            "done" => {
                let entries: Vec<DoneUndo> = serde_json::from_str(&payload)?;
                for e in &entries {
                    match e.prev {
                        Some(prev) => tx.execute(
                            "UPDATE done SET message_date = ?1 WHERE chat_guid = ?2",
                            params![prev, e.chat_guid],
                        )?,
                        None => tx.execute(
                            "DELETE FROM done WHERE chat_guid = ?1",
                            params![e.chat_guid],
                        )?,
                    };
                }
                entries.len()
            }
            "snooze" => {
                let entries: Vec<SnoozeUndo> = serde_json::from_str(&payload)?;
                for e in &entries {
                    match e.prev {
                        Some(prev) => tx.execute(
                            "UPDATE snoozed SET snoozed_until = ?1 WHERE chat_guid = ?2",
                            params![prev, e.chat_guid],
                        )?,
                        None => tx.execute(
                            "DELETE FROM snoozed WHERE chat_guid = ?1",
                            params![e.chat_guid],
                        )?,
                    };
                }
                entries.len()
            }
            "mute" => {
                let entries: Vec<MuteUndo> = serde_json::from_str(&payload)?;
                for e in &entries {
                    tx.execute(
                        "DELETE FROM muted WHERE chat_guid = ?1",
                        params![e.chat_guid],
                    )?;
                }
                entries.len()
            }
            other => {
                // Unknown action from a future version: drop the entry
                // rather than wedging undo forever.
                log::warn!("undo: unknown action {other:?}, discarding");
                0
            }
        };
        tx.execute("DELETE FROM action_log WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(Some(UndoOutcome { action, count }))
    }

    /// One-time migration of the legacy guid-keyed `completed` table
    /// into the chat-keyed `done` table. Needs chat.db (read-only) to
    /// map each message guid back to its chat and date, so it can't run
    /// before Full Disk Access is granted - callers invoke it after a
    /// successful chat.db read and it no-ops once the legacy table is
    /// gone. Guids whose message no longer exists in chat.db are
    /// dropped: their thread either vanished or will re-triage once.
    pub fn migrate_legacy_completed(&self, chat_db: &Path) -> AppResult<usize> {
        let mut conn = self.conn.lock().expect("state lock poisoned");
        let has_legacy: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='completed')",
            [],
            |row| row.get(0),
        )?;
        if !has_legacy {
            return Ok(0);
        }
        let guids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT last_message_guid FROM completed")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };

        // chat guid → latest done message date seen across the guids.
        let mut dates: HashMap<String, i64> = HashMap::new();
        if !guids.is_empty() {
            let chat_conn = Connection::open_with_flags(
                chat_db,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let mut stmt = chat_conn.prepare(
                "SELECT c.guid, m.date
                 FROM message m
                 JOIN chat_message_join cmj ON cmj.message_id = m.ROWID
                 JOIN chat c ON c.ROWID = cmj.chat_id
                 WHERE m.guid = ?1",
            )?;
            for guid in &guids {
                let row: Option<(String, i64)> = stmt
                    .query_row(params![guid], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()?;
                if let Some((chat_guid, raw_date)) = row {
                    let date = crate::pending::apple_to_unix(raw_date);
                    let entry = dates.entry(chat_guid).or_insert(date);
                    *entry = (*entry).max(date);
                }
            }
        }

        let now = now_unix();
        let migrated = dates.len();
        let tx = conn.transaction()?;
        for (chat_guid, date) in dates {
            tx.execute(
                "INSERT INTO done (chat_guid, message_date, done_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_guid) DO UPDATE SET
                     message_date = MAX(done.message_date, excluded.message_date)",
                params![chat_guid, date, now],
            )?;
        }
        tx.execute("DROP TABLE completed", [])?;
        tx.commit()?;
        Ok(migrated)
    }
}

fn log_action(
    tx: &rusqlite::Transaction<'_>,
    action: &str,
    payload: &str,
    now: i64,
) -> AppResult<()> {
    tx.execute(
        "INSERT INTO action_log (action, payload, created_at) VALUES (?1, ?2, ?3)",
        params![action, payload, now],
    )?;
    tx.execute(
        "DELETE FROM action_log WHERE id NOT IN
             (SELECT id FROM action_log ORDER BY id DESC LIMIT ?1)",
        params![ACTION_LOG_CAP],
    )?;
    Ok(())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
