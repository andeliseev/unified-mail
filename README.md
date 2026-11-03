# Unified Mail

A small, fast desktop email app for Gmail on Windows and macOS: one inbox for all your Gmail accounts.

- **Inbox, All Mail and Trash** for all your accounts together, or for one account at a time
- **New mail appears within seconds**, with no need to refresh, and a **notification** tells you about it when the app is in the background
- The **unread count** shows on the app's taskbar / Dock icon
- **Updates itself:** when a new version is out, Settings shows an **Update** button, and one click installs it
- **Archive**, **Delete** (to Trash) and **Move to Inbox**, each with **Undo**
- **Mark as unread** to come back to a message later
- Right-click a message for its actions
- Older mail loads as you scroll
- The installer is about 1.5 MB, and the app starts instantly

## Download

Get the latest version from the **[Releases page](https://github.com/andeliseev/unified-mail/releases/latest)**:

| System | File |
| --- | --- |
| Windows 10 / 11 | `Unified.Mail_<version>_x64-setup.exe` (or the `.msi`) |
| macOS (Apple Silicon and Intel) | `Unified.Mail_<version>_universal.dmg` |

### First launch

The app isn't code-signed yet, so your system asks once before opening it:

- **Windows:** "Windows protected your PC" → **More info** → **Run anyway**.
- **macOS:** open the app, then go to **System Settings → Privacy & Security** and click **Open Anyway**. Allow keychain access when asked; that's where the app keeps your sign-ins.

## Adding your Gmail accounts

Click **Add account**, choose your Google account in the browser window that opens, and click **Allow**. Repeat for each account.

- Google may warn that **"Google hasn't verified this app"**, because the app hasn't finished Google's review yet. Click **Advanced → Go to Unified Mail** to continue.
- **Work accounts (Google Workspace):** your administrator may block apps that Google hasn't verified. If sign-in is refused, ask them to allow Unified Mail.
- Google's permission screen asks for full Gmail access. Gmail requires that for instant new-mail notifications. The app only reads your mail, marks it read or unread, archives it, and moves it to or from Trash. It never permanently deletes anything.

## Keyboard shortcuts

| Key | Action |
| --- | --- |
| `J` / `K` or `↓` / `↑` | Next / previous message |
| `Space` / `PageDown`, `PageUp` | Scroll the open message |
| `E` | Archive |
| `#` or `Delete` | Delete (move to Trash) |
| `Shift+U` / `Shift+I` | Mark as unread / read |
| `Z` | Undo |

## Privacy

Unified Mail runs entirely on your computer and talks only to Google, plus GitHub to check for updates. There are no servers of ours, no analytics and no tracking.

- **Sign-ins** are kept in your system's secure storage: Windows Credential Manager or macOS Keychain.
- **Message lists** are cached in the app's data folder so the app opens instantly. Message content isn't saved.
- **Updates:** a few times a day the app downloads a small file from GitHub to see whether a new version is out. GitHub sees your IP address, nothing else. An update installs only when you click **Update** in Settings, and only if it's signed by the developer.
- **Notifications** show the sender and subject of new mail. Windows and macOS keep recent notifications in their notification center until you clear them.
- **Removing an account** in Settings revokes the app's access at Google and deletes its data from your computer.
- **Email content** is shown with scripts and forms blocked, and links open in your browser.

Read the full [privacy policy](https://andeliseev.github.io/unified-mail/privacy.html).

## Current limits

- Gmail only for now. Other providers, such as Yandex Mail, are planned.
- No compose, reply or search yet.
- Older messages you've scrolled to aren't refreshed in the background. Reopen the folder to update them.

## Building from source and contributing

The app is built with [Tauri 2](https://v2.tauri.app/): a Rust backend plus your system's own web view, with no bundled browser. To build it yourself, including the one-time Google Cloud setup, see [docs/development.md](docs/development.md). How it works inside is in [docs/](docs/README.md).

Bug reports and pull requests are welcome. To report a security problem, see [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)
