# Repli Hardened

This build starts from upstream commit
`152823956e312f7a4a1a0aa88d23b8b040538b91` (`v0.1.3`) and applies a
small privacy hardening delta:

- removes the Tauri updater dependency, plugin, capability, public key,
  endpoint, launch-time check, download/install code, and tray menu item;
- replaces contact names and message previews in notifications with a
  generic count and a generic “open the app” message;
- uses the distinct product name `Repli Hardened` and bundle identifier
  `local.codex.repli.hardened`;
- does not import the upstream app's legacy state database automatically.

This build has no automatic update path. Review and rebuild from a pinned
source commit when updating.

## Multi-source support (WhatsApp + Signal)

This build also reads WhatsApp and Signal alongside iMessage. That is
new local read surface, so it is spelled out here rather than left in
the commit log:

- **WhatsApp** - one additional read-only SQLite handle on
  `~/Library/Group Containers/group.net.whatsapp.WhatsApp.shared/ChatStorage.sqlite`.
  Plaintext on disk already; covered by the same Full Disk Access
  grant as `chat.db`. No new permission.
- **Signal** - one additional read-only SQLCipher handle on
  `~/Library/Application Support/Signal/sql/db.sqlite`, plus:
  - one read of `~/Library/Application Support/Signal/config.json`;
  - one read of the login-keychain item `Signal Safe Storage` via
    `/usr/bin/security` (the key never appears in our argv, and is
    held in a buffer that is zeroed on drop);
  - when Signal.app is closed, a throwaway copy of the
    **still-encrypted** db/wal/shm triple under `$TMPDIR`, deleted on
    drop. Nothing is ever decrypted to disk.
- **New dependencies**: `rusqlite` switches from `bundled` to
  `bundled-sqlcipher-vendored-openssl` (SQLCipher + OpenSSL compiled
  into the binary, so the bundle links nothing from `/opt/homebrew`),
  plus `aes`, `cbc`, `hmac`, `sha1` for Chromium's `os_crypt`
  unwrapping. PBKDF2 is hand-rolled over those primitives instead of
  adding a fifth crate.
- **Still no network path.** Three sources, zero sockets.
- **Notifications stay generic.** The hardening above applies to every
  source: a WhatsApp or Signal notification is still a count and
  "open the app", with no name, number, or message text.
- **Opt out per source** with `ENABLE_WHATSAPP` / `ENABLE_SIGNAL` in
  `src-tauri/src/config.rs`. A disabled source is never opened, and a
  disabled Signal never raises the keychain prompt.
