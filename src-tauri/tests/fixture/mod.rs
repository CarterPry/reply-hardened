//! Synthetic `chat.db` fixture generator.
//!
//! Builds a real SQLite file with the subset of the Messages schema
//! the app queries (`message`, `chat`, `handle`, `chat_message_join`,
//! `chat_handle_join`) so the state machine can be exercised without
//! ever touching a user's actual chat.db. Every integration test
//! builds its own throwaway db via this module.
//!
//! Conventions mirrored from the real database:
//!   - `chat.style` 45 = 1:1, 43 = group
//!   - `message.date` is nanoseconds since 2001-01-01 UTC
//!   - tapbacks reference their target through
//!     `associated_message_guid` as `p:0/<guid>`
//!   - messages on macOS 13+ often have NULL `text` and carry the
//!     string inside the typedstream-encoded `attributedBody`

use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const APPLE_EPOCH_OFFSET: i64 = 978_307_200;

/// Classic tapback "add" types (love, like, dislike, laugh, emphasize,
/// question). Removals are the same value + 1000.
pub const TAPBACK_ADDS: [i64; 6] = [2000, 2001, 2002, 2003, 2004, 2005];
/// Custom emoji reaction (iOS 18+ style) and its removal.
pub const CUSTOM_EMOJI_ADD: i64 = 2006;
pub const CUSTOM_EMOJI_REMOVE: i64 = 3006;
/// Sticker placed directly on a message.
pub const STICKER: i64 = 1000;

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct ChatRef {
    pub rowid: i64,
}

pub struct Fixture {
    pub path: PathBuf,
    pub conn: Connection,
    next_id: u64,
}

impl Fixture {
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("repli-fixture-{}-{}.db", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).expect("create fixture db");
        conn.execute_batch(
            "CREATE TABLE handle (
                ROWID INTEGER PRIMARY KEY,
                id TEXT NOT NULL,
                service TEXT
            );
            CREATE TABLE chat (
                ROWID INTEGER PRIMARY KEY,
                guid TEXT NOT NULL,
                chat_identifier TEXT NOT NULL,
                display_name TEXT,
                style INTEGER NOT NULL,
                is_filtered INTEGER DEFAULT 0,
                is_blackholed INTEGER DEFAULT 0,
                is_archived INTEGER DEFAULT 0
            );
            CREATE TABLE message (
                ROWID INTEGER PRIMARY KEY,
                guid TEXT NOT NULL,
                text TEXT,
                attributedBody BLOB,
                date INTEGER NOT NULL,
                is_from_me INTEGER NOT NULL,
                handle_id INTEGER DEFAULT 0,
                item_type INTEGER DEFAULT 0,
                associated_message_type INTEGER DEFAULT 0,
                associated_message_guid TEXT
            );
            CREATE TABLE chat_message_join (
                chat_id INTEGER NOT NULL,
                message_id INTEGER NOT NULL
            );
            CREATE TABLE chat_handle_join (
                chat_id INTEGER NOT NULL,
                handle_id INTEGER NOT NULL
            );",
        )
        .expect("create fixture schema");
        Self {
            path,
            conn,
            next_id: 1,
        }
    }

    fn fresh_guid(&mut self, prefix: &str) -> String {
        let id = self.next_id;
        self.next_id += 1;
        format!("{prefix}-{id:04}")
    }

    /// Register a handle ("+14155550123", "22395", "a@b.com").
    pub fn handle(&mut self, id: &str) -> i64 {
        self.conn
            .execute(
                "INSERT INTO handle (id, service) VALUES (?1, 'iMessage')",
                params![id],
            )
            .expect("insert handle");
        self.conn.last_insert_rowid()
    }

    /// A 1:1 chat with the given handle (chat.style = 45).
    pub fn dm(&mut self, handle_rowid: i64) -> ChatRef {
        let identifier: String = self
            .conn
            .query_row(
                "SELECT id FROM handle WHERE ROWID = ?1",
                params![handle_rowid],
                |r| r.get(0),
            )
            .expect("handle exists");
        let guid = format!("iMessage;-;{identifier}");
        self.conn
            .execute(
                "INSERT INTO chat (guid, chat_identifier, display_name, style)
                 VALUES (?1, ?2, NULL, 45)",
                params![guid, identifier],
            )
            .expect("insert dm chat");
        let rowid = self.conn.last_insert_rowid();
        self.conn
            .execute(
                "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
                params![rowid, handle_rowid],
            )
            .expect("join handle");
        ChatRef { rowid }
    }

    /// A group chat (chat.style = 43). `display_name: None` models a
    /// group that was never named; `Some("")` models the empty-string
    /// variant that also occurs in the wild.
    pub fn group(&mut self, display_name: Option<&str>, handles: &[i64]) -> ChatRef {
        let guid = self.fresh_guid("iMessage;+;chat");
        let identifier = guid.clone();
        self.conn
            .execute(
                "INSERT INTO chat (guid, chat_identifier, display_name, style)
                 VALUES (?1, ?2, ?3, 43)",
                params![guid, identifier, display_name],
            )
            .expect("insert group chat");
        let rowid = self.conn.last_insert_rowid();
        for h in handles {
            self.conn
                .execute(
                    "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
                    params![rowid, h],
                )
                .expect("join handle");
        }
        ChatRef { rowid }
    }

    pub fn set_chat_flag(&mut self, chat: ChatRef, column: &str, value: i64) {
        assert!(
            ["is_filtered", "is_blackholed", "is_archived"].contains(&column),
            "unexpected chat flag {column}"
        );
        self.conn
            .execute(
                &format!("UPDATE chat SET {column} = ?1 WHERE ROWID = ?2"),
                params![value, chat.rowid],
            )
            .expect("set chat flag");
    }

    pub fn set_chat_identifier(&mut self, chat: ChatRef, identifier: &str) {
        self.conn
            .execute(
                "UPDATE chat SET chat_identifier = ?1 WHERE ROWID = ?2",
                params![identifier, chat.rowid],
            )
            .expect("set chat identifier");
    }

    /// A real text message. `sender` is the handle ROWID for inbound
    /// messages (ignored, forced to 0, for `from_me`). Returns the
    /// message guid, which reactions target.
    pub fn msg(
        &mut self,
        chat: ChatRef,
        from_me: bool,
        sender: Option<i64>,
        text: &str,
        date_ns: i64,
    ) -> String {
        self.insert_message(chat, from_me, sender, Some(text), None, 0, None, date_ns)
    }

    /// A message with NULL `text` whose string lives in a typedstream
    /// `attributedBody` blob — the macOS 13+ shape.
    pub fn msg_attributed(
        &mut self,
        chat: ChatRef,
        from_me: bool,
        sender: Option<i64>,
        text: &str,
        date_ns: i64,
    ) -> String {
        let blob = typedstream_blob(text);
        self.insert_message(chat, from_me, sender, None, Some(blob), 0, None, date_ns)
    }

    /// A reaction event (tapback add/remove, custom emoji, sticker)
    /// targeting `target_guid`. Stored with the `p:0/<guid>` prefix
    /// form the real db uses.
    pub fn reaction(
        &mut self,
        chat: ChatRef,
        from_me: bool,
        sender: Option<i64>,
        assoc_type: i64,
        target_guid: &str,
        date_ns: i64,
    ) -> String {
        let assoc_guid = format!("p:0/{target_guid}");
        self.insert_message(
            chat,
            from_me,
            sender,
            None,
            None,
            assoc_type,
            Some(assoc_guid),
            date_ns,
        )
    }

    /// A system event row (join/leave/rename) — `item_type != 0`.
    pub fn system_event(&mut self, chat: ChatRef, date_ns: i64) -> String {
        let guid = self.fresh_guid("msg");
        self.conn
            .execute(
                "INSERT INTO message (guid, text, date, is_from_me, handle_id, item_type)
                 VALUES (?1, NULL, ?2, 0, 0, 2)",
                params![guid, date_ns],
            )
            .expect("insert system event");
        let msg_rowid = self.conn.last_insert_rowid();
        self.join_msg(chat, msg_rowid);
        guid
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_message(
        &mut self,
        chat: ChatRef,
        from_me: bool,
        sender: Option<i64>,
        text: Option<&str>,
        blob: Option<Vec<u8>>,
        assoc_type: i64,
        assoc_guid: Option<String>,
        date_ns: i64,
    ) -> String {
        let guid = self.fresh_guid("msg");
        let handle_id = if from_me { 0 } else { sender.unwrap_or(0) };
        self.conn
            .execute(
                "INSERT INTO message
                   (guid, text, attributedBody, date, is_from_me, handle_id,
                    item_type, associated_message_type, associated_message_guid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8)",
                params![
                    guid,
                    text,
                    blob,
                    date_ns,
                    from_me as i64,
                    handle_id,
                    assoc_type,
                    assoc_guid
                ],
            )
            .expect("insert message");
        let msg_rowid = self.conn.last_insert_rowid();
        self.join_msg(chat, msg_rowid);
        guid
    }

    fn join_msg(&mut self, chat: ChatRef, msg_rowid: i64) {
        self.conn
            .execute(
                "INSERT INTO chat_message_join (chat_id, message_id) VALUES (?1, ?2)",
                params![chat.rowid, msg_rowid],
            )
            .expect("join message");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Apple-epoch nanoseconds for "this many days before now".
pub fn days_ago(days: f64) -> i64 {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs() as i64;
    let unix = now_unix - (days * 86_400.0) as i64;
    (unix - APPLE_EPOCH_OFFSET) * 1_000_000_000
}

/// Minimal typedstream blob wrapping `text`, matching the layout the
/// production parser understands (NSString marker, '+', length byte).
/// Only supports texts shorter than 128 bytes — plenty for fixtures.
pub fn typedstream_blob(text: &str) -> Vec<u8> {
    assert!(text.len() < 0x80, "fixture blob supports short texts only");
    let mut blob = Vec::new();
    blob.extend_from_slice(b"\x04\x0Bstreamtyped");
    blob.extend_from_slice(b"\x84\x84\x07NSString\x00\x84\x84\x08NSObject\x00\x85");
    blob.push(0x2B); // '+'
    blob.push(text.len() as u8);
    blob.extend_from_slice(text.as_bytes());
    blob
}
