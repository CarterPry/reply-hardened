//! Pure assembly of the reply lists: takes raw rows from `chat.db`
//! (via `pending`), the contact directory, and the user's local state
//! (completed/snoozed/muted), and produces the list the UI renders.
//! No IO happens here - everything is injectable, which is what makes
//! the state machine testable against a fixture database.

use crate::config;
use crate::contacts::ContactDirectory;
use crate::pending::{Direction, PendingResults};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingReply {
    pub chat_guid: String,
    pub name: String,
    pub handle: String,
    pub last_message_guid: String,
    pub timestamp: i64,
    pub is_group: bool,
    pub member_count: u32,
    pub service: Option<String>,
    /// Resolved participant names for groups (empty for 1:1).
    pub participants: Vec<String>,
    /// First name of the latest sender - surfaced as the meta line in
    /// group rows so you can see who specifically owes a reply.
    pub last_sender: Option<String>,
    /// One-line preview of the latest message, truncated to
    /// `config::PREVIEW_CHARS`. Whitespace normalized (newlines →
    /// spaces). `None` when the row was a sticker, attachment-only,
    /// or the attributedBody parse failed.
    pub preview: Option<String>,
    /// What to type into Messages' search when opening a group chat.
    /// The group's real display name when it has one; otherwise
    /// participant first names space-separated - unlike the display
    /// label ("Mom, Dad +2"), that's something Messages search can
    /// actually match.
    pub search_hint: String,
    /// The thread was closed by a tapback rather than a real reply:
    /// in "owed" mode you reacted to their last message but never
    /// replied; in "waiting" mode they reacted to yours. Demoted in
    /// the UI and excluded from badge counts, but still visible.
    pub acknowledged: bool,
}

/// Everything `assemble` needs besides the rows themselves. All
/// injectable so tests can drive it without Tauri or the real HOME.
pub struct AssembleCtx<'a> {
    pub dir: &'a ContactDirectory,
    /// chat guid → done message date (unix seconds). A chat stays done
    /// while its last real message is no newer than this; a later real
    /// message resurfaces it (tapbacks never do - they're excluded from
    /// last-message selection upstream).
    pub done: &'a HashMap<String, i64>,
    pub snoozed: &'a HashMap<String, i64>,
    pub muted: &'a HashSet<String>,
    /// Unix seconds "now" - injected so staleness tests are deterministic.
    pub now: i64,
}

/// Number of rows that count toward the badge / tray: open threads
/// only, acknowledged ones are excluded.
pub fn open_count(rows: &[PendingReply]) -> usize {
    rows.iter().filter(|r| !r.acknowledged).count()
}

pub fn assemble(
    results: PendingResults,
    direction: Direction,
    ctx: &AssembleCtx,
) -> Vec<PendingReply> {
    let cutoff = ctx.now - config::STALE_AFTER_SECS;
    // In waiting mode a thread only counts once it's gone quiet.
    let newest_allowed = match direction {
        Direction::Owed => i64::MAX,
        Direction::Waiting => ctx.now - config::WAITING_MIN_AGE_SECS,
    };

    let mut out = Vec::with_capacity(results.rows.len());
    for r in results.rows {
        if let Some(&done_date) = ctx.done.get(&r.chat_guid) {
            if r.last_message_timestamp_unix <= done_date {
                continue;
            }
        }
        if r.last_message_timestamp_unix < cutoff || r.last_message_timestamp_unix > newest_allowed
        {
            continue;
        }
        if ctx.muted.contains(&r.chat_guid) || ctx.snoozed.contains_key(&r.chat_guid) {
            continue;
        }
        // Legacy behavior: acknowledged rows are cleared outright
        // instead of being shown demoted.
        if r.acknowledged && !config::TAPBACK_ACK_DEMOTES {
            continue;
        }
        let is_group = r.member_count > 1;
        if config::FILTER_SHORT_CODES && !is_group {
            let h = r.handle_id_str.as_deref().unwrap_or(&r.chat_identifier);
            if is_short_code(h) {
                continue;
            }
        }

        let participants: Vec<String> = if is_group {
            results
                .participants_by_chat
                .get(&r.chat_rowid)
                .map(|raw| raw.iter().map(|h| resolve_one(h, ctx.dir)).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let (name, handle, search_hint) = if is_group {
            let display = r
                .display_name
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());
            let (name, hint) = match display {
                Some(d) => (d.to_string(), d.to_string()),
                None => (
                    format_group_name(&participants),
                    search_hint_for_group(&participants),
                ),
            };
            // For groups, "handle" is informational only - open_chat
            // routes through the search hint for these.
            let handle = r.chat_identifier.clone();
            (name, handle, hint)
        } else {
            let h = r
                .handle_id_str
                .clone()
                .unwrap_or_else(|| r.chat_identifier.clone());
            let n = resolve_one(&h, ctx.dir);
            let hint = n.clone();
            (n, h, hint)
        };

        let last_sender = if is_group {
            r.handle_id_str
                .as_deref()
                .map(|h| first_name(&resolve_one(h, ctx.dir)).to_string())
        } else {
            None
        };

        let preview = r.preview.as_deref().and_then(clean_preview);
        out.push(PendingReply {
            chat_guid: r.chat_guid,
            name,
            handle,
            last_message_guid: r.last_message_guid,
            timestamp: r.last_message_timestamp_unix,
            is_group,
            member_count: r.member_count.max(0) as u32,
            service: r.handle_service,
            participants,
            last_sender,
            preview,
            search_hint,
            acknowledged: r.acknowledged,
        });
    }
    // Demote acknowledged rows below open ones. The sort is stable,
    // so both partitions keep their newest-first order from SQL.
    out.sort_by_key(|r| r.acknowledged);
    out
}

/// Bare 5-6 digit senders (OTP/marketing short codes like 22395).
/// Real numbers are 10+ digits and usually carry a `+`; emails have
/// an `@`. See `config::FILTER_SHORT_CODES`.
pub fn is_short_code(handle: &str) -> bool {
    let t = handle.trim();
    (config::SHORT_CODE_DIGITS_MIN..=config::SHORT_CODE_DIGITS_MAX).contains(&t.len())
        && !t.is_empty()
        && t.chars().all(|c| c.is_ascii_digit())
}

/// Search text for an unnamed group: first names of up to three
/// participants, space-separated. No commas or "+N" - Messages search
/// tokenizes on words and those extras kill the match.
fn search_hint_for_group(participants: &[String]) -> String {
    participants
        .iter()
        .take(3)
        .map(|p| first_name(p))
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolve_one(handle: &str, dir: &ContactDirectory) -> String {
    dir.lookup(handle)
        .map(str::to_string)
        .unwrap_or_else(|| handle.to_string())
}

/// "Mom" / "Mom, Dad" / "Mom, Dad, Sis" / "Mom, Dad +2" depending on
/// participant count. Uses first names only so groups stay readable.
fn format_group_name(participants: &[String]) -> String {
    let firsts: Vec<&str> = participants.iter().map(|p| first_name(p)).collect();
    match firsts.len() {
        0 => "Group".to_string(),
        1 => firsts[0].to_string(),
        2 => format!("{}, {}", firsts[0], firsts[1]),
        3 => format!("{}, {}, {}", firsts[0], firsts[1], firsts[2]),
        n => format!("{}, {} +{}", firsts[0], firsts[1], n - 2),
    }
}

fn first_name(full: &str) -> &str {
    full.split_whitespace().next().unwrap_or(full)
}

/// Normalize whitespace and bound the length of a preview string.
/// Returns `None` if there's nothing meaningful left after cleaning.
pub fn clean_preview(raw: &str) -> Option<String> {
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let chars: Vec<char> = collapsed.chars().collect();
    if chars.len() <= config::PREVIEW_CHARS {
        return Some(collapsed);
    }
    let mut out: String = chars.into_iter().take(config::PREVIEW_CHARS).collect();
    out.push('…');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_code_detection() {
        assert!(is_short_code("22395"));
        assert!(is_short_code("893746"));
        assert!(!is_short_code("+14155550123"));
        assert!(!is_short_code("14155550123"));
        assert!(!is_short_code("911")); // too short to be a short code
        assert!(!is_short_code("user@example.com"));
        assert!(!is_short_code(""));
    }

    #[test]
    fn first_name_handles_full_name() {
        assert_eq!(first_name("Alice Smith"), "Alice");
        assert_eq!(first_name("Bob"), "Bob");
        assert_eq!(first_name("user@example.com"), "user@example.com");
        assert_eq!(first_name(""), "");
    }

    #[test]
    fn clean_preview_collapses_and_truncates() {
        assert_eq!(clean_preview("hi"), Some("hi".to_string()));
        assert_eq!(
            clean_preview("  hi   there  "),
            Some("hi there".to_string())
        );
        assert_eq!(
            clean_preview("line one\nline two"),
            Some("line one line two".to_string())
        );
        assert_eq!(clean_preview(""), None);
        assert_eq!(clean_preview("   \n  "), None);
        let long: String = "x".repeat(config::PREVIEW_CHARS + 50);
        let result = clean_preview(&long).unwrap();
        assert_eq!(result.chars().count(), config::PREVIEW_CHARS + 1);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn format_group_name_truncates() {
        assert_eq!(format_group_name(&[]), "Group");
        assert_eq!(format_group_name(&["Alice".into()]), "Alice");
        assert_eq!(
            format_group_name(&["Alice".into(), "Bob".into()]),
            "Alice, Bob"
        );
        assert_eq!(
            format_group_name(&["Alice".into(), "Bob".into(), "Carol".into()]),
            "Alice, Bob, Carol"
        );
        assert_eq!(
            format_group_name(&[
                "Alice".into(),
                "Bob".into(),
                "Carol".into(),
                "Dan".into(),
                "Eve".into(),
            ]),
            "Alice, Bob +3"
        );
    }
}
