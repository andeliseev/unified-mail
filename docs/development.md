# Development

## Requirements

- [Rust](https://rustup.rs) stable. `rustup` installs it into `~/.cargo/bin`; open a new terminal so it's on `PATH`.
- Node.js 18+, needed only for the Tauri CLI (`@tauri-apps/cli`).
- Windows: MSVC Build Tools and WebView2 (preinstalled on Windows 10/11).
- macOS: Xcode Command Line Tools.
- A Google OAuth client in `src-tauri/google-client.json` (see the [README](../README.md)). Without it the app still runs and asks for a client in Settings.

## Commands

Install the Tauri CLI:

```bash
npm install
```

Run with hot reload (edits to `src/` reload the page; Rust changes rebuild the app):

```bash
npm run dev
```

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

The release profile in `Cargo.toml` is tuned for size: `opt-level = "s"`, `lto`, `codegen-units = 1`, `panic = "abort"`, `strip`. Before adding a crate, check what it adds with `npx tauri build --no-bundle`, and prefer what the OS already provides (see [architecture.md](architecture.md#design-decisions)).

## Debugging

- In `npm run dev` the Rust side's stderr goes to the terminal, including `[push] ...` lines from IMAP watchers. Release builds on Windows have no console (`windows_subsystem = "windows"`).
- Right-click → Inspect opens devtools in dev builds.
- Clear the app's state by deleting the app data folder (see [architecture.md](architecture.md#storage)). Saved sign-ins are separate: they stay in the OS keychain under service `unified-mail`.

## Testing the UI without Google

The UI talks to Rust only through `window.__TAURI__.core.invoke` and `window.__TAURI__.event.listen`. To check layout and behavior in a normal browser:

1. Copy `src/` somewhere else.
2. Add a script before `app.js` that defines `window.__TAURI__` with fake `invoke` answers for the commands in [architecture.md](architecture.md#commands-ui--rust), plus a no-op `listen`.
3. Serve the folder (for example `python -m http.server`).

This is how reading, the sandbox, the windows-1251 decoding, archive/delete and Settings were checked during development.

## Manual test checklist

- [ ] First run: no Settings prompt with a built-in client; **Add Gmail account** shown
- [ ] Sign in to two accounts; both appear, unified list sorted by date
- [ ] Send yourself mail: it appears within a few seconds (push)
- [ ] Read a message on the phone: it turns read in the app
- [ ] Archive (`E`) and delete (`#`): row disappears, Gmail web agrees
- [ ] Links in an email open in the default browser
- [ ] Sleep the computer for 10+ min, wake it: push reconnects (watch `[push]` logs)
- [ ] Remove an account: its mail leaves the list; the keychain entry is gone
