//! Integration tests: the real SQL + assembly pipeline run against a
//! synthetic chat.db built by `fixture`. Each test encodes one check
//! from the reply-state spec:
//!
//!   b* — group chat handling
//!   c* — tapback state machine
//!   d* — noise filtering / regression
//!
//! Run with: cargo test --manifest-path src-tauri/Cargo.toml

mod fixture;

use fixture::*;
use repli_lib::contacts::ContactDirectory;
use repli_lib::pending::{read_pending_replies_at, Direction};
use repli_lib::replies::{assemble, open_count, AssembleCtx, PendingReply};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs() as i64
}

fn run_with_dir(fx: &Fixture, direction: Direction, dir: &ContactDirectory) -> Vec<PendingReply> {
    let results = read_pending_replies_at(&fx.path, direction).expect("query fixture db");
    let completed = HashSet::new();
    let snoozed = HashMap::new();
    let muted = HashSet::new();
    assemble(
        results,
        direction,
        &AssembleCtx {
            dir,
            completed: &completed,
            snoozed: &snoozed,
            muted: &muted,
            now: now_unix(),
        },
    )
}

fn owed(fx: &Fixture) -> Vec<PendingReply> {
    run_with_dir(fx, Direction::Owed, &ContactDirectory::default())
}

fn waiting(fx: &Fixture) -> Vec<PendingReply> {
    run_with_dir(fx, Direction::Waiting, &ContactDirectory::default())
}

// ---------------------------------------------------------------
// 0b — group chat handling
// ---------------------------------------------------------------

/// Group threads are keyed by chat.guid: a group containing Alice and
/// a 1:1 with the same Alice are distinct rows, neither split nor
/// merged.
#[test]
fn b1_groups_keyed_by_chat_guid_not_handle() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let bob = fx.handle("+14155550102");
    let dm = fx.dm(alice);
    let grp = fx.group(Some("Ski Trip"), &[alice, bob]);
    fx.msg(dm, false, Some(alice), "dm hello", days_ago(1.0));
    fx.msg(grp, false, Some(alice), "group hello", days_ago(1.0));

    let rows = owed(&fx);
    assert_eq!(rows.len(), 2, "one DM row + one group row, no merging");
    let group_row = rows.iter().find(|r| r.is_group).expect("group row present");
    let dm_row = rows.iter().find(|r| !r.is_group).expect("dm row present");
    assert!(
        group_row.chat_guid.starts_with("iMessage;+;chat"),
        "group row keyed by the group chat guid, got {}",
        group_row.chat_guid
    );
    assert_eq!(dm_row.chat_guid, "iMessage;-;+14155550101");
    assert_eq!(group_row.member_count, 2);
}

/// Participants come from chat_handle_join and resolve through the
/// contact directory.
#[test]
fn b2_participants_resolve_via_chat_handle_join() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let bob = fx.handle("+14155550102");
    let grp = fx.group(Some("Ski Trip"), &[alice, bob]);
    fx.msg(grp, false, Some(alice), "who's in?", days_ago(1.0));

    let mut dir = ContactDirectory::default();
    dir.insert_phone("+14155550101", "Alice Smith");
    dir.insert_phone("+14155550102", "Bob Jones");
    let rows = run_with_dir(&fx, Direction::Owed, &dir);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].participants,
        vec!["Alice Smith".to_string(), "Bob Jones".to_string()]
    );
    assert_eq!(rows[0].last_sender.as_deref(), Some("Alice"));
}

/// A group with an empty (or whitespace) display_name falls back to
/// participant names for both the row label and the search hint.
#[test]
fn b3_empty_display_name_falls_back_to_participants() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let bob = fx.handle("+14155550102");
    let grp = fx.group(Some(""), &[alice, bob]);
    fx.msg(grp, false, Some(bob), "yo", days_ago(1.0));

    let mut dir = ContactDirectory::default();
    dir.insert_phone("+14155550101", "Alice Smith");
    dir.insert_phone("+14155550102", "Bob Jones");
    let rows = run_with_dir(&fx, Direction::Owed, &dir);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Alice, Bob");
    assert_eq!(rows[0].search_hint, "Alice Bob");
}

/// Every surfaced group row must be actionable: a non-empty search
/// hint (what open_chat types into Messages) and a non-empty handle,
/// even for unnamed groups with unresolvable participants.
#[test]
fn b4_group_rows_always_actionable() {
    let mut fx = Fixture::new();
    let a = fx.handle("+14155550101");
    let b = fx.handle("+14155550102");
    let named = fx.group(Some("Ski Trip"), &[a, b]);
    let unnamed = fx.group(None, &[a, b]);
    fx.msg(named, false, Some(a), "one", days_ago(2.0));
    fx.msg(unnamed, false, Some(b), "two", days_ago(1.0));

    for row in owed(&fx) {
        assert!(row.is_group);
        assert!(
            !row.search_hint.trim().is_empty(),
            "group '{}' has an empty search hint — tapping it would dead-tap",
            row.name
        );
        assert!(!row.handle.trim().is_empty());
        assert!(!row.name.trim().is_empty());
    }
}

/// In a group where the latest real message is from participant B
/// (even mid-conversation between other people), the thread is owed —
/// and never duplicated into waiting.
#[test]
fn b5_group_reply_between_participants_lands_owed() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let bob = fx.handle("+14155550102");
    let grp = fx.group(Some("Trip"), &[alice, bob]);
    fx.msg(grp, true, None, "i'm in", days_ago(5.0));
    fx.msg(grp, false, Some(alice), "same!", days_ago(4.0));
    fx.msg(grp, false, Some(bob), "let's book it", days_ago(3.0));

    let owed_rows = owed(&fx);
    assert_eq!(
        owed_rows.len(),
        1,
        "group is owed when someone else spoke last"
    );
    assert_eq!(owed_rows[0].last_sender.as_deref(), Some("+14155550102"));
    assert!(waiting(&fx).is_empty(), "not simultaneously in waiting");
}

// ---------------------------------------------------------------
// 0c — tapback state machine
// ---------------------------------------------------------------

/// A tapback row is never picked as the "last substantive message":
/// their tapback on my latest text leaves the thread in waiting (I
/// spoke last), not owed.
#[test]
fn c1_tapback_never_last_substantive_message() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    fx.msg(dm, false, Some(alice), "hey", days_ago(6.0));
    let mine = fx.msg(dm, true, None, "yo, all good", days_ago(5.0));
    fx.reaction(dm, false, Some(alice), 2000, &mine, days_ago(4.0));

    assert!(
        owed(&fx).is_empty(),
        "an inbound tapback must not count as an inbound message"
    );
    let w = waiting(&fx);
    assert_eq!(w.len(), 1, "thread belongs to waiting (my text was last)");
    assert_eq!(w[0].last_message_guid, mine);
}

/// Inbound tapback on my last outbound → ACKNOWLEDGED ("they
/// reacted"): still visible in waiting, excluded from the badge.
#[test]
fn c2_their_tapback_acknowledges_waiting_excluded_from_badge() {
    for add_type in TAPBACK_ADDS {
        let mut fx = Fixture::new();
        let alice = fx.handle("+14155550101");
        let dm = fx.dm(alice);
        let mine = fx.msg(dm, true, None, "sent you the doc", days_ago(5.0));
        fx.reaction(dm, false, Some(alice), add_type, &mine, days_ago(4.5));

        let w = waiting(&fx);
        assert_eq!(
            w.len(),
            1,
            "type {add_type}: acknowledged row must stay visible (demoted), not vanish"
        );
        assert!(
            w[0].acknowledged,
            "type {add_type}: row must be flagged acknowledged"
        );
        assert_eq!(
            open_count(&w),
            0,
            "type {add_type}: acknowledged rows don't count toward the badge"
        );
        assert!(owed(&fx).is_empty(), "type {add_type}: never lands in owed");
    }
}

/// My tapback on their last message → ACKNOWLEDGED ("you reacted,
/// never replied"): demoted but visible in owed, not silently cleared.
#[test]
fn c3_my_tapback_demotes_owed_not_cleared() {
    for add_type in TAPBACK_ADDS {
        let mut fx = Fixture::new();
        let alice = fx.handle("+14155550101");
        let dm = fx.dm(alice);
        let theirs = fx.msg(dm, false, Some(alice), "check this out", days_ago(5.0));
        fx.reaction(dm, true, None, add_type, &theirs, days_ago(4.5));

        let o = owed(&fx);
        assert_eq!(
            o.len(),
            1,
            "type {add_type}: 'you reacted, never replied' must stay visible"
        );
        assert!(o[0].acknowledged, "type {add_type}: flagged acknowledged");
        assert_eq!(open_count(&o), 0, "type {add_type}: excluded from badge");
    }
}

/// Question-mark exception (owed): their last message asks a question;
/// my tapback is not an answer — the row stays fully open.
#[test]
fn c4_question_mark_keeps_owed_open() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    let theirs = fx.msg(dm, false, Some(alice), "dinner on friday?", days_ago(5.0));
    fx.reaction(dm, true, None, 2001, &theirs, days_ago(4.5));

    let o = owed(&fx);
    assert_eq!(o.len(), 1);
    assert!(
        !o[0].acknowledged,
        "a tapback on a question does not acknowledge it"
    );
    assert_eq!(open_count(&o), 1, "still counts toward the badge");
}

/// Question-mark exception (waiting): I asked; their tapback is not an
/// answer — I'm still waiting.
#[test]
fn c5_question_mark_keeps_waiting_open() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    let mine = fx.msg(dm, true, None, "are you free tomorrow?", days_ago(5.0));
    fx.reaction(dm, false, Some(alice), 2000, &mine, days_ago(4.5));

    let w = waiting(&fx);
    assert_eq!(w.len(), 1);
    assert!(
        !w[0].acknowledged,
        "their thumbs-up on my question is not an answer"
    );
    assert_eq!(open_count(&w), 1);
}

/// A tapback removal (3000–3005) is not an acknowledgment — neither on
/// its own nor after cancelling an earlier add.
#[test]
fn c6_tapback_removal_is_not_acknowledgment() {
    // Add then remove: the add is cancelled.
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    let theirs = fx.msg(dm, false, Some(alice), "new plan attached", days_ago(6.0));
    fx.reaction(dm, true, None, 2000, &theirs, days_ago(5.0));
    fx.reaction(dm, true, None, 3000, &theirs, days_ago(4.0));
    let o = owed(&fx);
    assert_eq!(o.len(), 1);
    assert!(
        !o[0].acknowledged,
        "add-then-remove must return the row to open"
    );

    // Removal alone (no matching add) — degenerate but must not ack.
    let mut fx2 = Fixture::new();
    let bea = fx2.handle("+14155550102");
    let dm2 = fx2.dm(bea);
    let theirs2 = fx2.msg(dm2, false, Some(bea), "you around?", days_ago(6.0));
    fx2.reaction(dm2, true, None, 3002, &theirs2, days_ago(5.0));
    let o2 = owed(&fx2);
    assert_eq!(o2.len(), 1);
    assert!(
        !o2[0].acknowledged,
        "a bare removal row must not acknowledge"
    );
}

/// Re-adding a tapback after removing one acknowledges again — the
/// latest event per reactor wins.
#[test]
fn c7_readd_after_removal_acknowledges() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    let theirs = fx.msg(dm, false, Some(alice), "fyi closing thread", days_ago(6.0));
    fx.reaction(dm, true, None, 2000, &theirs, days_ago(5.0));
    fx.reaction(dm, true, None, 3000, &theirs, days_ago(4.0));
    fx.reaction(dm, true, None, 2000, &theirs, days_ago(3.0));

    let o = owed(&fx);
    assert_eq!(o.len(), 1);
    assert!(o[0].acknowledged, "re-added tapback acknowledges again");
}

/// Custom emoji reactions (2006) act like any other tapback, and their
/// removal (3006) cancels them.
#[test]
fn c8_custom_emoji_2006_behaves_like_tapback() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    let theirs = fx.msg(dm, false, Some(alice), "look at this dog", days_ago(6.0));
    fx.reaction(dm, true, None, CUSTOM_EMOJI_ADD, &theirs, days_ago(5.0));
    let o = owed(&fx);
    assert_eq!(o.len(), 1);
    assert!(o[0].acknowledged, "custom emoji reaction acknowledges");

    fx.reaction(dm, true, None, CUSTOM_EMOJI_REMOVE, &theirs, days_ago(4.0));
    let o = owed(&fx);
    assert_eq!(o.len(), 1);
    assert!(!o[0].acknowledged, "custom emoji removal cancels the ack");
}

/// Stickers placed on a message (1000) count as reactions too.
#[test]
fn c9_sticker_acknowledges() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    let theirs = fx.msg(dm, false, Some(alice), "trip pics", days_ago(5.0));
    fx.reaction(dm, true, None, STICKER, &theirs, days_ago(4.0));

    let o = owed(&fx);
    assert_eq!(o.len(), 1);
    assert!(o[0].acknowledged);
}

/// Badge counts equal exactly the number of OPEN rows in each list —
/// acknowledged rows appear in the list but never in the count.
#[test]
fn c10_badge_counts_match_state_machine() {
    let mut fx = Fixture::new();
    let a = fx.handle("+14155550101");
    let b = fx.handle("+14155550102");
    let c = fx.handle("+14155550103");
    // Open owed.
    let dm_a = fx.dm(a);
    fx.msg(dm_a, false, Some(a), "open one", days_ago(2.0));
    // Open owed (question + my tapback → still open).
    let dm_b = fx.dm(b);
    let q = fx.msg(dm_b, false, Some(b), "lunch?", days_ago(3.0));
    fx.reaction(dm_b, true, None, 2000, &q, days_ago(2.5));
    // Acknowledged owed (no question).
    let dm_c = fx.dm(c);
    let t = fx.msg(dm_c, false, Some(c), "cool video", days_ago(3.0));
    fx.reaction(dm_c, true, None, 2003, &t, days_ago(2.5));

    let o = owed(&fx);
    assert_eq!(o.len(), 3, "all three rows visible");
    assert_eq!(open_count(&o), 2, "badge counts only the open two");
}

/// My consecutive outbound messages collapse to a single waiting row
/// keyed by the newest message.
#[test]
fn c11_consecutive_outbound_messages_one_row() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    fx.msg(dm, true, None, "one", days_ago(6.0));
    fx.msg(dm, true, None, "two", days_ago(5.5));
    let newest = fx.msg(dm, true, None, "three", days_ago(5.0));

    let w = waiting(&fx);
    assert_eq!(w.len(), 1, "three consecutive texts are one thread");
    assert_eq!(w[0].last_message_guid, newest);
    assert!(owed(&fx).is_empty());
}

/// In a group, any participant reacting to my last message
/// acknowledges the waiting thread ("they reacted").
#[test]
fn c12_any_group_participant_reaction_acknowledges() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let bob = fx.handle("+14155550102");
    let grp = fx.group(Some("Trip"), &[alice, bob]);
    let mine = fx.msg(grp, true, None, "booked the cabin", days_ago(5.0));
    fx.reaction(grp, false, Some(bob), 2000, &mine, days_ago(4.0));

    let w = waiting(&fx);
    assert_eq!(w.len(), 1);
    assert!(w[0].acknowledged);
    assert_eq!(open_count(&w), 0);
}

/// A reaction targeting an OLDER message does not acknowledge the
/// latest one.
#[test]
fn c13_reaction_on_older_message_does_not_acknowledge() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    let old = fx.msg(dm, true, None, "old msg", days_ago(10.0));
    let newest = fx.msg(dm, true, None, "did you see my email", days_ago(5.0));
    fx.reaction(dm, false, Some(alice), 2000, &old, days_ago(4.0));

    let w = waiting(&fx);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].last_message_guid, newest);
    assert!(
        !w[0].acknowledged,
        "their tapback on an old message doesn't answer the new one"
    );
}

// ---------------------------------------------------------------
// 0d — noise filtering / regression
// ---------------------------------------------------------------

/// 1:1 threads from bare 5–6 digit short codes (OTP/marketing) never
/// surface; real numbers do.
#[test]
fn d1_short_code_senders_filtered() {
    let mut fx = Fixture::new();
    let code5 = fx.handle("22395");
    let code6 = fx.handle("893746");
    let real = fx.handle("+14155550101");
    for h in [code5, code6, real] {
        let dm = fx.dm(h);
        fx.msg(dm, false, Some(h), "Your code is 123456", days_ago(1.0));
    }

    let rows = owed(&fx);
    let handles: Vec<&str> = rows.iter().map(|r| r.handle.as_str()).collect();
    assert!(
        !handles.contains(&"22395"),
        "5-digit short code must be filtered, got {handles:?}"
    );
    assert!(
        !handles.contains(&"893746"),
        "6-digit short code must be filtered, got {handles:?}"
    );
    assert!(
        handles.contains(&"+14155550101"),
        "real numbers must not be filtered, got {handles:?}"
    );
}

/// Rows older than the staleness cutoff (90d) are hidden; younger ones
/// stay.
#[test]
fn d2_stale_rows_hidden() {
    let mut fx = Fixture::new();
    let old = fx.handle("+14155550101");
    let fresh = fx.handle("+14155550102");
    let dm_old = fx.dm(old);
    let dm_fresh = fx.dm(fresh);
    fx.msg(dm_old, false, Some(old), "ancient", days_ago(91.0));
    fx.msg(dm_fresh, false, Some(fresh), "recent", days_ago(89.0));

    let rows = owed(&fx);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].handle, "+14155550102");
}

/// Waiting rows need the minimum quiet period (3d) before surfacing.
#[test]
fn d3_waiting_minimum_age() {
    let mut fx = Fixture::new();
    let a = fx.handle("+14155550101");
    let b = fx.handle("+14155550102");
    let dm_young = fx.dm(a);
    let dm_ripe = fx.dm(b);
    fx.msg(dm_young, true, None, "sent yesterday", days_ago(1.0));
    fx.msg(dm_ripe, true, None, "sent last week", days_ago(4.0));

    let w = waiting(&fx);
    assert_eq!(w.len(), 1, "only the ≥3d-quiet thread surfaces");
    assert_eq!(w[0].handle, "+14155550102");
}

/// Chats Messages classifies as junk, and SMS-filter shadow chats,
/// never surface.
#[test]
fn d4_spam_and_smsf_chats_excluded() {
    let mut fx = Fixture::new();
    let a = fx.handle("+14155550101");
    let b = fx.handle("+14155550102");
    let c = fx.handle("+14155550103");

    let junk = fx.dm(a);
    fx.set_chat_flag(junk, "is_filtered", 4);
    fx.msg(junk, false, Some(a), "WIN A CRUISE", days_ago(1.0));

    let shadow = fx.dm(b);
    fx.set_chat_identifier(shadow, "+14155550102(smsft)");
    fx.msg(shadow, false, Some(b), "shadow dup", days_ago(1.0));

    let normal = fx.dm(c);
    fx.msg(normal, false, Some(c), "real message", days_ago(1.0));

    let rows = owed(&fx);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].handle, "+14155550103");
}

/// NULL text with the string in attributedBody: preview extracts, and
/// the question-mark exception still applies to it.
#[test]
fn d5_attributed_body_preview_and_question_exception() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let dm = fx.dm(alice);
    let theirs = fx.msg_attributed(dm, false, Some(alice), "coming tonight?", days_ago(5.0));
    fx.reaction(dm, true, None, 2000, &theirs, days_ago(4.0));

    let o = owed(&fx);
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].preview.as_deref(), Some("coming tonight?"));
    assert!(
        !o[0].acknowledged,
        "question in attributedBody must trigger the exception too"
    );
}

/// System events (joins/renames) are never the last substantive
/// message.
#[test]
fn d6_system_events_ignored() {
    let mut fx = Fixture::new();
    let alice = fx.handle("+14155550101");
    let bob = fx.handle("+14155550102");
    let grp = fx.group(Some("Trip"), &[alice, bob]);
    fx.msg(grp, false, Some(alice), "welcome!", days_ago(3.0));
    fx.system_event(grp, days_ago(1.0));

    let rows = owed(&fx);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].preview.as_deref(), Some("welcome!"));
}

/// Completed / snoozed / muted state still filters rows out.
#[test]
fn d7_local_state_filters_still_apply() {
    let mut fx = Fixture::new();
    let a = fx.handle("+14155550101");
    let b = fx.handle("+14155550102");
    let c = fx.handle("+14155550103");
    let dm_a = fx.dm(a);
    let dm_b = fx.dm(b);
    let dm_c = fx.dm(c);
    let guid_a = fx.msg(dm_a, false, Some(a), "done already", days_ago(1.0));
    fx.msg(dm_b, false, Some(b), "snoozed", days_ago(1.0));
    fx.msg(dm_c, false, Some(c), "muted", days_ago(1.0));

    let results = read_pending_replies_at(&fx.path, Direction::Owed).expect("query");
    let completed: HashSet<String> = [guid_a].into_iter().collect();
    let snoozed: HashMap<String, i64> =
        [("iMessage;-;+14155550102".to_string(), now_unix() + 3600)]
            .into_iter()
            .collect();
    let muted: HashSet<String> = ["iMessage;-;+14155550103".to_string()]
        .into_iter()
        .collect();
    let rows = assemble(
        results,
        Direction::Owed,
        &AssembleCtx {
            dir: &ContactDirectory::default(),
            completed: &completed,
            snoozed: &snoozed,
            muted: &muted,
            now: now_unix(),
        },
    );
    assert!(
        rows.is_empty(),
        "completed+snoozed+muted all filter, got {rows:?}"
    );
}
