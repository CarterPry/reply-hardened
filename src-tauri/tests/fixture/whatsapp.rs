//! Synthetic WhatsApp `ChatStorage.sqlite` fixture generator.
//!
//! Builds a real SQLite file with the subset of WhatsApp's Core Data
//! schema the app queries, so the WhatsApp reader can be exercised
//! without ever touching the user's actual store. Column names and
//! semantics mirror the real database:
//!   - `ZMESSAGEDATE` is seconds since 2001-01-01 (a REAL column)
//!   - `ZISFROMME` mirrors iMessage's `is_from_me`
//!   - `ZMESSAGETYPE` 6 is a system / group-event row
//!   - 1:1 jids end in `@s.whatsapp.net` (or `@lid` with no number),
//!     groups in `@g.us`

#![allow(dead_code)]

use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const CORE_DATA_EPOCH_OFFSET: i64 = 978_307_200;
pub const MESSAGE_TYPE_TEXT: i64 = 0;
pub const MESSAGE_TYPE_SYSTEM: i64 = 6;

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct WaFixture {
    pub path: PathBuf,
    pub conn: Connection,
    next_id: u64,
}

impl WaFixture {
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "reply-wa-fixture-{}-{}.sqlite",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).expect("create whatsapp fixture");
        conn.execute_batch(
            "CREATE TABLE ZWACHATSESSION (
                Z_PK INTEGER PRIMARY KEY,
                ZCONTACTJID VARCHAR,
                ZPARTNERNAME VARCHAR,
                ZSESSIONTYPE INTEGER DEFAULT 0,
                ZARCHIVED INTEGER DEFAULT 0,
                ZHIDDEN INTEGER DEFAULT 0,
                ZREMOVED INTEGER DEFAULT 0,
                ZLASTMESSAGEDATE TIMESTAMP
            );
            CREATE TABLE ZWAMESSAGE (
                Z_PK INTEGER PRIMARY KEY,
                ZCHATSESSION INTEGER,
                ZISFROMME INTEGER NOT NULL,
                ZMESSAGEDATE TIMESTAMP,
                ZTEXT VARCHAR,
                ZMESSAGETYPE INTEGER DEFAULT 0,
                ZSTANZAID VARCHAR,
                ZGROUPMEMBER INTEGER,
                ZPUSHNAME VARCHAR
            );
            CREATE TABLE ZWAGROUPMEMBER (
                Z_PK INTEGER PRIMARY KEY,
                ZCHATSESSION INTEGER,
                ZMEMBERJID VARCHAR,
                ZCONTACTNAME VARCHAR,
                ZFIRSTNAME VARCHAR,
                ZISACTIVE INTEGER DEFAULT 1
            );
            CREATE TABLE ZWAPROFILEPUSHNAME (
                Z_PK INTEGER PRIMARY KEY,
                ZJID VARCHAR,
                ZPUSHNAME VARCHAR
            );",
        )
        .expect("create whatsapp fixture schema");
        Self {
            path,
            conn,
            next_id: 1,
        }
    }

    fn fresh(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// A conversation. `jid` decides 1:1 vs group vs feed.
    pub fn session(&mut self, jid: &str, partner_name: Option<&str>) -> i64 {
        self.conn
            .execute(
                "INSERT INTO ZWACHATSESSION (ZCONTACTJID, ZPARTNERNAME, ZSESSIONTYPE)
                 VALUES (?1, ?2, ?3)",
                params![
                    jid,
                    partner_name,
                    if jid.ends_with("@g.us") { 1 } else { 0 }
                ],
            )
            .expect("insert session");
        self.conn.last_insert_rowid()
    }

    pub fn hide_session(&mut self, session_pk: i64) {
        self.conn
            .execute(
                "UPDATE ZWACHATSESSION SET ZHIDDEN = 1 WHERE Z_PK = ?1",
                params![session_pk],
            )
            .expect("hide session");
    }

    pub fn remove_session(&mut self, session_pk: i64) {
        self.conn
            .execute(
                "UPDATE ZWACHATSESSION SET ZREMOVED = 1 WHERE Z_PK = ?1",
                params![session_pk],
            )
            .expect("remove session");
    }

    /// A group participant, already name-resolved the way WhatsApp
    /// stores them. Returns the member row id for use as a sender.
    pub fn member(&mut self, session_pk: i64, jid: &str, name: Option<&str>) -> i64 {
        self.member_raw(session_pk, jid, name, name)
    }

    /// A participant whose `ZCONTACTNAME` and `ZFIRSTNAME` are set
    /// independently - how `@lid` contacts really look, where the name
    /// columns can hold base64 protobuf instead of a name.
    pub fn member_raw(
        &mut self,
        session_pk: i64,
        jid: &str,
        contact_name: Option<&str>,
        first_name: Option<&str>,
    ) -> i64 {
        self.conn
            .execute(
                "INSERT INTO ZWAGROUPMEMBER (ZCHATSESSION, ZMEMBERJID, ZCONTACTNAME, ZFIRSTNAME)
                 VALUES (?1, ?2, ?3, ?4)",
                params![session_pk, jid, contact_name, first_name],
            )
            .expect("insert member");
        self.conn.last_insert_rowid()
    }

    /// A row in WhatsApp's profile push-name table - the one name
    /// source in this store that is actually trustworthy.
    pub fn push_name(&mut self, jid: &str, name: &str) {
        self.conn
            .execute(
                "INSERT INTO ZWAPROFILEPUSHNAME (ZJID, ZPUSHNAME) VALUES (?1, ?2)",
                params![jid, name],
            )
            .expect("insert push name");
    }

    /// A message `secs_ago` seconds in the past.
    pub fn message(
        &mut self,
        session_pk: i64,
        from_me: bool,
        secs_ago: i64,
        text: Option<&str>,
    ) -> i64 {
        self.message_full(
            session_pk,
            from_me,
            secs_ago,
            text,
            MESSAGE_TYPE_TEXT,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn message_full(
        &mut self,
        session_pk: i64,
        from_me: bool,
        secs_ago: i64,
        text: Option<&str>,
        message_type: i64,
        group_member_pk: Option<i64>,
        push_name: Option<&str>,
    ) -> i64 {
        let id = self.fresh();
        let core_data_date = (now_unix() - secs_ago - CORE_DATA_EPOCH_OFFSET) as f64;
        self.conn
            .execute(
                "INSERT INTO ZWAMESSAGE
                    (ZCHATSESSION, ZISFROMME, ZMESSAGEDATE, ZTEXT, ZMESSAGETYPE,
                     ZSTANZAID, ZGROUPMEMBER, ZPUSHNAME)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_pk,
                    if from_me { 1 } else { 0 },
                    core_data_date,
                    text,
                    message_type,
                    format!("wa-stanza-{id:04}"),
                    group_member_pk,
                    push_name
                ],
            )
            .expect("insert whatsapp message");
        self.conn.last_insert_rowid()
    }
}

impl Drop for WaFixture {
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
