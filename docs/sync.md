# Sync and push

Push tells the app *when* something changed. Sync, via the Gmail REST API, fetches *what* changed. IMAP is used only as the signal.

## Folders

The app shows five folders (`Folder` in `state.rs`), each unified across accounts:

| Folder | Gmail list query | Unread query |
| --- | --- | --- |
| `inbox` | `labelIds=INBOX` | `labelIds=INBOX&labelIds=UNREAD` |
| `sent` | `labelIds=SENT` | `labelIds=SENT&labelIds=UNREAD` |
| `drafts` | `labelIds=DRAFT`, plus `drafts.list` for the draft ids (below) | none: drafts are never unread |
| `all` (All Mail) | no label, `q=-in:drafts`: everything outside Spam and Trash, without drafts | `labelIds=UNREAD&q=-in:drafts` |
| `trash` | `labelIds=TRASH&includeSpamTrash=true` | `labelIds=TRASH&labelIds=UNREAD&includeSpamTrash=true` |

Each folder has its own cache (`<folder>-cache.json`). A message's summary doesn't depend on the folder, so a message already known from another folder isn't fetched again. The UI syncs the Inbox, which drives the unread counts, plus the folder on screen.

- **Sent and Drafts** rows show whom a message went to, so summaries carry its To header (`to`). Summaries saved before that was noted have none, and those two folders fetch such rows again (`Folder::shows_recipients`); other folders keep using them.
- **Drafts:** a Gmail draft keeps its id while every save gives it a new message, so the Drafts folder's rows also carry their draft id (`draft`), from `drafts.list` (`gmail::draft_ids`: message id → draft id, 500 per page, four pages at most). Opening, saving and deleting go by that id (see [ui.md](ui.md#writing)).
- **All Mail leaves drafts out**, as Gmail's own All Mail does: a draft is a message, and one being written is saved again and again. Searches are of All Mail, so they leave drafts out too. Gmail's `-in:drafts` does this (`Folder::query`); were Gmail not to know the word, it would leave nothing out instead of failing.

## Sync (`AppState::refresh`)

All accounts sync one folder in parallel (`join_all`). For each account:

1. Two list calls run in parallel, asking only for ids (`fields=messages/id`):
   - the folder's query, newest `PAGE_SIZE` (50)
   - its unread query, newest 50

   Any unread message among the newest 50 of a folder is also among its newest 50 unread messages. So the second call is enough to keep read state accurate for everything shown, without re-fetching metadata.
2. Ids already in any folder's cache reuse that summary. Only new ids get a `format=metadata` request (From, To, Subject, Content-Type, snippet, date), `FETCH_CONCURRENCY` (8) at a time.

   The list's paperclip (`hasFiles`) says whether the reader will list files for the message. Only `multipart/mixed` email can have them (`gmail::may_carry_files`): mail attaches files there, while pictures shown in the text come in `multipart/related`, which the reader doesn't list as files either. Not all of it has files, though: Steam's emails are `multipart/mixed` with none, and a mailing list adds its footer as a part of its own. So a `multipart/mixed` message gets a second request, for its parts' types, names and headers without their content (`get_outline`, `OUTLINE_FIELDS`, five levels deep), and `carries_files` looks for a part the reader would list: one with a file name, or a picture with a Content-ID, unless the text shows that picture in place. Without the text, its headers tell: pictures shown in place aren't marked `Content-Disposition: attachment`, while pictures sent as files are (Gmail gives those a Content-ID too), and an email without an HTML body shows nothing in place. Gmail's `has:attachment` search would count signature logos too, and might not have indexed mail that just arrived. If the second request fails, the flag stays `None`: the message still shows, and is fetched again at the next sync. Summaries saved before the flag, or by 0.2.4, which went by the type alone and wrote it as `hasAttachments`, are fetched again once.
3. The account's entries in that folder's cache are replaced with exactly its current contents. This also drops messages moved elsewhere.

When an account fails (network, expired sign-in), its cached messages stay and the failure comes back in `errors`, so one bad account never empties the unified view. An expired sign-in is flagged (`expired: true`) so the UI can offer Reconnect instead of an error.

Each list call also asks for `nextPageToken`, only to learn whether older mail exists. That comes back per account as `more`.

## Older pages (`AppState::older`)

Scrolling asks for one account's next page with `load_older(folder, account, before)`. That's the same two list calls with the Gmail search `q=before:<seconds>`:
- `before` is the oldest date already loaded, rounded **up** to a whole second, because search works in seconds. Messages from that same second come back again and the UI drops the duplicates.
- The unread trick holds for any such query: an unread message in this page is among the newest 50 unread messages before the same date.
- In All Mail a third call lists the newest 50 **Inbox** messages before that date. By the same argument it says exactly which of the page's messages are in the Inbox.

Older pages don't go into the caches. The caches stay one newest page per folder, so they don't grow, and the next launch starts from the newest page again. How the UI merges pages across accounts is in [ui.md](ui.md#older-mail).

## Search (`AppState::search`)

`search_messages(account, query, before)` is an older page of All Mail with Gmail's `q` set to what was typed (`gmail::search_query`): the same three list calls, for the results, their unread ones and their Inbox ones, with no label. So it searches All Mail without Spam and Trash, as Gmail's own search does, and the unread and Inbox arguments above hold for it too. What was typed goes to Gmail as it is, so Gmail's search words (`from:`, `has:attachment`, `OR`…) work.
- The first page has no `before`. As the results scroll, `before:` is added as for older pages. What was typed goes in parentheses first, so `before:` applies to all of it, `OR` included, and a quote left open is closed, so it can't take `before:` into the quoted words.
- Results aren't written to the caches or to disk. Summaries fetched for them, and for older pages, are kept in memory (`AppState::recent`, up to `RECENT_MAX`, then emptied and filled again), so searching again, narrowing a search or scrolling back doesn't fetch them again. Read state still comes from the unread call each time.
- How the UI shows the results is in [ui.md](ui.md#search).

A single message that fails to load is skipped; the rest of the account still syncs. The failure is reported once ("N messages couldn't be loaded: ...") and remembered in `reported_failures`, so it doesn't pop up again on every push. The message is retried on each sync and appears once it loads. Responses are parsed with `serde_json` directly, so errors name the field and position (e.g. ``missing field `x` at line 1 column 300``), and network errors include their cause chain. Response types require only fields Google always sends (`id`), because Google omits empty values, such as the `value` of an empty header. An entirely empty response body is read as `{}`: Gmail returned one for `messages.list` with `fields=messages/id`, most likely when nothing matched (e.g. no unread mail). That used to fail the whole account. After every sync the folder's snapshot is written to its cache file. The next launch paints the Inbox from it immediately (`cached_folder`) and then syncs.

**Cost:** a sync with no new mail is two small requests per account. Gmail's quota is 250 units/s per user; a list call costs 5 units, metadata 5.

Read state changes through `set_unread`, which removes or adds the `UNREAD` label: when a message is opened, and for Mark as read / Mark as unread. Once Gmail confirms, every folder's cache gets the new state. Once Gmail confirms a move, the message is dropped from the caches of the folders it left; the folders it joined pick it up at their next sync.

## Actions and Undo on Gmail

| Action | Gmail calls | Undo |
| --- | --- | --- |
| Archive | `modify` remove `INBOX` | `restore_message(fromTrash: false, toInbox: true)`: add `INBOX` |
| Delete | `minimal` get of `labelIds`, then `trash` | `restore_message(fromTrash: true, toInbox: wasInInbox)`; back in Sent too when `wasSent` |
| Move to Inbox (in Trash) | `untrash`, then `modify` add `INBOX` | `trash` |
| Move to Inbox (archived, in All Mail) | `modify` add `INBOX` | Archive |

Delete first reads the message's labels, so Undo knows whether it was in the Inbox. A message deleted from All Mail that had been archived goes back to All Mail, not the Inbox. Gmail doesn't document whether `untrash` restores `INBOX`, so Undo and Move to Inbox add it explicitly. Adding a label that's already there is harmless.

The app never permanently deletes mail; that is a promise in the privacy policy.

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

The app's own actions (archive, delete, mark read or unread) also produce IDLE lines, which trigger one extra sync. That's harmless, and it confirms the change.

## UI side

- `mail-changed` → sync after `PUSH_DEBOUNCE_MS` (300 ms). Gmail often sends several lines for one change, and this collapses them into one sync.
- After each Inbox sync, new unread mail may show a desktop notification (see [ui.md](ui.md#notifications-and-the-icon-badge)).
- Safety net: sync every `AUTO_REFRESH_MS` (5 min) while the window is visible, and on window focus if the last sync was more than 30 s ago.
- A sync requested while another is running is queued, never dropped. This matters right after adding an account.
- Moves Gmail hasn't confirmed yet are kept in `state.pending` and re-applied to sync results (`setList`), so a message can't flash back into the folder it just left. Read/unread changes do the same through `state.pendingRead`.
- Push only watches the Inbox. Sent, Drafts, All Mail and Trash also update on every push (the folder on screen syncs with the Inbox), on the 5-minute timer, and when you open them. Sending syncs too, and the Drafts folder's rows follow what's saved, sent and discarded here at once. The Drafts count comes from the folder's cache until it's opened, so a draft started on a phone counts once Drafts has synced.

## Tunables

| Constant | File | Value |
| --- | --- | --- |
| `PAGE_SIZE` | state.rs | 50 messages per account |
| `FETCH_CONCURRENCY` | state.rs | 8 |
| `RECENT_MAX` | state.rs | 2000 summaries kept in memory for search results and older pages |
| `SEARCH_DELAY_MS` | app.js | 500 ms of no typing before a search runs |
| `IDLE_CYCLE` | idle.rs | 5 min |
| `STEP_TIMEOUT` | idle.rs | 30 s |
| `MIN_BACKOFF` / `MAX_BACKOFF` | idle.rs | 5 s / 5 min |
| `AUTO_REFRESH_MS` | app.js | 5 min |
| `REFRESH_ON_FOCUS_AFTER_MS` | app.js | 30 s |
| `PUSH_DEBOUNCE_MS` | app.js | 300 ms |
| HTTP timeout | state.rs | 30 s |
