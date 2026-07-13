use crate::contacts;
use crate::error::AppResult;
use crate::pending::{self, Direction};
use crate::replies::{self, AssembleCtx, PendingReply};
use crate::state::{DoneItem, MutedChat, StateStore, UndoOutcome};
use crate::unknown::{self, UnknownSender};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};

/// Fetch the reply list for a mode: "owed" (they messaged last - you
/// owe a reply) or "waiting" (you messaged last ≥3 days ago - you're
/// waiting on them). The tray count tracks open owed threads only -
/// acknowledged rows are visible in the list but don't count.
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
        update_tray_count(&app, replies::open_count(&result));
    }
    Ok(result)
}

fn replies_inner(state: &StateStore, direction: Direction) -> AppResult<Vec<PendingReply>> {
    let results = pending::read_pending_replies(direction)?;
    // chat.db was just readable, so the legacy done-state migration
    // (which needs it to map message guids → chats) can run. No-ops
    // once the legacy table is gone.
    match state.migrate_legacy_completed(&pending::chat_db_path()) {
        Ok(n) if n > 0 => log::info!("migrated {n} legacy done markers"),
        Ok(_) => {}
        Err(e) => log::warn!("legacy done migration failed, will retry: {e}"),
    }
    let dir = contacts::load_cached();
    let done = state.done_map()?;
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

    let ctx = AssembleCtx {
        dir: &dir,
        done: &done,
        snoozed: &snoozed,
        muted: &muted,
        now,
    };
    Ok(replies::assemble(results, direction, &ctx))
}

fn update_tray_count(app: &AppHandle, count: usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Skip the redraw when nothing changed - rewriting the tray title
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

/// People messaging you who aren't in Contacts: 1:1 threads with an
/// unknown handle, newest activity first. Shares the done/snooze/mute
/// state with the reply lists.
#[tauri::command]
pub fn get_unknown_senders(state: State<'_, StateStore>) -> AppResult<Vec<UnknownSender>> {
    let rows = unknown::read_unknown_senders(&pending::chat_db_path())?;
    let dir = contacts::load_cached();
    let done = state.done_map()?;
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
    Ok(unknown::assemble_unknown(
        rows, &dir, &done, &snoozed, &muted, now,
    ))
}

/// Hand a handle to Contacts as a prefilled new-contact card. macOS has
/// no URL scheme for "add contact", so we write a one-field vCard to a
/// temp file and `open` it - Contacts shows its import/add flow.
#[tauri::command]
pub fn add_to_contacts(handle: String) -> Result<(), String> {
    let h = handle.trim();
    if h.is_empty() {
        return Err("no handle to add".to_string());
    }
    let field = if h.contains('@') {
        format!("EMAIL;TYPE=INTERNET:{h}")
    } else {
        format!("TEL;TYPE=CELL:{h}")
    };
    let vcf = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{h}\r\n{field}\r\nEND:VCARD\r\n");
    let path = std::env::temp_dir().join("repli-new-contact.vcf");
    std::fs::write(&path, vcf).map_err(|e| format!("writing vcard: {e}"))?;
    std::process::Command::new("open")
        .arg(&path)
        .status()
        .map(|_| ())
        .map_err(|e| format!("opening vcard: {e}"))
}

/// Mark chats done. Always takes a batch (a single row is a batch of
/// one) so a bulk action lands as ONE undo entry.
#[tauri::command]
pub fn mark_done(state: State<'_, StateStore>, items: Vec<DoneItem>) -> AppResult<()> {
    state.mark_done(&items)
}

/// Hide chats from both lists until `until` (unix seconds).
#[tauri::command]
pub fn snooze_chats(
    state: State<'_, StateStore>,
    chat_guids: Vec<String>,
    until: i64,
) -> AppResult<()> {
    state.snooze_chats(&chat_guids, until)
}

/// Permanently hide chats. Names are captured for the unmute UI.
#[tauri::command]
pub fn mute_chats(state: State<'_, StateStore>, chats: Vec<MutedChat>) -> AppResult<()> {
    state.mute_chats(&chats)
}

/// Reverse the most recent triage action (single or bulk). Returns
/// what was undone so the UI can describe it, or None if the log is
/// empty.
#[tauri::command]
pub fn undo_last(state: State<'_, StateStore>) -> AppResult<Option<UndoOutcome>> {
    state.undo_last()
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
///   Automation (Apple Events)  - error -1743 / "Not authorized"
///   Accessibility (keystrokes) - error 1002 / "assistive access"
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
        if handle.trim().is_empty() {
            return Err("no handle for this chat".to_string());
        }
        let url = format!("imessage://{handle}");
        return std::process::Command::new("open")
            .arg(&url)
            .status()
            .map(|_| ())
            .map_err(|e| format!("opening chat: {e}"));
    }

    // A group with no display name and no resolvable participants has
    // nothing to type into Messages' search - typing nothing and
    // hitting Return would just select whatever chat was already
    // highlighted. Fail loudly instead of dead-tapping.
    if name.trim().is_empty() {
        return Err("no searchable name for this group".to_string());
    }

    let safe_name = name.replace('\\', "\\\\").replace('"', "\\\"");
    // `reopen` restores a window when Messages is running windowless -
    // otherwise ⌘F lands nowhere.
    let script = format!(
        r#"tell application "Messages" to reopen
tell application "Messages" to activate
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
