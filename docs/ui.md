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

There is one `state` object in `app.js`: accounts, messages (unified, newest first), the current filter (`null` = all inboxes), the selected/open message, the pending-removal set and sync flags.

Rendering is direct DOM work: `renderSidebar()` and `renderList()` rebuild their parts from `state`, using a `DocumentFragment` for the list. That's fast at this size (a few hundred rows). Untrusted text (sender, subject, snippet) is always set with `textContent`, never `innerHTML`. Each account gets a color from `PALETTE` by its position in the account list; the color is used for the row stripe, sidebar dot and chip.

## Reading a message

`select(key)` shows the headers right away from the summary, then calls `get_message`. A sequence counter (`openSeq`) discards answers that arrive after the user has already moved to another message.

### Bodies and charsets

Gmail returns body parts as base64url bytes in the part's own charset. `gmail.rs` passes them through unchanged along with the charset from `Content-Type`. `decodeBody()` decodes them with `TextDecoder(charset)`, which covers windows-1251, KOI8-R, ISO-8859-x and the rest, and falls back to UTF-8 for unknown labels. HTML is preferred. Plain text is escaped, wrapped in `<pre>`, and its URLs are turned into links.

### Sandbox

Email HTML is untrusted, so it renders in an iframe built with `srcdoc` and:

- `sandbox="allow-same-origin"` and **no** `allow-scripts`: scripts, forms and popups are all blocked. `allow-same-origin` is there only so the parent page can reach `contentDocument` to attach handlers. With scripts off, the email itself can't use it.
- A click handler on the frame's document cancels navigation and sends `http(s)`/`mailto` links to `open_external`, which opens them in the default browser. `<base target="_blank">` makes sure an unhandled link can't navigate the frame.
- `srcdoc` frames inherit the page's CSP (`tauri.conf.json`): images from `https:`/`http:`/`data:` are allowed and inline styles too. Tauri is told not to add nonces to `style-src` (`dangerousDisableAssetCspModification`), because a nonce would switch off `'unsafe-inline'`.
- The frame background is always white: emails assume white, even in dark mode.
- Some emails set `overflow: hidden` on `html` or `body`, which would make them impossible to scroll. The frame CSS forces `:root { overflow: auto !important }` and `:root > body { overflow: visible !important }`. `:root` outranks the emails' `html`/`body` selectors, so this wins even against their `!important`. Only an inline `style="...!important"` on the email's `<body>` could still override it.

## Actions

Archive and delete are **optimistic**. The row disappears and the next message opens immediately, then the command runs. On failure the message goes back into the list with an error toast. While a request is in flight the key stays in `state.pending`, so a concurrent sync can't bring the row back.

## Keyboard

| Key | Action |
| --- | --- |
| `J` / `↓` | Next message |
| `K` / `↑` | Previous message |
| `Space` / `PageDown` | Scroll the open message down (`Shift+Space` / `PageUp`: up) |
| `E` | Archive |
| `#` / `Delete` | Delete (Trash) |
| `Esc` | Close Settings |

The same handler is attached inside the email iframe (`onKey(e, true)`), so `J`/`K`/`E`/`#` keep working after clicking into a message. Inside the message, arrows, `Space` and `PageDown` are left to the frame, so they scroll the message instead of changing it.

## Settings and first run

- With a built-in client, the OAuth section is hidden. First run shows the empty list with an **Add Gmail account** button.
- Without one, Settings opens on first run and can't be closed until a client is saved. Pasting the downloaded client JSON into the Client ID field fills both fields.
- **Remove** on an account asks for a second click to confirm. The app avoids `confirm()` dialogs.
- **Add account** shows a "finish signing in in your browser" overlay. Cancel only hides the overlay: the backend keeps waiting until its 5-minute timeout, and a sign-in that completes later is still added.
