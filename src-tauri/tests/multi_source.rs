//! Integration tests for the WhatsApp and Signal sources: the real SQL
//! and the real assembly rules, run against synthetic databases built
//! by `fixture::whatsapp` / `fixture::signal`. No test touches a real
//! message store.
//!
//!   w* - WhatsApp source
//!   s* - Signal source
//!   x* - cross-source invariants
//!
//! Run with: cargo test --manifest-path src-tauri/Cargo.toml

mod fixture;

use fixture::signal::{SignalFixture, ME_CONVERSATION};
use fixture::whatsapp::{WaFixture, MESSAGE_TYPE_SYSTEM};
use reply_lib::contacts::ContactDirectory;
use reply_lib::pending::Direction;
use reply_lib::replies::{AssembleCtx, Filters, PendingReply};
use reply_lib::sources::Platform;
use reply_lib::unknown::UnknownSender;
use reply_lib::{signal, whatsapp};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

const DAY: i64 = 86_400;
/// Older than `WAITING_MIN_AGE_SECS` (3 days), so an outbound message
/// of this age is eligible for the "waiting" list.
const STALE_ENOUGH_TO_WAIT: i64 = 4 * DAY;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs() as i64
}

/// The user's local triage state, empty unless a test fills it.
#[derive(Default)]
struct Local {
    done: HashMap<String, i64>,
    snoozed: HashMap<String, i64>,
    muted: HashSet<String>,
}

fn run_wa(
    fx: &WaFixture,
    direction: Direction,
    dir: &ContactDirectory,
    local: &Local,
) -> Vec<PendingReply> {
    let ctx = AssembleCtx {
        dir,
        done: &local.done,
        snoozed: &local.snoozed,
        muted: &local.muted,
        now: now_unix(),
    };
    let filters = Filters::new(&ctx, direction);
    whatsapp::read_pending_at(&fx.path, direction, dir, &filters).expect("read whatsapp fixture")
}

fn run_wa_unknown(fx: &WaFixture, dir: &ContactDirectory, local: &Local) -> Vec<UnknownSender> {
    let ctx = AssembleCtx {
        dir,
        done: &local.done,
        snoozed: &local.snoozed,
        muted: &local.muted,
        now: now_unix(),
    };
    let filters = Filters::new(&ctx, Direction::Owed);
    whatsapp::read_unknown_at(&fx.path, dir, &filters).expect("read whatsapp unknown senders")
}

fn run_sig(
    fx: &SignalFixture,
    direction: Direction,
    dir: &ContactDirectory,
    local: &Local,
) -> Vec<PendingReply> {
    let ctx = AssembleCtx {
        dir,
        done: &local.done,
        snoozed: &local.snoozed,
        muted: &local.muted,
        now: now_unix(),
    };
    let filters = Filters::new(&ctx, direction);
    // `None`: the fixture is plaintext. Key recovery is covered by the
    // round-trip tests in `safestorage`.
    signal::read_pending_at(&fx.path, None, direction, dir, &filters).expect("read signal fixture")
}

fn run_sig_unknown(
    fx: &SignalFixture,
    dir: &ContactDirectory,
    local: &Local,
) -> Vec<UnknownSender> {
    let ctx = AssembleCtx {
        dir,
        done: &local.done,
        snoozed: &local.snoozed,
        muted: &local.muted,
        now: now_unix(),
    };
    let filters = Filters::new(&ctx, Direction::Owed);
    signal::read_unknown_at(&fx.path, None, dir, &filters).expect("read signal unknown senders")
}

fn names(rows: &[PendingReply]) -> Vec<&str> {
    rows.iter().map(|r| r.name.as_str()).collect()
}

// ---------------------------------------------------------------- w*

/// Their message last → you owe a reply. Yours last → you're waiting.
#[test]
fn w1_direction_split_matches_imessage_semantics() {
    let mut fx = WaFixture::new();
    let them = fx.session("15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.message(them, false, 3600, Some("you around?"));
    let me = fx.session("15552220000@s.whatsapp.net", Some("Ben Cole"));
    fx.message(me, true, STALE_ENOUGH_TO_WAIT, Some("sent you the file"));

    let dir = ContactDirectory::default();
    let local = Local::default();
    assert_eq!(
        names(&run_wa(&fx, Direction::Owed, &dir, &local)),
        ["Ana Diaz"]
    );
    assert_eq!(
        names(&run_wa(&fx, Direction::Waiting, &dir, &local)),
        ["Ben Cole"]
    );
}

/// An outbound message that is only hours old isn't a follow-up
/// candidate yet - the same 3-day floor iMessage uses.
#[test]
fn w2_waiting_respects_the_minimum_age() {
    let mut fx = WaFixture::new();
    let s = fx.session("15553330000@s.whatsapp.net", Some("Cleo"));
    fx.message(s, true, 3600, Some("ping"));
    assert!(run_wa(
        &fx,
        Direction::Waiting,
        &ContactDirectory::default(),
        &Local::default()
    )
    .is_empty());
}

/// "X joined", "subject changed" and friends are not messages: they
/// must never become the last message and mask a real one.
#[test]
fn w3_system_rows_never_win_last_message() {
    let mut fx = WaFixture::new();
    let s = fx.session("1203630@g.us", Some("Trip planning"));
    fx.member(s, "15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.message(s, false, 2 * DAY, Some("who is driving?"));
    // A newer system row. Without the ZMESSAGETYPE filter this would
    // be the last row and, having ZISFROMME = 0, would surface with no
    // text at all.
    fx.message_full(s, false, 60, None, MESSAGE_TYPE_SYSTEM, None, None);

    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].preview.as_deref(), Some("who is driving?"));
}

/// Groups: WhatsApp's own subject is the name, members are the
/// participants, and the last sender shows as a first name.
#[test]
fn w4_group_name_participants_and_sender() {
    let mut fx = WaFixture::new();
    let s = fx.session("1203631@g.us", Some("Climbing crew"));
    let ana = fx.member(s, "15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.member(s, "15552220000@s.whatsapp.net", Some("Ben Cole"));
    fx.message_full(s, false, 3600, Some("gym at 7?"), 0, Some(ana), None);

    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert!(row.is_group);
    assert_eq!(row.name, "Climbing crew");
    assert_eq!(row.member_count, 2);
    assert_eq!(row.participants, vec!["Ana Diaz", "Ben Cole"]);
    assert_eq!(row.last_sender.as_deref(), Some("Ana"));
    // No number for a group: open_chat drives WhatsApp's search instead.
    assert_eq!(row.handle, "");
    assert_eq!(row.search_hint, "Climbing crew");
}

/// An unnamed group falls back to participant first names, exactly as
/// the iMessage path does.
#[test]
fn w5_unnamed_group_is_labeled_from_participants() {
    let mut fx = WaFixture::new();
    let s = fx.session("1203632@g.us", None);
    fx.member(s, "15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.member(s, "15552220000@s.whatsapp.net", Some("Ben Cole"));
    fx.message(s, false, 3600, Some("hi all"));

    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows[0].name, "Ana, Ben");
    assert_eq!(rows[0].search_hint, "Ana Ben");
}

/// The address book wins over WhatsApp's own label, so one person
/// reads the same on every platform.
#[test]
fn w6_address_book_name_beats_whatsapp_name() {
    let mut fx = WaFixture::new();
    let s = fx.session("15551110000@s.whatsapp.net", Some("ana (work)"));
    fx.message(s, false, 3600, Some("call me"));
    let mut dir = ContactDirectory::default();
    dir.insert_phone("+15551110000", "Ana Diaz");

    let rows = run_wa(&fx, Direction::Owed, &dir, &Local::default());
    assert_eq!(rows[0].name, "Ana Diaz");
    assert_eq!(rows[0].handle, "+15551110000");
}

/// `@lid` chats carry no phone number: fall back to WhatsApp's name and
/// leave the handle empty rather than inventing one.
#[test]
fn w7_lid_chat_has_no_handle_but_still_surfaces() {
    let mut fx = WaFixture::new();
    let s = fx.session("88887777666655@lid", Some("Dara Osei"));
    fx.message(s, false, 3600, Some("quick question"));

    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows[0].name, "Dara Osei");
    assert_eq!(rows[0].handle, "");
    assert!(!rows[0].is_group);
}

/// Status broadcasts, channels, and chats the user hid or deleted are
/// not threads anyone owes a reply to.
#[test]
fn w8_feeds_and_buried_chats_are_excluded() {
    let mut fx = WaFixture::new();
    let status = fx.session("status@broadcast", Some("Status"));
    fx.message(status, false, 600, Some("someone posted"));
    let channel = fx.session("12345@newsletter", Some("Some channel"));
    fx.message(channel, false, 600, Some("new post"));
    let hidden = fx.session("15554440000@s.whatsapp.net", Some("Hidden"));
    fx.message(hidden, false, 600, Some("hi"));
    fx.hide_session(hidden);
    let removed = fx.session("15555550000@s.whatsapp.net", Some("Removed"));
    fx.message(removed, false, 600, Some("hi"));
    fx.remove_session(removed);
    let real = fx.session("15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.message(real, false, 600, Some("hi"));

    assert_eq!(
        names(&run_wa(
            &fx,
            Direction::Owed,
            &ContactDirectory::default(),
            &Local::default()
        )),
        ["Ana Diaz"]
    );
}

/// WhatsApp rows are keyed `wa:<jid>`, and the shared done/snooze/mute
/// state applies to them through exactly that key.
#[test]
fn w9_state_keys_are_namespaced_and_honored() {
    let mut fx = WaFixture::new();
    let s = fx.session("15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.message(s, false, 3600, Some("hi"));
    let key = "wa:15551110000@s.whatsapp.net";

    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows[0].chat_guid, key);
    assert_eq!(rows[0].platform, Platform::WhatsApp);

    // Marked done at a date at or after the last message → hidden.
    let mut local = Local::default();
    local.done.insert(key.to_string(), now_unix());
    assert!(run_wa(&fx, Direction::Owed, &ContactDirectory::default(), &local).is_empty());

    // Muted → hidden.
    let mut local = Local::default();
    local.muted.insert(key.to_string());
    assert!(run_wa(&fx, Direction::Owed, &ContactDirectory::default(), &local).is_empty());

    // Snoozed → hidden.
    let mut local = Local::default();
    local.snoozed.insert(key.to_string(), now_unix() + DAY);
    assert!(run_wa(&fx, Direction::Owed, &ContactDirectory::default(), &local).is_empty());
}

/// A newer real message reopens a thread that was marked done.
#[test]
fn w10_new_message_resurfaces_a_done_thread() {
    let mut fx = WaFixture::new();
    let s = fx.session("15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.message(s, false, 3600, Some("latest"));
    let mut local = Local::default();
    // Done as of two hours ago; the message above is newer.
    local.done.insert(
        "wa:15551110000@s.whatsapp.net".to_string(),
        now_unix() - 2 * 3600,
    );
    assert_eq!(
        names(&run_wa(
            &fx,
            Direction::Owed,
            &ContactDirectory::default(),
            &local
        )),
        ["Ana Diaz"]
    );
}

/// WhatsApp stores no reactions anywhere, so no WhatsApp row can be
/// acknowledged. Asserted so a future schema change that adds them
/// shows up as a failing test rather than silently wrong UI.
#[test]
fn w11_rows_are_never_acknowledged() {
    let mut fx = WaFixture::new();
    let s = fx.session("15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.message(s, false, 3600, Some("hi"));
    assert!(run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default()
    )
    .iter()
    .all(|r| !r.acknowledged));
}

/// Media messages have no text: the row still surfaces, with no
/// preview, rather than being dropped.
#[test]
fn w12_media_message_surfaces_without_a_preview() {
    let mut fx = WaFixture::new();
    let s = fx.session("15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.message_full(s, false, 3600, None, 1, None, None);
    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].preview, None);
}

/// Unknown senders: a number with no contact match and inbound
/// messages, summarized. Known contacts and threads you started alone
/// are excluded.
#[test]
fn w13_unknown_senders_are_numbers_with_no_contact() {
    let mut fx = WaFixture::new();
    let stranger = fx.session("15559990000@s.whatsapp.net", Some("+1 555 999 0000"));
    fx.message(stranger, false, 2 * DAY, Some("is this you?"));
    fx.message(stranger, false, DAY, Some("hello?"));
    let known = fx.session("15551110000@s.whatsapp.net", Some("Ana Diaz"));
    fx.message(known, false, DAY, Some("hi"));
    let outbound_only = fx.session("15558880000@s.whatsapp.net", None);
    fx.message(outbound_only, true, DAY, Some("hey"));
    let group = fx.session("1203633@g.us", Some("Some group"));
    fx.message(group, false, DAY, Some("hi all"));

    let mut dir = ContactDirectory::default();
    dir.insert_phone("+15551110000", "Ana Diaz");
    let rows = run_wa_unknown(&fx, &dir, &Local::default());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].handle, "+15559990000");
    assert_eq!(rows[0].message_count, 2);
    assert_eq!(rows[0].platform, Platform::WhatsApp);
    assert!(rows[0].first_date < rows[0].last_date);
}

/// Regression: WhatsApp writes base64 protobuf into its name columns
/// for `@lid` contacts. None of it may reach the UI as a person's
/// name - the profile push-name table wins, and a blob never does.
///
/// The strings here are verbatim from a real store, where this bug
/// rendered a row's meta line as "+CgAQgueA1gY=: Hopping on one second".
#[test]
fn w14_protobuf_blobs_never_become_names() {
    let mut fx = WaFixture::new();
    let s = fx.session("1203634@g.us", Some("Jacob <> David"));
    // The sender: no contact name, a blob in ZFIRSTNAME, and a real
    // name available only in ZWAPROFILEPUSHNAME.
    let jacob = fx.member_raw(s, "120692926361647@lid", None, Some("+CgAQgueA1gY="));
    fx.push_name("120692926361647@lid", "Jacob R");
    // A second member with a blob and NO push name: must fall back to
    // the jid, never to the blob.
    fx.member_raw(s, "215152175652912@lid", None, Some("+CgAQgueA1gY="));
    // ZPUSHNAME on the message is blob too.
    fx.message_full(
        s,
        false,
        3600,
        Some("Hopping on one second"),
        0,
        Some(jacob),
        Some("COi9hdYGIABIAZABAPABAtgC+8rT09"),
    );

    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.last_sender.as_deref(), Some("Jacob"));
    assert_eq!(row.preview.as_deref(), Some("Hopping on one second"));
    for label in row.participants.iter().chain(std::iter::once(&row.name)) {
        assert!(
            !label.contains("CgAQgueA1gY") && !label.contains("COi9hdYG"),
            "blob leaked into a label: {label}"
        );
    }
    // The member with no trustworthy name shows its jid, not a blob.
    assert!(row.participants.iter().any(|p| p == "215152175652912@lid"));
}

/// The address book still outranks WhatsApp's push-name table, so a
/// person reads the same on every platform.
#[test]
fn w15_address_book_outranks_the_push_name_table() {
    let mut fx = WaFixture::new();
    let s = fx.session("15551110000@s.whatsapp.net", Some("+CgAQgueA1gY="));
    fx.push_name("15551110000@s.whatsapp.net", "ana from whatsapp");
    fx.message(s, false, 3600, Some("hi"));

    // No contact: the push name is used, and the blob partner name is
    // ignored.
    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows[0].name, "ana from whatsapp");

    // With a contact, the address book wins.
    let mut dir = ContactDirectory::default();
    dir.insert_phone("+15551110000", "Ana Diaz");
    let rows = run_wa(&fx, Direction::Owed, &dir, &Local::default());
    assert_eq!(rows[0].name, "Ana Diaz");
}

/// WhatsApp's own label for a contact beats the name that contact
/// publishes for themselves - because the label is also the only
/// string WhatsApp's search will match, and search is the only way to
/// open a chat that has no phone number.
///
/// Verbatim from a real store: `215152175652912@lid` is "David
/// Gelberg" to WhatsApp and "And David Says" to himself. Preferring
/// the latter produced a row that typed "And David Says" into
/// WhatsApp's search and got "No results".
#[test]
fn w16_whatsapps_own_label_beats_the_contacts_push_name() {
    let mut fx = WaFixture::new();
    let s = fx.session("215152175652912@lid", Some("David Gelberg"));
    fx.push_name("215152175652912@lid", "And David Says");
    fx.message(s, false, 3600, Some("Sick"));

    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "David Gelberg");
    // The search hint is what gets typed into WhatsApp; it has to be
    // the string WhatsApp itself knows.
    assert_eq!(rows[0].search_hint, "David Gelberg");
}

/// A group member whose own member row carries no name is labeled the
/// way you know them - by their 1:1 chat's label - before falling back
/// to what they call themselves.
#[test]
fn w17_group_member_uses_the_label_you_know_them_by() {
    let mut fx = WaFixture::new();
    // The same person's 1:1 chat, which is where WhatsApp keeps the
    // label it shows for them.
    fx.session("215152175652912@lid", Some("David Gelberg"));
    fx.push_name("215152175652912@lid", "And David Says");

    let group = fx.session("1203635@g.us", Some("Jacob <> David"));
    let david = fx.member_raw(group, "215152175652912@lid", None, None);
    fx.message_full(
        group,
        false,
        3600,
        Some("Curious to hear"),
        0,
        Some(david),
        None,
    );

    let rows = run_wa(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    // "David", not "And".
    assert_eq!(rows[0].last_sender.as_deref(), Some("David"));
    assert!(rows[0].participants.contains(&"David Gelberg".to_string()));
}

// ---------------------------------------------------------------- s*

/// Incoming last → you owe. Outgoing last (and old enough) → waiting.
#[test]
fn s1_direction_split_matches_imessage_semantics() {
    let mut fx = SignalFixture::new();
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    fx.message(&ana, true, 3600, Some("you around?"));
    let ben = fx.private("aci-ben", Some("+15552220000"), Some("Ben Cole"));
    fx.message(&ben, false, STALE_ENOUGH_TO_WAIT, Some("sent it over"));

    let dir = ContactDirectory::default();
    let local = Local::default();
    assert_eq!(
        names(&run_sig(&fx, Direction::Owed, &dir, &local)),
        ["Ana Diaz"]
    );
    assert_eq!(
        names(&run_sig(&fx, Direction::Waiting, &dir, &local)),
        ["Ben Cole"]
    );
}

/// Call history, key changes and group-v2 changes live in `messages`
/// but are not messages: they must never win "last message".
#[test]
fn s2_non_message_rows_never_win_last_message() {
    let mut fx = SignalFixture::new();
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    fx.message(&ana, true, 2 * DAY, Some("call me back"));
    fx.message_typed(&ana, "call-history", 60, None, None);
    fx.message_typed(&ana, "keychange", 30, None, None);
    fx.message_typed(&ana, "profile-change", 10, None, None);

    let rows = run_sig(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].preview.as_deref(), Some("call me back"));
}

/// Your reaction on their last message acknowledges the thread (shown
/// demoted, excluded from the badge) - the Signal equivalent of a
/// tapback.
#[test]
fn s3_my_reaction_acknowledges_an_owed_thread() {
    let mut fx = SignalFixture::new();
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    let msg = fx.message(&ana, true, 3600, Some("made you a playlist"));
    fx.reaction(&ana, &msg, ME_CONVERSATION);

    let rows = run_sig(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].acknowledged);
    assert_eq!(reply_lib::replies::open_count(&rows), 0);
}

/// Their reaction on YOUR last message acknowledges a waiting thread.
#[test]
fn s4_their_reaction_acknowledges_a_waiting_thread() {
    let mut fx = SignalFixture::new();
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    let msg = fx.message(&ana, false, STALE_ENOUGH_TO_WAIT, Some("here you go"));
    fx.reaction(&ana, &msg, &ana);

    let rows = run_sig(
        &fx,
        Direction::Waiting,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].acknowledged);
}

/// A reaction on a question is not an answer: the row stays fully open.
#[test]
fn s5_reaction_on_a_question_keeps_the_thread_open() {
    let mut fx = SignalFixture::new();
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    let msg = fx.message(&ana, true, 3600, Some("are we still on for friday?"));
    fx.reaction(&ana, &msg, ME_CONVERSATION);

    let rows = run_sig(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].acknowledged);
}

/// My reaction must not acknowledge a thread where I'm the one waiting,
/// and theirs must not acknowledge one where I owe - the `fromId`
/// comparison has to be direction-aware.
#[test]
fn s6_reaction_direction_is_not_crossed() {
    let mut fx = SignalFixture::new();
    // I owe Ana; Ana reacted to her own message. Not an ack for me.
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    let m1 = fx.message(&ana, true, 3600, Some("look at this"));
    fx.reaction(&ana, &m1, &ana);
    // Ben owes me; I reacted to my own message. Not an ack from him.
    let ben = fx.private("aci-ben", Some("+15552220000"), Some("Ben Cole"));
    let m2 = fx.message(&ben, false, STALE_ENOUGH_TO_WAIT, Some("my message"));
    fx.reaction(&ben, &m2, ME_CONVERSATION);

    let dir = ContactDirectory::default();
    let local = Local::default();
    let owed = run_sig(&fx, Direction::Owed, &dir, &local);
    assert_eq!(names(&owed), ["Ana Diaz"]);
    assert!(!owed[0].acknowledged);
    let waiting = run_sig(&fx, Direction::Waiting, &dir, &local);
    assert_eq!(names(&waiting), ["Ben Cole"]);
    assert!(!waiting[0].acknowledged);
}

/// Groups: Signal's title is the name, members resolve to names with
/// our own account removed, and the sender resolves from the message's
/// service id.
#[test]
fn s7_group_name_participants_exclude_self() {
    let mut fx = SignalFixture::new();
    fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    fx.private("aci-ben", Some("+15552220000"), Some("Ben Cole"));
    let group = fx.group(Some("Climbing crew"), &["aci-ana", "aci-ben"]);
    fx.message_typed(&group, "incoming", 3600, Some("gym at 7?"), Some("aci-ana"));

    let rows = run_sig(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert!(row.is_group);
    assert_eq!(row.name, "Climbing crew");
    // Us excluded: two other members.
    assert_eq!(row.member_count, 2);
    assert_eq!(row.participants.len(), 2);
    assert!(row.participants.contains(&"Ana Diaz".to_string()));
    assert!(!row.participants.contains(&"Me".to_string()));
    assert_eq!(row.last_sender.as_deref(), Some("Ana"));
}

/// Note to Self is a conversation, but never one you owe a reply to.
/// Archived conversations are excluded too, like `is_archived` chats on
/// iMessage.
#[test]
fn s8_note_to_self_and_archived_are_excluded() {
    let mut fx = SignalFixture::new();
    fx.message(ME_CONVERSATION, true, 3600, Some("remember this"));
    let archived = fx.private("aci-cleo", Some("+15553330000"), Some("Cleo Vance"));
    fx.message(&archived, true, 3600, Some("hi"));
    fx.archive(&archived);
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    fx.message(&ana, true, 3600, Some("hi"));

    assert_eq!(
        names(&run_sig(
            &fx,
            Direction::Owed,
            &ContactDirectory::default(),
            &Local::default()
        )),
        ["Ana Diaz"]
    );
}

/// Signal rows are keyed `sg:<conversationId>` and the shared triage
/// state applies through that key.
#[test]
fn s9_state_keys_are_namespaced_and_honored() {
    let mut fx = SignalFixture::new();
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    fx.message(&ana, true, 3600, Some("hi"));
    let key = format!("sg:{ana}");

    let rows = run_sig(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows[0].chat_guid, key);
    assert_eq!(rows[0].platform, Platform::Signal);

    let mut local = Local::default();
    local.muted.insert(key.clone());
    assert!(run_sig(&fx, Direction::Owed, &ContactDirectory::default(), &local).is_empty());

    let mut local = Local::default();
    local.done.insert(key, now_unix());
    assert!(run_sig(&fx, Direction::Owed, &ContactDirectory::default(), &local).is_empty());
}

/// The address book wins over Signal's profile name.
#[test]
fn s10_address_book_name_beats_profile_name() {
    let mut fx = SignalFixture::new();
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("ana 🧗"));
    fx.message(&ana, true, 3600, Some("hi"));
    let mut dir = ContactDirectory::default();
    dir.insert_phone("+15551110000", "Ana Diaz");

    let rows = run_sig(&fx, Direction::Owed, &dir, &Local::default());
    assert_eq!(rows[0].name, "Ana Diaz");
    assert_eq!(rows[0].handle, "+15551110000");
}

/// Erased ("delete for everyone") messages are not candidates.
#[test]
fn s11_erased_messages_are_ignored() {
    let mut fx = SignalFixture::new();
    let ana = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    fx.message(&ana, true, 2 * DAY, Some("the real last message"));
    let erased = fx.message(&ana, true, 60, None);
    fx.conn
        .execute(
            "UPDATE messages SET isErased = 1 WHERE id = ?1",
            rusqlite::params![erased],
        )
        .expect("mark erased");

    let rows = run_sig(
        &fx,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    assert_eq!(rows[0].preview.as_deref(), Some("the real last message"));
}

/// Unknown senders: a Signal number with no contact match. Known
/// contacts, Note to Self, and ACI-only conversations are excluded.
#[test]
fn s12_unknown_senders_are_numbers_with_no_contact() {
    let mut fx = SignalFixture::new();
    let stranger = fx.private("aci-x", Some("+15559990000"), None);
    fx.message(&stranger, true, 2 * DAY, Some("hi, found you on signal"));
    let known = fx.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    fx.message(&known, true, DAY, Some("hi"));
    let no_number = fx.private("aci-y", None, Some("Someone"));
    fx.message(&no_number, true, DAY, Some("hi"));
    fx.message(ME_CONVERSATION, true, DAY, Some("note"));

    let mut dir = ContactDirectory::default();
    dir.insert_phone("+15551110000", "Ana Diaz");
    let rows = run_sig_unknown(&fx, &dir, &Local::default());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].handle, "+15559990000");
    assert_eq!(rows[0].platform, Platform::Signal);
    assert_eq!(rows[0].message_count, 1);
}

// ---------------------------------------------------------------- x*

/// The same person on two platforms produces two independently
/// triageable rows: marking the WhatsApp thread done must not hide the
/// Signal one.
#[test]
fn x1_same_person_two_platforms_triages_independently() {
    let mut wa = WaFixture::new();
    let wa_session = wa.session("15551110000@s.whatsapp.net", Some("Ana Diaz"));
    wa.message(wa_session, false, 3600, Some("whatsapp message"));

    let mut sig = SignalFixture::new();
    let sig_conv = sig.private("aci-ana", Some("+15551110000"), Some("Ana Diaz"));
    sig.message(&sig_conv, true, 3600, Some("signal message"));

    let mut dir = ContactDirectory::default();
    dir.insert_phone("+15551110000", "Ana Diaz");

    // Both surface, both named from the address book.
    assert_eq!(
        names(&run_wa(&wa, Direction::Owed, &dir, &Local::default())),
        ["Ana Diaz"]
    );
    assert_eq!(
        names(&run_sig(&sig, Direction::Owed, &dir, &Local::default())),
        ["Ana Diaz"]
    );

    // Mark only the WhatsApp thread done.
    let mut local = Local::default();
    local
        .done
        .insert("wa:15551110000@s.whatsapp.net".to_string(), now_unix());
    assert!(run_wa(&wa, Direction::Owed, &dir, &local).is_empty());
    assert_eq!(
        names(&run_sig(&sig, Direction::Owed, &dir, &local)),
        ["Ana Diaz"]
    );
}

/// Previews are cleaned the same way on every source: newlines
/// collapsed, length bounded.
#[test]
fn x2_previews_are_normalized_on_every_source() {
    let long = "x".repeat(400);
    let mut wa = WaFixture::new();
    let s = wa.session("15551110000@s.whatsapp.net", Some("Ana"));
    wa.message(s, false, 3600, Some(&format!("line one\nline two {long}")));
    let rows = run_wa(
        &wa,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    let preview = rows[0].preview.as_deref().expect("preview");
    assert!(!preview.contains('\n'));
    assert!(preview.ends_with('…'));

    let mut sig = SignalFixture::new();
    let conv = sig.private("aci-ana", Some("+15551110000"), Some("Ana"));
    sig.message(
        &conv,
        true,
        3600,
        Some(&format!("line one\nline two {long}")),
    );
    let rows = run_sig(
        &sig,
        Direction::Owed,
        &ContactDirectory::default(),
        &Local::default(),
    );
    let preview = rows[0].preview.as_deref().expect("preview");
    assert!(!preview.contains('\n'));
    assert!(preview.ends_with('…'));
}
