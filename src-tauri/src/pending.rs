use rusqlite::{params, Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};

/// Which side of the conversation the surfaced rows represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Last real message was inbound — the user owes a reply.
    Owed,
    /// Last real message was outbound — the user is waiting on a reply.
    Waiting,
}

/// One row from `chat.db` representing the most recent message in a
/// chat where that message was sent by the other party (i.e. a chat
/// where the user owes a reply).
#[derive(Debug, Clone)]
pub struct PendingReplyRow {
    pub chat_guid: String,
    pub chat_rowid: i64,
    pub chat_identifier: String,
    pub display_name: Option<String>,
    pub handle_id_str: Option<String>,
    pub handle_service: Option<String>,
    pub last_message_guid: String,
    pub last_message_timestamp_unix: i64,
    pub member_count: i64,
    /// The text of the latest message, if recoverable. Pulled from
    /// `message.text` first; falls back to a typedstream parse of
    /// `message.attributedBody` for messages where text is NULL
    /// (common on macOS 13+).
    pub preview: Option<String>,
    /// The relevant side already reacted (tapback/sticker) to this
    /// last message — the thread is acknowledged but not replied to.
    pub acknowledged: bool,
}

#[derive(Debug, Clone)]
pub struct PendingResults {
    pub rows: Vec<PendingReplyRow>,
    /// chat ROWID → ordered list of participant handles (raw, e.g.
    /// "+15551234567" or "alice@example.com"). Excludes the user's
    /// own handle (which iMessage already excludes from
    /// chat_handle_join).
    pub participants_by_chat: HashMap<i64, Vec<String>>,
}

pub fn chat_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Messages/chat.db")
}

pub fn read_pending_replies(direction: Direction) -> AppResult<PendingResults> {
    read_pending_replies_at(&chat_db_path(), direction)
}

/// Same as `read_pending_replies` but against an explicit database
/// path. This is the entry point the test suite uses to run the real
/// queries against a synthetic fixture db — the production path never
/// varies, but the logic must be testable without touching the user's
/// actual chat.db.
pub fn read_pending_replies_at(path: &Path, direction: Direction) -> AppResult<PendingResults> {
    // Surface FDA failures as a structured PermissionDenied before we
    // ever touch sqlite — the underlying errors aren't always clear.
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::NotFound {
                path: path.display().to_string(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(AppError::PermissionDenied {
                path: path.display().to_string(),
            });
        }
        Err(e) => {
            // macOS surfaces FDA blocks as EPERM (errno 1, "Operation not permitted")
            // which std maps to PermissionDenied on most kinds, but on some
            // filesystems it leaks through as Other. Catch by raw_os_error.
            if e.raw_os_error() == Some(1) {
                return Err(AppError::PermissionDenied {
                    path: path.display().to_string(),
                });
            }
            return Err(AppError::Io {
                message: e.to_string(),
            });
        }
    }

    let conn = Connection::open_with_flags(
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
    })?;

    let rows = run_pending_query(&conn, direction)?;
    let chat_rowids: Vec<i64> = rows.iter().map(|r| r.chat_rowid).collect();
    let participants_by_chat = run_participants_query(&conn, &chat_rowids)?;
    Ok(PendingResults {
        rows,
        participants_by_chat,
    })
}

fn run_pending_query(conn: &Connection, direction: Direction) -> AppResult<Vec<PendingReplyRow>> {
    // For each chat, pick the single most recent real message — text
    // or attachment, not a tapback/sticker reaction or a system event
    // (joins, renames). Surface chats whose last real message came
    // from the queried side, flagging whether the other side has
    // reacted to it (`acknowledged`).
    // member_count comes from chat_handle_join, which excludes
    // the user's own handle — so 1:1 chats have member_count = 1.
    //
    // Filter rationale:
    //   item_type = 0                  — drops join/leave/rename rows
    //   associated_message_type = 0    — drops tapback and sticker
    //                                    reactions; real messages and
    //                                    inline replies keep this at 0
    //   is_filtered = 0                — drops chats Messages itself
    //                                    classifies as junk/promotions
    //                                    (spam carries values 1–68;
    //                                    every known-contact chat is 0)
    //   is_blackholed / is_archived    — user explicitly buried these
    //   NOT LIKE '%(smsf%'             — SMS-filter shadow chats
    //                                    ((smsft)/(smsfp)/(smsft_rm)
    //                                    suffixes); some carry
    //                                    is_filtered = 0 despite being
    //                                    filter-category duplicates
    //
    // Acknowledgment: a reaction row points at its target via
    // associated_message_guid, stored as either the bare guid or a
    // `p:N/<guid>` / `bp:<guid>` prefix. Reaction "add" types:
    //   1000       — sticker placed on a message
    //   2000–2999  — classic tapbacks (love/like/…, incl. 2006 custom
    //                emoji reactions)
    //   4000–4999  — custom emoji tapbacks (iOS 18+ variant)
    // Removal types are the add value + 1000 (2000→3000, 2006→3006,
    // 4000→5000). An add counts only while no LATER removal of the
    // same type from the same reactor targets the same message —
    // add-then-remove returns the thread to open; re-adding
    // acknowledges again. Stickers (1000) have no removal pairing
    // (1000+1000=2000 would collide with the love tapback).
    //
    // For Owed we look for the user's reaction (is_from_me = 1) on the
    // latest inbound message; for Waiting, the other party's reaction
    // (is_from_me = 0) on the latest outbound message. A reaction does
    // NOT remove the row — it marks it `acknowledged`, which the UI
    // demotes and badge counts exclude. The question-mark exception
    // (a tapback on a question is not an answer) is applied in Rust
    // after the preview text is extracted, since the text may live in
    // attributedBody.
    let (last_from_me, reactor_from_me) = match direction {
        Direction::Owed => (0i64, 1i64),
        Direction::Waiting => (1i64, 0i64),
    };
    let sql = r#"
        WITH ranked AS (
            SELECT
                cmj.chat_id AS chat_id,
                m.ROWID     AS msg_rowid,
                m.guid      AS msg_guid,
                m.date      AS msg_date,
                m.is_from_me AS is_from_me,
                m.handle_id AS handle_id,
                ROW_NUMBER() OVER (
                    PARTITION BY cmj.chat_id
                    ORDER BY m.date DESC, m.ROWID DESC
                ) AS rn
            FROM message m
            JOIN chat_message_join cmj ON cmj.message_id = m.ROWID
            WHERE COALESCE(m.item_type, 0) = 0
              AND COALESCE(m.associated_message_type, 0) = 0
        ),
        member_counts AS (
            SELECT chat_id, COUNT(*) AS member_count
            FROM chat_handle_join
            GROUP BY chat_id
        )
        SELECT
            c.ROWID                      AS chat_rowid,
            c.guid                       AS chat_guid,
            c.chat_identifier            AS chat_identifier,
            c.display_name               AS display_name,
            h.id                         AS handle_id_str,
            h.service                    AS handle_service,
            r.msg_guid                   AS last_message_guid,
            r.msg_date                   AS last_message_date,
            win_msg.text                 AS msg_text,
            win_msg.attributedBody       AS msg_attributed_body,
            COALESCE(mc.member_count, 0) AS member_count,
            CASE WHEN EXISTS (
                SELECT 1
                FROM message rx
                JOIN chat_message_join cmj2 ON cmj2.message_id = rx.ROWID
                WHERE cmj2.chat_id = r.chat_id
                  AND rx.is_from_me = ?2
                  AND (
                      rx.associated_message_type = 1000
                      OR (rx.associated_message_type >= 2000
                          AND rx.associated_message_type < 3000)
                      OR (rx.associated_message_type >= 4000
                          AND rx.associated_message_type < 5000)
                  )
                  AND (
                      rx.associated_message_guid = r.msg_guid
                      OR rx.associated_message_guid LIKE '%/' || r.msg_guid
                      OR rx.associated_message_guid LIKE '%:' || r.msg_guid
                  )
                  AND (
                      rx.associated_message_type < 2000
                      OR NOT EXISTS (
                          SELECT 1
                          FROM message rm
                          JOIN chat_message_join cmj3 ON cmj3.message_id = rm.ROWID
                          WHERE cmj3.chat_id = r.chat_id
                            AND rm.is_from_me = rx.is_from_me
                            AND COALESCE(rm.handle_id, 0) = COALESCE(rx.handle_id, 0)
                            AND rm.associated_message_type = rx.associated_message_type + 1000
                            AND (
                                rm.associated_message_guid = r.msg_guid
                                OR rm.associated_message_guid LIKE '%/' || r.msg_guid
                                OR rm.associated_message_guid LIKE '%:' || r.msg_guid
                            )
                            AND (rm.date > rx.date
                                 OR (rm.date = rx.date AND rm.ROWID > rx.ROWID))
                      )
                  )
            ) THEN 1 ELSE 0 END AS acknowledged
        FROM ranked r
        JOIN chat c                ON c.ROWID = r.chat_id
        JOIN message win_msg       ON win_msg.ROWID = r.msg_rowid
        LEFT JOIN handle h         ON h.ROWID = r.handle_id
        LEFT JOIN member_counts mc ON mc.chat_id = r.chat_id
        WHERE r.rn = 1 AND r.is_from_me = ?1
          AND COALESCE(c.is_filtered, 0) = 0
          AND COALESCE(c.is_blackholed, 0) = 0
          AND COALESCE(c.is_archived, 0) = 0
          AND c.chat_identifier NOT LIKE '%(smsf%'
        ORDER BY r.msg_date ASC
    "#;

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![last_from_me, reactor_from_me], |row| {
        let date_raw: i64 = row.get("last_message_date")?;
        let text: Option<String> = row.get("msg_text")?;
        let body: Option<Vec<u8>> = row.get("msg_attributed_body")?;
        let preview = text
            .filter(|s| !s.is_empty())
            .or_else(|| body.as_deref().and_then(extract_attributed_body_text));
        let mut acknowledged: bool = row.get::<_, i64>("acknowledged")? != 0;
        // Question-mark exception: a tapback on a question is not an
        // answer. The last substantive message asked something, so the
        // thread stays fully open regardless of reactions.
        if crate::config::QUESTION_MARK_KEEPS_OPEN
            && acknowledged
            && preview.as_deref().is_some_and(|p| p.contains('?'))
        {
            acknowledged = false;
        }
        Ok(PendingReplyRow {
            chat_rowid: row.get("chat_rowid")?,
            chat_guid: row.get("chat_guid")?,
            chat_identifier: row.get("chat_identifier")?,
            display_name: row.get::<_, Option<String>>("display_name")?,
            handle_id_str: row.get::<_, Option<String>>("handle_id_str")?,
            handle_service: row.get::<_, Option<String>>("handle_service")?,
            last_message_guid: row.get("last_message_guid")?,
            last_message_timestamp_unix: apple_to_unix(date_raw),
            member_count: row.get("member_count")?,
            preview,
            acknowledged,
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Best-effort extraction of the message text from `attributedBody` —
/// a typedstream-encoded NSAttributedString.
///
/// The typedstream archive stores the string after class metadata for
/// `NSString`, prefixed with a `+` (0x2B) marker and a length encoding:
///   `< 0x80`         — that byte IS the length
///   `0x81 [u16 LE]`  — 16-bit length follows
///   `0x82 [u32 LE]`  — 32-bit length follows
///
/// We don't decode the full archive — we just locate the first NSString
/// payload, which is the message text in the cases we care about.
/// Returns `None` for anything we can't safely decode (attachments
/// without text, reactions, malformed blobs).
pub fn extract_attributed_body_text(bytes: &[u8]) -> Option<String> {
    let needle = b"NSString";
    let start = find_subslice(bytes, needle)? + needle.len();

    // Skip class metadata until we hit the typedstream object marker '+'.
    let mut pos = start;
    while pos < bytes.len() && bytes[pos] != 0x2B {
        pos += 1;
    }
    if pos >= bytes.len() {
        return None;
    }
    pos += 1;
    if pos >= bytes.len() {
        return None;
    }

    let len = match bytes[pos] {
        b if b < 0x80 => {
            pos += 1;
            b as usize
        }
        0x81 => {
            if pos + 3 > bytes.len() {
                return None;
            }
            let l = u16::from_le_bytes([bytes[pos + 1], bytes[pos + 2]]) as usize;
            pos += 3;
            l
        }
        0x82 => {
            if pos + 5 > bytes.len() {
                return None;
            }
            let l = u32::from_le_bytes([
                bytes[pos + 1],
                bytes[pos + 2],
                bytes[pos + 3],
                bytes[pos + 4],
            ]) as usize;
            pos += 5;
            l
        }
        _ => return None,
    };

    if len == 0 || len > 1_000_000 || pos.saturating_add(len) > bytes.len() {
        return None;
    }
    let s = std::str::from_utf8(&bytes[pos..pos + len]).ok()?;
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn run_participants_query(
    conn: &Connection,
    chat_rowids: &[i64],
) -> AppResult<HashMap<i64, Vec<String>>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    if chat_rowids.is_empty() {
        return Ok(out);
    }
    // SQLite supports ~999 bound params per stmt; we batch defensively.
    for batch in chat_rowids.chunks(500) {
        let placeholders = std::iter::repeat("?")
            .take(batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT chj.chat_id, h.id
             FROM chat_handle_join chj
             JOIN handle h ON h.ROWID = chj.handle_id
             WHERE chj.chat_id IN ({placeholders})
             ORDER BY chj.chat_id, h.id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            let chat_id: i64 = row.get(0)?;
            let handle: String = row.get(1)?;
            Ok((chat_id, handle))
        })?;
        for r in rows {
            let (cid, handle) = r?;
            out.entry(cid).or_default().push(handle);
        }
    }
    Ok(out)
}

/// Apple's `message.date` is "seconds since 2001-01-01 UTC" on older
/// macOS and "nanoseconds since 2001-01-01 UTC" on macOS 10.13+. The
/// Apple → Unix epoch shift is 978,307,200 seconds.
fn apple_to_unix(raw: i64) -> i64 {
    const APPLE_EPOCH_OFFSET: i64 = 978_307_200;
    // Anything above 10^12 is unambiguously nanoseconds — even seconds-
    // since-2001 won't reach that for centuries.
    if raw.abs() > 1_000_000_000_000 {
        raw / 1_000_000_000 + APPLE_EPOCH_OFFSET
    } else {
        raw + APPLE_EPOCH_OFFSET
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_to_unix_handles_seconds() {
        // 2010-01-01 UTC = 1262304000 unix; offset from 2001 = 283996800.
        assert_eq!(apple_to_unix(283_996_800), 1_262_304_000);
    }

    #[test]
    fn apple_to_unix_handles_nanoseconds() {
        // 2024-01-01 UTC = 1704067200 unix; offset from 2001 = 725760000s
        //   = 725_760_000_000_000_000 ns.
        assert_eq!(apple_to_unix(725_760_000_000_000_000), 1_704_067_200);
    }

    #[test]
    fn extract_attributed_body_short_text() {
        // Synthesized minimal blob: padding, "NSString", padding, '+',
        // length=5, "hello".
        let mut blob = Vec::new();
        blob.extend_from_slice(b"\x04\x0Bstreamtyped");
        blob.extend_from_slice(b"\x84\x84\x07NSString\x00\x84\x84\x08NSObject\x00\x85");
        blob.push(0x2B); // '+'
        blob.push(5); // length
        blob.extend_from_slice(b"hello");
        assert_eq!(
            extract_attributed_body_text(&blob).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn extract_attributed_body_returns_none_when_no_marker() {
        assert_eq!(extract_attributed_body_text(b""), None);
        assert_eq!(extract_attributed_body_text(b"random bytes"), None);
    }

    #[test]
    fn extract_attributed_body_handles_u16_length() {
        // 0x81 marker + u16 LE length = 3, then "abc".
        let mut blob = Vec::new();
        blob.extend_from_slice(b"NSString\x84\x85");
        blob.push(0x2B);
        blob.push(0x81);
        blob.push(0x03);
        blob.push(0x00);
        blob.extend_from_slice(b"abc");
        assert_eq!(extract_attributed_body_text(&blob).as_deref(), Some("abc"));
    }
}
