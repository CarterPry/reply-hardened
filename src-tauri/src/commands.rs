use crate::contacts::{self, ContactDirectory};
use crate::error::AppResult;
use crate::pending;
use crate::state::StateStore;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};

/// Hide rows whose last inbound message is older than this. Anything
/// past three months is effectively dead — surfacing it as "owes a
/// reply" creates noise, not signal.
const STALE_AFTER_SECS: i64 = 90 * 86_400;

/// Cap the preview string at this many characters before sending to
/// the frontend. The UI truncates visually too, but bounding payload
/// size keeps things sane for messages with very long attachments.
const PREVIEW_CHARS: usize = 140;

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
    /// First name of the latest sender — surfaced as the meta line in
    /// group rows so you can see who specifically owes a reply.
    pub last_sender: Option<String>,
    /// One-line preview of the latest message, truncated to
    /// `PREVIEW_CHARS`. Whitespace normalized (newlines → spaces).
    /// `None` when the row was a sticker, attachment-only, or the
    /// attributedBody parse failed.
    pub preview: Option<String>,
}

#[tauri::command]
pub fn get_pending_replies(
    app: AppHandle,
    state: State<'_, StateStore>,
) -> AppResult<Vec<PendingReply>> {
    let result = pending_replies_inner(&state)?;
    update_badge(&app, result.len());
    Ok(result)
}

/// Same logic as `get_pending_replies`. Exposed separately to match
/// the spec — the frontend uses this on focus events.
#[tauri::command]
pub fn refresh(app: AppHandle, state: State<'_, StateStore>) -> AppResult<Vec<PendingReply>> {
    let result = pending_replies_inner(&state)?;
    update_badge(&app, result.len());
    Ok(result)
}

fn update_badge(app: &AppHandle, count: usize) {
    let value = if count == 0 { None } else { Some(count as i64) };
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    if let Err(e) = win.set_badge_count(value) {
        log::warn!("set_badge_count failed: {e}");
    }
}

#[tauri::command]
pub fn mark_complete(state: State<'_, StateStore>, last_message_guid: String) -> AppResult<()> {
    state.mark_complete(&last_message_guid)
}

#[tauri::command]
pub fn open_fda_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .status()
        .map(|_| ())
        .map_err(|e| format!("opening FDA settings pane: {e}"))
}

/// Open the conversation in Messages.app. 1:1 chats route through the
/// `imessage://` URL scheme keyed by the contact's handle. Group chats
/// have no first-class URL scheme, so we activate Messages and try
/// AppleScript to surface the right thread by chat GUID — modern
/// macOS has restricted Messages' scriptability, so this is best-effort.
#[tauri::command]
pub fn open_chat(handle: String, is_group: bool, chat_guid: String) -> Result<(), String> {
    if !is_group {
        let url = format!("imessage://{handle}");
        return std::process::Command::new("open")
            .arg(&url)
            .status()
            .map(|_| ())
            .map_err(|e| format!("opening chat: {e}"));
    }

    // Group: bring Messages forward, then attempt scripted nav by guid.
    let _ = std::process::Command::new("open")
        .args(["-a", "Messages"])
        .status();
    let safe_guid = chat_guid.replace('"', "\\\"");
    let script = format!(
        r#"tell application "Messages"
    try
        set theChat to first chat whose id is "{safe_guid}"
        set selected chat to theChat
    end try
end tell"#
    );
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status();
    Ok(())
}

fn pending_replies_inner(state: &StateStore) -> AppResult<Vec<PendingReply>> {
    let results = pending::read_pending_replies()?;
    let dir = contacts::load();
    let completed = state.completed_guids()?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cutoff = now - STALE_AFTER_SECS;

    let mut out = Vec::with_capacity(results.rows.len());
    for r in results.rows {
        if completed.contains(&r.last_message_guid) {
            continue;
        }
        if r.last_message_timestamp_unix < cutoff {
            continue;
        }
        let is_group = r.member_count > 1;

        let participants: Vec<String> = if is_group {
            results
                .participants_by_chat
                .get(&r.chat_rowid)
                .map(|raw| raw.iter().map(|h| resolve_one(h, &dir)).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let (name, handle) = if is_group {
            let display = r
                .display_name
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());
            let name = match display {
                Some(d) => d.to_string(),
                None => format_group_name(&participants),
            };
            // For groups, "handle" is informational only — open_chat
            // routes through chat_guid for these.
            let handle = r.chat_identifier.clone();
            (name, handle)
        } else {
            let h = r
                .handle_id_str
                .clone()
                .unwrap_or_else(|| r.chat_identifier.clone());
            let n = resolve_one(&h, &dir);
            (n, h)
        };

        let last_sender = if is_group {
            r.handle_id_str
                .as_deref()
                .map(|h| first_name(&resolve_one(h, &dir)).to_string())
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
        });
    }
    Ok(out)
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
fn clean_preview(raw: &str) -> Option<String> {
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let chars: Vec<char> = collapsed.chars().collect();
    if chars.len() <= PREVIEW_CHARS {
        return Some(collapsed);
    }
    let mut out: String = chars.into_iter().take(PREVIEW_CHARS).collect();
    out.push('…');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let long: String = "x".repeat(PREVIEW_CHARS + 50);
        let result = clean_preview(&long).unwrap();
        assert_eq!(result.chars().count(), PREVIEW_CHARS + 1);
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
