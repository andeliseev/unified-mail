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

- The **access token** is cached in memory per account and treated as expired 60 s before Google's `expires_in`. When it expires, it's refreshed with the refresh token from the keychain.
- The **refresh token** lives in the keychain (`keyring` crate: Windows Credential Manager, macOS Keychain).
- If Google answers a refresh with `invalid_grant`, or the keychain has no token for the account, the account is marked expired (`AppState.expired`) until it signs in again. Syncs report it with `expired: true`, and the UI shows a **Reconnect** line above the message list instead of an error toast (see [ui.md](ui.md#expired-sign-ins)). Other commands for that account fail with *"The sign-in has expired for X. Click Reconnect above the message list to sign in again."* While a Google project is in Testing, its refresh tokens expire after 7 days, so this comes up weekly.
- A failed IMAP login drops the cached access token, so the next attempt fetches a fresh one.
- Removing an account revokes the token at `oauth2.googleapis.com/revoke` (best effort, in the background) and deletes it from the keychain.

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

The pages are in `site/` (see [development.md](development.md#website-home-page-and-privacy-policy)).

Google Workspace admins can block unverified apps or IMAP for their domain. Using a project inside the same Workspace with the **Internal** audience avoids the verification warning for that domain.
