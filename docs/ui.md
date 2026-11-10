# UI

Plain HTML/CSS/JS in `src/`, with no framework and no build step. `withGlobalTauri` exposes `window.__TAURI__`, so there's no npm package for the API either.

| File | Contents |
| --- | --- |
| `index.html` | Three-pane layout (sidebar, list, reader) plus Settings, sign-in wait overlay, toast |
| `styles.css` | Theme tokens on `:root`, a dark palette under `prefers-color-scheme: dark`, layout. Settings → Appearance changes what `prefers-color-scheme` reports, so the stylesheet needs no theme classes |
| `app.js` | All behavior, in one file split into sections |

## Layout

`#app` is a three-column grid filling the window. Its single row is `minmax(0, 1fr)` on purpose: with the default `auto` row, a long message list makes the row as tall as the list. The panes then overflow the window (which has `overflow: hidden`), so neither the list nor the message can scroll and the reader's bottom is cut off. Each pane scrolls on its own: `#messages` for the list, the email iframe for the message.

### Title bar

The window has no grey system title bar, so the icon and name appear once, at the top of the sidebar.

`initTitleBar` (when the user agent says Windows or Macintosh):
- adds `own-title-bar` to `<html>` and shows `#window-bar`, a 32 px strip at the top of the reader;
- makes the sidebar's brand, the list header and that strip drag areas (`data-tauri-drag-region="deep"`: anywhere inside drags, except on buttons). Dragging is the system's own, so on Windows dragging to a screen edge snaps. A double-click maximizes or restores; on macOS it zooms, on mouse-up, as the system does.

**Windows:**
- `main.rs` builds the window with `decorations(false)`. Tauri still resizes it from the edges (its undecorated-resize handler), keeps the shadow, and on Windows 11 the rounded corners.
- `initTitleBar` wires Minimize, Maximize / Restore Down and Close (`core:window:allow-minimize`, `allow-toggle-maximize`, `allow-close`, `allow-start-dragging` in `capabilities/default.json`).
- The buttons are 46 × 32 px with glyphs from the system's Segoe Fluent Icons (Windows 11) or Segoe MDL2 Assets (Windows 10), as in Windows' own title bars, so no icon font is bundled. Close turns red on hover. Maximize switches to Restore Down on every resize (`onResized` → `isMaximized`).
- They're `position: fixed` with a z-index above Settings, the sign-in overlay, the menu and the toast, so the window can always be closed.
- They dim while another window is in front (`onFocusChanged`, the window's focus). The page's own `blur` isn't used: it also fires when a click moves focus into the email frame.

Not available without the system title bar: the Snap Layouts flyout when hovering Maximize (Windows 11), and the title bar's right-click menu (Restore, Move, Size…). `Win+↑/↓/←/→` and dragging to the screen edges still snap.

**macOS:**
- `main.rs` keeps the system's window buttons but makes the title bar see-through over the page (`TitleBarStyle::Overlay`) and hides its title. The window keeps the title for Mission Control, the Window menu and VoiceOver.
- `traffic_light_position(18, 18)` puts the buttons over the sidebar, in line with the brand's icon and centered on the list header's row, whose 28 px start 12 px down. `.mac .brand` pads the brand down to leave them room, and `#window-buttons` stays hidden.
- `accept_first_mouse(true)`: the drag areas are in the page, so the first click on an inactive window has to reach the page for the window to drag at once, as with a system title bar. Other first clicks act at once too, as on Windows: clicking a message in an inactive window opens it.
- In full screen the system hides the buttons, so the room above the brand stays empty.

### Closing the window

On macOS, closing the window (its close button, `Cmd+W`, or Close Window in the File and Window menus) only hides it, as Mail does. The app stays in the Dock, so push keeps running and new mail still notifies. `Cmd+Q` quits.

- `on_event` in `main.rs` catches the window's `CloseRequested`, prevents the close and hides the window. `Cmd+Q` ends the app without asking the window to close, so it isn't caught.
- In full screen it hides the whole app instead, as `Cmd+H` does: a hidden full-screen window would leave its empty Space on screen.
- `show_window` brings it back, unhiding the app too. It runs on a click on the Dock icon (`RunEvent::Reopen` with no visible window), a click on a notification, a second launch, and a sign-in finishing in the browser.
- While the window is hidden the page counts as hidden, so the periodic refresh pauses (`document.hidden`). Push still syncs and notifies.

On Windows closing the window quits the app.

## State and rendering

There is one `state` object in `app.js`. It holds the accounts, one message list per folder (`state.lists.inbox` / `all` / `trash`, unified, newest first), the folder on screen, and the account filter (`null` = all accounts). It also holds the selected/open message, the in-flight moves (`pending`) and read/unread changes (`pendingRead`), the current Undo, and sync flags.

The sidebar lists the folders, then the accounts. The two combine: "Trash · work@…" is that account's Trash. Unread counts always come from the Inbox list, as in Gmail.

Rendering is direct DOM work: `renderSidebar()` and `renderList()` rebuild their parts from `state`, using a `DocumentFragment` for the list. That's fast at this size (a few hundred rows). Untrusted text (sender, subject, snippet) is always set with `textContent`, never `innerHTML`. Each account gets a color from `PALETTE` by its position in the account list; the color is used for the row stripe, sidebar dot and chip.

Dates are formatted in English (`en-US`), like the rest of the UI. The system's locale would mix in its language ("4 нояб."). The clock follows the system's hour cycle, 24-hour or AM/PM. The list shows a time for today, "Nov 4" for this year and "Nov 4, 2024" for older mail; the reader shows "Nov 4, 2026, 15:31".

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
- The frame's handlers (clicks, keys, right-click) and the first fit are set up as soon as the email is parsed (`whenParsed`, which polls until the frame's document is the email's `about:srcdoc` one). They don't wait for the frame's `load`, which waits for every image: one slow tracking pixel held it for 20+ s, leaving the email unfitted and its links dead until then. Timers rather than `requestAnimationFrame`, which pauses while the window is minimized.
- A click handler on the frame's document cancels navigation and sends `http(s)`/`mailto` links to `open_external`, which opens them in the default browser. `<base target="_blank">` makes sure an unhandled link can't navigate the frame; links' own `target` attributes are removed, so it applies to every link.
- **On macOS WebKit doesn't run these handlers.** It counts the page's listeners on the frame's document as that frame's scripts, which the sandbox forbids, so none of them fire there. Chromium runs them. `macos.rs` passes things on from the webview instead:
  - The frame also gets `allow-popups`, so a link click asks for a new window. `on_navigation` sees that request and opens http(s) and mailto links with `open_external` instead. The same handler keeps the whole webview on the app's own pages.
  - `willOpenMenu:withEvent:` empties WebKit's menu, except in text fields, and sends `email-context-menu` with the click's position. `onEmailContextMenu` finds the element with `elementFromPoint`, since the page can still read the frame's DOM, and shows the app's menu. It then focuses the page, so a click in the email closes the menu.
  - `keyDown:` sends every key without Cmd, Ctrl or Option as `email-key`. `onEmailKey` uses it only while the email has focus. The frame's own `keydown` handler isn't attached on macOS, so no key acts twice if WebKit ever runs it.
  - The image `load` listener doesn't fire either, so on macOS a wide image that arrives late refits the email only at the frame's `load`.
- `srcdoc` frames inherit the page's CSP (`tauri.conf.json`): images from `https:`/`http:`/`data:` are allowed and inline styles too. Tauri is told not to add nonces to `style-src` (`dangerousDisableAssetCspModification`), because a nonce would switch off `'unsafe-inline'`.
- In light mode the frame is white, as emails expect. In dark mode see [Dark mode for emails](#dark-mode-for-emails).
- Some emails set `overflow: hidden` on `html` or `body`, which would make them impossible to scroll. The frame CSS forces `:root { overflow: auto !important }` and `:root > body { overflow: visible !important }`. `:root` outranks the emails' `html`/`body` selectors, so this wins even against their `!important`. Only an inline `style="...!important"` on the email's `<body>` could still override it.
- **Wide emails are scaled to fit.** Newsletters and receipts are often laid out at a fixed 600–700 px, wider than the reader at the default window size (about 610 px). Instead of cutting them off with a sideways scroll bar, `fitToFrame` measures the email and sets CSS `zoom` on its `<body>` to fit, as Apple Mail and Outlook do. It never goes below `MIN_EMAIL_ZOOM` (0.6), so text stays readable; anything wider than that scrolls. It runs:
  - once the email is parsed;
  - again after each image arrives, since an image can widen the layout (`load` captured on the document, coalesced by `scheduleFit`);
  - once more on the frame's `load`;
  - when the window is resized (a `ResizeObserver` on `#m-body`). `zoom` changes the layout size, unlike `transform`, so the scroll bar really goes away, and clicks, link targets and the right-click menu's position stay correct.
- HTML emails bring their own margins, so the frame adds only 8 px at the sides. Plain text gets 24 px, through `pre.plain`'s padding.

### Dark mode for emails

While the app is dark (`appIsDark()`), emails are shown dark too (`setEmailColors`, run when the email is parsed). The frame stays hidden until its colors are set, so it doesn't flash white:

1. **The email's own dark styles win.** `adjustColorSchemeRules` looks for `prefers-color-scheme: dark` in its CSS and in `<picture><source media>`. If it has any, they apply as written: the frame reports the app's dark preference. The frame adds only dark defaults (`FRAME_DARK_CSS`) behind it.
2. **Otherwise the email is darkened** (`darkenEmail`). For each element:
   - light backgrounds and borders turn dark, keeping their hue (white becomes `#1f1f1f`, the frame's dark background);
   - dark text turns light;
   - saturated mid tones (buttons, banners) and images stay as they are.
   
   "Light" and "dark" go by luminance, not HSL lightness, so a strong blue like `#1a73e8` counts as a color to keep. New values are set inline with `!important`, which beats the email's CSS.
3. **Original / Dark** (`#colors`, shown only in dark mode) redraws the email in its original light colors and back (`state.originalColors`, reset for each message). The frame can't be told to prefer light, since its `prefers-color-scheme` follows the app whatever its `color-scheme` says. So Original rewrites the email's own rules instead: `prefers-color-scheme: dark` becomes `max-width: 0px` (never) and `light` becomes `min-width: 0px` (always). Only the email's CSS changes, never its text.

A theme change (Appearance, or the system with System) redraws the open email.

Known weak spot: a dark logo on a transparent background (a black wordmark PNG) stays dark on the darkened background. Original is the way out.

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

Actions from the [right-click menu](#right-click-menu) and the [row buttons](#row-buttons) work on that row's message (`act(kind, m)`), even if another message is open. The open message stays open, and Undo doesn't open the row's message. If the row is the open message, the next one opens, as with the reader's buttons.

### Row buttons

The row under the mouse shows small icon buttons in place of its time: the folder's actions, then Mark as read / unread, in the reader's order. A click acts on that row's message without opening it (`onRowAction`).

- `renderList` clones them from `<template id="row-actions">` for each row (`rowActions`) and keeps the folder's actions. Move to Inbox stands for both `restore` (Trash) and `unarchive` (All Mail), like the reader's button. In All Mail it passes the Inbox keys it already built for the tags, so `actionsFor` doesn't search the Inbox list once per row.
- The read button shows an open envelope for Mark as read and a closed one for Mark as unread, switched by the row's `unread` class. `showUnread` updates its tooltip.
- The buttons share the row's background (`background: inherit`, hover or selected), so a long sender name runs under them. Delete turns red on hover, like the reader's Delete.
- They're `tabindex="-1"`, so a list of hundreds of rows doesn't add hundreds of Tab stops. The keyboard has the same actions as shortcuts.

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

Right-clicks in the email arrive at the frame's document, or on macOS through `email-context-menu` (see [Sandbox](#sandbox)); the frame's position is added to place the menu. The menu closes on any click, key, scroll, resize, or focus moving into the email. Copy uses `navigator.clipboard.writeText`, which needs the real click.

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

The Delete button's tooltip names `Del`, which everyone has; `#` is Gmail's key for it, kept for Gmail habits.

Shortcuts work in any keyboard layout (`shortcutKey`). With a Latin layout they go by the character typed (`e.key`), so Dvorak and AZERTY users get their own `J`, `K`, `E`. With any other layout, such as Russian, the letters type something else (`J` is `о`), so they go by the key's position (`e.code`, `KeyJ`), and `Shift+3` (Russian `№`) counts as `#`.

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

- **Appearance:** System, Light or Dark, as a three-way switch (`#theme`). `set_theme` sets the window's theme and saves it in `settings.json`.
  - The window's theme decides the macOS title bar and what the page's `prefers-color-scheme` reports. On Windows Tauri passes it on to WebView2; on macOS the page follows the window. So the CSS dark palette follows it as is.
  - At launch Rust creates the window itself, already in the saved theme (`"create": false` in the config). A theme set on a window that already exists reaches the page but not the Windows title bar. That was the reason; since the app draws its own title bar on Windows, it also builds the window without one there (see [Title bar](#title-bar)).
  - The email frame stays white either way.

- With a built-in client, the OAuth section is hidden. First run shows the empty list with an **Add Gmail account** button.
- Without one, Settings opens on first run and can't be closed until a client is saved. Pasting the downloaded client JSON into the Client ID field fills both fields.
- **Remove** on an account asks for a second click to confirm. The app avoids `confirm()` dialogs.
- **Add account** shows a "finish signing in in your browser" overlay. Cancel only hides the overlay: the backend keeps waiting until its 5-minute timeout, and a sign-in that completes later is still added. **Reconnect** works the same way (`signIn(account)`).
- An account whose sign-in expired also has a **Reconnect** button next to Remove (see [Expired sign-ins](#expired-sign-ins)).

### Expired sign-ins

When Google stops accepting an account's sign-in (revoked access, a changed password, or a Testing project's 7-day limit), syncs report it with `expired: true` (see [auth.md](auth.md#tokens)). Each sync covers every account, so `state.expired` is replaced with that sync's list. For each expired account:

- a line above the message list (`#reconnect`, `renderReconnect`), in the Update button's orange: "Sign-in expired for X" and a **Reconnect** button;
- "!" in place of its unread count in the sidebar, with a tooltip saying why;
- **Reconnect** next to Remove in Settings.

Reconnect opens Google's sign-in with the account preselected (`reconnect_account`). On success the line goes away, push restarts with the new token, and the folders sync. Expired accounts no longer get the error toast after every sync; other sync problems still do.
- **Updates:** `checkForUpdate` runs 10 s after launch (after the first sync), every 6 hours (`UPDATE_CHECK_MS`), and every time Settings opens, so a release published while the app is open shows up there. When a newer version exists:
  - an orange **Update to X** button (`--update`) appears next to the version, and the sidebar's Settings button gets an orange dot;
  - clicking the button shows "Updating…" and runs `install_update`; a failure shows a toast, brings the button back and checks again;
  - check failures (offline, nothing released yet, the test app's empty endpoint list) stay quiet.
- At the bottom (`#about`), three lines:
  - the app's version, from Tauri's `getVersion()` (the version in `tauri.conf.json`, which the Release workflow sets), with the update controls;
  - links to GitHub, Releases, the website, the privacy policy and the Support page, opened in the browser like the other `data-href` links;
  - the copyright line from `LICENSE`.
