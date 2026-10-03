# Repli

A minimal macOS menu bar app that surfaces conversations where you owe
someone a reply and, in a second tab, conversations where someone owes
you one. Clean, scannable, just names and a single action.

Three sources, one list: **iMessage**, **WhatsApp**, and **Signal**.
Every rule is the same on all three - the same staleness window, the
same 3-day follow-up floor, the same done / snooze / mute - so the
list is one triage queue rather than three. Rows carry a small chip
naming their source, shown only when the list actually spans more than
one platform.

**Privacy: nothing leaves your machine.** Repli reads local databases
only (Messages, WhatsApp, Signal, and your address book), makes zero
network calls, and has no analytics or telemetry of any kind. The only
writes go to Repli's own small state file. Signal's database is
encrypted, so Repli also reads one login-keychain item to unwrap the
key - see [Permissions](#permissions). You can verify all of this;
it's why the code is open.

## Install

Download the latest DMG from
[Releases](https://github.com/srobins23/repli/releases), open it, and
**drag Repli to Applications** - don't launch it from inside the disk
image window (macOS refuses to run it from there).

The build is ad-hoc signed but not notarized (no Apple Developer ID
yet), so the first launch from Applications will say macOS "could not
verify" the app. Two ways past that, pick one:

- Close the dialog, open **System Settings → Privacy & Security**,
  scroll down, and click **Open Anyway** next to the Repli message, or
- run `xattr -rd com.apple.quarantine /Applications/Repli.app` in
  Terminal.

You'll also need to grant Full Disk Access (see
[Permissions](#permissions)). Apple Silicon only for now; Intel users
can build from source below.

## How it works

### The sources

| Source | Database | Encrypted | Reactions |
|---|---|---|---|
| iMessage | `~/Library/Messages/chat.db` | no | tapbacks + stickers |
| WhatsApp | `~/Library/Group Containers/group.net.whatsapp.WhatsApp.shared/ChatStorage.sqlite` | no | not stored by WhatsApp |
| Signal | `~/Library/Application Support/Signal/sql/db.sqlite` | SQLCipher 4 | yes |

All three are opened **read-only**, in place, with SQLite's read-only
flag - never copied, modified, or written to. (The one exception is a
throwaway copy of Signal's *still-encrypted* file when Signal.app is
closed; see [Limitations](#limitations).)

A source that can't be read degrades on its own: a missing app, a
denied keychain prompt or a schema change takes that platform out of
the list and prints one line at the bottom of the window explaining
why. The other sources keep working. Only iMessage is load-bearing -
if `chat.db` can't be read you get the Full Disk Access prompt.

Turn a source off entirely with `ENABLE_WHATSAPP` / `ENABLE_SIGNAL` in
[`src-tauri/src/config.rs`](src-tauri/src/config.rs); a disabled source
is never opened and raises no keychain prompt.

### The rules

- **Identity.** Names come from your address book first, on every
  platform, so one person reads the same whether they texted you on
  iMessage, WhatsApp or Signal. Only then does Repli fall back to the
  name the app itself publishes, and finally to the raw number. On
  WhatsApp that fallback is deliberately narrow: see the name-column
  warning in [Limitations](#limitations).
- **One row per conversation per platform.** The same person on two
  apps is two rows, triaged independently - replying on Signal doesn't
  clear the WhatsApp thread, because it didn't.
- **Owe reply** tab: for each chat on every source, finds the most
  recent real message;
  surfaces it if that message was inbound (`is_from_me = 0`). That
  covers both unread messages and "read but ghosted" threads. Oldest
  first: the person you've ghosted longest is at the top, and
  timestamps go amber at 3 days, red at 7.
- **Waiting** tab: the inverse. Threads where your last message has
  sat unanswered for at least 3 days. Follow-up radar.
- Reacting to a message **acknowledges** the thread rather than
  clearing it: on iMessage, classic tapbacks (2000-range, incl. 2006
  custom emoji), custom emoji tapbacks (4000-range, iOS 18+), and
  stickers (1000) demote the row: dimmed, sorted last, excluded from
  the badge counts, labeled "You reacted, never replied" / "They
  reacted". Removing a tapback (3000-range) reopens the thread. Signal
  reactions work identically (its `reactions` table records who
  reacted, so the owed/waiting direction is respected). WhatsApp
  stores no reactions at all, so WhatsApp rows are never acknowledged.
  Exception on every source: if the last message contains a question
  mark, a reaction is not an answer and the row stays fully open.
- Junk never surfaces: on iMessage, chats Messages classifies as
  spam/promotions (`is_filtered`), SMS-filter shadow chats
  (`(smsft)`/`(smsfp)`), blackholed/archived chats, and bare 5-6 digit
  short-code senders (OTP/marketing). On WhatsApp, status broadcasts
  (`@broadcast`), channels (`@newsletter`), and chats you hid or
  deleted. On Signal, archived conversations and Note to Self. System
  rows - "X joined", subject changes, key changes, profile changes,
  call history - are never candidates for "last message" on any
  source, so they can't mask the real one.
- Group chats are treated as a single thread. Unnamed groups get
  labels built from participant names (WhatsApp and Signal usually
  have a real subject, which is used as-is). Signal group membership
  excludes your own account, matching iMessage's member counts.
- Names resolve from `~/Library/Application Support/AddressBook/`
  (cached for 5 minutes), falling back to the platform's own display
  name, then to phone number or email.
- Row actions: **check** marks it handled, **moon** snoozes the chat
  until 9am tomorrow, **bell-slash** mutes the chat permanently
  (unmute from the footer). If the same contact messages you again,
  the row reappears. State is keyed per platform - iMessage keeps its
  bare chat guid (so state written by older versions still applies),
  WhatsApp and Signal namespace theirs as `wa:` / `sg:`.

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

- `src-tauri/src/sources.rs` - the `Platform` enum, per-platform state
  key namespacing, and the per-source status the UI reports.
- `src-tauri/src/pending.rs` - the chat.db query: picks each chat's
  last substantive message (tapbacks, stickers, and system events are
  never candidates) and computes the acknowledged flag by pairing
  reaction adds with later removals per reactor.
- `src-tauri/src/whatsapp.rs` - the same shape of query against
  WhatsApp's Core Data store.
- `src-tauri/src/signal.rs` - the same against Signal Desktop's
  SQLCipher database, including the WAL fallback.
- `src-tauri/src/safestorage.rs` - recovery of Signal's database key
  from `config.json` + the login keychain (Chromium `os_crypt`:
  PBKDF2-HMAC-SHA1 then AES-128-CBC). PBKDF2 is implemented here over
  `hmac`/`sha1` rather than pulling another dependency, and is covered
  by the RFC 6070 vectors.
- `src-tauri/src/replies.rs` - pure assembly: `Filters` (staleness,
  waiting age, snooze/mute/done) is shared by all three sources, so a
  rule can't drift between platforms. Name resolution and group labels
  live here too. No IO; everything is injectable, which is what makes
  it testable.
- `src-tauri/src/state.rs` - Repli's own state (completed, snoozed,
  muted) in a separate SQLite file in the app data directory. No
  message database is ever written to.
- `src-tauri/src/config.rs` - all behavioral constants, including the
  per-source on/off switches.
- `src/` - the React frontend; talks to Rust over Tauri IPC only.

## Prerequisites

- macOS
- Node 20+
- Rust toolchain (`rustup`)

> **Build the Rust target outside iCloud Drive.** If this checkout
> lives under an iCloud-synced folder (`~/Documents`, `~/Desktop`),
> point `CARGO_TARGET_DIR` somewhere else - e.g.
> `export CARGO_TARGET_DIR=~/Library/Caches/repli-target`. iCloud
> stamps `com.apple.fileprovider.fpfs#P` on the built `.app`, which
> makes `codesign` fail with *"resource fork, Finder information, or
> similar detritus not allowed"*, and syncing a multi-gigabyte target
> directory also produces spurious `os error 60` write timeouts
> mid-build. If you hit the codesign error anyway:
> `xattr -cr <app> && codesign --force --deep --sign - <app>`.

## Setup

```bash
git clone https://github.com/srobins23/repli.git
cd repli
npm install
```

## Permissions

### Full Disk Access (required)

Repli reads files under `~/Library/`, which macOS protects behind
**Full Disk Access (FDA)**. One grant covers all three message
databases. The app detects FDA-blocked reads and shows an inline
prompt instead of crashing.

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

### Keychain (only for Signal)

Signal's database key lives in `config.json`, encrypted under a
password in your login keychain (the generic password **"Signal Safe
Storage"**). The first time Repli reads it, macOS asks you to allow
access - click **Always Allow** so the 30-second poll doesn't re-ask.
Deny it and Signal simply drops out of the list with a one-line
explanation; nothing else is affected. Set `ENABLE_SIGNAL = false` in
`config.rs` if you'd rather it never ask.

### Accessibility (for opening groups, and all Signal chats)

Only two kinds of conversation can be opened by URL - an iMessage 1:1
(`imessage://`) and a WhatsApp 1:1 (`whatsapp://send?phone=`).
Everything else (any group, and every Signal thread, since Signal
Desktop publishes no conversation URL) is opened by activating the app
and driving its search box with System Events keystrokes, which needs
the **Accessibility** permission. The app detects the denial and shows
an inline banner with an "Open Settings" shortcut.

## Run

```bash
npm run tauri dev      # dev mode with hot reload
npm run tauri build    # production .app
```

## Tests & contributing

The state machine is covered by integration tests that run the real
SQL and assembly pipeline against **synthetic databases** - one per
source. No test in the default run touches a real message database.
One command:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

The fixture generators live in `src-tauri/tests/fixture/`:
[`mod.rs`](src-tauri/tests/fixture/mod.rs) builds a throwaway
`chat.db` (chats, handles, messages, tapbacks, attributedBody blobs),
[`whatsapp.rs`](src-tauri/tests/fixture/whatsapp.rs) a Core Data
`ChatStorage.sqlite`, and
[`signal.rs`](src-tauri/tests/fixture/signal.rs) a plaintext stand-in
for Signal's schema (the reader takes the key as an `Option`, so
fixtures pass `None`; the key derivation itself is covered by RFC 6070
vectors and a Chromium-wrapping round-trip in `safestorage`). If
you're adding behavior, start with a fixture-backed test in
`state_machine.rs` (iMessage) or `multi_source.rs` (WhatsApp, Signal,
cross-source).

There is also an opt-in smoke test that runs the real queries against
**your own** databases, printing counts only - never a name, number,
or message. It is how you check that Signal's key still unwraps and
that every schema still matches after an app update:

```bash
cargo test --manifest-path src-tauri/Cargo.toml \
    --test live_smoke -- --ignored --nocapture
```

Before opening a PR:

```bash
npm run typecheck
npm run lint
cargo fmt    --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test   --manifest-path src-tauri/Cargo.toml
```

## Limitations

- **Opening a chat is only deterministic where a URL scheme exists.**
  That means iMessage 1:1 (`imessage://`) and WhatsApp 1:1 with a phone
  number (`whatsapp://send?phone=`). Everything else is opened by
  filling the app's own search box - the name is put on the clipboard
  and pasted (typing it would mangle any emoji, and plenty of chats
  have one), which needs the Accessibility permission. What happens
  next differs per app, and each case below is observed, not assumed:
  - **Messages** opens the top hit when Return is pressed in the
    search field.
  - **Signal** needs the result list entered first (Down, then
    Return). It lists matching conversations above message hits, so
    the first one is the right one.
  - **WhatsApp** does nothing on Return, and orders results by
    recency rather than by match: searching `S25 x The Landing 🔥✨`
    returns two unrelated recent chats before the group itself. No
    keystroke reliably picks the right row, so Repli stops at the
    filled search box and says "Searched in WhatsApp - click the chat
    there". One click, rather than a chance of opening someone else's
    conversation.
- **WhatsApp's name columns contain protobuf.** For `@lid` contacts
  this store writes base64-encoded protobuf into the columns that
  normally hold names - `ZWAGROUPMEMBER.ZFIRSTNAME` reading
  `+CgAQgueA1gY=`, `ZWAMESSAGE.ZPUSHNAME` reading
  `COi9hdYGIABIAZABAPABAtgC…`, and `ZWACHATSESSION.ZLASTMESSAGETEXT`
  likewise (which is why the last message is recomputed from
  `ZWAMESSAGE` instead of read from the session row). Repli prefers
  `ZWAPROFILEPUSHNAME`, the one name source here that is never a blob,
  and validates every other candidate with `plausible_name` before it
  can reach the screen. A contact with no trustworthy name shows its
  jid rather than a blob.
- **WhatsApp reactions are invisible.** WhatsApp's Mac database stores
  no reactions anywhere - no table, no column - so a thread you only
  reacted to looks unanswered, and WhatsApp rows are never shown
  demoted. (A test asserts this, so if a future WhatsApp version starts
  storing them, it fails loudly instead of going quietly wrong.)
- **WhatsApp `@lid` chats have no number.** WhatsApp's newer
  privacy-preserving ids aren't reversible to a phone number, so those
  rows use WhatsApp's own display name, can't be matched against your
  contacts, and open via search rather than a URL.
- **Signal reads best while Signal is running.** Signal Desktop keeps
  its database in WAL mode; a read-only connection can share that
  while the app is open. With Signal closed, SQLite can't build the
  index it needs, so Repli falls back to a throwaway copy of the
  **still-encrypted** db/wal/shm triple in your temp directory, reads
  it, and deletes it immediately. Nothing is ever decrypted to disk.
- **Disappearing messages.** Repli reads whatever the apps have
  already stored locally; a message that expired before Repli looked
  is simply not there. Repli never copies message text anywhere.
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

Recipients also need to grant their own Full Disk Access, because the
app reads *their* databases, on their machine - plus a keychain
approval if they use Signal. Nothing ever leaves the device.

## License

[MIT](LICENSE)
