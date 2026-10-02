//! WhatsApp source: reads the WhatsApp for Mac message store.
//!
//! WhatsApp keeps a Core Data SQLite file in its shared group
//! container. It is plaintext on disk (the transport is E2EE, the local
//! cache is not), so this is the same shape of read as `chat.db`:
//! read-only, in place, never copied or written.
//!
//! Schema subset used (Core Data mangles names with a `Z` prefix):
//!   ZWACHATSESSION  - one row per conversation. `ZCONTACTJID` is the
//!                     WhatsApp id, `ZPARTNERNAME` the name WhatsApp
//!                     itself shows, `ZHIDDEN`/`ZREMOVED` the chats the
//!                     user buried.
//!   ZWAMESSAGE      - `ZISFROMME` mirrors iMessage's `is_from_me`,
//!                     `ZMESSAGEDATE` is Core Data time (seconds since
//!                     2001-01-01), `ZMESSAGETYPE` 6 is a system/group
//!                     event, `ZTEXT` is NULL for media.
//!   ZWAGROUPMEMBER  - group participants, already name-resolved by
//!                     WhatsApp (`ZCONTACTNAME` / `ZFIRSTNAME`).
//!
//! Two honest limitations, both structural rather than lazy:
//!   - **No reaction state.** This database stores no reactions
//!     anywhere (there is no reaction table and no reaction column), so
//!     a thread you only reacted to cannot be detected and `acknowledged`
//!     is always false. On iMessage that flag demotes a row; here every
//!     surfaced row is fully open.
//!   - **`@lid` chats have no phone number.** WhatsApp's newer
//!     privacy-preserving ids are not reversible to a number, so those
//!     rows fall back to WhatsApp's own display name and are skipped by
//!     the unknown-senders view (there is nothing to look up or add to
//!     Contacts).

use rusqlite::{params, Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::contacts::ContactDirectory;
use crate::error::{AppError, AppResult};
use crate::pending::Direction;
use crate::replies::{
    clean_preview, first_name, format_group_name, is_short_code, resolve_one,
    search_hint_for_group, Filters, PendingReply,
};
use crate::sources::Platform;
use crate::unknown::UnknownSender;

/// Core Data stores timestamps as seconds since 2001-01-01 UTC, the
/// same epoch as Apple's `message.date` but never in nanoseconds.
const CORE_DATA_EPOCH_OFFSET: i64 = 978_307_200;

/// `ZWAMESSAGE.ZMESSAGETYPE` for a system / group-event row ("you were
/// added", "the subject changed"). Never a candidate for "last real
/// message", exactly like `item_type != 0` on iMessage.
const MESSAGE_TYPE_SYSTEM: i64 = 6;

pub fn db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library/Group Containers/group.net.whatsapp.WhatsApp.shared/ChatStorage.sqlite")
}

fn core_data_to_unix(raw: f64) -> i64 {
    raw as i64 + CORE_DATA_EPOCH_OFFSET
}

/// `15551234567@s.whatsapp.net` → `+15551234567`. Device suffixes
/// (`:12@`) are stripped. Returns None for group ids, `@lid` ids,
/// broadcasts and anything else that isn't a dialable number.
pub fn jid_phone(jid: &str) -> Option<String> {
    let (local, domain) = jid.split_once('@')?;
    if domain != "s.whatsapp.net" {
        return None;
    }
    let local = local.split(':').next().unwrap_or(local);
    if local.is_empty() || !local.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("+{local}"))
}

pub fn is_group_jid(jid: &str) -> bool {
    jid.ends_with("@g.us")
}

/// Status broadcasts and channel/newsletter feeds are not conversations
/// anyone owes a reply to.
pub fn is_feed_jid(jid: &str) -> bool {
    jid.ends_with("@broadcast") || jid.ends_with("@newsletter")
}

/// Open the store read-only, mapping a missing file and an FDA block to
/// the same structured errors the iMessage reader uses.
fn open_ro(path: &Path) -> AppResult<Connection> {
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
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        let s = e.to_string();
        if s.contains("authorization denied") || s.contains("unable to open") {
            AppError::PermissionDenied {
                path: path.display().to_string(),
            }
        } else {
            AppError::Database { message: s }
        }
    })
}

/// One conversation's most recent real message, before name resolution.
#[derive(Debug, Clone)]
struct SessionRow {
    session_pk: i64,
    jid: String,
    partner_name: Option<String>,
    last_message_id: String,
    timestamp: i64,
    preview: Option<String>,
    member_count: i64,
    /// Group sender of the last message, as WhatsApp resolved it.
    sender_name: Option<String>,
}

pub fn read_pending(
    direction: Direction,
    dir: &ContactDirectory,
    filters: &Filters,
) -> AppResult<Vec<PendingReply>> {
    read_pending_at(&db_path(), direction, dir, filters)
}

/// Same as `read_pending` against an explicit path, so tests can run
/// the real SQL against a synthetic store.
pub fn read_pending_at(
    path: &Path,
    direction: Direction,
    dir: &ContactDirectory,
    filters: &Filters,
) -> AppResult<Vec<PendingReply>> {
    let conn = open_ro(path)?;
    let rows = query_sessions(&conn, direction)?;
    let group_pks: Vec<i64> = rows
        .iter()
        .filter(|r| is_group_jid(&r.jid))
        .map(|r| r.session_pk)
        .collect();
    let members = query_group_members(&conn, &group_pks)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let chat_key = Platform::WhatsApp.chat_key(&r.jid);
        // WhatsApp stores no reactions, so nothing here can be
        // "acknowledged" - see the module header.
        if !filters.keep(&chat_key, r.timestamp, false) {
            continue;
        }
        let is_group = is_group_jid(&r.jid);
        let phone = jid_phone(&r.jid);
        if !is_group {
            if let Some(p) = phone.as_deref() {
                if is_short_code(p.trim_start_matches('+')) {
                    continue;
                }
            }
        }

        let participants: Vec<String> = if is_group {
            members.get(&r.session_pk).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };

        let (name, search_hint) = if is_group {
            match non_empty(r.partner_name.as_deref()) {
                Some(subject) => (subject.to_string(), subject.to_string()),
                None => (
                    format_group_name(&participants),
                    search_hint_for_group(&participants),
                ),
            }
        } else {
            // Prefer the user's own address book so a person reads the
            // same on every platform, then WhatsApp's name, then the
            // number.
            let resolved = phone
                .as_deref()
                .and_then(|p| dir.lookup(p).map(str::to_string))
                .or_else(|| non_empty(r.partner_name.as_deref()).map(str::to_string))
                .or_else(|| phone.clone())
                .unwrap_or_else(|| r.jid.clone());
            (resolved.clone(), resolved)
        };

        out.push(PendingReply {
            platform: Platform::WhatsApp,
            chat_guid: chat_key,
            name,
            // Empty for groups and for `@lid` chats: `open_chat` falls
            // back to driving WhatsApp's search when there's no number.
            handle: phone.unwrap_or_default(),
            last_message_guid: r.last_message_id,
            timestamp: r.timestamp,
            is_group,
            member_count: if is_group {
                r.member_count.max(1) as u32
            } else {
                1
            },
            service: Some("WhatsApp".to_string()),
            participants,
            last_sender: if is_group {
                r.sender_name.as_deref().map(|s| first_name(s).to_string())
            } else {
                None
            },
            preview: r.preview.as_deref().and_then(clean_preview),
            search_hint,
            acknowledged: false,
        });
    }
    Ok(out)
}

fn query_sessions(conn: &Connection, direction: Direction) -> AppResult<Vec<SessionRow>> {
    let is_from_me = match direction {
        Direction::Owed => 0i64,
        Direction::Waiting => 1i64,
    };
    // Window over real messages per session and keep rn = 1: the same
    // "last substantive message" rule the iMessage query uses. System
    // rows (ZMESSAGETYPE 6) are excluded from the ranking entirely, so
    // a "subject changed" event can't mask the real last message.
    let sql = r#"
        WITH ranked AS (
            SELECT
                m.Z_PK          AS msg_pk,
                m.ZCHATSESSION  AS session_pk,
                m.ZISFROMME     AS is_from_me,
                m.ZMESSAGEDATE  AS msg_date,
                m.ZTEXT         AS msg_text,
                m.ZSTANZAID     AS stanza_id,
                m.ZGROUPMEMBER  AS group_member_pk,
                m.ZPUSHNAME     AS push_name,
                ROW_NUMBER() OVER (
                    PARTITION BY m.ZCHATSESSION
                    ORDER BY m.ZMESSAGEDATE DESC, m.Z_PK DESC
                ) AS rn
            FROM ZWAMESSAGE m
            WHERE COALESCE(m.ZMESSAGETYPE, 0) <> ?2
              AND m.ZCHATSESSION IS NOT NULL
              AND m.ZMESSAGEDATE IS NOT NULL
        ),
        member_counts AS (
            SELECT ZCHATSESSION AS session_pk, COUNT(*) AS member_count
            FROM ZWAGROUPMEMBER
            GROUP BY ZCHATSESSION
        )
        SELECT
            s.Z_PK                       AS session_pk,
            s.ZCONTACTJID                AS jid,
            s.ZPARTNERNAME               AS partner_name,
            r.msg_pk                     AS msg_pk,
            r.stanza_id                  AS stanza_id,
            r.msg_date                   AS msg_date,
            r.msg_text                   AS msg_text,
            r.push_name                  AS push_name,
            gm.ZCONTACTNAME              AS member_name,
            gm.ZFIRSTNAME                AS member_first_name,
            gm.ZMEMBERJID                AS member_jid,
            COALESCE(mc.member_count, 0) AS member_count
        FROM ranked r
        JOIN ZWACHATSESSION s        ON s.Z_PK = r.session_pk
        LEFT JOIN ZWAGROUPMEMBER gm  ON gm.Z_PK = r.group_member_pk
        LEFT JOIN member_counts mc   ON mc.session_pk = s.Z_PK
        WHERE r.rn = 1
          AND r.is_from_me = ?1
          AND COALESCE(s.ZHIDDEN, 0) = 0
          AND COALESCE(s.ZREMOVED, 0) = 0
          AND s.ZCONTACTJID IS NOT NULL
        ORDER BY r.msg_date DESC
    "#;
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![is_from_me, MESSAGE_TYPE_SYSTEM], |row| {
        let jid: String = row.get("jid")?;
        let msg_pk: i64 = row.get("msg_pk")?;
        let stanza: Option<String> = row.get("stanza_id")?;
        let date: f64 = row.get("msg_date")?;
        let member_name: Option<String> = row.get("member_name")?;
        let member_first: Option<String> = row.get("member_first_name")?;
        let member_jid: Option<String> = row.get("member_jid")?;
        let push_name: Option<String> = row.get("push_name")?;
        Ok(SessionRow {
            session_pk: row.get("session_pk")?,
            jid,
            partner_name: row.get("partner_name")?,
            // Stanza ids are stable across devices; fall back to the
            // row id so the frontend's "is this new?" check always has
            // something to compare.
            last_message_id: stanza
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("wa-pk-{msg_pk}")),
            timestamp: core_data_to_unix(date),
            preview: row.get::<_, Option<String>>("msg_text")?,
            member_count: row.get("member_count")?,
            sender_name: non_empty(member_name.as_deref())
                .or_else(|| non_empty(member_first.as_deref()))
                .or_else(|| non_empty(push_name.as_deref()))
                .map(str::to_string)
                .or_else(|| member_jid.as_deref().and_then(jid_phone)),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        let r = r?;
        if is_feed_jid(&r.jid) {
            continue;
        }
        out.push(r);
    }
    Ok(out)
}

/// Participant display names per group session, in a stable order.
fn query_group_members(
    conn: &Connection,
    session_pks: &[i64],
) -> AppResult<HashMap<i64, Vec<String>>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    if session_pks.is_empty() {
        return Ok(out);
    }
    for batch in session_pks.chunks(500) {
        let placeholders = std::iter::repeat("?")
            .take(batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT ZCHATSESSION, ZCONTACTNAME, ZFIRSTNAME, ZMEMBERJID
             FROM ZWAGROUPMEMBER
             WHERE ZCHATSESSION IN ({placeholders})
             ORDER BY ZCHATSESSION, ZCONTACTNAME, ZMEMBERJID"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            let session: i64 = row.get(0)?;
            let name: Option<String> = row.get(1)?;
            let first: Option<String> = row.get(2)?;
            let jid: Option<String> = row.get(3)?;
            let label = non_empty(name.as_deref())
                .or_else(|| non_empty(first.as_deref()))
                .map(str::to_string)
                .or_else(|| jid.as_deref().and_then(jid_phone))
                .or(jid)
                .unwrap_or_else(|| "Unknown".to_string());
            Ok((session, label))
        })?;
        for r in rows {
            let (session, label) = r?;
            out.entry(session).or_default().push(label);
        }
    }
    Ok(out)
}

/// Unknown senders, WhatsApp edition: 1:1 chats from a number that
/// resolves to no contact. `@lid` chats are skipped - with no number
/// there is nothing to check against Contacts or hand to the add flow.
pub fn read_unknown_at(
    path: &Path,
    dir: &ContactDirectory,
    filters: &Filters,
) -> AppResult<Vec<UnknownSender>> {
    let conn = open_ro(path)?;
    let sql = r#"
        WITH real AS (
            SELECT
                m.ZCHATSESSION AS session_pk,
                m.Z_PK         AS msg_pk,
                m.ZTEXT        AS msg_text,
                ROW_NUMBER() OVER (
                    PARTITION BY m.ZCHATSESSION
                    ORDER BY m.ZMESSAGEDATE DESC, m.Z_PK DESC
                ) AS rn,
                COUNT(*)       OVER (PARTITION BY m.ZCHATSESSION) AS msg_count,
                SUM(CASE WHEN m.ZISFROMME = 0 THEN 1 ELSE 0 END)
                               OVER (PARTITION BY m.ZCHATSESSION) AS inbound_count,
                MIN(m.ZMESSAGEDATE) OVER (PARTITION BY m.ZCHATSESSION) AS first_date,
                MAX(m.ZMESSAGEDATE) OVER (PARTITION BY m.ZCHATSESSION) AS last_date
            FROM ZWAMESSAGE m
            WHERE COALESCE(m.ZMESSAGETYPE, 0) <> ?1
              AND m.ZCHATSESSION IS NOT NULL
              AND m.ZMESSAGEDATE IS NOT NULL
        )
        SELECT
            s.ZCONTACTJID  AS jid,
            r.msg_text     AS msg_text,
            r.msg_count    AS msg_count,
            r.inbound_count AS inbound_count,
            r.first_date   AS first_date,
            r.last_date    AS last_date
        FROM real r
        JOIN ZWACHATSESSION s ON s.Z_PK = r.session_pk
        WHERE r.rn = 1
          AND COALESCE(s.ZREMOVED, 0) = 0
          AND s.ZCONTACTJID IS NOT NULL
          AND s.ZCONTACTJID LIKE '%@s.whatsapp.net'
    "#;
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![MESSAGE_TYPE_SYSTEM], |row| {
        let jid: String = row.get("jid")?;
        let first: f64 = row.get("first_date")?;
        let last: f64 = row.get("last_date")?;
        Ok((
            jid,
            row.get::<_, Option<String>>("msg_text")?,
            row.get::<_, i64>("msg_count")?,
            row.get::<_, i64>("inbound_count")?,
            core_data_to_unix(first),
            core_data_to_unix(last),
        ))
    })?;

    let mut out = Vec::new();
    for r in rows {
        let (jid, text, count, inbound, first_date, last_date) = r?;
        let Some(phone) = jid_phone(&jid) else {
            continue;
        };
        let chat_key = Platform::WhatsApp.chat_key(&jid);
        if inbound == 0
            || dir.lookup(&phone).is_some()
            || is_short_code(phone.trim_start_matches('+'))
            || last_date < filters.cutoff()
            || filters.muted.contains(&chat_key)
            || filters.snoozed.contains_key(&chat_key)
            || filters.done.get(&chat_key).is_some_and(|&d| last_date <= d)
        {
            continue;
        }
        out.push(UnknownSender {
            platform: Platform::WhatsApp,
            chat_guid: chat_key,
            handle: phone,
            service: Some("WhatsApp".to_string()),
            first_date,
            last_date,
            message_count: count,
            preview: text.as_deref().and_then(clean_preview),
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.last_date));
    Ok(out)
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Unused outside the resolver, but keeps the contact-name path honest
/// if a future change wants raw-handle resolution like iMessage's.
#[allow(dead_code)]
fn resolve_phone(phone: &str, dir: &ContactDirectory) -> String {
    resolve_one(phone, dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jid_phone_extracts_dialable_numbers() {
        assert_eq!(
            jid_phone("15551234567@s.whatsapp.net").as_deref(),
            Some("+15551234567")
        );
        // device-suffixed sender jid
        assert_eq!(
            jid_phone("15551234567:12@s.whatsapp.net").as_deref(),
            Some("+15551234567")
        );
    }

    #[test]
    fn jid_phone_rejects_non_numbers() {
        assert_eq!(jid_phone("1234-5678@lid"), None);
        assert_eq!(jid_phone("99887766@lid"), None);
        assert_eq!(jid_phone("1203630@g.us"), None);
        assert_eq!(jid_phone("status@broadcast"), None);
        assert_eq!(jid_phone("nonsense"), None);
        assert_eq!(jid_phone("@s.whatsapp.net"), None);
    }

    #[test]
    fn group_and_feed_jids_are_classified() {
        assert!(is_group_jid("1203630@g.us"));
        assert!(!is_group_jid("15551234567@s.whatsapp.net"));
        assert!(is_feed_jid("status@broadcast"));
        assert!(is_feed_jid("0123@newsletter"));
        assert!(!is_feed_jid("1203630@g.us"));
    }

    #[test]
    fn core_data_epoch_matches_apple_epoch() {
        // 2024-01-01 UTC = 1704067200 unix = 725760000 seconds since 2001.
        assert_eq!(core_data_to_unix(725_760_000.0), 1_704_067_200);
        // Core Data columns are REAL; fractional seconds truncate.
        assert_eq!(core_data_to_unix(725_760_000.75), 1_704_067_200);
    }
}
