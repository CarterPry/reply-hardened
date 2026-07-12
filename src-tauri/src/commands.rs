use crate::contacts::{self, ContactDirectory};
use crate::error::AppResult;
use crate::pending::{self, Direction};
use crate::state::{MutedChat, StateStore};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};

/// Hide rows whose last message is older than this. Anything past
/// three months is effectively dead — surfacing it creates noise,
/// not signal.
const STALE_AFTER_SECS: i64 = 90 * 86_400;

/// In "waiting on them" mode, only surface threads where your
/// unanswered message has sat for at least this long. A text sent an
/// hour ago isn't a follow-up candidate yet.
const WAITING_MIN_AGE_SECS: i64 = 3 * 86_400;

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
    /// What to type into Messages' search when opening a group chat.
    /// The group's real display name when it has one; otherwise
    /// participant first names space-separated — unlike the display
    /// label ("Mom, Dad +2"), that's something Messages search can
    /// actually match.
    pub search_hint: String,
}

/// Fetch the reply list for a mode: "owed" (they messaged last — you
/// owe a reply) or "waiting" (you messaged last ≥3 days ago — you're
/// waiting on them). The tray count tracks the owed list only.
#[tauri::command]
pub fn get_replies(
    app: AppHandle,
    state: State<'_, StateStore>,
    mode: String,
) -> AppResult<Vec<PendingReply>> {
    let direction = if mode == "waiting" {
        Direction::Waiting
    } else {
        Direction::Owed
    };
    let result = replies_inner(&state, direction)?;
    if direction == Direction::Owed {
        update_tray_count(&app, result.len());
    }
    Ok(result)
}

fn update_tray_count(app: &AppHandle, count: usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Skip the redraw when nothing changed — rewriting the tray title
    // every 30s poll makes the menu bar item flicker.
    static LAST: AtomicUsize = AtomicUsize::new(usize::MAX);
    if LAST.swap(count, Ordering::Relaxed) == count {
        return;
    }
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let title = if count == 0 {
        None
    } else {
        Some(count.to_string())
    };
    if let Err(e) = tray.set_title(title) {
        log::warn!("tray set_title failed: {e}");
    }
}

#[tauri::command]
pub fn mark_complete(state: State<'_, StateStore>, last_message_guid: String) -> AppResult<()> {
    state.mark_complete(&last_message_guid)
}

/// Hide a chat from both lists until `until` (unix seconds).
#[tauri::command]
pub fn snooze_chat(state: State<'_, StateStore>, chat_guid: String, until: i64) -> AppResult<()> {
    state.snooze_chat(&chat_guid, until)
}

/// Permanently hide a chat. `name` is captured for the unmute UI.
#[tauri::command]
pub fn mute_chat(state: State<'_, StateStore>, chat_guid: String, name: String) -> AppResult<()> {
    state.mute_chat(&chat_guid, &name)
}

#[tauri::command]
pub fn unmute_chat(state: State<'_, StateStore>, chat_guid: String) -> AppResult<()> {
    state.unmute_chat(&chat_guid)
}

#[tauri::command]
pub fn list_muted(state: State<'_, StateStore>) -> AppResult<Vec<MutedChat>> {
    state.muted_chats()
}

#[tauri::command]
pub fn open_fda_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .status()
        .map(|_| ())
        .map_err(|e| format!("opening FDA settings pane: {e}"))
}

#[tauri::command]
pub fn open_accessibility_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status()
        .map(|_| ())
        .map_err(|e| format!("opening Accessibility settings pane: {e}"))
}

#[tauri::command]
pub fn open_automation_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation")
        .status()
        .map(|_| ())
        .map_err(|e| format!("opening Automation settings pane: {e}"))
}

/// Open the conversation in Messages.app. 1:1 chats route through the
/// `imessage://` URL scheme keyed by the contact's handle. The URL
/// scheme cannot address an existing group (it always opens a draft),
/// so for groups we activate Messages, drive the sidebar search via
/// keystrokes (⌘F + name + Return), and let Messages select the match.
///
/// The keystroke path needs two macOS permissions, each with its own
/// failure signature in osascript's stderr:
///   Automation (Apple Events)  — error -1743 / "Not authorized"
///   Accessibility (keystrokes) — error 1002 / "assistive access"
/// Both are mapped to structured errors the frontend turns into a
/// banner pointing at the right Settings pane. Running the script
/// directly (no pre-probe) lets macOS show its own grant prompt on
/// first use.
#[tauri::command]
pub fn open_chat(
    handle: String,
    is_group: bool,
    chat_guid: String,
    name: String,
) -> Result<(), String> {
    let _ = chat_guid; // retained on the API for future use (e.g. exact-match selection)

    if !is_group {
        let url = format!("imessage://{handle}");
        return std::process::Command::new("open")
            .arg(&url)
            .status()
            .map(|_| ())
            .map_err(|e| format!("opening chat: {e}"));
    }

    let safe_name = name.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "Messages" to activate
delay 0.4
tell application "System Events"
    tell process "Messages"
        keystroke "f" using {{command down}}
        delay 0.25
        keystroke "{safe_name}"
        delay 0.6
        key code 36
    end tell
end tell"#
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("opening group chat: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("assistive") || stderr.contains("(1002)") {
        return Err("accessibility_denied".to_string());
    }
    if stderr.contains("-1743") || stderr.contains("Not authorized") {
        return Err("automation_denied".to_string());
    }
    Err(format!("opening group chat: {}", stderr.trim()))
}

fn replies_inner(state: &StateStore, direction: Direction) -> AppResult<Vec<PendingReply>> {
    let results = pending::read_pending_replies(direction)?;
    let dir = contacts::load_cached();
    let completed = state.completed_guids()?;
    let snoozed = state.snoozed_until()?;
    let muted: std::collections::HashSet<String> = state
        .muted_chats()?
        .into_iter()
        .map(|m| m.chat_guid)
        .collect();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cutoff = now - STALE_AFTER_SECS;
    // In waiting mode a thread only counts once it's gone quiet.
    let newest_allowed = match direction {
        Direction::Owed => i64::MAX,
        Direction::Waiting => now - WAITING_MIN_AGE_SECS,
    };

    let mut out = Vec::with_capacity(results.rows.len());
    for r in results.rows {
        if completed.contains(&r.last_message_guid) {
            continue;
        }
        if r.last_message_timestamp_unix < cutoff || r.last_message_timestamp_unix > newest_allowed
        {
            continue;
        }
        if muted.contains(&r.chat_guid) || snoozed.contains_key(&r.chat_guid) {
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
            // For groups, "handle" is informational only — open_chat
            // routes through the search hint for these.
            let handle = r.chat_identifier.clone();
            (name, handle, hint)
        } else {
            let h = r
                .handle_id_str
                .clone()
                .unwrap_or_else(|| r.chat_identifier.clone());
            let n = resolve_one(&h, &dir);
            let hint = n.clone();
            (n, h, hint)
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
            search_hint,
        });
    }
    Ok(out)
}

/// Search text for an unnamed group: first names of up to three
/// participants, space-separated. No commas or "+N" — Messages search
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
