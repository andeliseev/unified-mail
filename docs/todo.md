# To do / ideas for later

Nothing here is committed to; it's a list of things noticed while building v0.1. Rough priority order within each section.

## Not verified yet

- [ ] macOS: links, the right-click menu and keyboard shortcuts inside an email, now passed on by `macos.rs` because WebKit doesn't run the page's handlers in the sandboxed frame. Clicking a link did nothing there, and the right-click menu was WebKit's, with items that did nothing
- [ ] macOS: after an update, the app comes back under its kept-in-Dock icon (`macos::relaunch_after_exit`). Checked with a throwaway app kept in the Dock, which updated itself as the updater does (a new copy of its bundle swapped in) and then restarted: Tauri's `open -n` gave it a second Dock icon, and the wait-then-`open` brought it back under the kept one. A real update shows it first when updating from a version newer than 0.2.0
- [ ] Saving attachments from real mail: so far unit tests and a fake backend only. On macOS also the Downloads prompt, and whether it comes back after an update
- [ ] macOS: a wide image that arrives late refits the email only at the frame's `load`, because WebKit doesn't run the image `load` listener in the frame either. A `ResizeObserver` from the page might work there
- [ ] Notifications from the installed app: they should carry the app's name and icon, since the installer's shortcut registers the app's ID with Windows. Dev builds run from `target\` and show as "Windows PowerShell". Checked so far: toasts and the mail sound in the test app, the taskbar badge, and the second-launch handling. A click on a toast isn't checked yet
- [ ] macOS `.dmg` from the Release workflow: first run of a downloaded copy, with the Gatekeeper prompt. A local build isn't quarantined, so it never shows that prompt
- [ ] Sending against real Gmail: so far unit tests of the email `mime.rs` writes and a fake backend. To check with a test account: the upload endpoint with `threadId`; answers threading in Gmail and in another mail app; From with the Send-as name, and without one; Bcc kept from what recipients get; names and subjects outside ASCII; a 20 MB attachment; a forward's files and the quoted email's pictures; Undo, and quitting during the Undo time (`Cmd+Q`, closing the window on Windows)
- [ ] Drafts against real Gmail: creating and updating through the upload endpoint (`PUT …/drafts/{id}`), `drafts.send`, a draft opened here from one made in Gmail on the web (text above the quote, its answered email found by In-Reply-To), Sent and Drafts rows with their To, and whether `-in:drafts` keeps drafts out of All Mail and search
- [ ] macOS: scrolling the reader with `Space`, `PageDown` and the arrows after clicking into the email. `scrollMessage` scrolls only if WebKit didn't (80 ms later); Chromium was checked in the browser pane, WebKit not yet. Also wheel and trackpad scrolling over the email, now that the frame itself doesn't scroll
- [ ] Windows: the file picker (`<input type="file">`) in WebView2, and `Ctrl+Enter`
- [ ] macOS: the line under the pointer that says where a link goes (WebKit's private `_webView:mouseDidMoveOverElement:withFlags:userInfo:`, see [ui.md](ui.md#sandbox)) and links handed to the page to check (`email-link`). Checked in the browser pane, where the frame's own handlers run as on Windows; not yet in the app on macOS
- [ ] macOS: the sign-in hand-over across a real update (see [auth.md](auth.md#macos-keychain)). Checked with two builds of a test program on the login keychain, not yet from one release to the next: the first release with the hand-over still asks once per account, and the update after it shouldn't

## Gaps in current features

- [ ] Push only watches the Inbox; All Mail and Trash refresh with it, on focus and every 5 min
- [ ] More folders: Starred, Spam, user labels
- [ ] Reconnect against a real expired sign-in (checked with a fake backend only so far). One way: remove the app's access at myaccount.google.com/permissions for a test account
- [ ] An account whose sign-in expires mid-session shows Reconnect only after the next sync; opening a message from it before that shows the error as a toast
- [ ] **Push status** in the UI: show when an account's IDLE connection is down, instead of only logging `[push]`
- [ ] Older pages aren't re-synced: something archived elsewhere stays visible there until the folder is reopened or the app restarts
- [ ] Undo of a move reopens the message only once Gmail confirms: if another email or a new message was opened meanwhile, the late `select` takes the reader back
- [ ] Mark as unread right after opening a message, before its body arrives, is undone by the automatic mark-as-read when the body comes
- [ ] An account whose sign-in expires mid-session keeps `paging.more`, so the merged list stops at its frontier and "Couldn't load older messages" comes back at every scroll to the end
- [ ] A sync that was running when an account was removed brings its rows and unread mail back until the next sync (`applySync` in the page, `refresh` in Rust)
- [ ] After a sync removes the open message (archived on the phone, say), `J`/`K` start again at the top of the list
- [ ] Send waits for a save already on its way before `send_message` registers the email: with a slow upload, the Undo toast can run out first, and quitting then leaves the email unsent
- [ ] The Outbox's Undo: one that arrives before a big `send_message` has crossed IPC says "Too late to undo", and job numbers start again after a page reload. An email still uploading 20 s into quitting (`SEND_ON_QUIT`), or whose sending fails then, is dropped without a word
- [ ] Search results don't update by themselves: push and syncs don't run the search again, only Enter does
- [ ] Search leaves out Spam and Trash (`includeSpamTrash` off), so typing `in:trash` or `in:anywhere` probably finds nothing more. Trash results would also need Trash's actions
- [ ] Search: recent searches, or suggestions of senders while typing
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
- [ ] Drafts: quitting within 2 s of typing loses those last words, since the page can't save as it closes. Rust could ask the page for its drafts on `ExitRequested` (Windows), but `Cmd+Q` doesn't raise it
- [ ] Drafts with more than 5 MB of files save only after a minute without typing, or when left, since every save uploads the files again (Gmail keeps no draft's files between saves). Uploading the files once, as a draft, and the text in later saves isn't possible with the API
- [ ] A Gmail draft opened here loses its formatting (bold, links), and its pasted pictures become attachments, once it's saved again: the editor is plain text
- [ ] The Drafts count comes from the folder's cache until Drafts is opened, so drafts started on a phone count only after that; and only the newest 50 per account count
- [ ] Writing: suggest addresses while typing (senders in the loaded lists, or Gmail's contacts, which needs another permission); the Gmail signature (`sendAs.signature`); drag and drop files (Tauri takes file drops before the page, as `tauri://drag-drop` events with paths); pictures pasted into the text; bold, links and other formatting
- [ ] A conversation view would show answers under their email, and drafts under the email they answer, as Gmail does
- [ ] Sending from another Send-as address of the account
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

- [ ] Code signing: Windows (SmartScreen) and macOS notarization (Gatekeeper) need paid certificates, for macOS an Apple Developer ID. With one, the keychain would list every release by its team ID, so a version installed from a `.dmg` wouldn't ask for the password either, and the sign-in hand-over could go (see [auth.md](auth.md#macos-keychain)). The Mac app's self-signed certificate keeps the folder permissions across updates (see [development.md](development.md#mac-signing))
- [ ] The test app shares keychain items with the installed app (service `unified-mail`, keyed by email). A service of its own would keep a sign-in there from replacing the installed app's item
- [ ] The first real update: installed apps get the updater only from the first release that has it, so the update after that one is the first end-to-end check (Windows NSIS and MSI, and macOS, which is untested). Locally checked so far: finding an update, downloading it, and refusing bad signatures
- [ ] Third-party licenses: the MIT/Apache crates built into the app ask for their notices to ship with it. Generate a list (e.g. `cargo about`) and link it from Settings or include it in the installer
- [ ] **Google verification**, so users don't see the "unverified app" warning and the user cap goes away. Steps from Google's [restricted scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification):
  - Prove ownership of `andeliseev.github.io` in Google Search Console (a verification file in the site repo)
  - Home page and privacy policy with the Limited Use statement (done)
  - Justify the `https://mail.google.com/` scope: IMAP is needed for push. Google asks for the narrowest scope, so expect questions
  - Record an unlisted YouTube demo of the sign-in and of every feature that uses the scope
  - Security assessment (CASA): Google requires it for apps that access mail "from or through a third-party server". Unified Mail has no server, but whether that exempts it is decided in review. If required: an approved lab, paid, renewed yearly
- [ ] Linux: enable the `keyring` Secret Service feature, test on WebKitGTK

## Security hardening

From the review on 2026-11-13; none of it urgent.

- [ ] Release workflow: keep `TAURI_SIGNING_PRIVATE_KEY` away from the build itself (every build script and proc-macro can read it while `tauri build` runs), for example building with `--no-bundle` and bundling and signing in a step of its own, and delete the `.p12` right after signing. The actions are pinned to commits already, and the build job's checkout doesn't keep the write token
- [ ] `requireSignedVersion` in the updater's config, once the release signatures are checked to carry the version: without it, an edited `latest.json` could offer an older signed build as new
- [ ] Narrower Tauri permissions: the `core:*` permissions the page uses instead of `core:default`, an `AppManifest` command list in `build.rs`, and localhost navigation (`macos.rs`) only in debug builds. This matters only if script ever got into the page
- [ ] Email images: `img-src` allows `http:`, so an opened email can make requests to `127.0.0.1` or the router. Leaving `http:` out would also break old newsletters' pictures
- [ ] Sign-in: the loopback listener ends the sign-in on a request whose `state` doesn't match (an `?error=` one included), so anything that hits its port can cancel it. It could answer and skip such requests
- [ ] The macOS sign-in hand-over file: accept it only when fresh and for this build, and have the relaunch helper delete one the new version didn't pick up
- [ ] The webview's cache keeps opened emails' remote pictures and fonts (the privacy policy says so). It could be cleared when an account is removed, or from Settings

## Code quality

- [ ] Frontend tests: the fake `window.__TAURI__` approach from [development.md](development.md) could become a real test harness
- [ ] Integration tests for `gmail.rs` against a mock HTTP server
- [ ] Typed errors in Rust if the UI ever needs to react to specific errors (e.g. show Reconnect only for expired sign-ins)
- [ ] Accessibility: list keyboard focus, ARIA roles for the message list and dialogs, arrow keys in the right-click menu
- [ ] Report to Tauri that its macOS restart (`open -n` before the old copy quits) leaves a second Dock icon. `macos::relaunch_after_exit` can go once Tauri waits for the old copy
