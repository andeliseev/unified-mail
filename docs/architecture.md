# Architecture

Unified Mail is a [Tauri 2](https://v2.tauri.app/) app. A Rust process owns all network access, tokens and storage. The UI is static HTML/CSS/JS rendered by the operating system's webview (WebView2 on Windows, WKWebView on macOS). The UI talks to Rust only through Tauri commands and events.

```mermaid
flowchart LR
  subgraph Webview["Webview (src/)"]
    UI["app.js<br/>state, rendering, shortcuts"]
  end
  subgraph Rust["Rust process (src-tauri/src)"]
    Main["main.rs<br/>commands"]
    State["state.rs<br/>accounts, tokens, inbox cache"]
    Gmail["gmail.rs<br/>REST client"]
    OAuth["oauth.rs<br/>sign-in"]
    Idle["idle.rs<br/>IMAP IDLE watchers"]
  end
  UI -- invoke --> Main
  Idle -- "mail-changed event" --> UI
  Main --> State
  State --> Gmail
  State --> OAuth
  Idle --> State
  Gmail --> GAPI["gmail.googleapis.com"]
  OAuth --> GAUTH["accounts.google.com<br/>oauth2.googleapis.com"]
  Idle --> IMAP["imap.gmail.com:993"]
  Main -- "updater plugin" --> GH["github.com<br/>releases: latest.json"]
```

## Rust modules

| Module | Responsibility |
| --- | --- |
| `main.rs` | Tauri setup, command handlers, starting push watchers at launch, notifications and the icon badge |
| `state.rs` | `AppState`: OAuth client, account list, access-token cache, inbox cache, file persistence, keychain |
| `gmail.rs` | Gmail REST calls (list, metadata, full message, archive, trash, mark read/unread) and MIME body selection |
| `oauth.rs` | Installed-app OAuth: system browser, loopback redirect, PKCE, token exchange, refresh, revoke |
| `idle.rs` | One long-lived IMAP IDLE connection per account; emits `mail-changed` |
| `build.rs` | Embeds the Google OAuth client at compile time (see [auth.md](auth.md)) |

## Commands (UI → Rust)

All return `Result<_, String>`. The string is shown to the user as is, so error messages are written for people.

| Command | Args | Returns |
| --- | --- | --- |
| `get_setup` | | `{ builtInClient, clientId, accounts }` |
| `save_client` | `clientId, clientSecret` | Only used when no client is built in |
| `add_account` | | Email address; runs the browser sign-in and starts push |
| `remove_account` | `email` | Stops push, revokes the token, deletes it from the keychain |
| `cached_folder` | `folder` | Last saved snapshot of `inbox`, `all` or `trash`, for instant first paint |
| `refresh_folder` | `folder` | `{ messages, errors, more }`: each account's newest page; errors per account; `more[account]` says whether older mail exists |
| `load_older` | `folder, account, before` | `{ messages, more, inbox, warning }`: the account's next 50 messages older than `before` (ms); `inbox` lists which are in the Inbox (All Mail only) |
| `get_message` | `account, id` | Headers plus HTML/text body. It doesn't change the message; the UI then calls `set_unread` |
| `set_unread` | `account, id, unread` | Marks the message read (on opening, Mark as read) or unread (Mark as unread) |
| `archive_message` | `account, id` | Removes the `INBOX` label |
| `trash_message` | `account, id` | Moves the message to Trash; returns `{ wasInInbox }` for Undo |
| `restore_message` | `account, id, fromTrash, toInbox` | Takes it out of Trash and/or adds `INBOX`: Undo, and "Move to Inbox" |
| `open_external` | `url` | Opens `http(s):`/`mailto:` links in the default app |

Two more commands are for the [new-mail alerts](ui.md#notifications-and-the-icon-badge):

| Command | Args | Returns |
| --- | --- | --- |
| `notify` | `title, body, account?, id?` | Shows a desktop notification; clicking it opens that message |
| `set_badge` | `count, icon, size` | Unread count on the app icon: Dock badge on macOS; on Windows `icon` (RGBA, `size`×`size`, drawn by the UI) becomes the taskbar overlay icon |

Events (Rust → UI):
- `mail-changed`, with the account email as payload.
- `open-message`, `{ account, id }` or null, when a notification is clicked.

And two for [updates](development.md#updates):

| Command | Args | Returns |
| --- | --- | --- |
| `check_update` | | The newer version on GitHub, or null. The found update is kept in `PendingUpdate` |
| `install_update` | | Downloads it, checks its signature against `plugins.updater.pubkey`, installs it and restarts. On Windows the app exits and the installer starts it again |

Plugins:
- `tauri-plugin-notification`: Windows toasts and macOS notifications.
- `tauri-plugin-single-instance`: keeps one copy of the app running; launching it again just brings the window forward.
- `tauri-plugin-updater`: used only from Rust, so it needs no capability permissions.

## Storage

| Data | Where | Format |
| --- | --- | --- |
| Refresh tokens | OS keychain, service `unified-mail`, user = email | `keyring` crate |
| Access tokens | Memory only | Expire 60 s early; wall clock, see below |
| OAuth client (Settings fallback) | `<app data>/client.json` | JSON |
| Account list | `<app data>/accounts.json` | JSON array of emails |
| Folder snapshots | `<app data>/inbox-cache.json`, `all-cache.json`, `trash-cache.json` | JSON array of `MessageSummary` |

`<app data>` is `%APPDATA%\io.unifiedmail.desktop` on Windows and `~/Library/Application Support/io.unifiedmail.desktop` on macOS. The test app (`npm run dev`) uses `io.unifiedmail.devtest` instead (see [development.md](development.md#testing-without-your-own-mail)). Files are written to a `.tmp` file and then renamed, so a crash can't leave a half-written file.

## Design decisions

- **No frontend framework, no bundler.** `src/` is served as is (`frontendDist`). Fewer dependencies, nothing to build, tiny assets.
- **Use what the OS already ships.** Webview instead of bundled Chromium; `native-tls` (SChannel / Secure Transport) instead of rustls, for the updater too (its default features would add rustls and zip, +1.4 MB); the webview's `TextDecoder` for legacy charsets instead of `encoding_rs`. All of this keeps the binary small (see [development.md](development.md)).
- **Locks are never held across `.await`.** `AppState` uses `std::sync::Mutex`, locked only for quick in-memory reads and writes. Network calls happen outside the lock, and Tauri async commands must be `Send`.
- **Access-token expiry uses `SystemTime`, not `Instant`.** On macOS `Instant` stops while the machine sleeps, which would leave expired tokens looking valid.
- **Errors are `String`s.** Every failure ends up as a message in the UI, so a typed error enum would add code without adding value yet.
- **Message ids are checked.** They go into URL paths, so `gmail.rs` accepts only alphanumeric ids.
