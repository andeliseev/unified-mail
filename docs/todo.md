# To do / ideas for later

Nothing here is committed to; it's a list of things noticed while building v0.1. Rough priority order within each section.

## Not verified yet

- [ ] End-to-end with a real Google account: sign-in, sync, push, archive, trash
- [ ] macOS build: keychain prompts for unsigned builds, WKWebView behavior of the sandboxed email iframe (link interception was only checked in Chromium/WebView2)
- [ ] Installers (`npm run build`): NSIS/MSI on Windows, DMG on macOS

## Gaps in current features

- [ ] **Undo** for archive and delete (a toast with an Undo button that adds `INBOX` back or calls `untrash`)
- [ ] **Reconnect button** for an expired account, instead of an error toast that says to use Add account
- [ ] **Push status** in the UI: show when an account's IDLE connection is down, instead of only logging `[push]`
- [ ] Only 50 newest messages per account: add **Load more** (`pageToken`)
- [ ] Message-level list: add **conversation (thread) view** like Gmail's
- [ ] Sync only the account that sent `mail-changed`, not all accounts
- [ ] Marking read runs in the background; a sync within ~200 ms of opening can briefly show it unread again
- [ ] Inline `cid:` images and attachments aren't shown
- [ ] Optional blocking of remote images (tracking pixels), with a "Load images" button
- [ ] Remember window size and position

## New features

- [ ] Desktop **notifications** for new mail (`tauri-plugin-notification`), now that push exists
- [ ] Unread **badge** on the taskbar / Dock icon
- [ ] **Tray icon**: keep running in the background when the window is closed
- [ ] Mark unread, star, move to label
- [ ] Search (Gmail `q=` query)
- [ ] Reply / compose / send (Gmail API `messages.send`)
- [ ] Gmail History API (`history.list`) for incremental sync: one call per account instead of two lists

## Distribution

- [ ] GitHub Actions to build Windows and macOS installers ([tauri-action](https://github.com/tauri-apps/tauri-action)), with `GOOGLE_CLIENT_ID`/`GOOGLE_CLIENT_SECRET` as repo secrets
- [ ] Code signing: Windows (SmartScreen) and macOS signing plus notarization (Gatekeeper)
- [ ] Auto-update (`tauri-plugin-updater`)
- [ ] Google verification, only if the app goes public beyond 100 users (restricted scope, yearly security assessment)
- [ ] Linux: enable the `keyring` Secret Service feature, test on WebKitGTK

## Code quality

- [ ] Frontend tests: the fake `window.__TAURI__` approach from [development.md](development.md) could become a real test harness
- [ ] Integration tests for `gmail.rs` against a mock HTTP server
- [ ] Typed errors in Rust if the UI ever needs to react to specific errors (e.g. show Reconnect only for expired sign-ins)
- [ ] Accessibility: list keyboard focus, ARIA roles for the message list and dialogs
