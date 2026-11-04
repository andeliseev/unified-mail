# To do / ideas for later

Nothing here is committed to; it's a list of things noticed while building v0.1. Rough priority order within each section.

## Not verified yet

- [ ] End-to-end with a real Google account: sign-in, sync, push, archive, trash
- [ ] macOS build: keychain prompts for unsigned builds, WKWebView behavior of the sandboxed email iframe (link interception and the right-click menu, including Copy, were only checked in Chromium/WebView2)
- [ ] Mark as unread and the right-click menu in the real app with a test Gmail account (checked with a fake backend only so far)
- [ ] Notifications from the installed app: they should carry the app's name and icon, since the installer's shortcut registers the app's ID with Windows. Dev builds run from `target\` and show as "Windows PowerShell". Checked so far: toasts and the mail sound in the test app, the taskbar badge, and the second-launch handling. A click on a toast isn't checked yet
- [x] Installers (`npm run build`): NSIS/MSI on Windows (built and installed locally)
- [ ] macOS `.dmg` from the Release workflow: first run on a real Mac (ad-hoc signing, Gatekeeper prompt)
- [x] First run of the Release workflow on GitHub (v0.1.0)

## Gaps in current features

- [x] **Undo** for Archive, Delete and Move to Inbox; **All Mail** and **Trash** folders
- [ ] Undo, Trash and All Mail against real Gmail accounts (tested with a mock backend only so far)
- [ ] Push only watches the Inbox; All Mail and Trash refresh with it, on focus and every 5 min
- [ ] More folders: Sent, Starred, Spam, user labels
- [x] **Reconnect button** for an expired account, instead of an error toast that says to use Add account
- [ ] Reconnect against a real expired sign-in (checked with a fake backend only so far). One way: remove the app's access at myaccount.google.com/permissions for a test account
- [ ] An account whose sign-in expires mid-session shows Reconnect only after the next sync; opening a message from it before that shows the error as a toast
- [ ] **Push status** in the UI: show when an account's IDLE connection is down, instead of only logging `[push]`
- [x] Older mail loads while scrolling (pages of 50 per account, merged in date order)
- [ ] Older pages aren't re-synced: something archived elsewhere stays visible there until the folder is reopened or the app restarts
- [ ] Search across all mail (Gmail `q=`), for finding old messages without scrolling
- [ ] Rendering re-creates every row; with many thousands of loaded rows, switch to a virtualized list
- [ ] Message-level list: add **conversation (thread) view** like Gmail's
- [ ] Sync only the account that sent `mail-changed`, not all accounts
- [x] Marking read ran in the background, so a sync right after opening could briefly show it unread again. Now it's tracked like moves (`pendingRead`)
- [ ] A sync that reads Gmail just before a move or read change lands, but arrives after it's confirmed, can briefly show the old state until the next sync
- [ ] Inline `cid:` images and attachments aren't shown
- [ ] Dark mode for emails: dark logos on transparent backgrounds stay hard to see; maybe a light backing behind small transparent images. Original could also be remembered per sender instead of per opened message
- [ ] Dark mode for emails on macOS (WKWebView) is untested: rewriting the email's media rules and the frame following the app's dark preference
- [ ] Optional blocking of remote images (tracking pixels), with a "Load images" button
- [ ] Remember window size and position

## New features

- [x] Desktop **notifications** for new mail (`tauri-plugin-notification`)
- [x] Unread **badge** on the taskbar / Dock icon
- [ ] A setting to turn notifications off inside the app (Windows and macOS can already turn them off per app)
- [ ] Clicking an old notification in the Windows Action Center only brings the window forward, without opening the message. That needs a COM activator, which the notification plugin doesn't have
- [ ] The badge, like the sidebar, counts unread mail only among loaded messages (each account's newest 50 in the Inbox, plus older pages scrolled to). Gmail's `labels.get` for `INBOX` would give the exact count
- [ ] macOS: notifications and the Dock badge are untested; notifications may need a signed app, and they're silent (no sound is set there)
- [ ] **Tray icon**: keep running in the background when the window is closed
- [x] Mark as unread / read (button, `Shift+U` / `Shift+I`, right-click menu)
- [ ] Star, move to label
- [ ] **App language** setting (e.g. Russian). The UI is English only. Dates are English too ("Nov 4"), with the system's clock style (24-hour or AM/PM); with a language setting they'd follow the chosen language. Text fields keep the system's own right-click menu, so on a Russian system that shows in Russian
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
- [x] **Provider layer:** `provider.rs` has the `Provider` enum and `Mailbox` (list page, unread ids, summaries, full message, read/unread, archive, trash, restore); `state.rs` no longer knows Gmail. Accounts carry their provider in `accounts.json`. Still Gmail-specific: sign-in and token refresh (`oauth.rs`, Google's client) are one match arm in `AppState`, and push's XOAUTH2 login.
- [ ] **Moves that change ids:** `archive`, `trash` and `restore` return nothing (or only `was_in_inbox`), because Gmail ids never change. For IMAP they'll need to return the message's new id, and the UI's pending moves and Undo will need to use it.
- [ ] **Privacy policy:** add Yandex (what's accessed, where it's stored).

## Distribution

- [x] GitHub Actions release workflow for Windows and macOS installers (see [development.md](development.md#releases-github-actions))
- [x] Open source: MIT license, public repo, so releases are public downloads
- [ ] Code signing: Windows (SmartScreen) and macOS signing plus notarization (Gatekeeper)
- [x] Auto-update (`tauri-plugin-updater`): Settings offers an Update button
- [ ] The first real update: installed apps get the updater only from the first release that has it, so the update after that one is the first end-to-end check (Windows NSIS and MSI, and macOS, which is untested). Locally checked so far: finding an update, downloading it, and refusing bad signatures
- [ ] Third-party licenses: the MIT/Apache crates built into the app ask for their notices to ship with it. Generate a list (e.g. `cargo about`) and link it from Settings or include it in the installer
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
- [ ] Accessibility: list keyboard focus, ARIA roles for the message list and dialogs, arrow keys in the right-click menu
