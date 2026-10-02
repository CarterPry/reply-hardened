//! Opt-in smoke test against the REAL local databases.
//!
//! Everything else in this suite runs on synthetic fixtures and never
//! touches real data. This file is the exception, and it is `#[ignore]`d
//! so it only runs when asked for:
//!
//!     cargo test --manifest-path src-tauri/Cargo.toml \
//!         --test live_smoke -- --ignored --nocapture
//!
//! It exists because three things can only be proven against the real
//! thing: that Signal's key still unwraps out of the login keychain,
//! that SQLCipher opens the live 60-MB database read-only while Signal
//! is running, and that each platform's schema still matches the
//! queries. It prints counts only - never a name, number, or message.
//!
//! Requires Full Disk Access for whatever process runs it (your
//! terminal in dev), and will raise a keychain prompt for "Signal Safe
//! Storage" the first time.

mod fixture;

use repli_lib::contacts;
use repli_lib::pending::{self, Direction};
use repli_lib::replies::{assemble, AssembleCtx, Filters, PendingReply};
use repli_lib::{signal, whatsapp};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs() as i64
}

/// Counts only: how many rows, how many of them groups, how many
/// acknowledged, and how many carry a preview.
fn describe(label: &str, rows: &[PendingReply]) {
    let groups = rows.iter().filter(|r| r.is_group).count();
    let acked = rows.iter().filter(|r| r.acknowledged).count();
    let with_preview = rows.iter().filter(|r| r.preview.is_some()).count();
    let named = rows.iter().filter(|r| !r.name.trim().is_empty()).count();
    println!(
        "{label:28} rows={:<4} groups={groups:<3} acknowledged={acked:<3} \
         with_preview={with_preview:<4} named={named}",
        rows.len()
    );
    // Every row must be renderable: a name to show and a state key to
    // triage with.
    for r in rows {
        assert!(!r.name.trim().is_empty(), "row with no display name");
        assert!(!r.chat_guid.trim().is_empty(), "row with no state key");
        assert!(r.timestamp > 0, "row with no timestamp");
    }
}

#[test]
#[ignore = "reads the real local message databases; run with --ignored"]
fn live_sources_are_readable() {
    // Bypass the 5-minute cache: this runs once, deliberately.
    let dir = contacts::load();
    let done: HashMap<String, i64> = HashMap::new();
    let snoozed: HashMap<String, i64> = HashMap::new();
    let muted: HashSet<String> = HashSet::new();

    for direction in [Direction::Owed, Direction::Waiting] {
        let ctx = AssembleCtx {
            dir: &dir,
            done: &done,
            snoozed: &snoozed,
            muted: &muted,
            now: now_unix(),
        };
        let filters = Filters::new(&ctx, direction);
        let tag = match direction {
            Direction::Owed => "owed",
            Direction::Waiting => "waiting",
        };

        let imessage = pending::read_pending_replies(direction)
            .map(|results| assemble(results, direction, &ctx))
            .expect("iMessage read failed - is Full Disk Access granted?");
        describe(&format!("iMessage/{tag}"), &imessage);

        let wa = whatsapp::read_pending(direction, &dir, &filters).expect("WhatsApp read failed");
        describe(&format!("WhatsApp/{tag}"), &wa);

        let sig = signal::read_pending(direction, &dir, &filters)
            .unwrap_or_else(|e| panic!("Signal read failed: {}", e.detail()));
        describe(&format!("Signal/{tag}"), &sig);

        // State keys must be unique within the merged list, since
        // that is what the UI keys rows by and what triage writes.
        let mut keys = HashSet::new();
        for row in imessage.iter().chain(wa.iter()).chain(sig.iter()) {
            assert!(
                keys.insert(row.chat_guid.clone()),
                "duplicate state key across sources"
            );
        }
        println!("{tag:28} merged unique keys={}", keys.len());
    }
}

/// Signal's key must come out of the keychain and unwrap to a usable
/// SQLCipher key. Separated so a failure here is unambiguous.
#[test]
#[ignore = "reads the real login keychain; run with --ignored"]
fn live_signal_key_unwraps() {
    let key = repli_lib::safestorage::signal_db_key()
        .unwrap_or_else(|e| panic!("key recovery failed: {}", e.detail()));
    let hex = key.as_hex_str().expect("key is not 64 hex characters");
    // Never print the key itself.
    println!("signal key recovered: {} hex characters", hex.len());
    assert_eq!(hex.len(), 64);
}
