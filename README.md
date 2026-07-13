# Repli

A minimal macOS menu bar app that surfaces iMessage conversations
where you owe someone a reply and, in a second tab, conversations
where someone owes you one. Clean, scannable, just names and a single
action.

**Privacy: nothing leaves your machine.** Repli reads two local
databases (your Messages history and your address book), makes zero
network calls, and has no analytics or telemetry of any kind. The
only writes go to Repli's own small state file. You can verify all of
this; it's why the code is open.

## Install

Download the latest DMG from
[Releases](https://github.com/srobins23/repli/releases), open it, and
drag Repli to Applications.

The build is currently **unsigned** (no Apple Developer ID yet), so
macOS will warn that it "cannot verify" the app or call it "damaged"
on first open. Two ways past that, pick one:

- Open **System Settings → Privacy & Security**, scroll down, and
  click **Open Anyway** next to the Repli message, or
- run `xattr -d com.apple.quarantine /Applications/Repli.app` in
  Terminal.

You'll also need to grant Full Disk Access (see
[Permissions](#permissions)). Apple Silicon only for now; Intel users
can build from source below.

## How it works

- Reads `~/Library/Messages/chat.db` **read-only** with `rusqlite`.
  The database is opened in place with SQLite's read-only flag and is
  never copied, modified, or written to.
- **Owe reply** tab: for each chat, finds the most recent real message;
  surfaces it if that message was inbound (`is_from_me = 0`). That
  covers both unread messages and "read but ghosted" threads. Oldest
  first: the person you've ghosted longest is at the top, and
  timestamps go amber at 3 days, red at 7.
- **Waiting** tab: the inverse. Threads where your last message has
  sat unanswered for at least 3 days. Follow-up radar.
- Reacting to a message **acknowledges** the thread rather than
  clearing it: classic tapbacks (2000-range, incl. 2006 custom emoji),
  custom emoji tapbacks (4000-range, iOS 18+), and stickers (1000)
  demote the row: dimmed, sorted last, excluded from the badge
  counts, labeled "You reacted, never replied" / "They reacted".
  Removing a tapback (3000-range) reopens the thread. Exception: if
  the last message contains a question mark, a tapback is not an
  answer and the row stays fully open.
- Junk never surfaces: chats Messages classifies as spam/promotions
  (`is_filtered`), SMS-filter shadow chats (`(smsft)`/`(smsfp)`),
  blackholed/archived chats, and bare 5-6 digit short-code senders
  (OTP/marketing) are all excluded.
- Group chats are treated as a single thread, keyed by the chat's
  guid. Unnamed groups get labels built from participant names.
- Names resolve from `~/Library/Application Support/AddressBook/`
  (cached for 5 minutes), falling back to phone number or email when
  no contact match is found.
- Row actions: **check** marks it handled, **moon** snoozes the chat
  until 9am tomorrow, **bell-slash** mutes the chat permanently
  (unmute from the footer). If the same contact messages you again,
  the row reappears.

Every threshold and heuristic above (staleness window, waiting age,
question-mark rule, demote-vs-clear, short-code range) is a documented
constant in [`src-tauri/src/config.rs`](src-tauri/src/config.rs).

## Menu bar app

The app lives in the menu bar with no dock icon. The tray icon shows the
count of open owed replies; left-click toggles the window, right-click
has Quit. Closing the window hides it; the 30-second poll keeps the
count current in the background.

## Architecture

Tauri 2 (Rust) + React 18 + TypeScript + Vite.

- `src-tauri/src/pending.rs` - the chat.db query: picks each chat's
  last substantive message (tapbacks, stickers, and system events are
  never candidates) and computes the acknowledged flag by pairing
  reaction adds with later removals per reactor.
- `src-tauri/src/replies.rs` - pure assembly: filtering (staleness,
  short codes, snooze/mute/complete), name resolution, group labels.
  No IO; everything is injectable, which is what makes it testable.
- `src-tauri/src/state.rs` - Repli's own state (completed, snoozed,
  muted) in a separate SQLite file in the app data directory.
  `chat.db` is never written to.
- `src-tauri/src/config.rs` - all behavioral constants.
- `src/` - the React frontend; talks to Rust over Tauri IPC only.

## Prerequisites

- macOS
- Node 20+
- Rust toolchain (`rustup`)

## Setup

```bash
git clone https://github.com/srobins23/repli.git
cd repli
npm install
```

## Permissions

### Full Disk Access (required)

Repli reads files under `~/Library/`, which macOS protects behind
**Full Disk Access (FDA)**. The app detects FDA-blocked reads and
shows an inline prompt instead of crashing.

1. Open **System Settings → Privacy & Security → Full Disk Access**.
2. Click the **+** button.
3. Add the application(s) for your situation:
   - **In dev:** add the terminal/IDE that runs `npm run tauri dev`
     (e.g. Terminal.app, iTerm, VS Code, Cursor). This is the #1
     stuck-point: the *parent process* needs FDA, not the app.
   - **In a built app:** add `Repli.app` from the bundle path
     printed by `npm run tauri build`.
4. **Quit the app fully and reopen it**, via the tray icon's Quit,
   not just closing the window (closing only hides it). This is the
   #2 stuck-point: FDA takes effect on relaunch.

The in-app prompt has an "Open Settings" button that jumps straight
to the FDA pane.

### Accessibility (only for opening group chats)

Opening a **group** chat drives Messages' sidebar search via System
Events keystrokes, which needs the **Accessibility** permission. The
app detects the denial and shows an inline banner with an "Open
Settings" shortcut. 1:1 chats open via the `imessage://` URL scheme
and need nothing.

## Run

```bash
npm run tauri dev      # dev mode with hot reload
npm run tauri build    # production .app
```

## Tests & contributing

The state machine is covered by integration tests that run the real
SQL and assembly pipeline against a **synthetic chat.db**. No test
ever touches a real Messages database. One command:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

The fixture generator lives in
[`src-tauri/tests/fixture/mod.rs`](src-tauri/tests/fixture/mod.rs):
it builds a throwaway SQLite file with the Messages schema subset the
app queries (chats, handles, messages, tapbacks, attributedBody
blobs). If you're adding behavior, start by writing a fixture-backed
test in `src-tauri/tests/state_machine.rs` that describes it.

Before opening a PR:

```bash
npm run typecheck
npm run lint
cargo fmt    --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test   --manifest-path src-tauri/Cargo.toml
```

## Limitations

- **No group deep-linking.** macOS offers no URL scheme that opens an
  existing group conversation, so Repli drives Messages' search field
  with synthesized keystrokes. It's best-effort: it needs the
  Accessibility permission and can mis-select when group names are
  ambiguous.
- **attributedBody decoding is partial.** On macOS 13+, message text
  often lives in a typedstream-encoded `attributedBody` blob. Repli
  extracts the first NSString payload, which covers normal texts but
  can miss some edited or heavily formatted messages (those rows show
  no preview).
- **No sync.** Completed/snoozed/muted state is local to the machine.
- **No compose surface.** Repli never sends messages; tap a row to
  open the conversation in Messages.

## Distributing to other people

The build is **unsigned**. macOS Gatekeeper will refuse to open an
unsigned app that arrived with a quarantine flag (AirDrop, Messages,
email, browser download); the recipient sees "damaged" or "Apple
cannot check it for malicious software." To distribute for real you
need an Apple Developer ID ($99/yr), then sign + notarize:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
npm run tauri build    # Tauri picks up the identity and signs
xcrun notarytool submit --wait ...   # then staple
```

Recipients also need to grant their own Full Disk Access, because the app
reads *their* `chat.db`, on their machine. Nothing ever leaves the
device.

## License

[MIT](LICENSE)
