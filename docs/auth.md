# Authentication

## Sign-in flow (`oauth.rs`)

This is Google's [OAuth for installed apps](https://developers.google.com/identity/protocols/oauth2/native-app) flow:

1. Bind a TCP listener on `127.0.0.1:<random port>`. The redirect URI is `http://127.0.0.1:<port>`; Google accepts any loopback port for Desktop clients.
2. Generate a PKCE verifier (S256 challenge) and a random `state`.
3. Open the consent URL in the **system browser** (`open` crate) with `access_type=offline` and `prompt=select_account consent`. The prompt makes Google always show the account chooser and always return a refresh token.
4. Accept connections until one carries `?code=...`. Connections that send nothing (browser preconnects) are dropped after 3 s, and requests without a code (favicon) get a placeholder page. A wrong `state` aborts the sign-in. The whole wait times out after 5 minutes.
5. Exchange the code at `oauth2.googleapis.com/token` (with `code_verifier`) for an access token and a refresh token.
6. Call Gmail `users/me/profile` to learn the account's email address.
7. Save the refresh token in the OS keychain and the email in `accounts.json`, then start push for the account.

Signing in again to an account that's already added just replaces its token. That's also how an expired sign-in is fixed: **Reconnect** (`reconnect_account`) runs the same sign-in with `login_hint` set to the account, so Google's chooser preselects it. If a different account is chosen there anyway, nothing is saved and the user is told which account to pick.

## Built-in client (`build.rs`)

Google requires every app that uses the Gmail API to have its own OAuth client. The client identifies the app, not the user. `build.rs` embeds one at compile time:

1. The `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET` environment variables, if both are set; otherwise
2. `src-tauri/google-client.json`, the "Desktop app" JSON downloaded from Cloud Console (git-ignored).

The values become `EMBEDDED_GOOGLE_CLIENT_ID` / `EMBEDDED_GOOGLE_CLIENT_SECRET`, read with `option_env!` in `ClientCreds::embedded()`. With no client, the build prints a cargo warning and the app falls back to asking for one in Settings (`client.json`).

Google treats a Desktop client's secret as non-confidential, so shipping it inside the binary is the standard approach for installed apps.

## Scope

The app requests one scope, `https://mail.google.com/`, because it's the only scope Gmail accepts for IMAP, which [push](sync.md) needs. `gmail.modify` would cover everything else the app does (read, label changes, trash), but it can't open an IMAP session. Google's consent screen therefore says the app could permanently delete mail. The code never calls anything that does.

## Tokens

- The **access token** is cached in memory per account and treated as expired 60 s before Google's `expires_in`. When it expires, it's refreshed with the refresh token from the keychain. A token Gmail turns down (401), such as after the app's access was removed in the Google account, is dropped at once (`gmail.rs` notes it, and `cached_token` passes it over), so the next call gets a new one, or finds the sign-in expired.
- The **refresh token** lives in the keychain ([keychain.rs](../src-tauri/src/keychain.rs)), under service `unified-mail`, keyed by email address: Windows Credential Manager through the `keyring` crate, the macOS keychain straight through the Security framework (`security-framework`). It's read from there once per launch, on a blocking thread (`spawn_blocking`) since on macOS the read may wait for an answer to the password prompt, and then kept in memory (`AppState.refresh_tokens`).
- One refresh at a time per account (`AppState.refreshing`): push and every folder's sync start together at launch, and the calls that come second wait and then use the first one's token. Before that, each read the keychain and asked Google on its own.
- **macOS keychain prompts.** On a Mac, reading a keychain item that an earlier build of the app saved asks for the user's password, so every update would ask once per account. Updates hand the sign-ins over to the new version instead (see [macOS keychain](#macos-keychain)).
- If the keychain read fails for another reason (on macOS: the prompt was refused), the account is put in `AppState.unreadable` and not read again until the app restarts or the account signs in again, so the prompt doesn't keep coming back. Its syncs report *"Couldn't read the saved sign-in for X from the system keychain…"*, which says to reopen the app and click Always Allow. It isn't marked expired: the sign-in itself is fine.
- If Google answers a refresh with `invalid_grant`, or the keychain has no token for the account, the account is marked expired (`AppState.expired`) until it signs in again. Syncs report it with `expired: true`, and the UI shows a **Reconnect** line above the message list instead of an error toast (see [ui.md](ui.md#expired-sign-ins)). Other commands for that account fail with *"The sign-in has expired for X. Click Reconnect above the message list to sign in again."* While a Google project is in Testing, its refresh tokens expire after 7 days, so this comes up weekly. While an account is expired, `access_token` returns that error at once: no keychain read, which on macOS could ask for the password on every sync and push retry, and no call to Google.
- A failed IMAP login drops the cached access token, so the next attempt fetches a fresh one.
- Removing an account revokes the token this launch has read at `oauth2.googleapis.com/revoke` (best effort, in the background) and deletes it from the keychain, without reading it. A token not read this launch isn't read then: on macOS that could ask for the keychain password while the window waits, since the command runs on the main thread.

## macOS keychain

macOS keeps two lists on each keychain item, and an app reads the item without asking only if it's on both:

- **Apps** (the access list), each by its designated requirement. **Always Allow** in the password prompt adds the app; **Allow** lets one read through, which is why the app reads each token only once per launch. Releases are signed with a self-signed certificate ([development.md](development.md#mac-signing)), so every release meets the same requirement and stays on this list.
- **Builds** (the partition list). An app with an Apple Developer ID is on it by its team ID, but any other app, this one included, by the hash of its exact build (`cdhash:…`), whatever certificate signed it. So to this list every update is a new app, and its first read of each account's token asks for the password; **Always Allow** adds that build too. Signing releases (0.2.6) was meant to stop the prompts, on the assumption that the first list was all that counted. On the user's Mac, after three signed updates that each asked, each item's first list held the certificate once, and its second list one more build per update.

Deleting an item and saving a new one don't ask, though, from any build, and a new item lists only the build that saved it. The app relies on that:

- **Saving a sign-in** (`Keychain::save`: signing in, Reconnect, taking over from an update) deletes the account's item and saves a new one. It doesn't save over it: the `keyring` crate reads an item before saving over it, which asks, and changing an item keeps its lists.
- **Deleting** finds the item by reference, which doesn't read it (`SecItemCopyMatching` returning refs), and deletes the item itself (`SecKeychainItemDelete`). `SecItemDelete` refuses an item another build saved: *"Invalid attempt to change the owner of this item"*.
- **Updates hand the sign-ins over.** Once `install_update` has downloaded the update and checked its signature, just before installing it, `AppState::hand_over_sign_ins` writes the sign-ins this launch has read to `sign-ins-for-update.json` in the app data folder (JSON, created with mode 0600 through a new temp file). As the new version starts, `AppState::load` reads the file, deletes it, and saves each account still in `accounts.json` as its own item (`take_over_sign_ins`). It keeps the tokens in memory too, so it never reads the old items. If the install fails, the file is deleted (`cancel_hand_over`). The tokens are in that file only from the install to the new version's start, normally a second or two.
- **What still asks:** the first launch of the version that brings the hand-over, since the version before it doesn't hand over; a version installed from a downloaded `.dmg` instead of from the app; an account whose prompt was refused this launch, since it isn't handed over; and local builds and the test app, which share these items with the installed app (see [development.md](development.md#testing-without-your-own-mail)). An account whose sign-in has expired isn't handed over: the new version asks for it once, finds it expired, and doesn't read it again until Reconnect.
- Checked on the login keychain with two builds of a test program around keychain.rs, with keychain dialogs off (`SecKeychainSetUserInteractionAllowed(false)`, so whatever would ask fails instead). The second build couldn't read the first one's item, but replaced it without asking and then read its own; the new item listed only that build; either build could delete it. A throwaway keychain (`security create-keychain`) is no place to test this: its items had no list of builds, which is how the certificate was wrongly thought to be enough. An update from one release to the next isn't checked yet.

Windows Credential Manager has no such lists, so nothing there asks.

## Google project limits

Gmail scopes are *restricted*, which has consequences:

| Project status | Effect |
| --- | --- |
| Testing | Only listed test users can sign in, and **sign-ins expire after 7 days** |
| In production, unverified | "Google hasn't verified this app" warning (Advanced → continue); at most 100 users; no 7-day expiry |
| In production, verified | Needs Google's review plus a yearly third-party security assessment (paid). Only worth it for public distribution |

Publishing (Testing → In production) needs these values on the **Branding** page:

| Field | Value |
| --- | --- |
| Application home page | https://andeliseev.github.io/unified-mail/ |
| Application privacy policy link | https://andeliseev.github.io/unified-mail/privacy.html |
| Authorized domains | `andeliseev.github.io` |

The pages are in the [andeliseev.github.io](https://github.com/andeliseev/andeliseev.github.io) repository (see [development.md](development.md#website-home-page-and-privacy-policy)).

Google Workspace admins can block unverified apps or IMAP for their domain. Using a project inside the same Workspace with the **Internal** audience avoids the verification warning for that domain.
