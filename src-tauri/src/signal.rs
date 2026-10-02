//! Signal source: reads Signal Desktop's local message database.
//!
//! Unlike `chat.db` and WhatsApp's store, this one is encrypted -
//! SQLCipher 4, keyed by a value Signal keeps in `config.json` wrapped
//! under a login-keychain password (see `safestorage`). The read itself
//! is still read-only, in place, and local: we hand SQLCipher the key,
//! run two SELECTs, and close.
//!
//! Schema subset used:
//!   conversations - `type` is 'private' or 'group'; the display name
//!                   lives in `name` (groups) or
//!                   `profileFullName`/`profileName` (people); `e164`
//!                   is the phone number when known; group membership
//!                   is `json.membersV2[].aci`.
//!   messages      - `type` 'incoming'/'outgoing' is the direction
//!                   (the analogue of `is_from_me`); `sent_at` and
//!                   `received_at` are milliseconds; `body` is NULL for
//!                   attachment-only messages.
//!   reactions     - one row per reaction, `fromId` being the reactor's
//!                   conversation id. This is what makes the
//!                   `acknowledged` state work on Signal the same way
//!                   tapbacks do on iMessage.
//!
//! Rows Signal keeps in `messages` that are not messages - call
//! history, key changes, profile changes, group-v2 changes, timer
//! notifications, stories - are excluded from the "last real message"
//! ranking, mirroring the `item_type = 0` rule on iMessage.

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::contacts::ContactDirectory;
use crate::error::{AppError, AppResult};
use crate::pending::Direction;
use crate::replies::{clean_preview, first_name, format_group_name, Filters, PendingReply};
use crate::safestorage;
use crate::sources::Platform;
use crate::unknown::UnknownSender;

pub fn db_path() -> PathBuf {
    safestorage::signal_support_dir().join("sql/db.sqlite")
}

fn ms_to_unix(ms: i64) -> i64 {
    ms / 1000
}

/// A temp directory that deletes itself. Used only for the
/// WAL-fallback copy below.
struct ScratchDir(PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An open, decrypted handle on the Signal database. Holds the scratch
/// copy alive (and deletes it) when the WAL fallback was needed.
struct SignalDb {
    conn: Connection,
    _scratch: Option<ScratchDir>,
}

/// Open read-only and unlock with `key`.
///
/// Signal Desktop normally has the database open in WAL mode. A
/// read-only connection can share that, but only while Signal is
/// running: with the app closed, SQLite cannot build the `-shm` index
/// it needs and refuses to open the file. In that case we fall back to
/// a throwaway copy of the (still encrypted) db/wal/shm triple, read
/// that, and delete it on drop.
fn open_keyed(path: &Path, key: Option<&str>) -> AppResult<SignalDb> {
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::NotFound {
                path: path.display().to_string(),
            });
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(1) {
                return Err(AppError::PermissionDenied {
                    path: path.display().to_string(),
                });
            }
            return Err(AppError::Io {
                message: e.to_string(),
            });
        }
    }

    match try_open(path, key) {
        Ok(conn) => Ok(SignalDb {
            conn,
            _scratch: None,
        }),
        Err(first) => {
            // Only the "can't attach to the WAL" family of failures is
            // worth a copy; a wrong key or a blocked read is terminal.
            if !is_wal_attach_failure(&first) {
                return Err(first);
            }
            let scratch = copy_to_scratch(path)?;
            let copied = scratch.0.join("db.sqlite");
            let conn = try_open(&copied, key).map_err(|_| first)?;
            Ok(SignalDb {
                conn,
                _scratch: Some(scratch),
            })
        }
    }
}

fn try_open(path: &Path, key: Option<&str>) -> AppResult<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(map_open_error(path))?;
    if let Some(k) = key {
        // The key is validated as 64 hex characters before it gets
        // here (`Secret::as_hex_str`), so this interpolation cannot
        // carry anything but hex digits.
        debug_assert!(k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()));
        conn.execute_batch(&format!(
            "PRAGMA key = \"x'{k}'\";
             PRAGMA cipher_compatibility = 4;"
        ))
        .map_err(|e| AppError::Database {
            message: format!("unlocking Signal database: {e}"),
        })?;
    }
    // Force a real page read: a wrong key only fails here, not on the
    // PRAGMA.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })
    .map_err(map_open_error(path))?;
    Ok(conn)
}

fn map_open_error(path: &Path) -> impl Fn(rusqlite::Error) -> AppError + '_ {
    move |e: rusqlite::Error| {
        let s = e.to_string();
        if s.contains("authorization denied") || s.contains("not authorized") {
            AppError::PermissionDenied {
                path: path.display().to_string(),
            }
        } else {
            AppError::Database { message: s }
        }
    }
}

fn is_wal_attach_failure(e: &AppError) -> bool {
    match e {
        AppError::Database { message } => {
            let m = message.to_lowercase();
            m.contains("unable to open database file")
                || m.contains("database is locked")
                || m.contains("disk i/o error")
        }
        _ => false,
    }
}

fn copy_to_scratch(path: &Path) -> AppResult<ScratchDir> {
    let dir = std::env::temp_dir().join(format!(
        "repli-signal-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir)?;
    let scratch = ScratchDir(dir);
    // The copies stay SQLCipher-encrypted; nothing is decrypted to disk.
    std::fs::copy(path, scratch.0.join("db.sqlite"))?;
    for suffix in ["-wal", "-shm"] {
        let mut from = path.as_os_str().to_owned();
        from.push(suffix);
        let from = PathBuf::from(from);
        if from.exists() {
            let _ = std::fs::copy(&from, scratch.0.join(format!("db.sqlite{suffix}")));
        }
    }
    Ok(scratch)
}

/// Resolve the key, then read. Key failures are returned as structured
/// errors so the caller can tell "Signal isn't installed" from "the
/// keychain prompt was denied".
pub fn read_pending(
    direction: Direction,
    dir: &ContactDirectory,
    filters: &Filters,
) -> Result<Vec<PendingReply>, SignalError> {
    let key = safestorage::signal_db_key().map_err(SignalError::Key)?;
    let hex = key
        .as_hex_str()
        .ok_or_else(|| SignalError::Key(safestorage::KeyError::Undecryptable("bad key".into())))?;
    read_pending_at(&db_path(), Some(hex), direction, dir, filters).map_err(SignalError::Read)
}

pub fn read_unknown(
    dir: &ContactDirectory,
    filters: &Filters,
) -> Result<Vec<UnknownSender>, SignalError> {
    let key = safestorage::signal_db_key().map_err(SignalError::Key)?;
    let hex = key
        .as_hex_str()
        .ok_or_else(|| SignalError::Key(safestorage::KeyError::Undecryptable("bad key".into())))?;
    read_unknown_at(&db_path(), Some(hex), dir, filters).map_err(SignalError::Read)
}

/// Either the key couldn't be recovered, or the database couldn't be
/// read once it was.
#[derive(Debug)]
pub enum SignalError {
    Key(safestorage::KeyError),
    Read(AppError),
}

impl SignalError {
    pub fn detail(&self) -> String {
        match self {
            SignalError::Key(e) => e.detail(),
            SignalError::Read(AppError::NotFound { .. }) => {
                "No Signal message database on this Mac".to_string()
            }
            SignalError::Read(AppError::PermissionDenied { .. }) => {
                "Can't read Signal's database - grant Full Disk Access".to_string()
            }
            SignalError::Read(e) => e.to_string(),
        }
    }

    pub fn is_absent(&self) -> bool {
        match self {
            SignalError::Key(e) => e.is_absent(),
            SignalError::Read(AppError::NotFound { .. }) => true,
            _ => false,
        }
    }

    pub fn is_blocked(&self) -> bool {
        match self {
            SignalError::Key(e) => e.is_blocked(),
            SignalError::Read(AppError::PermissionDenied { .. }) => true,
            _ => false,
        }
    }
}

/// Everything needed to turn member service-ids into display names.
struct Directory {
    /// aci → (display name, e164 if known)
    by_aci: HashMap<String, (String, Option<String>)>,
    /// Our own conversation id, used to tell our reactions from theirs
    /// and to skip Note to Self.
    me: Option<String>,
}

fn load_directory(conn: &Connection, dir: &ContactDirectory) -> AppResult<Directory> {
    let mut by_aci = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT serviceId, profileFullName, name, profileName, e164
         FROM conversations
         WHERE type = 'private' AND serviceId IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        let aci: String = row.get(0)?;
        let full: Option<String> = row.get(1)?;
        let name: Option<String> = row.get(2)?;
        let profile: Option<String> = row.get(3)?;
        let e164: Option<String> = row.get(4)?;
        Ok((aci, full, name, profile, e164))
    })?;
    for r in rows {
        let (aci, full, name, profile, e164) = r?;
        // Address book first so a person reads the same on every
        // platform, then whatever Signal knows.
        let label = e164
            .as_deref()
            .and_then(|p| dir.lookup(p).map(str::to_string))
            .or_else(|| non_empty(full.as_deref()).map(str::to_string))
            .or_else(|| non_empty(name.as_deref()).map(str::to_string))
            .or_else(|| non_empty(profile.as_deref()).map(str::to_string))
            .or_else(|| e164.clone())
            .unwrap_or_else(|| "Unknown".to_string());
        by_aci.insert(aci, (label, e164));
    }

    // items['uuid_id'] is "<our aci>.<device id>".
    let me: Option<String> = conn
        .query_row(
            "SELECT c.id FROM conversations c
             WHERE c.serviceId = (
                 SELECT CASE
                     WHEN instr(json_extract(i.json, '$.value'), '.') > 0
                     THEN substr(json_extract(i.json, '$.value'), 1,
                                 instr(json_extract(i.json, '$.value'), '.') - 1)
                     ELSE json_extract(i.json, '$.value')
                 END
                 FROM items i WHERE i.id = 'uuid_id')",
            [],
            |row| row.get(0),
        )
        .optional()?;

    Ok(Directory { by_aci, me })
}

/// group conversation id → member acis (ours removed).
fn load_group_members(
    conn: &Connection,
    me_aci: Option<&str>,
) -> AppResult<HashMap<String, Vec<String>>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT c.id, json_extract(m.value, '$.aci')
         FROM conversations c, json_each(json_extract(c.json, '$.membersV2')) m
         WHERE c.type = 'group'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for r in rows {
        let (conv, aci) = r?;
        let Some(aci) = aci else { continue };
        if Some(aci.as_str()) == me_aci {
            continue;
        }
        out.entry(conv).or_default().push(aci);
    }
    Ok(out)
}

pub fn read_pending_at(
    path: &Path,
    key: Option<&str>,
    direction: Direction,
    dir: &ContactDirectory,
    filters: &Filters,
) -> AppResult<Vec<PendingReply>> {
    let db = open_keyed(path, key)?;
    let conn = &db.conn;
    let directory = load_directory(conn, dir)?;
    let me_aci = directory.me.as_deref().and_then(|me| {
        conn.query_row(
            "SELECT serviceId FROM conversations WHERE id = ?1",
            params![me],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    });
    let group_members = load_group_members(conn, me_aci.as_deref())?;

    let (msg_type, ack_clause) = match direction {
        // You owe them: their message is last, and YOUR reaction on it
        // is the acknowledgement.
        Direction::Owed => ("incoming", "rx.fromId IS ?2"),
        // They owe you: your message is last, and ANY reaction that
        // isn't yours is theirs.
        Direction::Waiting => ("outgoing", "rx.fromId IS NOT ?2"),
    };
    let sql = format!(
        r#"
        WITH ranked AS (
            SELECT
                m.conversationId  AS conversation_id,
                m.id              AS msg_id,
                m.sent_at         AS sent_at,
                m.received_at     AS received_at,
                m.body            AS body,
                m.sourceServiceId AS source_service_id,
                m.type            AS msg_type,
                ROW_NUMBER() OVER (
                    PARTITION BY m.conversationId
                    ORDER BY COALESCE(m.received_at, m.sent_at) DESC, m.rowid DESC
                ) AS rn
            FROM messages m
            WHERE m.type IN ('incoming', 'outgoing')
              AND COALESCE(m.isErased, 0) = 0
        )
        SELECT
            c.id              AS conversation_id,
            c.type            AS conversation_type,
            c.name            AS group_name,
            c.profileFullName AS profile_full_name,
            c.profileName     AS profile_name,
            c.e164            AS e164,
            r.msg_id          AS msg_id,
            r.sent_at         AS sent_at,
            r.received_at     AS received_at,
            r.body            AS body,
            r.source_service_id AS source_service_id,
            CASE WHEN EXISTS (
                SELECT 1 FROM reactions rx
                WHERE rx.messageId = r.msg_id AND {ack_clause}
            ) THEN 1 ELSE 0 END AS acknowledged
        FROM ranked r
        JOIN conversations c ON c.id = r.conversation_id
        WHERE r.rn = 1
          AND r.msg_type = ?1
          AND COALESCE(json_extract(c.json, '$.isArchived'), 0) = 0
        ORDER BY COALESCE(r.received_at, r.sent_at) DESC
    "#
    );

    let mut stmt = conn.prepare(&sql)?;
    let me_key = directory.me.clone().unwrap_or_default();
    let rows = stmt.query_map(params![msg_type, me_key], |row| {
        let conv_id: String = row.get("conversation_id")?;
        let conv_type: String = row.get("conversation_type")?;
        let sent_at: Option<i64> = row.get("sent_at")?;
        let received_at: Option<i64> = row.get("received_at")?;
        Ok(RawSignalRow {
            conversation_id: conv_id,
            is_group: conv_type == "group",
            group_name: row.get("group_name")?,
            profile_full_name: row.get("profile_full_name")?,
            profile_name: row.get("profile_name")?,
            e164: row.get("e164")?,
            msg_id: row.get("msg_id")?,
            timestamp: ms_to_unix(sent_at.or(received_at).unwrap_or(0)),
            body: row.get("body")?,
            source_service_id: row.get("source_service_id")?,
            acknowledged: row.get::<_, i64>("acknowledged")? != 0,
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        let r = r?;
        // Note to Self is a conversation, but never one you owe.
        if directory.me.as_deref() == Some(r.conversation_id.as_str()) {
            continue;
        }
        let chat_key = Platform::Signal.chat_key(&r.conversation_id);
        let mut acknowledged = r.acknowledged;
        let preview = r.body.as_deref().and_then(clean_preview);
        // Same exception as iMessage: a reaction on a question is not
        // an answer, so the row stays fully open.
        if crate::config::QUESTION_MARK_KEEPS_OPEN
            && acknowledged
            && preview.as_deref().is_some_and(|p| p.contains('?'))
        {
            acknowledged = false;
        }
        if !filters.keep(&chat_key, r.timestamp, acknowledged) {
            continue;
        }

        let participants: Vec<String> = if r.is_group {
            group_members
                .get(&r.conversation_id)
                .map(|acis| {
                    acis.iter()
                        .map(|aci| {
                            directory
                                .by_aci
                                .get(aci)
                                .map(|(label, _)| label.clone())
                                .unwrap_or_else(|| "Unknown".to_string())
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let name = if r.is_group {
            non_empty(r.group_name.as_deref())
                .map(str::to_string)
                .unwrap_or_else(|| format_group_name(&participants))
        } else {
            r.e164
                .as_deref()
                .and_then(|p| dir.lookup(p).map(str::to_string))
                .or_else(|| non_empty(r.profile_full_name.as_deref()).map(str::to_string))
                .or_else(|| non_empty(r.profile_name.as_deref()).map(str::to_string))
                .or_else(|| r.e164.clone())
                .unwrap_or_else(|| "Unknown".to_string())
        };

        out.push(PendingReply {
            platform: Platform::Signal,
            chat_guid: chat_key,
            name: name.clone(),
            // Signal Desktop exposes no URL scheme for an existing
            // conversation, so the handle is informational and
            // `open_chat` always goes through the app's search.
            handle: r.e164.clone().unwrap_or_default(),
            last_message_guid: r.msg_id,
            timestamp: r.timestamp,
            is_group: r.is_group,
            member_count: if r.is_group {
                (participants.len().max(1)) as u32
            } else {
                1
            },
            service: Some("Signal".to_string()),
            participants,
            last_sender: if r.is_group {
                r.source_service_id
                    .as_deref()
                    .and_then(|aci| directory.by_aci.get(aci))
                    .map(|(label, _)| first_name(label).to_string())
            } else {
                None
            },
            preview,
            // Signal's search matches the conversation title, which is
            // exactly `name`.
            search_hint: name,
            acknowledged,
        });
    }
    Ok(out)
}

struct RawSignalRow {
    conversation_id: String,
    is_group: bool,
    group_name: Option<String>,
    profile_full_name: Option<String>,
    profile_name: Option<String>,
    e164: Option<String>,
    msg_id: String,
    timestamp: i64,
    body: Option<String>,
    source_service_id: Option<String>,
    acknowledged: bool,
}

/// Unknown senders, Signal edition: 1:1 conversations with a phone
/// number that matches no contact. Conversations with no number (ACI
/// only) are skipped - there is nothing to look up or add.
pub fn read_unknown_at(
    path: &Path,
    key: Option<&str>,
    dir: &ContactDirectory,
    filters: &Filters,
) -> AppResult<Vec<UnknownSender>> {
    let db = open_keyed(path, key)?;
    let conn = &db.conn;
    let me: Option<String> = conn
        .query_row(
            "SELECT c.id FROM conversations c
             WHERE c.serviceId = (
                 SELECT CASE
                     WHEN instr(json_extract(i.json, '$.value'), '.') > 0
                     THEN substr(json_extract(i.json, '$.value'), 1,
                                 instr(json_extract(i.json, '$.value'), '.') - 1)
                     ELSE json_extract(i.json, '$.value')
                 END
                 FROM items i WHERE i.id = 'uuid_id')",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let sql = r#"
        WITH real AS (
            SELECT
                m.conversationId AS conversation_id,
                m.body           AS body,
                ROW_NUMBER() OVER (
                    PARTITION BY m.conversationId
                    ORDER BY COALESCE(m.received_at, m.sent_at) DESC, m.rowid DESC
                ) AS rn,
                COUNT(*) OVER (PARTITION BY m.conversationId) AS msg_count,
                SUM(CASE WHEN m.type = 'incoming' THEN 1 ELSE 0 END)
                    OVER (PARTITION BY m.conversationId) AS inbound_count,
                MIN(COALESCE(m.sent_at, m.received_at))
                    OVER (PARTITION BY m.conversationId) AS first_at,
                MAX(COALESCE(m.sent_at, m.received_at))
                    OVER (PARTITION BY m.conversationId) AS last_at
            FROM messages m
            WHERE m.type IN ('incoming', 'outgoing')
              AND COALESCE(m.isErased, 0) = 0
        )
        SELECT
            c.id            AS conversation_id,
            c.e164          AS e164,
            r.body          AS body,
            r.msg_count     AS msg_count,
            r.inbound_count AS inbound_count,
            r.first_at      AS first_at,
            r.last_at       AS last_at
        FROM real r
        JOIN conversations c ON c.id = r.conversation_id
        WHERE r.rn = 1 AND c.type = 'private'
    "#;
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>("conversation_id")?,
            row.get::<_, Option<String>>("e164")?,
            row.get::<_, Option<String>>("body")?,
            row.get::<_, i64>("msg_count")?,
            row.get::<_, i64>("inbound_count")?,
            row.get::<_, Option<i64>>("first_at")?,
            row.get::<_, Option<i64>>("last_at")?,
        ))
    })?;

    let mut out = Vec::new();
    for r in rows {
        let (conv_id, e164, body, count, inbound, first_at, last_at) = r?;
        if me.as_deref() == Some(conv_id.as_str()) {
            continue;
        }
        let Some(phone) = e164.filter(|p| !p.trim().is_empty()) else {
            continue;
        };
        let chat_key = Platform::Signal.chat_key(&conv_id);
        let first_date = ms_to_unix(first_at.unwrap_or(0));
        let last_date = ms_to_unix(last_at.unwrap_or(0));
        if inbound == 0
            || dir.lookup(&phone).is_some()
            || last_date < filters.cutoff()
            || filters.muted.contains(&chat_key)
            || filters.snoozed.contains_key(&chat_key)
            || filters.done.get(&chat_key).is_some_and(|&d| last_date <= d)
        {
            continue;
        }
        out.push(UnknownSender {
            platform: Platform::Signal,
            chat_guid: chat_key,
            handle: phone,
            service: Some("Signal".to_string()),
            first_date,
            last_date,
            message_count: count,
            preview: body.as_deref().and_then(clean_preview),
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.last_date));
    Ok(out)
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milliseconds_become_seconds() {
        assert_eq!(ms_to_unix(1_704_067_200_000), 1_704_067_200);
        assert_eq!(ms_to_unix(1_704_067_200_999), 1_704_067_200);
    }

    #[test]
    fn wal_failures_are_retryable_but_key_failures_are_not() {
        assert!(is_wal_attach_failure(&AppError::Database {
            message: "unable to open database file".into()
        }));
        assert!(is_wal_attach_failure(&AppError::Database {
            message: "database is locked".into()
        }));
        assert!(!is_wal_attach_failure(&AppError::Database {
            message: "file is not a database".into()
        }));
        assert!(!is_wal_attach_failure(&AppError::PermissionDenied {
            path: "x".into()
        }));
    }
}
