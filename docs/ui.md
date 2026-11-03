# UI

Plain HTML/CSS/JS in `src/`, with no framework and no build step. `withGlobalTauri` exposes `window.__TAURI__`, so there's no npm package for the API either.

| File | Contents |
| --- | --- |
| `index.html` | Three-pane layout (sidebar, list, reader) plus Settings, sign-in wait overlay, toast |
| `styles.css` | Theme tokens on `:root`, a dark palette under `prefers-color-scheme: dark`, layout |
| `app.js` | All behavior, in one file split into sections |

## Layout

`#app` is a three-column grid filling the window. Its single row is `minmax(0, 1fr)` on purpose: with the default `auto` row, a long message list makes the row as tall as the list. The panes then overflow the window (which has `overflow: hidden`), so neither the list nor the message can scroll and the reader's bottom is cut off. Each pane scrolls on its own: `#messages` for the list, the email iframe for the message.

## State and rendering

There is one `state` object in `app.js`. It holds the accounts, one message list per folder (`state.lists.inbox` / `all` / `trash`, unified, newest first), the folder on screen, and the account filter (`null` = all accounts). It also holds the selected/open message, the in-flight moves (`pending`) and read/unread changes (`pendingRead`), the current Undo, and sync flags.

The sidebar lists the folders, then the accounts. The two combine: "Trash · work@…" is that account's Trash. Unread counts always come from the Inbox list, as in Gmail.

Rendering is direct DOM work: `renderSidebar()` and `renderList()` rebuild their parts from `state`, using a `DocumentFragment` for the list. That's fast at this size (a few hundred rows). Untrusted text (sender, subject, snippet) is always set with `textContent`, never `innerHTML`. Each account gets a color from `PALETTE` by its position in the account list; the color is used for the row stripe, sidebar dot and chip.

## Reading a message

`select(key)` shows the headers right away from the summary, then calls `get_message`. A sequence counter (`openSeq`) discards answers that arrive after the user has already moved to another message. Once the body is shown, an unread message is marked read (see [Read and unread](#read-and-unread)).

### Bodies and charsets

Gmail returns body parts as base64url bytes. `gmail.rs` passes them through unchanged along with the charset from the part's `Content-Type`. That header can't be trusted on its own: Gmail may already have converted the bytes to UTF-8 while the header still names the original charset. A Russian Google email declaring KOI8-R came out garbled when decoded as KOI8-R. So `decodeBody()` works in two steps:
1. It first tries strict UTF-8 (`TextDecoder('utf-8', { fatal: true })`).
2. If the bytes aren't valid UTF-8, it decodes them with the declared charset, via `TextDecoder(charset)`, which covers windows-1251, KOI8-R, ISO-8859-x and the rest. Unknown labels fall back to UTF-8.

Text really in a legacy charset is almost never valid UTF-8, so step 2 still handles it. HTML is preferred. Plain text is escaped, wrapped in `<pre>`, and its URLs are turned into links.

### Sandbox

Email HTML is untrusted, so it renders in an iframe built with `srcdoc` and:

- `sandbox="allow-same-origin"` and **no** `allow-scripts`: scripts, forms and popups are all blocked. `allow-same-origin` is there only so the parent page can reach `contentDocument` to attach handlers. With scripts off, the email itself can't use it.
- A click handler on the frame's document cancels navigation and sends `http(s)`/`mailto` links to `open_external`, which opens them in the default browser. `<base target="_blank">` makes sure an unhandled link can't navigate the frame.
- `srcdoc` frames inherit the page's CSP (`tauri.conf.json`): images from `https:`/`http:`/`data:` are allowed and inline styles too. Tauri is told not to add nonces to `style-src` (`dangerousDisableAssetCspModification`), because a nonce would switch off `'unsafe-inline'`.
- The frame background is always white: emails assume white, even in dark mode.
- Some emails set `overflow: hidden` on `html` or `body`, which would make them impossible to scroll. The frame CSS forces `:root { overflow: auto !important }` and `:root > body { overflow: visible !important }`. `:root` outranks the emails' `html`/`body` selectors, so this wins even against their `!important`. Only an inline `style="...!important"` on the email's `<body>` could still override it.

## Actions

Which actions an open message offers depends on the folder, and in All Mail on whether the message is in the Inbox (`actionsFor`):

| Folder | Actions |
| --- | --- |
| Inbox | Archive, Delete |
| All Mail, message in the Inbox (shown with an **Inbox** tag) | Archive, Delete |
| All Mail, archived message | Move to Inbox, Delete |
| Trash | Move to Inbox |

Whether an All Mail message is in the Inbox comes from the Inbox list (`inInbox`), with no extra requests. Both lists hold each account's newest 50 messages, and every Inbox message is also in All Mail. So a message among an account's newest 50 in All Mail that's in the Inbox is always among its newest 50 in the Inbox. Older All Mail pages loaded by scrolling report their Inbox messages themselves (`inboxExtra`, see [Older mail](#older-mail)).

When an action leaves the message in the folder on screen (Archive and Move to Inbox in All Mail), it stays open and only its tag and buttons change. Otherwise the next message opens. `render()` also redraws the buttons, so they stay right after Undo or a failed action.

`ACTIONS` in `app.js` describes each action: the folder lists the message **leaves** and **joins**, the Gmail command, and how to **undo** it, given that command's result (Delete returns `wasInInbox`).

All actions are **optimistic** (`perform`):
1. The move is applied to the folder lists at once, the next message opens, and the command runs.
2. Until Gmail answers, the move sits in `state.pending` and is re-applied to every sync result.
3. On failure the move is reversed and an error toast replaces the Undo toast.

A message only joins lists that are already loaded; other lists pick it up at their next sync.

**Undo:** after an action, the toast shows an **Undo** button for 8 s (`UNDO_MS`), and `Z` presses it. Undo waits for the original command to finish, then performs the reverse move the same optimistic way. If the message was open and is back in the folder on screen, it opens again. Only the last action can be undone. Sync error toasts never replace an Undo toast; they show at the next sync instead.

Actions from the [right-click menu](#right-click-menu) work on that row's message (`act(kind, m)`), even if another message is open. The open message stays open, and Undo doesn't open the row's message.

## Read and unread

Opening a message marks it read, as in Gmail. **Mark as unread** (button in the reader, `Shift+U`, right-click menu) marks it unread again. It stays open, and stays unread until it's opened again. The button switches to **Mark as read** (`Shift+I`).

`setUnread` works like the actions: the new state shows at once, `set_unread` runs, and until Gmail answers the change sits in `state.pendingRead` and is re-applied to every sync result. If Gmail fails, the old state comes back with an error toast. Changes to one message are chained, so they reach Gmail in order. Without that, opening a message and quickly marking it unread could end up read.

## Right-click menu

Right-click shows the app's own menu (`onContextMenu`, `#menu`). The webview's menu (Back, Reload, Save as, Print, Share, Inspect) means nothing in this app, and it comes in the system's language. Its Share, for example, would share the app's internal page.

| Where | Items |
| --- | --- |
| Message row | Mark as read / unread, then the folder's actions (as in [Actions](#actions)). The row is outlined while the menu is open |
| Link in an email | Open link (`open_external`), Copy link |
| Selected text (email or app) | Copy |
| Text fields | The system's own Cut/Copy/Paste menu, as in every app |
| Anywhere else | Nothing |

Right-clicks in the email arrive at the frame's document; the frame's position is added to place the menu. The menu closes on any click, key, scroll, resize, or focus moving into the email. Copy uses `navigator.clipboard.writeText`, which needs the real click.

## Older mail

Each folder starts with every account's newest 50 messages. Older ones load as you scroll.

- `state.paging[folder][account]` holds two things for each account:
  - `frontier`: the oldest date loaded with no gaps above it.
  - `more`: whether older mail exists.
- **The list stops at the cut-off** (`visible()`), the *newest* frontier among accounts with more to load. Below it, that account's messages aren't loaded yet. Showing other accounts' older mail there would make it look like the busy account had none. Messages below the cut-off stay in the list, hidden until it moves down.
- **Loading** (`maybeLoadOlder` → `loadOlder`): when the list is within 400 px of its end, including right after a render of a short list, the account(s) at the cut-off get their next page (`load_older` with `before` = frontier). The frontier then moves back, and loading repeats while the list is still at its end. A failed load waits 15 s before retrying, and a page that brings nothing older stops that account, so a Gmail quirk can't cause a loop.
- **Syncs** (`applySync`) refresh only each account's newest page. Messages below it that scrolling loaded are kept, with the deeper frontier, as long as Gmail still reports more mail.
- **Inbox tags on older All Mail pages:** `load_older` also returns which messages are in the Inbox, using the same "newest 50 before the same date" argument as for unread. Those go into `state.inboxExtra`. Actions update it, and Inbox syncs drop entries they show have left the Inbox.

## Keyboard

| Key | Action |
| --- | --- |
| `J` / `↓` | Next message |
| `K` / `↑` | Previous message |
| `Space` / `PageDown` | Scroll the open message down (`Shift+Space` / `PageUp`: up) |
| `E` | Archive (Inbox only) |
| `#` / `Delete` | Delete, i.e. move to Trash (Inbox and All Mail) |
| `Shift+U` / `Shift+I` | Mark as unread / read |
| `Z` | Undo the last action, while its toast shows |
| `Esc` | Close Settings or the right-click menu |

The same handler is attached inside the email iframe (`onKey(e, true)`), so `J`/`K`/`E`/`#`/`Z` and `Shift+U`/`Shift+I` keep working after clicking into a message. Inside the message, arrows, `Space` and `PageDown` are left to the frame, so they scroll the message instead of changing it.

## Notifications and the icon badge

**Notifications** (`notifyNewMail`, after each Inbox sync):
- `state.newestSeen[account]` is the date of the account's newest Inbox message so far. Unread mail newer than that notifies.
- An account's first sync (at launch, or after adding it) only sets the mark. Mail that was already there doesn't notify, and neither does mail that arrived while the app was closed.
- Older mail that moves back into the Inbox (Undo, Move to Inbox, moved in Gmail web) is older than the mark, so it doesn't notify either.
- Nothing shows while the window is in front (`document.hasFocus()`).
- Each message gets its own notification: the sender as title, the subject as body, plus the account when there are several. More than `MAX_NOTIFICATIONS` (3) at once become one "N new messages" summary listing the senders.

Clicking a notification while it's on screen brings the window forward and sends `open-message`. `openFromNotification` then switches to the Inbox, drops an account filter that would hide the message, and opens it. On Windows, a click on an old notification in the Action Center isn't reported to the app. It launches the app instead, and the single-instance plugin turns that into bringing the window forward.

**Badge** (`renderBadge`, from `renderSidebar`): the Inbox's unread count across all accounts, the same number as the sidebar's Inbox with no filter. It's sent only when it changes.
- macOS shows it as a Dock badge.
- Windows has no badge numbers for desktop apps. `badgeIcon` draws a red disc with the count (up to "99+") on a canvas, `16 × devicePixelRatio` pixels, between 16 and 32. Its RGBA bytes become the taskbar button's overlay icon.

## Settings and first run

- With a built-in client, the OAuth section is hidden. First run shows the empty list with an **Add Gmail account** button.
- Without one, Settings opens on first run and can't be closed until a client is saved. Pasting the downloaded client JSON into the Client ID field fills both fields.
- **Remove** on an account asks for a second click to confirm. The app avoids `confirm()` dialogs.
- **Add account** shows a "finish signing in in your browser" overlay. Cancel only hides the overlay: the backend keeps waiting until its 5-minute timeout, and a sign-in that completes later is still added.
