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
//!   ZWAGROUPMEMBER  - group participants (`ZMEMBERJID`, plus
//!                     `ZCONTACTNAME` / `ZFIRSTNAME` - see the warning
//!                     below).
//!   ZWAPROFILEPUSHNAME - jid → the name a contact publishes for
//!                     themselves. A clean fallback, but not what
//!                     WhatsApp displays: see the precedence note on
//!                     `NameBook`.
//!
//! **The name columns cannot be trusted.** For `@lid` contacts this
//! store writes base64-encoded protobuf into the very columns that
//! normally hold names: `ZWAGROUPMEMBER.ZFIRSTNAME` holds values like
//! `+CgAQgueA1gY=` and `ZWAMESSAGE.ZPUSHNAME` holds
//! `COi9hdYGIABIAZABAPABAtgC…` (4,361 of 6,101 messages in the author's
//! own store). `ZWACHATSESSION.ZLASTMESSAGETEXT` is the same kind of
//! blob, which is why the last message is recomputed from ZWAMESSAGE
//! rather than read from the session row. Every candidate name is
//! therefore passed through `plausible_name` before it can reach the
//! UI, and `ZWAPROFILEPUSHNAME` is preferred over all of them.
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
    clean_preview, first_name, format_group_name, is_short_code, search_hint_for_group, Filters,
    PendingReply,
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
    /// JID of the group member who sent the last message, when the
    /// message carries one (93 inbound group messages in the author's
    /// store do not).
    sender_jid: Option<String>,
    /// Name candidates WhatsApp attached to the sender, best first.
    /// Unvalidated - `resolve_person` filters them.
    sender_names: Vec<String>,
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
    let book = load_name_book(&conn)?;
    let rows = query_sessions(&conn, direction)?;
    let group_pks: Vec<i64> = rows
        .iter()
        .filter(|r| is_group_jid(&r.jid))
        .map(|r| r.session_pk)
        .collect();
    let members = query_group_members(&conn, &group_pks, dir, &book)?;

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
            // The group subject is user-authored, but validate it
            // anyway - it lands in the same column family as the blobs.
            match clean_name(r.partner_name.as_deref()) {
                Some(subject) => (subject.to_string(), subject.to_string()),
                None => (
                    format_group_name(&participants),
                    search_hint_for_group(&participants),
                ),
            }
        } else {
            let resolved = resolve_person(Some(&r.jid), &[r.partner_name.as_deref()], dir, &book)
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
                let candidates: Vec<Option<&str>> =
                    r.sender_names.iter().map(|s| Some(s.as_str())).collect();
                resolve_person(r.sender_jid.as_deref(), &candidates, dir, &book)
                    .map(|n| first_name(&n).to_string())
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
            sender_jid: member_jid,
            sender_names: [member_name, member_first, push_name]
                .into_iter()
                .flatten()
                .collect(),
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

/// Participant display names per group session, in a stable order,
/// resolved through `resolve_person` so no blob can become a name.
fn query_group_members(
    conn: &Connection,
    session_pks: &[i64],
    dir: &ContactDirectory,
    book: &NameBook,
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
            let label = resolve_person(
                jid.as_deref(),
                &[name.as_deref(), first.as_deref()],
                dir,
                book,
            )
            .or_else(|| jid.clone())
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

/// The store's two jid-keyed name sources, in trust order.
///
/// **Precedence matters more than cleanliness here.** A name is not
/// just a label: for any chat without a phone number (every `@lid`
/// contact, every group) the only way to open the thread is to type
/// the name into the app's own search, so a name WhatsApp does not
/// recognize cannot open anything. `ZPARTNERNAME` is the label
/// WhatsApp displays and searches by; `ZWAPROFILEPUSHNAME` is whatever
/// the contact set for themselves, and the two disagree for 29 of 66
/// chats in the author's store - `215152175652912@lid` is
/// "David Gelberg" to WhatsApp and "And David Says" to himself.
/// Preferring the push name there produced a row that searched for
/// "And David Says" and found nothing.
///
/// So: names you or WhatsApp assigned beat names the contact assigned
/// themselves, and the push-name table exists for the case that
/// started all this - `@lid` rows whose own name columns hold protobuf.
struct NameBook {
    /// jid → `ZWACHATSESSION.ZPARTNERNAME` of that person's 1:1 chat.
    session_labels: HashMap<String, String>,
    /// jid → the name the contact publishes for themselves.
    push_names: HashMap<String, String>,
}

fn load_name_book(conn: &Connection) -> AppResult<NameBook> {
    Ok(NameBook {
        session_labels: query_name_map(
            conn,
            "SELECT ZCONTACTJID, ZPARTNERNAME FROM ZWACHATSESSION
             WHERE ZCONTACTJID IS NOT NULL AND ZPARTNERNAME IS NOT NULL
               AND ZCONTACTJID NOT LIKE '%@g.us'",
        )?,
        push_names: query_name_map(
            conn,
            "SELECT ZJID, ZPUSHNAME FROM ZWAPROFILEPUSHNAME
             WHERE ZJID IS NOT NULL AND ZPUSHNAME IS NOT NULL",
        )?,
    })
}

/// Run a `(jid, name)` query, keeping only names that survive
/// validation. Both tables are small - a few hundred rows.
fn query_name_map(conn: &Connection, sql: &str) -> AppResult<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for r in rows {
        let (jid, name) = r?;
        if let Some(clean) = clean_name(Some(&name)) {
            out.insert(jid, clean.to_string());
        }
    }
    Ok(out)
}

/// The one name-resolution chain, used for 1:1 partners, group
/// participants and message senders alike:
///   1. the user's own address book, by phone number - so a person
///      reads the same on every platform;
///   2. the name candidates the row itself carried, validated
///      (`ZPARTNERNAME`, `ZCONTACTNAME`, `ZFIRSTNAME`);
///   3. the label WhatsApp shows for that jid's own 1:1 chat - which
///      names a group member the way you know them, not the way they
///      named themselves;
///   4. the contact's published push name;
///   5. the phone number.
///
/// See `NameBook` for why 2 and 3 outrank 4. Returns None when nothing
/// survives (an `@lid` contact with no name anywhere has no usable
/// label at all).
fn resolve_person(
    jid: Option<&str>,
    row_names: &[Option<&str>],
    dir: &ContactDirectory,
    book: &NameBook,
) -> Option<String> {
    let phone = jid.and_then(jid_phone);
    if let Some(name) = phone.as_deref().and_then(|p| dir.lookup(p)) {
        return Some(name.to_string());
    }
    for candidate in row_names {
        if let Some(name) = clean_name(*candidate) {
            return Some(name.to_string());
        }
    }
    if let Some(name) = jid.and_then(|j| book.session_labels.get(j)) {
        return Some(name.clone());
    }
    if let Some(name) = jid.and_then(|j| book.push_names.get(j)) {
        return Some(name.clone());
    }
    phone
}

/// Trim, then drop anything that isn't plausibly a name.
fn clean_name(s: Option<&str>) -> Option<&str> {
    non_empty(s).filter(|t| plausible_name(t))
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Could this string be a human-readable display name?
///
/// Guards against the base64 protobuf this store writes into its name
/// columns (see the module header). Showing `+CgAQgueA1gY=` where a
/// person's name belongs is worse than showing their phone number.
///
/// The rule matches the *blob signature* rather than demanding a name
/// look conventional, because labels in the wild are messier than any
/// guess about them: group chats get named `🐯-🦈`, and an unnamed
/// group's composed label can read `+12135514874, Adina +9`. Both of
/// those are real and both must survive. So a string is rejected only
/// when it carries base64 padding, is a bare identifier, or is long,
/// space-free, and mixes letters with digits inside the base64
/// alphabet.
pub fn plausible_name(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    // Base64 padding never appears in a name.
    if t.ends_with('=') {
        return false;
    }
    // A bare run of digits is an identifier, not a name.
    if t.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // A phone number is a legitimate last-resort label.
    let phone_shaped = t.strip_prefix('+').is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '-' | '(' | ')' | '.'))
    });
    if phone_shaped {
        return true;
    }
    // The blobs are long, space-free, and mix letters with digits
    // inside the base64 alphabet. Real handles shaped like that
    // ("Sarah1234567890") are rarer than the blobs, and the fallback
    // is a phone number rather than nothing.
    let has_space = t.chars().any(char::is_whitespace);
    let has_digit = t.chars().any(|c| c.is_ascii_digit());
    let has_letter = t.chars().any(|c| c.is_ascii_alphabetic());
    let base64_alphabet = t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='));
    if !has_space && has_digit && has_letter && base64_alphabet && t.chars().count() >= 12 {
        return false;
    }
    true
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
    fn plausible_name_rejects_the_stores_protobuf_blobs() {
        // Verbatim values from a real ChatStorage.sqlite.
        assert!(!plausible_name("+CgAQgueA1gY="));
        assert!(!plausible_name("COi9hdYGIABIAZABAPABAtgC+8rT09"));
        assert!(!plausible_name("CMfngNYGIAA6FDNCRjk5QTRGNkQ5Rj"));
        assert!(!plausible_name("EID1JCoICID1JBACGABIAGIkRTNGMzJCN0EtQ0RG"));
        assert!(!plausible_name(""));
        assert!(!plausible_name("   "));
        // A bare identifier is not a name.
        assert!(!plausible_name("12345"));
    }

    #[test]
    fn plausible_name_keeps_real_names_and_numbers() {
        assert!(plausible_name("Ana Diaz"));
        assert!(plausible_name("Jacob"));
        assert!(plausible_name("JacobRintamaki"));
        assert!(plausible_name("Jacob <> David"));
        assert!(plausible_name("Ana-Maria O'Brien"));
        assert!(plausible_name("张伟"));
        assert!(plausible_name("Mr2000"));
        // Group chats get named things like this; an over-strict rule
        // that dropped them would be worse than the bug it fixes.
        assert!(plausible_name("🐯-🦈"));
        assert!(plausible_name("🏠"));
        assert!(plausible_name("Flat 3 🍳"));
        // Phone-shaped strings are a legitimate last-resort label.
        assert!(plausible_name("+15551234567"));
        assert!(plausible_name("+1 (555) 123-4567"));
        // A composed group label, verbatim from a real run: a '+' that
        // starts a phone number but keeps going into names.
        assert!(plausible_name("+12135514874, Adina +9"));
        assert!(plausible_name("Mom, Dad +2"));
    }

    #[test]
    fn core_data_epoch_matches_apple_epoch() {
        // 2024-01-01 UTC = 1704067200 unix = 725760000 seconds since 2001.
        assert_eq!(core_data_to_unix(725_760_000.0), 1_704_067_200);
        // Core Data columns are REAL; fractional seconds truncate.
        assert_eq!(core_data_to_unix(725_760_000.75), 1_704_067_200);
    }
}
