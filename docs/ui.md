# UI

Plain HTML/CSS/JS in `src/`, with no framework and no build step. `withGlobalTauri` exposes `window.__TAURI__`, so there's no npm package for the API either.

| File | Contents |
| --- | --- |
| `index.html` | Three-pane layout (sidebar, list, reader) plus Settings, sign-in wait overlay, toast |
| `styles.css` | Theme tokens on `:root`, a dark palette under `prefers-color-scheme: dark`, layout. Settings → Appearance changes what `prefers-color-scheme` reports, so the stylesheet needs no theme classes |
| `app.js` | All behavior, in one file split into sections |

## Layout

`#app` is a three-column grid filling the window. Its single row is `minmax(0, 1fr)` on purpose: with the default `auto` row, a long message list makes the row as tall as the list. The panes then overflow the window (which has `overflow: hidden`), so neither the list nor the message can scroll and the reader's bottom is cut off. Each pane scrolls on its own: `#messages` for the list, and for the message `#m-scroll`, which holds its header, the email and the reply under it (see [One scroll](#one-scroll)).

The columns are `230px minmax(250px, var(--list-width, 400px)) minmax(400px, 1fr)`:
- **The list is as wide as it was dragged.** `#list-resize`, an 8px strip over its right border, drags it between `LIST_MIN` (250) and `LIST_MAX` (600) px (`initListResize`), never squeezing the reader below `READER_MIN`, so the saved width is the one seen. It's saved with `set_list_width` in `settings.json` and applied at launch. A double-click goes back to the default, 400px.
- While dragging, `.resizing` on `<html>` keeps the email frame from taking the mouse and the page from being selected. The drag uses pointer capture, so it follows the mouse over the email too.
- **Narrowing the window** takes the reader down to 400px first, then the list down to 250px. The grid gives the list its width before the reader's `1fr` gets the rest. 400px is where a 600px newsletter still fits at `MIN_EMAIL_ZOOM` (0.6) with the frame's margins. 230 + 250 + 400 = 880px, the window's `minWidth` (`tauri.conf.json`, `tauri.dev.conf.json`).

### Title bar

The window has no grey system title bar, so the icon and name appear once, at the top of the sidebar.

`initTitleBar` (when the user agent says Windows or Macintosh):
- adds `own-title-bar` to `<html>`; on Windows it shows `#window-bar`, a 32 px strip at the top of the reader that holds the window buttons;
- makes the sidebar's brand, the list header and the strip above the message drag areas (`data-tauri-drag-region="deep"`: anywhere inside drags, except on buttons). That strip is `#window-bar` on Windows and the message's own button row (`.msg-actions`, a new message's too) on macOS. Dragging is the system's own, so on Windows dragging to a screen edge snaps. A double-click maximizes or restores; on macOS it zooms, on mouse-up, as the system does.

**Windows:**
- `main.rs` builds the window with `decorations(false)`. Tauri still resizes it from the edges (its undecorated-resize handler), keeps the shadow, and on Windows 11 the rounded corners.
- `initTitleBar` wires Minimize, Maximize / Restore Down and Close (`core:window:allow-minimize`, `allow-toggle-maximize`, `allow-close`, `allow-start-dragging` in `capabilities/default.json`).
- The buttons are 46 × 32 px with glyphs from the system's Segoe Fluent Icons (Windows 11) or Segoe MDL2 Assets (Windows 10), as in Windows' own title bars, so no icon font is bundled. Close turns red on hover. Maximize switches to Restore Down on every resize (`onResized` → `isMaximized`).
- They're `position: fixed` with a z-index above Settings, the sign-in overlay, the menu and the toast, so the window can always be closed.
- They dim while another window is in front (`onFocusChanged`, the window's focus). The page's own `blur` isn't used: it also fires when a click moves focus into the email frame.

Not available without the system title bar: the Snap Layouts flyout when hovering Maximize (Windows 11), and the title bar's right-click menu (Restore, Move, Size…). `Win+↑/↓/←/→` and dragging to the screen edges still snap.

**macOS:**
- `main.rs` keeps the system's window buttons but makes the title bar see-through over the page (`TitleBarStyle::Overlay`) and hides its title. The window keeps the title for Mission Control, the Window menu and VoiceOver.
- One line runs 26 px down the top of the window: the window buttons, the list header's row (its 28 px start 12 px down) and the message's buttons (`.mac .msg-actions` pads them 10 px down; there's no `#window-bar` above them). `traffic_light_position(18, 28)` puts the window buttons on it, over the sidebar, the close button starting 18 px in: AppKit puts them 9 px above the bottom of a title bar as tall as they are (14 px) plus the inset, so their center is the inset minus 2. Measured in the app; the inset of 18 used before left them centered 16 px down, above the line. `.mac .brand` pads the brand 13 px below them (46 px). Its icon's blue square starts 14 px in, a little left of the close button (`.brand` pads the 28 px icon 2 px in, and the square starts 2/24 into it).
- The see-through title bar is as tall as that (42 px), over the top of the page, and clicks under it reach the page: the message's buttons and the list's refresh button work in their top half too (checked in a WebKit window set up like the app's).
- `accept_first_mouse(true)`: the drag areas are in the page, so the first click on an inactive window has to reach the page for the window to drag at once, as with a system title bar. Other first clicks act at once too, as on Windows: clicking a message in an inactive window opens it.
- In full screen the system hides the buttons, so the brand moves up into their room, as on Windows (`.mac.full-screen .brand`). `on_event` passes every resize to `macos::on_resized`, which reads the window's style mask in AppKit and sends `full-screen` (`true` or `false`) when it changes; `initTitleBar` sets `full-screen` on `<html>` from it. The mask changes as soon as the switch starts, with its first resize. Tauri's `isFullscreen` keeps saying full screen until the animation leaving it ends, about 0.6 s later, so it isn't used on resize; a page that loads in full screen asks it once.

### Closing the window

On macOS, closing the window (its close button, `Cmd+W`, or Close Window in the File and Window menus) only hides it, as Mail does. The app stays in the Dock, so push keeps running and new mail still notifies. `Cmd+Q` quits.

- `on_event` in `main.rs` catches the window's `CloseRequested`, prevents the close and hides the window. `Cmd+Q` ends the app without asking the window to close, so it isn't caught.
- In full screen it hides the whole app instead, as `Cmd+H` does: a hidden full-screen window would leave its empty Space on screen.
- `show_window` brings it back, unhiding the app too. It runs on a click on the Dock icon (`RunEvent::Reopen` with no visible window), a click on a notification, a second launch, and a sign-in finishing in the browser.
- While the window is hidden the page counts as hidden, so the periodic refresh pauses (`document.hidden`). Push still syncs and notifies.

On Windows closing the window quits the app.

## State and rendering

There is one `state` object in `app.js`. It holds the accounts, one message list per folder (`state.lists.inbox` / `all` / `trash`, unified, newest first) plus the search's results (`state.lists.search`, see [Search](#search)), the folder chosen in the sidebar, and the account filter (`null` = all accounts). It also holds the selected/open message, the in-flight moves (`pending`) and read/unread changes (`pendingRead`), the current Undo, the messages being written (`drafts`, `composing`, `replies`, see [Writing](#writing)), and sync flags.

The sidebar lists the folders (Inbox, Sent, Drafts, All Mail, Trash), then the accounts. The two combine: "Trash · work@…" is that account's Trash. Unread counts always come from the Inbox list, as in Gmail; Drafts shows how many drafts there are. Sent rows show whom each email went to ("To: Anna Petrova"), from its To header (`recipientNames`).

Rendering is direct DOM work: `renderSidebar()` and `renderList()` rebuild their parts from `state`, using a `DocumentFragment` for the list. That's fast at this size (a few hundred rows). Untrusted text (sender, subject, snippet) is always set with `textContent`, never `innerHTML`. Each account gets a color from `PALETTE` by its position in the account list; the color is used for the row stripe, sidebar dot and chip.

A row whose message has files the reader will list (`hasFiles`, from its parts: see [sync.md](sync.md#sync-appstaterefresh)) shows a paperclip (`CLIP_ICON`) next to its time. Under the mouse the row's buttons take that place, so the paperclip hides with the time.

Dates are formatted in English (`en-US`), like the rest of the UI. The system's locale would mix in its language ("4 нояб."). The clock follows the system's hour cycle, 24-hour or AM/PM. The list shows a time for today, "Nov 4" for this year and "Nov 4, 2024" for older mail; the reader shows "Nov 4, 2026, 15:31".

## Reading a message

`select(key)` shows the headers right away from the summary, then calls `get_message`. A sequence counter (`openSeq`) discards answers that arrive after the user has already moved to another message. Once the body is shown, an unread message is marked read (see [Read and unread](#read-and-unread)).

### One scroll

The message's buttons (`.msg-actions`: Archive, Delete…) stay at the top. Under them `#m-scroll` scrolls the rest as one page, as on the Gmail website: the subject and sender, the attachments, the email, and at its end Reply / Reply all / Forward or the reply being written (`#m-end`, see [Writing](#writing)). So the whole email stays readable above a reply.

- **The frame is as tall as the email** (`fitHeight`). An email's height can't be read in a frame taller than it: `scrollHeight` is then the frame's. So the frame is squeezed to nothing, measured and set again in one go, before anything is drawn, and the reader keeps its scroll position. That runs with every fit (`fitToFrame`: once the email is parsed, as images arrive on Windows, at the frame's `load`, and when the reader's width changes).
- **Pictures arriving** make an email taller, and on macOS their `load` events don't reach the page (see [Sandbox](#sandbox)). `watchHeight` checks every 250 ms until a second after the frame's `load` (30 s at most) and grows the frame when the email overflows it, which needs no squeeze.
- A sideways scroll bar, on an email too wide to shrink, takes room at the frame's bottom; the height includes it.
- **Keys:** `Space`, `PageUp`/`PageDown`, the arrows and `Home`/`End` in the email scroll the reader (`scrollMessage`), since the frame has nothing to scroll and Chromium doesn't pass a frame's key scrolling on to the page around it (checked in the browser pane). On macOS WebKit gets the key as well (`email-key`, see [Sandbox](#sandbox)); it may scroll the reader itself, so `scrollMessage` only scrolls if the reader hasn't moved 80 ms later. Outside the email, `Space` and `PageUp`/`PageDown` scroll the reader too, and the arrows go through the list.

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
- The frame is as tall as the email and doesn't scroll up and down itself; the reader does (see [One scroll](#one-scroll)). The frame CSS sets `:root { overflow-x: auto !important; overflow-y: hidden !important }` and `:root > body { overflow: visible !important }`, so an email too wide to shrink still scrolls sideways. Some emails set `overflow` on `html` or `body`; `:root` outranks their `html`/`body` selectors, so this wins even against their `!important`. Only an inline `style="...!important"` on the email's `<body>` could still override it.
- **Wide emails are scaled to fit.** Newsletters and receipts are often laid out at a fixed 600–700 px, wider than the reader at the default window size (about 610 px). Instead of cutting them off with a sideways scroll bar, `fitToFrame` measures the email and sets CSS `zoom` on its `<body>` to fit, as Apple Mail and Outlook do. It never goes below `MIN_EMAIL_ZOOM` (0.6), so text stays readable; anything wider than that scrolls. It runs:
  - once the email is parsed;
  - again after each image arrives, since an image can widen the layout (`load` captured on the document, coalesced by `scheduleFit`);
  - once more on the frame's `load`;
  - when the window is resized (a `ResizeObserver` on `#m-body`). `zoom` changes the layout size, unlike `transform`, so the scroll bar really goes away, and clicks, link targets and the right-click menu's position stay correct.
- HTML emails bring their own margins, so the frame adds only 8 px at the sides. Plain text gets 24 px, through `pre.plain`'s padding.

### Dark mode for emails

While the app is dark (`appIsDark()`), emails are shown dark too (`setEmailColors`, run when the email is parsed). The frame stays invisible (`opacity: 0`) until its colors are set, so it doesn't flash white. Not `visibility: hidden`: in WebKit (macOS) a frame shown after that doesn't scroll by trackpad or mouse wheel, only by dragging its scroll bar, until something lays it out again (an image arriving, a resize).

The frame's scroll bar follows the email. A darkened email has `color-scheme: dark` (`FRAME_DARK_CSS`), so the thumb is light, as in the message list. On macOS the frame's scroll bar is only a see-through thumb over the frame's own background, which is white in the CSS, so a darkened email's frame gets the email's dark background (`EMAIL_DARK_BACKGROUND`, `#1f1f1f`) behind it.

How an email is darkened:

1. **The email's own dark styles win.** `adjustColorSchemeRules` looks for `prefers-color-scheme: dark` in its CSS and in `<picture><source media>`. If it has any, they apply as written: the frame reports the app's dark preference. The frame adds only dark defaults (`FRAME_DARK_CSS`) behind it.
2. **Otherwise the email is darkened** (`darkenEmail`). For each element:
   - light backgrounds and borders turn dark, keeping their hue (white becomes `#1f1f1f`, the frame's dark background);
   - dark text turns light;
   - saturated mid tones (buttons, banners) and images stay as they are.
   
   "Light" and "dark" go by luminance, not HSL lightness, so a strong blue like `#1a73e8` counts as a color to keep. New values are set inline with `!important`, which beats the email's CSS.
3. **Original / Dark** (`#colors`, shown only in dark mode) redraws the email in its original light colors and back (`state.originalColors`, reset for each message). The frame can't be told to prefer light, since its `prefers-color-scheme` follows the app whatever its `color-scheme` says. So Original rewrites the email's own rules instead: `prefers-color-scheme: dark` becomes `max-width: 0px` (never) and `light` becomes `min-width: 0px` (always). Only the email's CSS changes, never its text.

A theme change (Appearance, or the system with System) redraws the open email.

Known weak spot: a dark logo on a transparent background (a black wordmark PNG) stays dark on the darkened background. Original is the way out.

### Pictures in the email (`cid:` links)

An email can carry its pictures inside the message, with `<img src="cid:…">` naming a part by its Content-ID: signature logos (Outlook), screenshots pasted into the text, some reports and receipts. The frame can't load `cid:` links (the CSP allows images from `data:`, `https:` and `http:` only), so the reader puts the pictures in itself:
- `inlinePictures` finds the `cid:` links in the HTML and the attachments whose Content-ID they name (URL-decoded, ignoring case), pictures only. Those leave the attachment chips, as in Gmail; a picture with a Content-ID that the email doesn't link stays a chip.
- Once the text is on screen, `loadInlinePictures` asks `inline_images` for their data. Like `save_attachments`, Rust finds the parts in a fresh copy of the message. The data becomes `data:` URLs (base64url turned into base64), kept as `full.pictures`.
- `showInlinePictures` points the links at them: `src` and `background` attributes, and `url(cid:…)` in a `style` attribute. It runs on the open email when the data arrives, and on every redraw (Original / Dark, a theme change) as soon as the email is parsed, with nothing fetched again.
- If the request fails, a toast says so and the pictures stay empty. An answer for a message that's no longer open is dropped (`openSeq`).

### Attachments

`get_message` also lists the message's files (`attachments`, each `{ part, name, mimeType, size, contentId }`): every part with a file name, and every picture with a Content-ID (named `image.png` and the like when it has no name), depth first. A forwarded email attached as a message counts as one file, without the files inside it. The pictures the email shows in place leave this list (see [Pictures in the email](#pictures-in-the-email-cid-links)).

`renderAttachments` puts a chip per file under the header (`#m-attachments`): an icon by kind, the name and the size in units of 1024. The kind (image, PDF, archive, document or other) comes from the type Gmail gives, or the extension for `application/octet-stream`. A save icon shows on hover; its room is kept while it's hidden, so the chips don't move under the mouse. With two or more files, **Save all** follows them.

A click calls `save_attachments` with the files' Gmail part IDs, a job number and a channel (`onProgress`). While it runs, the chip shows how much has arrived in place of its size, such as "1.2 of 3.4 MB" (`formatProgress`, in the unit and precision of the total), and a 2 px line fills up along its bottom (`--progress`); Save all shows the same for all the files. A cross takes the place of the save icon (`CANCEL_ICON`; Save all gets one while it saves), and under the mouse the button turns red: a click there calls `cancel_save` with the job number. That drops the download, which also ends its request to Gmail, and `save_attachments` returns no paths, so no toast follows. Files are only written once every one has arrived, so a cancelled save leaves nothing in Downloads.
- The size and the progress share one grid cell (`.save-label`). While saving it's as wide as the finished progress text, which sits in it unseen (`.sizer`), and digits are tabular, so the button keeps one width as the numbers grow. The mouse changes only its colors: when the word Cancel replaced the progress there (0.2.4), a button held at its right edge could narrow, lose the mouse, widen and flicker, and you couldn't see the progress while about to cancel.
- Rust reads each download piece by piece (`read_counting`) and adds what arrives to the total: three quarters of the base64 that came, up to the part's size. `save_attachments` passes that on at most ten times a second. The page computes the total from the sizes it shows and drops progress that comes after the save is over.
- Rust fetches the message again and finds each file by its part, so an attachment ID that changed since the message was opened doesn't matter. Each file's data is in the message or comes from `attachments.get`, which gets 10 minutes (`DOWNLOAD_TIMEOUT`) rather than the 30 s other Gmail requests get: attachments come as base64 in JSON, a third larger than the file.
- Names come from `files::safe_file_name`, the same names the chips show. There are no folders in them, no characters Windows refuses, no control characters, no direction marks that could disguise the extension (`invoice\u202Efdp.exe` reads as `invoiceexe.pdf`), no leading dot, no device names Windows reserves (CON, NUL…), and at most 200 bytes, keeping the extension.
- Files go to the Downloads folder and never replace one there: a taken name becomes `name (1).ext`, `name (2).ext`… (`create_new`).
- Each file is marked as coming from email: on macOS with LaunchServices' quarantine (type email attachment, agent Unified Mail), so Gatekeeper checks it before it opens and asks first if it's an app; on Windows with the Mark of the Web (`Zone.Identifier`, internet zone), so SmartScreen and Office's Protected View apply.
- macOS asks once whether Unified Mail may access the Downloads folder. After "Don't Allow", saving fails with a message pointing to System Settings → Privacy & Security → Files & Folders. Before the Mac app was signed, every update was a new app to macOS, so the question could come back; signed releases keep the answer (see [development.md](development.md#mac-signing)).

The toast then says what was saved, with **Show in Finder** (**Show in folder** on Windows). That's `show_in_folder`, which only takes files in the Downloads folder: Finder selects all of them, File Explorer the first. The toast's button runs any action, but `Z` presses it only for Undo (`undo: true` in `toast`).

## Actions

Which actions an open message offers depends on the folder, and in All Mail on whether the message is in the Inbox (`actionsFor`):

| Folder | Actions |
| --- | --- |
| Inbox | Archive, Delete |
| All Mail, message in the Inbox (shown with an **Inbox** tag) | Archive, Delete |
| All Mail, archived message | Move to Inbox, Delete |
| Sent | Delete |
| Drafts | None: a click opens the draft to write (see [Writing](#writing)) |
| Trash | Move to Inbox |

Whether an All Mail message is in the Inbox comes from the Inbox list (`inInbox`), with no extra requests. Both lists hold each account's newest 50 messages, and every Inbox message is also in All Mail. So a message among an account's newest 50 in All Mail that's in the Inbox is always among its newest 50 in the Inbox. Older All Mail pages loaded by scrolling report their Inbox messages themselves (`inboxExtra`, see [Older mail](#older-mail)).

When an action leaves the message in the folder on screen (Archive and Move to Inbox in All Mail), it stays open and only its tag and buttons change. Otherwise the next message opens. `render()` also redraws the buttons, so they stay right after Undo or a failed action.

`ACTIONS` in `app.js` describes each action: the folder lists the message **leaves** and **joins**, the Gmail command, and how to **undo** it, given that command's result (Delete returns `wasInInbox`).

All actions are **optimistic** (`perform`):
1. The move is applied to the folder lists at once, the next message opens, and the command runs.
2. Until Gmail answers, the move sits in `state.pending` and is re-applied to every sync result.
3. On failure the move is reversed, into only the lists that held the message (`held`: Delete leaves the Inbox, Sent and All Mail, but an archived message was never in the Inbox), and an error toast replaces the Undo toast.

A message only joins lists that are already loaded; other lists pick it up at their next sync.

A list with nothing to show says why when loading failed, rather than its empty text ("Inbox zero"): `state.listErrors` keeps each folder's last sync errors (the accounts that failed, or the whole sync), and `listFailure` names the first for the account on screen. A search does the same with `search.failed` (`searchFailure`), and its line above the results says "Couldn't search" when every account on screen failed.

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
| A draft written here (its row) | Open draft, Discard draft |
| One of Gmail's drafts in the Drafts folder | Open draft |
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

## Search

The magnifier in the list's header, `/`, or `Cmd+F` (`Ctrl+F` on Windows) opens the search field in the header's place (`openSearch`), at the header's height, so nothing moves. A line under the header (`#search-scope`) says where it searches, then what it found: "12 results in all accounts", or "50+ results" while more can load.

- **What it searches:** every account's All Mail, with Gmail's own search (`search_messages`, see [sync.md](sync.md#search-appstatesearch)). Gmail's search words work: `from:`, `has:attachment`, `before:`, `"exact words"`, `OR`… As in Gmail, Spam and Trash are left out. Results come newest first, 50 per account, merged like a folder's pages, and more load as you scroll (`loadOlder`, with the cut-off from [Older mail](#older-mail)).
- **When:** once typing pauses for `SEARCH_DELAY_MS` (500 ms), or at once on Enter. Enter also searches again when the field hasn't changed, for mail that arrived since: results don't update by themselves. A counter (`state.search.seq`) drops answers for an earlier query.
- **Accounts:** an account chosen in the sidebar narrows the results shown, and the scope line names it. The search itself covers every account, so switching accounts doesn't search again. Accounts whose sign-in expired are skipped. An account whose search fails is named in a toast, and the other accounts' results still show.
- **The list:** once a search has run, `listName()` says `search` instead of the folder, and everything that works on the list on screen (`visible`, `byKey`, `loadOlder`, `actionsFor`…) uses `state.lists.search`. Results are All Mail's messages:
  - those in the Inbox get the **Inbox** tag. Each search page says which of its messages are in the Inbox, kept in `inboxExtra` as for older All Mail pages;
  - they get All Mail's actions. Archive keeps a result in the list. Delete takes it out, and Undo or a failed Delete puts it back: `shift` notes the results that left (`searchLeft`), since a message joining All Mail may not match the search.
- **Marks:** the searched words are marked in the sender, subject and preview (`searchPattern`, `setMarkedText`, which builds text nodes and `<mark>` elements, never HTML). Gmail's operators (`from:`, `-word`, `OR`…) and single letters aren't marked. Gmail also matches the rest of the email, attachment names and other forms of a word, so some results show no mark.
- **Keys in the field:** Enter searches, `↓` goes to the results, where J/K and the other shortcuts work, and Esc closes. Esc closes it from the list too.
- **Closing** (Esc, the ×, or a folder in the sidebar) shows the folder again, scrolled where it was. A result open in the reader that isn't in the folder closes with it.
- On macOS the list header is a window drag area. The field is left out of it (`data-tauri-drag-region="false"`), so a click on its icon or edge reaches the field instead of moving the window.

## Writing

A pencil in the list's header (or `C`) writes a **new message**, and Reply / Reply all / Forward **answer** the open email. Picked in mockups on 2026-11-12: a new message in the reader, an answer under its email.

**A new message** (`openCompose`, `#compose`) takes the reader's place. Its button row, where the email's sits, has Send, Attach and Discard. Under it:
- **From**: the account, with its color and a menu of the others. It starts as the account chosen in the sidebar, else the one last sent from (`state.lastFrom`, for as long as the app runs), else the first.
- **To**, then Cc and Bcc once `Cc Bcc` is clicked, **Subject**, the files, and the text.
- While it's written it's a row at the top of the list (`draftRow`): a red **Draft** tag, whom it's to, its subject and its start, in the From account's color. It stays at the top of every folder and of search results. Opening another email leaves it there, and a click on the row (or the pencil, or `C`) brings it back. Leaving one with nothing in it at all drops it. `Esc` leaves it too.
- `state.drafts` holds new messages written or opened here, newest first. The pencil brings back the newest; Undo, a failed send, or opening one from Drafts can add others.

**Answering** (`startReply`): the buttons at the end of the email, the Reply arrow and the ⋮ menu next to its date, or `R` / `A` / `F`. They wait for the email to load, since the answer quotes it. The answer opens under the email (`replyBox`, in `#m-end`), and the reader scrolls down to it:
- First the kind of answer, its icon with a ▾ menu to switch; switching keeps the text. Then the recipients, `Cc Bcc`, the files, and the text, which grows as it's written (`growText`) while the reader follows the cursor down. `···` shows the email as the answer quotes it. The footer has Send, attach, `···`, the account it goes from (the one the email came to, not a menu), and discard.
- **Recipients** (`setKind`): Reply goes to the sender, or to Reply-To when the email has one. Reply all adds the email's To and Cc. Answering one's own email goes to whom it went to, as Gmail does. The account itself is never a recipient. Forward starts with none, and with the email's files as chips; any can be taken off. The subject gets `Re:` or `Fwd:` unless it has one.
- An answer waits in `state.replies`, by its email's key, and the email's row shows a red **Draft** tag. Opening another email drops an answer with nothing written in it, except one opened from Gmail's Drafts (`fromGmail`): what was written in it may be where the editor doesn't show it, or only in its recipients, and dropping it would delete it from Gmail.
- Clicking Attach, a file's cross or `···` redraws the answer, which replaces the button clicked, so the text gets the focus back. Typed next on the page, letters would be shortcuts (`E` archives the email, `R`/`A`/`F` change the answer and its recipients). While a button of the message being written has the focus, shortcuts don't run at all.

**Fields** (`recipientRow`): each recipient is a chip, its name shown and its address under the mouse. Enter, a comma or a semicolon turns what's typed into chips, and so does leaving the field or sending. Pasting a list adds each address (`parseAddresses`, which reads `Name <a@b.c>`, quoted names and lists as headers write them). Backspace in the empty field removes the last chip. A chip that isn't an email address (`isAddress`, the same rule as `check_address` in `mime.rs`) is red, and sending names it.

**Files:** Attach opens the system's file picker (a hidden `<input type="file">`; WebView2 has it, and wry gives WKWebView one). Files are read as base64 (`FileReader`) and listed as chips with a cross. Up to 25 MB in all (`MAX_ATTACHMENTS`), counting a forward's files and the quoted email's pictures, as Gmail allows; a file that doesn't fit is named in a toast.

**Sending** (`sendDraft`, Send or `Cmd+Enter` / `Ctrl+Enter`). Undo before `send_message` has started stops it at every step: after a save on its way, and after the Gmail draft of an earlier From is deleted (`flow.undone`).
1. What's typed in the recipient fields becomes chips, and `draftProblem` checks there's someone to send it to, every address looks right, and the account is there and signed in. A problem shows as a toast, and nothing is sent.
2. The message leaves the screen. The toast says "Sent from <account>" with **Undo** for 8 s (`UNDO_MS`), which `Z` presses too.
3. `send_message` waits `UNDO_MS` plus a second (`SEND_GRACE_MS`), so an Undo clicked at the last moment still arrives. Undo calls `cancel_send`, and the message comes back as it was. If the app quits meanwhile, it's sent right away (see [architecture.md](architecture.md#sending)).
4. Once Gmail has it, the app syncs, so it shows in Sent. If sending fails, the message comes back with the reason in a toast.
5. A message saved as a Gmail draft (below) goes as that draft: any save on its way finishes first, then `send_message` gets `draftId`, saves it once more with what was last written and sends it with `drafts.send`, which moves it to Sent. Meanwhile the Drafts folder leaves it out (`state.sending`). Undo clicked before `send_message` has started just brings it back.

**What's sent** (`outgoing`): a new message is plain text. An answer also has HTML, like Gmail's: the text, then the email answered in its own formatting (`emailHtml`), in a quote (`On <date>, <sender> wrote:`) or under a "Forwarded message" header. The pictures it shows in place (`full.inline`) go along with their Content-IDs, and so do a forward's files: Rust fetches them from Gmail (`parts`). The plain text quotes the email as text (`htmlToText` for HTML emails), with `>` before each line. Answers carry the email's Message-ID and References and go in its conversation (`threadId`), so they thread in Gmail and elsewhere.

The quoted email keeps its inline styles, but not its `<style>` rules: in the recipient's mail app (Apple Mail, say) they'd also style what's written above the quote, and could hide it, so a crafted email could make a forward show only its own words. Fixed, absolute and sticky positioning go too, since they could cover the text.

**Discard** takes the message away and deletes its Gmail draft, with Undo in the toast too, which saves it as a new draft.

### Drafts in Gmail

What's written is saved in the account's Gmail Drafts, as Gmail does, so a draft is also in Gmail on the web and on a phone, and drafts started there can be finished here. Picked as option G in mockups on 2026-11-12, over keeping drafts on this computer.

- **When** (`draftChanged`, `saveDraft`): `DRAFT_SAVE_MS` (2 s) after typing stops, and at once when the message is left (another email, `Esc`, the pencil) or the window goes behind another or is hidden (`saveAllDrafts`). Nothing is saved before something is written. A draft carrying more than `BIG_DRAFT` (5 MB) of files waits `BIG_DRAFT_SAVE_MS` (a minute) instead, or until it's left: Gmail keeps no draft's files between saves, so every save sends them all again. Quitting in the 2 s after typing loses those last words, since the page can't save as it closes.
- **How** (`save_draft`): the first save creates a Gmail draft (`gmailId`, in `gmailAccount`), later ones update it. Saves run one after another (`draft.saving`, a promise chain), so a draft is never created twice, and each sends what's written when it starts. The email is written as for sending (`outgoing`): an answer with its quote, the quoted email's pictures and its threading, so the draft is complete in Gmail too. Recipients still being typed are left out if they'd break a header (`mime::Email::write`). A draft whose From changed is deleted from the other account and made anew. The account a save went to is the one in what it sent (`payload.account`), so From changed while a save is on its way still moves the draft at the next save. A failed save shows **Not saved** (its title says why) and tries again after `DRAFT_RETRY_MS` (30 s).
- **What the editor says** (`renderSaveState`): "Saving…", then "Saved to Gmail" in a new message's button row, or "Saved" in an answer's footer.
- **The Drafts folder** lists the drafts written here first (with what's written now), then Gmail's others (`gmailDraftRow`, rows of the `drafts` folder, which carry their draft id), leaving out the ones written or being sent here. Its count in the sidebar adds up both. The account chosen in the sidebar narrows it as in other folders. `J`/`K` don't walk through it.
- **Opening one of Gmail's drafts** (`openGmailDraft`, `open_draft`): Rust returns its recipients, subject, text and HTML, its files with their data (each save sends them again), and the email it answers, found in its conversation by its In-Reply-To (`gmail::find_in_thread`). An answer opens under that email (`select` with the email's summary, since it isn't in the Drafts list), and the email's full text arrives with it for the quote; when that email is already open, the draft takes its full text from the reader. Its text is what was written above the quote (`typedText`: cut at "On …, … wrote:" or "Forwarded message" in the plain text, or `.gmail_quote` removed from the HTML), plus, in an answer, the lines without `>` after that first line: some mail apps answer under the quote or between its lines. The pictures of its quote are left out, since saving makes the quote again. If another email or a new message was opened while it loaded, the draft goes to the list without taking the reader. Anything else opens in the editor, keeping its threading (`thread`) when the email it answered isn't found. A draft already open here isn't opened twice.
- Formatting from Gmail (bold, links, pasted pictures) doesn't survive the plain-text editor: pictures become attachments, and the rest is plain text once the draft is saved here.

## Keyboard

| Key | Action |
| --- | --- |
| `J` / `↓` | Next message |
| `K` / `↑` | Previous message |
| `Space` / `PageDown` | Scroll the open message down (`Shift+Space` / `PageUp`: up) |
| `E` | Archive (Inbox only) |
| `#` / `Delete` | Delete, i.e. move to Trash (Inbox and All Mail) |
| `Shift+U` / `Shift+I` | Mark as unread / read |
| `Z` | Undo the last action, while its toast shows (also Send and Discard) |
| `/`, `Cmd+F` (`Ctrl+F` on Windows) | Search (see [Search](#search)) |
| `C` | New message (see [Writing](#writing)) |
| `R` / `A` / `F` | Reply / Reply all / Forward the open message |
| `Cmd+Enter` (`Ctrl+Enter` on Windows) | Send, while writing |
| `Esc` | Close Settings, the right-click menu or the search. While writing: leave a new message (it waits in the list), or leave the reply's field |

The Delete button's tooltip names `Del`, which everyone has; `#` is Gmail's key for it, kept for Gmail habits.

A key held down repeats only for moving and scrolling (`J`, `K`, the arrows, `Space`, `PageDown`, `PageUp`), so holding `E` or `Delete` doesn't archive or delete a run of messages. `F5` and `Ctrl+R` (`Cmd+R`) do nothing: in WebView2 they'd reload the page, losing Undo and what isn't saved yet, and `Ctrl+R` is Reply in Outlook.

Shortcuts work in any keyboard layout (`shortcutKey`). With a Latin layout they go by the character typed (`e.key`), so Dvorak and AZERTY users get their own `J`, `K`, `E`. With any other layout, such as Russian, the letters type something else (`J` is `о`), so they go by the key's position (`e.code`, `KeyJ`), and `Shift+3` (Russian `№`) counts as `#`. `Cmd+F` / `Ctrl+F` follows the same rule. `/` goes by the character typed in every layout, since the key that types it differs between layouts; there's always `Cmd+F` / `Ctrl+F`.

The same handler is attached inside the email iframe (`onKey(e, true)`), so the shortcuts keep working after clicking into a message. Inside the message the arrows scroll it instead of changing it, and `Space`, `PageDown` and the arrows scroll the reader around the email (see [One scroll](#one-scroll)). Shortcuts don't act while typing in a field, so writing a message types its letters.

## Notifications and the icon badge

**Notifications** (`notifyNewMail`, after each Inbox sync):
- `state.newestSeen[account]` is the date of the account's newest Inbox message so far. Unread mail newer than that notifies.
- An account's first sync (at launch, or after adding it) only sets the mark. Mail that was already there doesn't notify, and neither does mail that arrived while the app was closed.
- Older mail that moves back into the Inbox (Undo, Move to Inbox, moved in Gmail web) is older than the mark, so it doesn't notify either.
- Nothing shows while the window is in front (`document.hasFocus()`).
- Each message gets its own notification: the sender as title, the subject as body, plus the account when there are several. More than `MAX_NOTIFICATIONS` (3) at once become one "N new messages" summary listing the senders.
- They play a sound, which `notify` in `main.rs` has to name: the system's Mail sound on Windows, the default notification sound on macOS. Both systems let people turn it off per app.

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
