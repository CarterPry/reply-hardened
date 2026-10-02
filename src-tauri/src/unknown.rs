//! "Unknown senders" view: 1:1 threads whose handle has no AddressBook
//! match, with enough context (dates, counts, latest snippet) to decide
//! whether to reply, dismiss, or add the person to Contacts.
//!
//! This module is the iMessage reader plus the `UnknownSender` shape
//! the other sources fill in (`whatsapp::read_unknown_at`,
//! `signal::read_unknown_at`), so "unknown sender" means the same
//! thing - no AddressBook match - on all three.
//!
//! Unlike the reply lists, this view deliberately INCLUDES chats
//! Messages classifies as filtered (`is_filtered != 0`): with "Filter
//! Unknown Senders" enabled, that flag is set on exactly the threads
//! this view exists to surface. Blackholed/archived chats and SMS-filter
//! shadow chats stay excluded, as do short-code senders and threads
//! with no real (non-tapback) messages.

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config;
use crate::contacts::ContactDirectory;
use crate::error::{AppError, AppResult};
use crate::pending::{apple_to_unix, extract_attributed_body_text};
use crate::replies::is_short_code;
use crate::sources::Platform;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownSender {
    /// Which app the thread lives in.
    pub platform: Platform,
    pub chat_guid: String,
    /// Raw handle: phone number or email.
    pub handle: String,
    pub service: Option<String>,
    /// Unix seconds of the first and most recent real message.
    pub first_date: i64,
    pub last_date: i64,
    /// Count of real (non-tapback, non-system) messages in the thread.
    pub message_count: i64,
    /// Snippet of the most recent real message (attributedBody
    /// fallback applied). None for attachment-only messages.
    pub preview: Option<String>,
}

/// Raw row before contact/state filtering - separated so tests can
/// drive the filter logic without a real AddressBook.
#[derive(Debug, Clone)]
pub struct UnknownSenderRow {
    pub chat_guid: String,
    pub handle: String,
    pub service: Option<String>,
    pub first_date: i64,
    pub last_date: i64,
    pub message_count: i64,
    pub inbound_count: i64,
    pub preview: Option<String>,
}

pub fn read_unknown_senders(path: &Path) -> AppResult<Vec<UnknownSenderRow>> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| AppError::Database {
        message: e.to_string(),
    })?;

    // Same shape as the pending query: window over real messages per
    // chat, keep the rn=1 row for the snippet, aggregate dates/counts.
    let sql = r#"
        WITH real AS (
            SELECT
                cmj.chat_id AS chat_id,
                m.ROWID     AS msg_rowid,
                ROW_NUMBER() OVER (
                    PARTITION BY cmj.chat_id
                    ORDER BY m.date DESC, m.ROWID DESC
                ) AS rn,
                COUNT(*) OVER (PARTITION BY cmj.chat_id) AS msg_count,
                SUM(CASE WHEN m.is_from_me = 0 THEN 1 ELSE 0 END)
                    OVER (PARTITION BY cmj.chat_id) AS inbound_count,
                MIN(m.date) OVER (PARTITION BY cmj.chat_id) AS first_date,
                MAX(m.date) OVER (PARTITION BY cmj.chat_id) AS last_date
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
            c.guid                 AS chat_guid,
            h.id                   AS handle_id_str,
            h.service              AS handle_service,
            win.text               AS msg_text,
            win.attributedBody     AS msg_attributed_body,
            r.msg_count            AS msg_count,
            r.inbound_count        AS inbound_count,
            r.first_date           AS first_date,
            r.last_date            AS last_date
        FROM real r
        JOIN chat c                ON c.ROWID = r.chat_id
        JOIN member_counts mc      ON mc.chat_id = r.chat_id AND mc.member_count = 1
        JOIN chat_handle_join chj  ON chj.chat_id = r.chat_id
        JOIN handle h              ON h.ROWID = chj.handle_id
        JOIN message win           ON win.ROWID = r.msg_rowid
        WHERE r.rn = 1
          AND COALESCE(c.is_blackholed, 0) = 0
          AND COALESCE(c.is_archived, 0) = 0
          AND c.chat_identifier NOT LIKE '%(smsf%'
    "#;

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        let text: Option<String> = row.get("msg_text")?;
        let body: Option<Vec<u8>> = row.get("msg_attributed_body")?;
        let preview = text
            .filter(|s| !s.is_empty())
            .or_else(|| body.as_deref().and_then(extract_attributed_body_text));
        Ok(UnknownSenderRow {
            chat_guid: row.get("chat_guid")?,
            handle: row.get("handle_id_str")?,
            service: row.get("handle_service")?,
            first_date: apple_to_unix(row.get("first_date")?),
            last_date: apple_to_unix(row.get("last_date")?),
            message_count: row.get("msg_count")?,
            inbound_count: row.get("inbound_count")?,
            preview,
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Filter raw rows down to actual unknown senders and apply the same
/// local state (done/snoozed/muted) as the reply lists. Newest first.
pub fn assemble_unknown(
    rows: Vec<UnknownSenderRow>,
    dir: &ContactDirectory,
    done: &HashMap<String, i64>,
    snoozed: &HashMap<String, i64>,
    muted: &HashSet<String>,
    now: i64,
) -> Vec<UnknownSender> {
    let cutoff = now - config::STALE_AFTER_SECS;
    let mut out: Vec<UnknownSender> = rows
        .into_iter()
        .filter(|r| {
            // They messaged you (not just a thread you started), the
            // handle resolves to no contact, and it isn't a short code.
            r.inbound_count > 0
                && dir.lookup(&r.handle).is_none()
                && !is_short_code(&r.handle)
                && r.last_date >= cutoff
                && !muted.contains(&r.chat_guid)
                && !snoozed.contains_key(&r.chat_guid)
                && done.get(&r.chat_guid).map_or(true, |&d| r.last_date > d)
        })
        .map(|r| UnknownSender {
            platform: Platform::IMessage,
            chat_guid: r.chat_guid,
            handle: r.handle,
            service: r.service,
            first_date: r.first_date,
            last_date: r.last_date,
            message_count: r.message_count,
            preview: r.preview.as_deref().and_then(crate::replies::clean_preview),
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.last_date));
    out
}
