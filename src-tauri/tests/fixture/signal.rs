//! Synthetic Signal Desktop database fixture generator.
//!
//! Signal's real database is SQLCipher-encrypted; the reader takes the
//! key as `Option<&str>`, so these fixtures are plaintext SQLite built
//! with the same table shapes and the key passed as `None`. The key
//! derivation itself is covered separately by the round-trip tests in
//! `safestorage`.
//!
//! Semantics mirrored from the real schema:
//!   - `messages.type` is 'incoming' / 'outgoing' for real messages;
//!     'call-history', 'keychange', 'group-v2-change' etc. are not
//!     messages and must never win "last message"
//!   - `sent_at` / `received_at` are milliseconds
//!   - group membership lives in `conversations.json.membersV2[].aci`
//!   - `reactions.fromId` is the reactor's conversation id

#![allow(dead_code)]

use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Our own account: the fixture writes `items['uuid_id']` as
/// "<aci>.<device>", exactly like Signal Desktop.
pub const ME_ACI: &str = "00000000-0000-4000-8000-00000000mine";
pub const ME_CONVERSATION: &str = "conv-me";

pub struct SignalFixture {
    pub path: PathBuf,
    pub conn: Connection,
    next_id: u64,
}

impl SignalFixture {
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "reply-signal-fixture-{}-{}.sqlite",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).expect("create signal fixture");
        conn.execute_batch(
            "CREATE TABLE conversations (
                id STRING PRIMARY KEY,
                json TEXT,
                active_at INTEGER,
                type STRING,
                members TEXT,
                name TEXT,
                profileName TEXT,
                profileFamilyName TEXT,
                profileFullName TEXT,
                e164 TEXT,
                serviceId TEXT,
                groupId TEXT
            );
            CREATE TABLE messages (
                rowid INTEGER PRIMARY KEY ASC,
                id STRING UNIQUE,
                json TEXT,
                conversationId STRING,
                sent_at INTEGER,
                received_at INTEGER,
                type STRING,
                body TEXT,
                isErased INTEGER DEFAULT 0,
                sourceServiceId TEXT
            );
            CREATE TABLE reactions (
                conversationId STRING,
                emoji STRING,
                fromId STRING,
                messageReceivedAt INTEGER,
                targetAuthorAci STRING,
                targetTimestamp INTEGER,
                unread INTEGER,
                messageId STRING,
                timestamp NUMBER
            );
            CREATE TABLE items (id STRING PRIMARY KEY, json TEXT);",
        )
        .expect("create signal fixture schema");

        let mut fx = Self {
            path,
            conn,
            next_id: 1,
        };
        // Our own account + Note to Self conversation.
        fx.conn
            .execute(
                "INSERT INTO items (id, json) VALUES ('uuid_id', ?1)",
                params![format!(r#"{{"id":"uuid_id","value":"{ME_ACI}.2"}}"#)],
            )
            .expect("insert uuid_id");
        fx.private_with_id(ME_CONVERSATION, ME_ACI, Some("+15550000000"), Some("Me"));
        fx
    }

    fn fresh(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// A 1:1 conversation. `profile_full_name` is what Signal shows
    /// when the number isn't in Contacts.
    pub fn private(
        &mut self,
        aci: &str,
        e164: Option<&str>,
        profile_full_name: Option<&str>,
    ) -> String {
        let id = format!("conv-{}", self.fresh());
        self.private_with_id(&id, aci, e164, profile_full_name);
        id
    }

    pub fn private_with_id(
        &mut self,
        id: &str,
        aci: &str,
        e164: Option<&str>,
        profile_full_name: Option<&str>,
    ) {
        self.conn
            .execute(
                "INSERT INTO conversations (id, json, type, serviceId, e164, profileFullName)
                 VALUES (?1, ?2, 'private', ?3, ?4, ?5)",
                params![id, "{}", aci, e164, profile_full_name],
            )
            .expect("insert private conversation");
    }

    /// A group whose membership includes us plus `member_acis`.
    pub fn group(&mut self, name: Option<&str>, member_acis: &[&str]) -> String {
        let id = format!("group-{}", self.fresh());
        let members: Vec<String> = std::iter::once(ME_ACI.to_string())
            .chain(member_acis.iter().map(|a| a.to_string()))
            .map(|aci| format!(r#"{{"aci":"{aci}","role":2}}"#))
            .collect();
        let json = format!(r#"{{"membersV2":[{}]}}"#, members.join(","));
        self.conn
            .execute(
                "INSERT INTO conversations (id, json, type, name, groupId)
                 VALUES (?1, ?2, 'group', ?3, ?1)",
                params![id, json, name],
            )
            .expect("insert group conversation");
        id
    }

    pub fn archive(&mut self, conversation_id: &str) {
        self.conn
            .execute(
                "UPDATE conversations
                 SET json = json_set(COALESCE(json,'{}'), '$.isArchived', json('true'))
                 WHERE id = ?1",
                params![conversation_id],
            )
            .expect("archive conversation");
    }

    /// A message `secs_ago` seconds in the past. Returns its id.
    pub fn message(
        &mut self,
        conversation_id: &str,
        incoming: bool,
        secs_ago: i64,
        body: Option<&str>,
    ) -> String {
        let kind = if incoming { "incoming" } else { "outgoing" };
        self.message_typed(conversation_id, kind, secs_ago, body, None)
    }

    /// A message of an arbitrary `type` - used to prove that
    /// non-message rows never become the "last real message".
    pub fn message_typed(
        &mut self,
        conversation_id: &str,
        kind: &str,
        secs_ago: i64,
        body: Option<&str>,
        source_aci: Option<&str>,
    ) -> String {
        let id = format!("msg-{}", self.fresh());
        let ms = (now_unix() - secs_ago) * 1000;
        self.conn
            .execute(
                "INSERT INTO messages
                    (id, json, conversationId, sent_at, received_at, type, body, sourceServiceId)
                 VALUES (?1, '{}', ?2, ?3, ?3, ?4, ?5, ?6)",
                params![id, conversation_id, ms, kind, body, source_aci],
            )
            .expect("insert signal message");
        id
    }

    /// A reaction on `message_id` by `from_conversation_id`.
    pub fn reaction(&mut self, conversation_id: &str, message_id: &str, from: &str) {
        self.conn
            .execute(
                "INSERT INTO reactions
                    (conversationId, emoji, fromId, messageId, timestamp)
                 VALUES (?1, '👍', ?2, ?3, ?4)",
                params![conversation_id, from, message_id, now_unix() * 1000],
            )
            .expect("insert reaction");
    }
}

impl Drop for SignalFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs() as i64
}
