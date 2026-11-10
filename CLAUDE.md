# Unified Mail

[README.md](README.md) is for people using the app. Building, including the Google OAuth client setup, is in [docs/development.md](docs/development.md). Implementation notes are in [docs/](docs/README.md): architecture, auth, sync/push, UI, development, and [todo.md](docs/todo.md) for known gaps and ideas.

## Commands

- `npm run dev`: run the app with hot reload, as the separate test app "Unified Mail (test)"
- `cd src-tauri && cargo test`: unit tests (`-- --include-ignored` adds a live imap.gmail.com check)
- `npx tauri build --no-bundle`: release exe only, for size checks

## Working rules

- **Never run the app on real Gmail accounts.** A bug could archive, delete or mark real mail. Test the UI with a fake backend, and the real app only as the test app (`npm run dev`, or `--config src-tauri/tauri.dev.conf.json`) with test accounts the user names. A build with the app's real identity (`com.andeliseev.unified-mail`) opens the accounts of the installed app. See [docs/development.md](docs/development.md#testing-without-your-own-mail).
- **Small binary is a core goal.** Check the size impact of any new crate with `npx tauri build --no-bundle` and record notable changes in [docs/development.md](docs/development.md#size-budget). Prefer what the OS already ships (webview, native-tls, `TextDecoder`) over bundled code.
- `src/` is plain HTML/CSS/JS served as is. Don't add a framework or build step.
- Never hold a `std::sync::Mutex` guard across `.await` (Tauri async commands must be `Send`).
- Set untrusted email text with `textContent`. Email HTML only goes into the sandboxed iframe (see [docs/ui.md](docs/ui.md#sandbox)).
- Command errors are user-facing `String`s; write them for people.
- Never commit `src-tauri/google-client.json` (the OAuth client, git-ignored).
- Commits are authored as `Andrei Eliseev <andeliseev@gmail.com>` (set in this repo's git config). Never add `Co-Authored-By` lines or any other Claude/AI attribution to commit messages, pull requests or release notes.
- `README.md` stays user-facing (download, first launch, sign-in, privacy). Developer details go in `docs/`.
- The website (home page, privacy policy, support page) is in its own repository, [andeliseev.github.io](https://github.com/andeliseev/andeliseev.github.io), in `unified-mail/` ([docs/development.md](docs/development.md#website-home-page-and-privacy-policy)). When what the app accesses or stores changes, update the privacy policy there; when the app icon changes, its favicon too.
- When behavior changes, update the matching file in `docs/`. Add things you notice but don't do to `docs/todo.md`.
- Every change people using the app would notice gets a line under **Unreleased** in [CHANGELOG.md](CHANGELOG.md), written for them (what changed for them, not how), in the same commit. The Release workflow turns those lines into the release's notes. Changes only to docs or the release tooling don't go there.
