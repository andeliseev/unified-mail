# Unified Mail

A small, fast desktop mail client for Windows and macOS: one inbox for all your Gmail accounts.

- Unified inbox across any number of Gmail accounts, plus a per-account view
- Read messages (HTML or plain text, any charset), marked read when opened
- **Archive** (`E`) and **Delete** (`#` or `Delete`), the same as Gmail's buttons. Delete moves the message to Trash, which Gmail empties after 30 days.
- `J`/`K` or arrow keys move between messages
- **Push:** new mail shows up within a second or two (IMAP IDLE). The last inbox is cached, so the app opens instantly.

Built with [Tauri 2](https://v2.tauri.app/): a Rust backend plus the operating system's own webview (WebView2 on Windows, WKWebView on macOS). It ships no bundled browser, so the app is a few MB and uses little memory. The UI is plain HTML/CSS/JS with no framework and no build step.

## Signing in

For people using the app, adding an account is just **Add account** → choose a Google account → **Allow**.
Repeat for each account.

That works because the app's Google OAuth client is built into the binary. Google requires every app
that reads Gmail to have one; it identifies the app, not the user. Whoever builds the app sets it up once:

## One-time Google setup for the builder (about 5 minutes)

1. Open <https://console.cloud.google.com/> and create a project (any name).
2. Enable the Gmail API: <https://console.cloud.google.com/apis/library/gmail.googleapis.com> → **Enable**.
3. Open **Google Auth Platform** (<https://console.cloud.google.com/auth/overview>) → **Get started**:
   - App name: `Unified Mail`, support email: yours
   - Audience: **External** (or **Internal** if every account is in the same Google Workspace as the project)
4. **Audience** page: add each Gmail address you want to use under **Test users**.
5. **Clients** → **Create client** → Application type **Desktop app** → Create → **Download JSON**.
6. Save that file as `src-tauri/google-client.json`. It's git-ignored. Then build or run the app (see [Development](#development)).
   As an alternative, set the `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET` environment variables when building.

The build bakes the client into the executable, and Settings no longer mentions it. A build with no
client (you'll see a cargo warning) falls back to asking for a Client ID and secret in Settings.

Google treats a desktop app's client secret as non-confidential. Shipping it inside the app is the
standard approach for installed apps.

> **7-day sign-in limit.** While the Google project's publishing status is *Testing*, Google expires
> sign-ins after 7 days, and you'd have to re-add the account. To avoid that, click **Publish app** on
> the Audience page. Google will then show an "app isn't verified" warning when you sign in. That's
> expected for a personal app: click **Advanced → Go to Unified Mail**.

> **Work / Workspace accounts.** Your admin may block unverified apps from Gmail data. If sign-in is
> refused, ask them to allow the OAuth client ID, or put the Cloud project in the Workspace and use
> the **Internal** audience.

### What the app can access

It requests one scope, `https://mail.google.com/` (full Gmail access). That's the only scope Gmail accepts for IMAP, which push needs, so Google's consent screen says the app could permanently delete mail. It never does: it only reads, archives and moves to Trash. Everything runs on your machine and talks directly to Google:

| Stored | Where |
| --- | --- |
| Refresh tokens | OS keychain (Windows Credential Manager / macOS Keychain), service `unified-mail` |
| OAuth client, account list, inbox cache | App data dir: `%APPDATA%\io.unifiedmail.desktop` or `~/Library/Application Support/io.unifiedmail.desktop` |

Removing an account in Settings revokes its token at Google and deletes it from the keychain.

Email HTML is rendered in a sandboxed frame with scripts, forms and popups disabled. Links open in your default browser. Remote images load, as in most mail clients.

## Development

Requirements: [Rust](https://rustup.rs) (stable) and Node.js 18+.
On Windows you also need the MSVC Build Tools; on macOS, the Xcode Command Line Tools.

```bash
npm install
```

```bash
npm run dev
```

Builds a release executable plus installers (`.msi`/`.exe` on Windows, `.dmg`/`.app` on macOS):

```bash
npm run build
```

Output lands in `src-tauri/target/release/bundle/`. Build each platform on that platform: Windows installers on Windows, macOS ones on a Mac.

Run the backend unit tests:

```bash
cd src-tauri && cargo test
```

More commands, the size budget and debugging tips are in [docs/development.md](docs/development.md).

## Docs

| | |
| --- | --- |
| [Architecture](docs/architecture.md) | Components, Rust modules, commands, storage |
| [Authentication](docs/auth.md) | Sign-in flow, built-in client, scope, tokens |
| [Sync and push](docs/sync.md) | How the inbox stays current, IMAP IDLE |
| [UI](docs/ui.md) | Frontend, email sandboxing, shortcuts |
| [Development](docs/development.md) | Setup, tests, builds, debugging |
| [To do](docs/todo.md) | Known limitations and ideas for later |

If a Google Workspace admin has turned IMAP off, push can't connect. The app then falls back to syncing every 5 minutes and whenever its window regains focus.
