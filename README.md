# iMessage Reply Tracker

A minimal local menu bar tool that surfaces iMessage conversations
where you owe someone a reply — and, in a second tab, conversations
where someone owes you one. Instagram-notifications style: clean,
scannable, just names and a single action.

## How it works

- Reads `~/Library/Messages/chat.db` **read-only** with `rusqlite`.
- **Owe reply** tab: for each chat, finds the most recent real message;
  surfaces it if that message was inbound (`is_from_me = 0`). That
  covers both unread messages and "read but ghosted" threads. Oldest
  first — the person you've ghosted longest is at the top, and
  timestamps go amber at 3 days, red at 7.
- **Waiting** tab: the inverse — threads where your last message has
  sat unanswered for at least 3 days. Follow-up radar.
- Reacting to a message counts as replying: classic tapbacks
  (2000-range), custom emoji tapbacks (4000-range, iOS 18+), and
  stickers placed on a message (1000) all dismiss the row.
- Junk never surfaces: chats Messages classifies as spam/promotions
  (`is_filtered`), SMS-filter shadow chats (`(smsft)`/`(smsfp)`), and
  blackholed/archived chats are excluded.
- Group chats are treated as a single thread.
- Names resolve from `~/Library/Application Support/AddressBook/`
  (cached for 5 minutes) — falls back to phone number or email when
  no contact match is found.
- Row actions: **check** marks it handled, **moon** snoozes the chat
  until 9am tomorrow, **bell-slash** mutes the chat permanently
  (unmute from the footer). All state lives in a separate SQLite file
  in the app's data directory; if the same contact messages you again,
  the row reappears.

## Menu bar app

The app lives in the menu bar — no dock icon. The tray icon shows the
count of owed replies; left-click toggles the window, right-click has
Quit. Closing the window hides it; the 30-second poll keeps the count
current in the background.

## Stack

Tauri 2 (Rust) + React 18 + TypeScript + Vite. Same patterns as
the Notable app.

## Prerequisites

- macOS
- Node 20+
- Rust toolchain (`rustup`)

## Setup

```bash
git clone https://github.com/srobins23/imessage-reply-tracker.git
cd imessage-reply-tracker
npm install
```

## Permissions

### Full Disk Access (required)

This app reads files under `~/Library/`, which macOS protects behind
**Full Disk Access (FDA)**. The app detects FDA-blocked reads and
shows an inline prompt — it does not crash.

1. Open **System Settings → Privacy & Security → Full Disk Access**.
2. Click the **+** button.
3. Add the application(s) listed below for your situation:
   - **In dev:** add the terminal/IDE that runs `npm run tauri dev`
     (e.g. Terminal.app, iTerm, VS Code, Cursor).
   - **In a built app:** add `iMessage Reply Tracker.app` from the
     bundle path printed by `npm run tauri build`.
4. Quit the app fully and reopen it.

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

## Distributing to other people

The build is **unsigned**. macOS Gatekeeper will refuse to open an
unsigned app that arrived with a quarantine flag (AirDrop, Messages,
email, browser download) — the recipient sees "damaged" or "Apple
cannot check it for malicious software." To distribute for real you
need an Apple Developer ID ($99/yr), then sign + notarize:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
npm run tauri build    # Tauri picks up the identity and signs
xcrun notarytool submit --wait ...   # then staple
```

Recipients also need to grant their own Full Disk Access — the app
reads *their* `chat.db`, on their machine. Nothing ever leaves the
device.

## Useful checks

```bash
npm run typecheck                                          # tsc
npm run lint                                               # eslint
cargo fmt   --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test  --manifest-path src-tauri/Cargo.toml
```

## What the app does NOT do

- It does not write to `chat.db`.
- It does not send messages — there is no compose surface. Tap a row
  to clear it from your list once you've replied elsewhere.
- It does not sync across devices. Completed/snoozed/muted state
  lives in the local app data directory.
