use crate::config;
use crate::contacts;
use crate::contacts::ContactDirectory;
use crate::error::AppResult;
use crate::pending::{self, Direction};
use crate::replies::{self, AssembleCtx, Filters, PendingReply};
use crate::signal;
use crate::sources::{Platform, SourceStatus};
use crate::state::{DoneItem, MutedChat, StateStore, UndoOutcome};
use crate::unknown::{self, UnknownSender};
use crate::whatsapp;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};

/// Outcome of the most recent poll, per source. Written by
/// `replies_inner`, read by `get_sources` - so the UI can say "Signal
/// is not being read" instead of implying the list is complete.
#[derive(Default)]
pub struct SourceStatusStore(Mutex<Vec<SourceStatus>>);

impl SourceStatusStore {
    fn set(&self, statuses: Vec<SourceStatus>) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = statuses;
        }
    }

    fn get(&self) -> Vec<SourceStatus> {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// The shared inputs every source needs: resolved contacts, the user's
/// triage state, and "now".
struct ReadContext {
    dir: ContactDirectory,
    done: HashMap<String, i64>,
    snoozed: HashMap<String, i64>,
    muted: HashSet<String>,
    now: i64,
}

impl ReadContext {
    fn load(state: &StateStore) -> AppResult<Self> {
        Ok(Self {
            dir: contacts::load_cached(),
            done: state.done_map()?,
            snoozed: state.snoozed_until()?,
            muted: state
                .muted_chats()?
                .into_iter()
                .map(|m| m.chat_guid)
                .collect(),
            now: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        })
    }

    fn assemble_ctx(&self) -> AssembleCtx<'_> {
        AssembleCtx {
            dir: &self.dir,
            done: &self.done,
            snoozed: &self.snoozed,
            muted: &self.muted,
            now: self.now,
        }
    }

    fn filters(&self, direction: Direction) -> Filters<'_> {
        Filters::new(&self.assemble_ctx(), direction)
    }
}

/// Newest first, with acknowledged rows demoted below open ones -
/// the same order the single-source version produced, now applied
/// across the merged list.
fn sort_merged(rows: &mut [PendingReply]) {
    rows.sort_by(|a, b| {
        a.acknowledged
            .cmp(&b.acknowledged)
            .then(b.timestamp.cmp(&a.timestamp))
    });
}

/// Classify a non-iMessage read failure for the status footer.
fn status_for_app_error(platform: Platform, e: &crate::error::AppError) -> SourceStatus {
    use crate::error::AppError;
    match e {
        AppError::NotFound { .. } => SourceStatus::absent(
            platform,
            format!("{} isn't set up on this Mac", platform.label()),
        ),
        AppError::PermissionDenied { .. } => SourceStatus::blocked(
            platform,
            format!(
                "Can't read {}'s database - grant Full Disk Access",
                platform.label()
            ),
        ),
        other => SourceStatus::failed(platform, other.to_string()),
    }
}

fn status_for_signal_error(e: &signal::SignalError) -> SourceStatus {
    if e.is_absent() {
        SourceStatus::absent(Platform::Signal, e.detail())
    } else if e.is_blocked() {
        SourceStatus::blocked(Platform::Signal, e.detail())
    } else {
        SourceStatus::failed(Platform::Signal, e.detail())
    }
}

/// Fetch the reply list for a mode: "owed" (they messaged last - you
/// owe a reply) or "waiting" (you messaged last ≥3 days ago - you're
/// waiting on them). The tray count tracks open owed threads only -
/// acknowledged rows are visible in the list but don't count.
#[tauri::command]
pub fn get_replies(
    app: AppHandle,
    state: State<'_, StateStore>,
    statuses: State<'_, SourceStatusStore>,
    mode: String,
) -> AppResult<Vec<PendingReply>> {
    let direction = if mode == "waiting" {
        Direction::Waiting
    } else {
        Direction::Owed
    };
    let result = replies_inner(&state, &statuses, direction)?;
    if direction == Direction::Owed {
        update_tray_count(&app, replies::open_count(&result));
    }
    Ok(result)
}

/// Per-source status from the last poll. iMessage is always present in
/// the list; the others report why they are silent when they are.
#[tauri::command]
pub fn get_sources(statuses: State<'_, SourceStatusStore>) -> Vec<SourceStatus> {
    statuses.get()
}

/// Read every enabled source and merge.
///
/// iMessage is load-bearing: if its read fails the whole command fails,
/// which is what drives the Full Disk Access prompt. WhatsApp and
/// Signal are additive - a missing app, a denied keychain prompt or a
/// schema change degrades that one source to a status line and leaves
/// the rest of the list intact.
fn replies_inner(
    state: &StateStore,
    statuses: &SourceStatusStore,
    direction: Direction,
) -> AppResult<Vec<PendingReply>> {
    let results = pending::read_pending_replies(direction)?;
    // chat.db was just readable, so the legacy done-state migration
    // (which needs it to map message guids → chats) can run. No-ops
    // once the legacy table is gone.
    match state.migrate_legacy_completed(&pending::chat_db_path()) {
        Ok(n) if n > 0 => log::info!("migrated {n} legacy done markers"),
        Ok(_) => {}
        Err(e) => log::warn!("legacy done migration failed, will retry: {e}"),
    }
    let ctx = ReadContext::load(state)?;
    let mut out = replies::assemble(results, direction, &ctx.assemble_ctx());
    let mut source_status = vec![SourceStatus::ready(Platform::IMessage)];

    if config::ENABLE_WHATSAPP {
        match whatsapp::read_pending(direction, &ctx.dir, &ctx.filters(direction)) {
            Ok(rows) => {
                out.extend(rows);
                source_status.push(SourceStatus::ready(Platform::WhatsApp));
            }
            Err(e) => {
                log::warn!("whatsapp source unavailable: {e}");
                source_status.push(status_for_app_error(Platform::WhatsApp, &e));
            }
        }
    }

    if config::ENABLE_SIGNAL {
        match signal::read_pending(direction, &ctx.dir, &ctx.filters(direction)) {
            Ok(rows) => {
                out.extend(rows);
                source_status.push(SourceStatus::ready(Platform::Signal));
            }
            Err(e) => {
                log::warn!("signal source unavailable: {}", e.detail());
                source_status.push(status_for_signal_error(&e));
            }
        }
    }

    statuses.set(source_status);
    sort_merged(&mut out);
    Ok(out)
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
    let ctx = ReadContext::load(&state)?;
    let mut out =
        unknown::assemble_unknown(rows, &ctx.dir, &ctx.done, &ctx.snoozed, &ctx.muted, ctx.now);
    // The age window is all that applies here, so either direction's
    // filters would do; Owed leaves `newest_allowed` unbounded.
    let filters = ctx.filters(Direction::Owed);

    if config::ENABLE_WHATSAPP {
        match whatsapp::read_unknown_at(&whatsapp::db_path(), &ctx.dir, &filters) {
            Ok(rows) => out.extend(rows),
            Err(e) => log::warn!("whatsapp unknown senders unavailable: {e}"),
        }
    }
    if config::ENABLE_SIGNAL {
        match signal::read_unknown(&ctx.dir, &filters) {
            Ok(rows) => out.extend(rows),
            Err(e) => log::warn!("signal unknown senders unavailable: {}", e.detail()),
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.last_date));
    Ok(out)
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

/// Open the conversation in the app that owns it.
///
/// Only two conversations types can be addressed by URL:
///   iMessage 1:1  - `imessage://<handle>`
///   WhatsApp 1:1  - `whatsapp://send?phone=<digits>`
/// Neither scheme can target an existing *group* (iMessage opens a
/// draft instead), and Signal Desktop publishes no conversation URL at
/// all. For everything else we activate the app, drive its search box
/// with keystrokes (⌘F + name + Return), and let it select the match.
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
    platform: Option<String>,
    handle: String,
    is_group: bool,
    chat_guid: String,
    name: String,
) -> Result<(), String> {
    let _ = chat_guid; // retained on the API for future use (e.g. exact-match selection)
    let platform = platform.unwrap_or_else(|| "imessage".to_string());

    match platform.as_str() {
        "whatsapp" => {
            // WhatsApp's scheme takes bare digits. `@lid` chats have no
            // number, so those fall through to search like groups do.
            let digits: String = handle.chars().filter(|c| c.is_ascii_digit()).collect();
            if !is_group && !digits.is_empty() {
                return open_url(&format!("whatsapp://send?phone={digits}"));
            }
            app_search(Platform::WhatsApp, &name)
        }
        "signal" => app_search(Platform::Signal, &name),
        _ => {
            if !is_group {
                if handle.trim().is_empty() {
                    return Err("no handle for this chat".to_string());
                }
                return open_url(&format!("imessage://{handle}"));
            }
            app_search(Platform::IMessage, &name)
        }
    }
}

fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(url)
        .status()
        .map(|_| ())
        .map_err(|e| format!("opening chat: {e}"))
}

/// The AppleScript that drives one app's search box. Split out from
/// `app_search` so its shape is testable without keystroking anything
/// at a real application.
fn search_script(platform: Platform, query: &str) -> String {
    let app = platform.app_name();
    // Messages filters its sidebar as you type; WhatsApp (Catalyst)
    // and Signal (Electron) both need noticeably longer before the
    // first result is selectable, and pressing Return early just
    // re-opens whatever thread was already showing.
    let settle = match platform {
        Platform::IMessage => "0.6",
        Platform::WhatsApp | Platform::Signal => "1.0",
    };
    // In Messages, Return on the focused search field opens the top
    // hit. WhatsApp's Catalyst search and Signal's Electron one keep
    // focus in the text field, where Return does nothing useful - the
    // selection has to be moved into the result list first (key code
    // 125 = Down). Harmless where Return alone would have worked:
    // Down lands on the first result either way.
    let select_first = match platform {
        Platform::IMessage => "",
        Platform::WhatsApp | Platform::Signal => "        key code 125\n        delay 0.2\n",
    };
    // AppleScript string literals take backslash and quote escapes;
    // a chat named `say "hi"` must not end the literal early.
    let safe_query = query.replace('\\', "\\\\").replace('"', "\\\"");
    // `reopen` restores a window when the app is running windowless -
    // otherwise ⌘F lands nowhere.
    format!(
        r#"tell application "{app}" to reopen
tell application "{app}" to activate
delay 0.4
tell application "System Events"
    tell process "{app}"
        keystroke "f" using {{command down}}
        delay 0.25
        keystroke "{safe_query}"
        delay {settle}
{select_first}        key code 36
    end tell
end tell"#
    )
}

/// Activate `platform`'s app and type `query` into its search box.
///
/// Best-effort by construction: it needs Accessibility + Automation,
/// and the app picks the match, so an ambiguous name can land on the
/// wrong thread. That is still better than the alternative, which is
/// no way to open a group at all.
fn app_search(platform: Platform, query: &str) -> Result<(), String> {
    // A thread with no display name and no resolvable participants has
    // nothing to type - typing nothing and hitting Return would just
    // select whatever was already highlighted. Fail loudly instead of
    // dead-tapping.
    if query.trim().is_empty() {
        return Err("no searchable name for this chat".to_string());
    }
    let app = platform.app_name();
    let script = search_script(platform, query);
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("opening chat in {app}: {e}"))?;
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
    Err(format!("opening chat in {app}: {}", stderr.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imessage_search_presses_return_without_moving_the_selection() {
        let script = search_script(Platform::IMessage, "Mom Dad");
        assert!(script.contains(r#"tell process "Messages""#));
        assert!(script.contains(r#"keystroke "Mom Dad""#));
        assert!(script.contains("key code 36"));
        // No Down: Messages opens the top hit straight from the field.
        assert!(!script.contains("key code 125"));
    }

    #[test]
    fn whatsapp_and_signal_select_the_first_result_before_return() {
        for (platform, app) in [
            (Platform::WhatsApp, "WhatsApp"),
            (Platform::Signal, "Signal"),
        ] {
            let script = search_script(platform, "David Gelberg");
            assert!(script.contains(&format!(r#"tell process "{app}""#)));
            let down = script.find("key code 125").expect("selects first result");
            let ret = script.find("key code 36").expect("presses return");
            assert!(down < ret, "{app}: Down must come before Return");
            // Slower apps get a longer settle before the selection moves.
            assert!(script.contains("delay 1.0"));
        }
    }

    #[test]
    fn a_quoted_chat_name_cannot_break_out_of_the_applescript_literal() {
        let script = search_script(Platform::Signal, r#"say "hi" \ now"#);
        assert!(script.contains(r#"keystroke "say \"hi\" \\ now""#));
        // The literal still closes exactly once per keystroke line.
        let line = script
            .lines()
            .find(|l| l.trim_start().starts_with("keystroke \"say"))
            .expect("keystroke line");
        assert!(line.trim_end().ends_with('"'));
    }
}
