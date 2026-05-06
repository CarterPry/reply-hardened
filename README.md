# iMessage Reply Tracker

A minimal local tool that surfaces iMessage conversations where you owe
someone a reply. Instagram-notifications style: clean, scannable,
just names and a single action.

## How it works

- Reads `~/Library/Messages/chat.db` **read-only** with `rusqlite`.
- For each chat, finds the most recent message; surfaces it if the
  most recent message was inbound (`is_from_me = 0`). That covers both
  unread messages and "read but ghosted" threads.
- Group chats are treated as a single thread.
- Names resolve from `~/Library/Application Support/AddressBook/` —
  falls back to phone number or email when no contact match is found.
- Tap the checkmark to mark a row complete. The state is stored
  in a separate SQLite file in the app's data directory; if the same
  contact messages you again, the row reappears.

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

## Full Disk Access

This app reads files under `~/Library/`, which macOS protects behind
**Full Disk Access (FDA)**. The app detects FDA-blocked reads and
shows an inline prompt — it does not crash.

### Granting FDA

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

## Run

```bash
npm run tauri dev      # dev mode with hot reload
npm run tauri build    # production .app
```

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
- It does not sync across devices. Completed state lives in the local
  app data directory.
