# To do / ideas for later

Nothing here is committed to; it's a list of things noticed while building v0.1. Rough priority order within each section.

## Not verified yet

- [ ] End-to-end with a real Google account: sign-in, sync, push, archive, trash
- [ ] macOS build: keychain prompts for unsigned builds, WKWebView behavior of the sandboxed email iframe (link interception was only checked in Chromium/WebView2)
- [x] Installers (`npm run build`): NSIS/MSI on Windows (built and installed locally)
- [ ] macOS `.dmg` from the Release workflow: first run on a real Mac (ad-hoc signing, Gatekeeper prompt)
- [x] First run of the Release workflow on GitHub (v0.1.0)

## Gaps in current features

- [x] **Undo** for Archive, Delete and Move to Inbox; **All Mail** and **Trash** folders
- [ ] Undo, Trash and All Mail against real Gmail accounts (tested with a mock backend only so far)
- [ ] Push only watches the Inbox; All Mail and Trash refresh with it, on focus and every 5 min
- [ ] More folders: Sent, Starred, Spam, user labels
- [ ] **Reconnect button** for an expired account, instead of an error toast that says to use Add account
- [ ] **Push status** in the UI: show when an account's IDLE connection is down, instead of only logging `[push]`
- [x] Older mail loads while scrolling (pages of 50 per account, merged in date order)
- [ ] Older pages aren't re-synced: something archived elsewhere stays visible there until the folder is reopened or the app restarts
- [ ] Search across all mail (Gmail `q=`), for finding old messages without scrolling
- [ ] Rendering re-creates every row; with many thousands of loaded rows, switch to a virtualized list
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

## Yandex Mail accounts

Looked into on 2026-11-03, not started. A big feature: about as much work as the Gmail support. Suggested stages: (1) Inbox, Trash, Archive, push and Undo, then (2) All Mail.

- [ ] **Everything goes over IMAP** (`imap.yandex.com:993`); Yandex has no mail REST API like Gmail's. Its server advertises (pre-login `CAPABILITY`): `IDLE`, `AUTH=XOAUTH2`, `AUTH=PLAIN`, `MOVE`, `UIDPLUS`, `XLIST`, `UNSELECT`, `ENABLE`, `ID`. No `CONDSTORE`/`QRESYNC`, so syncs compare UIDs and flags.
- [ ] **Push:** reuse `idle.rs` with Yandex's server and login. `IDLE` is supported.
- [ ] **Sign-in**, one of:
  - Yandex OAuth (preferred, same flow as Google): register the app once at oauth.yandex.ru with IMAP mail access, and embed its client ID like the Google one. Check whether Yandex accepts loopback redirects (`http://127.0.0.1:<port>`) and PKCE. If not, use a fixed port or its verification-code page.
  - Or an app password the user creates in Yandex ID and pastes in (`AUTH=PLAIN`). Simpler to build, clunkier to use.
  - Either way, each user enables IMAP once in Yandex Mail settings (Почтовые программы → IMAP, with app passwords and OAuth tokens).
- [ ] **Folders:** Inbox → `INBOX`; Trash → the `\Trash` folder (found via `XLIST`); Archive → move to Yandex's "Archive" folder, creating it if missing, as Yandex's own Archive button does. All Mail doesn't exist on Yandex: show Inbox + Archive merged.
- [ ] **Reading:** fetch envelopes, flags and bodies over IMAP, and decode MIME ourselves (Gmail's API does this for us): structure, charsets, RFC 2047 headers. Use a MIME crate such as `mail-parser`, checking the size impact first (estimated +0.5–1 MB). Previews (snippets) come from the start of the text body.
- [ ] **Identity:** IMAP UIDs are per folder and change on move. Messages need a provider-specific id (folder + UIDVALIDITY + UID), with `UIDPLUS` supplying the new UID after a move for Undo. `keyOf`, the pending moves, Undo and the All Mail tags currently assume Gmail's global ids.
- [ ] **Provider layer:** split `gmail.rs`/`state.rs` behind a common interface (list page, older page, get message, archive, trash, restore, mark read), so the UI stays the same.
- [ ] **Privacy policy:** add Yandex (what's accessed, where it's stored).

## Distribution

- [x] GitHub Actions release workflow for Windows and macOS installers (see [development.md](development.md#releases-github-actions))
- [x] Open source: MIT license, public repo, so releases are public downloads
- [ ] Code signing: Windows (SmartScreen) and macOS signing plus notarization (Gatekeeper)
- [ ] Auto-update (`tauri-plugin-updater`)
- [ ] **Google verification**, so users don't see the "unverified app" warning and the user cap goes away. Steps from Google's [restricted scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification):
  - Prove ownership of `andeliseev.github.io` in Google Search Console (a verification file in the site repo)
  - Home page and privacy policy with the Limited Use statement (done)
  - Justify the `https://mail.google.com/` scope: IMAP is needed for push. Google asks for the narrowest scope, so expect questions
  - Record an unlisted YouTube demo of the sign-in and of every feature that uses the scope
  - Security assessment (CASA): Google requires it for apps that access mail "from or through a third-party server". Unified Mail has no server, but whether that exempts it is decided in review. If required: an approved lab, paid, renewed yearly
- [ ] Linux: enable the `keyring` Secret Service feature, test on WebKitGTK

## Code quality

- [ ] Frontend tests: the fake `window.__TAURI__` approach from [development.md](development.md) could become a real test harness
- [ ] Integration tests for `gmail.rs` against a mock HTTP server
- [ ] Typed errors in Rust if the UI ever needs to react to specific errors (e.g. show Reconnect only for expired sign-ins)
- [ ] Accessibility: list keyboard focus, ARIA roles for the message list and dialogs
