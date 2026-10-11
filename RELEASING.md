# Releasing Reply

Reply auto-updates via [tauri-plugin-updater]. Every release ships a
DMG (for first installs) plus updater artifacts (`.app.tar.gz` +
`.sig`) and a `latest.json` manifest that running apps poll from
GitHub Releases.

## One-time setup (already done / per machine)

- **Updater keypair** lives at `~/.tauri/reply_updater.key` (private,
  passwordless) and `~/.tauri/reply_updater.key.pub` (public). The
  public key is baked into `src-tauri/tauri.conf.json`.
  **Never commit the private key.** Back it up somewhere safe (e.g.
  a password manager): if it's lost, shipped apps can never verify
  another update and users must re-download manually.
  To sign on another machine or CI, set
  `TAURI_SIGNING_PRIVATE_KEY_PATH=~/.tauri/reply_updater.key`
  (or put the key contents in `TAURI_SIGNING_PRIVATE_KEY`).
- The update feed URL is
  `https://github.com/srobins23/repli/releases/latest/download/latest.json`
  — i.e. `latest.json` must be attached to every release. No separate
  hosting needed.

## ⚠️ Signing / notarization status

The bundle is currently **ad-hoc signed** (`signingIdentity: "-"`) and
**not notarized**. Consequences:

- Gatekeeper still warns on first download (users need the
  right-click → Open workaround documented in the README).
- The updater itself works with ad-hoc signing (update integrity comes
  from the minisign signature above, not Apple's), **but** macOS may
  re-prompt for Full Disk Access / Accessibility after an update
  because the code signature isn't stable across builds.

To fix both properly you need a **Developer ID Application
certificate** (Apple Developer Program, $99/yr), then:
set `signingIdentity` to the cert name, and notarize + staple in the
build (`APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` env vars make
`tauri build` do this automatically). Until then, expect the
permission re-grant papercut on updates.

## Release checklist

1. **Bump the version** (keep in sync):
   - `src-tauri/tauri.conf.json` → `version`
   - `src-tauri/Cargo.toml` → `version`
   - `package.json` → `version`
2. **Checks**: `npm run typecheck && npm run lint` and
   `cargo fmt --check && cargo clippy && cargo test`
   (all with `--manifest-path src-tauri/Cargo.toml`).
3. **Build + sign updater artifacts**:
   ```sh
   TAURI_SIGNING_PRIVATE_KEY_PATH=~/.tauri/reply_updater.key npm run tauri build
   ```
   Outputs land in `src-tauri/target/release/bundle/`:
   - `dmg/Reply_<ver>_aarch64.dmg` — for new installs
   - `macos/Reply.app.tar.gz` — the update payload
   - `macos/Reply.app.tar.gz.sig` — its minisign signature
4. **Write `latest.json`** (the update manifest). Template:
   ```json
   {
     "version": "<ver>",
     "notes": "One-line summary of the release.",
     "pub_date": "2026-07-12T00:00:00Z",
     "platforms": {
       "darwin-aarch64": {
         "signature": "<contents of Reply.app.tar.gz.sig>",
         "url": "https://github.com/srobins23/repli/releases/download/v<ver>/Reply.app.tar.gz"
       }
     }
   }
   ```
   (Add a `darwin-x86_64` entry if an Intel build is ever shipped.)
5. **Tag + release**:
   ```sh
   git tag v<ver> && git push origin main --tags
   gh release create v<ver> \
     src-tauri/target/release/bundle/dmg/Reply_<ver>_aarch64.dmg \
     src-tauri/target/release/bundle/macos/Reply.app.tar.gz \
     src-tauri/target/release/bundle/macos/Reply.app.tar.gz.sig \
     latest.json \
     --title "v<ver>" --notes "..."
   ```
6. **Verify**: run the previous version and use the tray menu's
   "Check for Updates…" — it should install and relaunch as `<ver>`.

Running apps also check quietly at launch and show a notification when
a new version is available; installing is always user-initiated via
the menu.

[tauri-plugin-updater]: https://v2.tauri.app/plugin/updater/
