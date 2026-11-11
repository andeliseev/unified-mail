# To do / ideas for later

Nothing here is committed to; it's a list of things noticed while building v0.1. Rough priority order within each section.

## Not verified yet

- [ ] macOS: links, the right-click menu and keyboard shortcuts inside an email, now passed on by `macos.rs` because WebKit doesn't run the page's handlers in the sandboxed frame. Clicking a link did nothing there, and the right-click menu was WebKit's, with items that did nothing
- [ ] macOS: after an update, the app comes back under its kept-in-Dock icon (`macos::relaunch_after_exit`). Checked with a throwaway app kept in the Dock, which updated itself as the updater does (a new copy of its bundle swapped in) and then restarted: Tauri's `open -n` gave it a second Dock icon, and the wait-then-`open` brought it back under the kept one. A real update shows it first when updating from a version newer than 0.2.0
- [ ] Saving attachments from real mail: so far unit tests and a fake backend only. On macOS also the Downloads prompt, and whether it comes back after an update
- [ ] macOS: a wide image that arrives late refits the email only at the frame's `load`, because WebKit doesn't run the image `load` listener in the frame either. A `ResizeObserver` from the page might work there
- [ ] Notifications from the installed app: they should carry the app's name and icon, since the installer's shortcut registers the app's ID with Windows. Dev builds run from `target\` and show as "Windows PowerShell". Checked so far: toasts and the mail sound in the test app, the taskbar badge, and the second-launch handling. A click on a toast isn't checked yet
- [ ] macOS `.dmg` from the Release workflow: first run of a downloaded copy, with the Gatekeeper prompt. A local build isn't quarantined, so it never shows that prompt
- [ ] The Release workflow's Mac signing (rcodesign through the `codesign` stand-in) hasn't run on GitHub yet (see [development.md](development.md#mac-signing)). Checked locally with rcodesign 0.29.0: test apps signed as Tauri signs verify on a Mac that doesn't trust the certificate, and a second build read the first one's keychain item without a prompt. Not checked: the download and the stand-in on the runner, the real app, and a real update

## Gaps in current features

- [ ] Push only watches the Inbox; All Mail and Trash refresh with it, on focus and every 5 min
- [ ] More folders: Sent, Starred, Spam, user labels
- [ ] Reconnect against a real expired sign-in (checked with a fake backend only so far). One way: remove the app's access at myaccount.google.com/permissions for a test account
- [ ] An account whose sign-in expires mid-session shows Reconnect only after the next sync; opening a message from it before that shows the error as a toast
- [ ] **Push status** in the UI: show when an account's IDLE connection is down, instead of only logging `[push]`
- [ ] Older pages aren't re-synced: something archived elsewhere stays visible there until the folder is reopened or the app restarts
- [ ] Search across all mail (Gmail `q=`), for finding old messages without scrolling
- [ ] Rendering re-creates every row; with many thousands of loaded rows, switch to a virtualized list
- [ ] Message-level list: add **conversation (thread) view** like Gmail's
- [ ] Sync only the account that sent `mail-changed`, not all accounts
- [ ] A sync that reads Gmail just before a move or read change lands, but arrives after it's confirmed, can briefly show the old state until the next sync
- [ ] The list's paperclip only looks inside `multipart/mixed` email, so files under another type show none: Apple Mail sends rich text with files as `multipart/alternative` around a `multipart/mixed`, and an email can be a single file. It also tells pictures shown in the text by their headers, not by the text's `cid:` links as the reader does, so a picture marked inline that the text doesn't show is listed in the reader with no paperclip. Opening the message tells for sure; the row (and its cached summary) could take the reader's answer
- [ ] Attachments: open one directly (Quick Look, the default app), not only save it
- [ ] Dark mode for emails: dark logos on transparent backgrounds stay hard to see; maybe a light backing behind small transparent images. Original could also be remembered per sender instead of per opened message
- [ ] Optional blocking of remote images (tracking pixels), with a "Load images" button
- [ ] Remember window size and position
- [ ] Own title bar on Windows: no Snap Layouts flyout on Maximize and no title bar right-click menu (Restore, Move, Size…). `tauri-plugin-decorum` brings Snap Layouts back, but it's another crate; check its size first
- [ ] Own title bar on Windows 10: Tauri says an undecorated window with a shadow gets a 1px white border there, which would show in dark mode. Only checked on Windows 11

## New features

- [ ] A setting to turn notifications off inside the app (Windows and macOS can already turn them off per app)
- [ ] Clicking an old notification in the Windows Action Center only brings the window forward, without opening the message. That needs a COM activator, which the notification plugin doesn't have
- [ ] The badge, like the sidebar, counts unread mail only among loaded messages (each account's newest 50 in the Inbox, plus older pages scrolled to). Gmail's `labels.get` for `INBOX` would give the exact count
- [ ] **Tray icon** on Windows: keep running in the background when the window is closed, as the app already does in the Dock on macOS
- [ ] macOS: once the window is closed, only the Dock icon or a notification brings it back. A Window menu item for it, like Mail's Window → Message Viewer, would add a keyboard way
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
- [ ] **Provider layer:** split `gmail.rs`/`state.rs` behind a common interface (list page, older page, get message, archive, trash, restore, mark read/unread), so the UI stays the same.
- [ ] **Privacy policy:** add Yandex (what's accessed, where it's stored).

## Distribution

- [ ] Code signing: Windows (SmartScreen) and macOS notarization (Gatekeeper) need paid certificates, for macOS an Apple Developer ID. The Mac app's self-signed certificate already keeps the keychain and folder permissions across updates (see [development.md](development.md#mac-signing))
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
- [ ] Report to Tauri that its macOS restart (`open -n` before the old copy quits) leaves a second Dock icon. `macos::relaunch_after_exit` can go once Tauri waits for the old copy
