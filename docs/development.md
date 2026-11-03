# Development

## Requirements

- [Rust](https://rustup.rs) stable. `rustup` installs it into `~/.cargo/bin`; open a new terminal so it's on `PATH`.
- Node.js 18+, needed only for the Tauri CLI (`@tauri-apps/cli`).
- Windows: MSVC Build Tools and WebView2 (preinstalled on Windows 10/11).
- macOS: Xcode Command Line Tools.
- A Google OAuth client in `src-tauri/google-client.json` (see [below](#google-oauth-client-one-time-about-5-minutes)). Without it the app still runs and asks for a client in Settings.

## Google OAuth client (one time, about 5 minutes)

Google requires every app that reads Gmail to have its own OAuth client. It identifies the app, not the user. The build bakes it into the executable, so people using the app only ever see Google's sign-in.

1. Open <https://console.cloud.google.com/> and create a project (any name).
2. Enable the Gmail API: <https://console.cloud.google.com/apis/library/gmail.googleapis.com> → **Enable**.
3. Open **Google Auth Platform** (<https://console.cloud.google.com/auth/overview>) → **Get started**:
   - App name: `Unified Mail`, support email: yours
   - Audience: **External** (or **Internal** if every account is in the same Google Workspace as the project)
4. **Audience** page: add each Gmail address you want to use under **Test users**.
5. **Clients** → **Create client** → Application type **Desktop app** → Create → **Download JSON**.
6. Save that file as `src-tauri/google-client.json`. It's git-ignored; never commit it. As an alternative, set the `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET` environment variables when building.

A build with no client (you'll see a cargo warning) falls back to asking for a Client ID and secret in Settings. Google treats a desktop app's client secret as non-confidential, so shipping it inside the app is the standard approach for installed apps. If you fork the project and ship your own builds, use your own client, not the official one.

> **7-day sign-in limit.** While the Google project's publishing status is *Testing*, Google expires
> sign-ins after 7 days. To avoid that, click **Publish app** on the Audience page. Until Google verifies
> the app, users see an "app isn't verified" warning at sign-in (Advanced → Go to Unified Mail), and the
> number of users is capped. See [auth.md](auth.md#google-project-limits).

## Commands

Install the Tauri CLI:

```bash
npm install
```

Run with hot reload (edits to `src/` reload the page; Rust changes rebuild the app):

```bash
npm run dev
```

This runs as a separate test app, **Unified Mail (test)**, with its own data folder, so it never opens the accounts of an installed Unified Mail. See [Testing without your own mail](#testing-without-your-own-mail).

Release build with installers in `src-tauri/target/release/bundle/`:

```bash
npm run build
```

Release executable only, no installers (quick size check):

```bash
npx tauri build --no-bundle
```

Unit tests:

```bash
cd src-tauri && cargo test
```

Also run the live test against `imap.gmail.com` (fake token; checks that Gmail rejects it cleanly rather than hanging):

```bash
cd src-tauri && cargo test -- --include-ignored
```

The first `npm run build` on Windows downloads the NSIS and WiX installer tools. Build macOS bundles on a Mac.

## Size budget

The point of the app is to stay small. Current numbers on Windows x64:

| Build | Size |
| --- | --- |
| `unified-mail.exe` release, before push | 3.9 MB |
| `unified-mail.exe` release, with IMAP push | 4.2 MB |
| `unified-mail.exe` release, with notifications and single instance (+230 KB) | 4.5 MB |
| NSIS installer (`...-setup.exe`) | 1.5 MB |
| MSI installer | 2.0 MB |

The release profile in `Cargo.toml` is tuned for size: `opt-level = "s"`, `lto`, `codegen-units = 1`, `panic = "abort"`, `strip`. Before adding a crate, check what it adds with `npx tauri build --no-bundle`, and prefer what the OS already provides (see [architecture.md](architecture.md#design-decisions)).

## Releases (GitHub Actions)

[`.github/workflows/release.yml`](../.github/workflows/release.yml) builds and publishes a release in one click.

**One-time setup:** in the GitHub repo, open **Settings → Secrets and variables → Actions → New repository secret** and add:

| Secret | Value |
| --- | --- |
| `GOOGLE_CLIENT_ID` | `client_id` from `src-tauri/google-client.json` |
| `GOOGLE_CLIENT_SECRET` | `client_secret` from the same file |

`build.rs` reads them and bakes the client into the app, just as it does with the local file. The workflow refuses to run without them, so a release can never ship without a client.

**To release:** **Actions → Release → Run workflow**, choose what to raise, and run it:

| Choice | From 1.2.3 | Use for |
| --- | --- | --- |
| `patch` (default) | 1.2.4 | Fixes |
| `minor` | 1.3.0 | New features |
| `major` | 2.0.0 | Big changes |

Or type an exact version (e.g. `0.2.0`) in the second box; it wins over the choice. Versions must be plain `X.Y.Z`, because MSI installers reject suffixes like `-beta`.

To see what a run would release, from the repo root:

```bash
node scripts/next-version.mjs patch
```

It prints the last release and the next version. The last release is the highest `vX.Y.Z` tag on GitHub (`git ls-remote --tags origin` lists them). An installed app shows its own version at the bottom of Settings. The run is named "Release (patch)" or "Release (0.2.0)" in the Actions list, and its summary page names the version.

The workflow then:

1. Works out the version with [`scripts/next-version.mjs`](../scripts/next-version.mjs). It stops if that isn't newer than the last release.
2. Sets it with [`scripts/set-version.mjs`](../scripts/set-version.mjs) in `package.json`, `package-lock.json`, `tauri.conf.json`, `Cargo.toml` and `Cargo.lock`, and commits "Release vX.Y.Z" to the branch. Run `git pull` afterwards before you commit locally.
3. Creates a draft release `vX.Y.Z`, replacing a draft left by a failed run.
4. Builds Windows (NSIS `.exe` + `.msi`) and macOS (one universal `.dmg` for Apple Silicon and Intel) in parallel, and uploads them to the draft.
5. Publishes the release only if both builds succeeded. Only then does the `vX.Y.Z` tag exist.

**If a run fails,** fix the cause and run it again with the same choice. The tag doesn't exist yet, so the same version comes out, and its draft is replaced. Don't set the version locally: the workflow does it, and it refuses to go below a version already in `package.json`.

**Notes:**

- The repo is public, so releases are public downloads. The README's Download section links to the latest one.
- Builds aren't code-signed. Windows shows a SmartScreen warning. The macOS app is ad-hoc signed (`bundle.macOS.signingIdentity: "-"`) so it can run on Apple Silicon, but it isn't notarized, so Gatekeeper asks once (System Settings → Privacy & Security → Open Anyway). The release notes explain both.
- Actions minutes are free for public repos. The universal macOS build compiles the app twice, so it's the slowest job; `rust-cache` makes repeat builds faster.

## Website (home page and privacy policy)

Google requires a home page and a privacy policy, on a domain listed under **Authorized domains**, before the OAuth app can be published. They live in `site/` and are served by GitHub Pages from the repo [andeliseev/andeliseev.github.io](https://github.com/andeliseev/andeliseev.github.io), which gets only the `site/` folder's history.

| Page | URL |
| --- | --- |
| Home | https://andeliseev.github.io/unified-mail/ |
| Privacy policy | https://andeliseev.github.io/unified-mail/privacy.html |

After changing `site/`, commit the change on its own (its message becomes public) and publish:

```bash
git subtree push --prefix=site https://github.com/andeliseev/andeliseev.github.io.git main
```

If what the app accesses or stores changes, update `site/unified-mail/privacy.html` and its effective date.

## Debugging

- In `npm run dev` the Rust side's stderr goes to the terminal, including `[push] ...` lines from IMAP watchers. Release builds on Windows have no console (`windows_subsystem = "windows"`).
- `Ctrl+Shift+I` (`Cmd+Option+I` on macOS) opens devtools in dev builds. Right-click shows the app's own menu, not the webview's, so it has no Inspect.
- Clear the app's state by deleting the app data folder (see [architecture.md](architecture.md#storage)). Saved sign-ins are separate: they stay in the OS keychain under service `unified-mail`.

## Testing without your own mail

Never run a development build on your real Gmail accounts: a bug could archive, delete or mark real mail.

- `npm run dev` merges [`src-tauri/tauri.dev.conf.json`](../src-tauri/tauri.dev.conf.json): identifier `io.unifiedmail.devtest` (its own data folder, starting with no accounts) and the window title "Unified Mail (test)".
- Sign in there only with test Gmail accounts. Saved sign-ins are keychain entries keyed by email address and shared with the installed app, so signing in or removing an account that the installed app also uses affects both.
- Release builds (`npm run build`, `npx tauri build`) use the real identity, `io.unifiedmail.desktop`. On a computer where Unified Mail is installed and signed in, a release build opens those same accounts. To run one for testing, add `--config src-tauri/tauri.dev.conf.json`.
- For the UI alone, use a fake backend, as described next.

## Testing the UI without Google

The UI talks to Rust only through `window.__TAURI__.core.invoke` and `window.__TAURI__.event.listen`. To check layout and behavior in a normal browser:

1. Copy `src/` somewhere else.
2. Add a script before `app.js` that defines `window.__TAURI__` with fake `invoke` answers for the commands in [architecture.md](architecture.md#commands-ui--rust), plus a no-op `listen`.
3. Serve the folder (for example `python -m http.server`).

This is how reading, the sandbox, the windows-1251 decoding, archive/delete and Settings were checked during development.

To test in the **real WebView2**, including the release CSP, without touching your own accounts:

1. Build as the test app, which gets its own data folder and no accounts:
   ```bash
   npx tauri build --no-bundle --config src-tauri/tauri.dev.conf.json
   ```
2. Start `src-tauri/target/release/unified-mail.exe` with the environment variable `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9334`.
3. Drive the page over the Chrome DevTools Protocol (`http://127.0.0.1:9334/json`):
   - `Runtime.evaluate` can call `app.js` functions such as `showBody(...)`, because they're globals.
   - `Input.dispatchMouseEvent` and `Input.dispatchKeyEvent` send real wheel and key input without moving your mouse.

   This is how the email scrolling bug was reproduced and its fix checked.
4. Afterwards, delete `%APPDATA%\io.unifiedmail.devtest` and `%LOCALAPPDATA%\io.unifiedmail.devtest`, then rebuild normally.

## Manual test checklist

- [ ] First run: no Settings prompt with a built-in client; **Add Gmail account** shown
- [ ] Sign in to two accounts; both appear, unified list sorted by date
- [ ] Send yourself mail: it appears within a few seconds (push)
- [ ] With the app in the background, send mail to a test account: a notification with the mail sound; clicking it brings the window forward and opens the message. With the app in front: no notification
- [ ] The taskbar / Dock icon shows the unread count, and it clears when everything is read
- [ ] Launch the app a second time: the open window comes forward, and no second copy starts
- [ ] Read a message on the phone: it turns read in the app
- [ ] Archive (`E`) and delete (`#`): row disappears, Gmail web agrees
- [ ] `Z` right after Archive / Delete: the message comes back to the Inbox (check Gmail web)
- [ ] Trash → Move to Inbox, then Undo: it goes back to Trash
- [ ] All Mail: delete an archived message, Undo: it returns to All Mail, not the Inbox
- [ ] All Mail: Inbox messages carry an **Inbox** tag; Move to Inbox on an archived one adds the tag, Undo removes it
- [ ] Open an unread message, click **Mark as unread** (or `Shift+U`), open another one: the first stays unread, in Gmail web too
- [ ] Right-click a row: Mark as read/unread and the folder's actions, in English; Archive from there leaves the open message open
- [ ] Right-click a link in an email: Open link, Copy link; select text and right-click: Copy
- [ ] Links in an email open in the default browser
- [ ] With 50+ messages, the list scrolls, and a long email scrolls to its end (test with a full inbox, not an empty one)
- [ ] An account with no unread mail still syncs
- [ ] Scroll to the bottom of each folder: older mail keeps loading, dates stay in order across accounts, "Loading older messages…" disappears at the very end
- [ ] Sleep the computer for 10+ min, wake it: push reconnects (watch `[push]` logs)
- [ ] Remove an account: its mail leaves the list; the keychain entry is gone
