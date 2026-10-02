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
