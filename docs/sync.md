# Sync and push

Push tells the app *when* something changed. Sync, via the Gmail REST API, fetches *what* changed. IMAP is used only as the signal.

## Sync (`AppState::refresh_inbox`)

All accounts sync in parallel (`join_all`). For each account:

1. Two list calls run in parallel, asking only for ids (`fields=messages/id`):
   - `labelIds=INBOX`, newest `PAGE_SIZE` (50)
   - `labelIds=INBOX&labelIds=UNREAD`, newest 50

   Any unread message among the newest 50 inbox messages is also among the newest 50 unread inbox messages. So the second call is enough to keep read state accurate for everything shown, without re-fetching metadata.
2. Ids already in the cache reuse their cached summary. Only new ids get a `format=metadata` request (From, Subject, snippet, date), `FETCH_CONCURRENCY` (8) at a time.
3. The account's cache entries are replaced with exactly its current inbox. This also drops messages archived or deleted elsewhere.

When an account fails (network, expired sign-in), its cached messages stay and the failure comes back in `errors`, so one bad account never empties the unified inbox.

A single message that fails to load is skipped; the rest of the account still syncs. The failure is reported once ("N messages couldn't be loaded: ...") and remembered in `reported_failures`, so it doesn't pop up again on every push. The message is retried on each sync and appears once it loads. Responses are parsed with `serde_json` directly, so errors name the field and position (e.g. ``missing field `x` at line 1 column 300``), and network errors include their cause chain. Response types require only fields Google always sends (`id`), because Google omits empty values, such as the `value` of an empty header. An entirely empty response body is read as `{}`: Gmail returned one for `messages.list` with `fields=messages/id`, most likely when nothing matched (e.g. no unread mail). That used to fail the whole account. After every sync the snapshot is written to `inbox-cache.json`. The next launch paints it immediately (`cached_inbox`) and then syncs.

**Cost:** a sync with no new mail is two small requests per account. Gmail's quota is 250 units/s per user; a list call costs 5 units, metadata 5.

Opening a message also marks it read: `get_message` starts a background `UNREAD` removal and updates the cache. Archive and trash remove the entry from the cache once Gmail confirms.

## Push (`idle.rs`)

Each account keeps one TLS connection to `imap.gmail.com:993`:

1. Authenticate with SASL `XOAUTH2` using the account's access token. When a login fails, Gmail sends a JSON error as a second challenge; the authenticator answers it with an empty line so the exchange ends instead of hanging.
2. `SELECT INBOX`, then `IDLE`.
3. Wait up to `IDLE_CYCLE` (5 min):
   - **Any server line** (new message, expunge, flag change): emit `mail-changed`, send `DONE`, and IDLE again.
   - **Timeout:** `DONE` and IDLE again. Gmail ends IDLE after about 29 min, and the periodic round trip also exposes connections that died silently, e.g. while the computer slept.
   - **Stream ended:** the server closed the connection; reconnect.
4. Every IMAP step has a 30 s timeout (`STEP_TIMEOUT`).

When a connection fails, the watcher logs `[push] <account>: <reason>` to stderr and reconnects. The wait starts at 5 s, doubles up to 5 min, and resets after a healthy IDLE cycle. After a reconnect it emits `mail-changed` once, in case something changed while it was offline.

Watchers live in `idle::Watchers` (`account → JoinHandle`). They start for every saved account at launch and after `add_account` (which replaces any running watcher), and stop on `remove_account`.

The app's own actions (archive, delete, mark read) also produce IDLE lines, which trigger one extra sync. That's harmless, and it confirms the change.

## UI side

- `mail-changed` → sync after `PUSH_DEBOUNCE_MS` (300 ms). Gmail often sends several lines for one change, and this collapses them into one sync.
- Safety net: sync every `AUTO_REFRESH_MS` (5 min) while the window is visible, and on window focus if the last sync was more than 30 s ago.
- A sync requested while another is running is queued, never dropped. This matters right after adding an account.
- Messages being archived or deleted are kept in `state.pending` and filtered out of sync results, so they can't flash back into the list.

## Tunables

| Constant | File | Value |
| --- | --- | --- |
| `PAGE_SIZE` | state.rs | 50 messages per account |
| `FETCH_CONCURRENCY` | state.rs | 8 |
| `IDLE_CYCLE` | idle.rs | 5 min |
| `STEP_TIMEOUT` | idle.rs | 30 s |
| `MIN_BACKOFF` / `MAX_BACKOFF` | idle.rs | 5 s / 5 min |
| `AUTO_REFRESH_MS` | app.js | 5 min |
| `REFRESH_ON_FOCUS_AFTER_MS` | app.js | 30 s |
| `PUSH_DEBOUNCE_MS` | app.js | 300 ms |
| HTTP timeout | state.rs | 30 s |
