# Security

Unified Mail handles access to people's email, so security reports are very welcome.

## Reporting a problem

Please **don't open a public issue** for a security problem. Instead, report it privately:

- With GitHub's **Report a vulnerability** button on the repository's **Security** tab, or
- By email to **andeliseev@gmail.com**

Include what you found, how to reproduce it, and what an attacker could do with it. You'll get a reply as soon as possible, and credit in the release notes if you'd like.

## What's in scope

For example:
- Email content escaping its sandbox: scripts running, or reaching the app or its commands
- Sign-in tokens leaking: out of the system keychain, into logs, or to anywhere other than Google
- The sign-in flow being hijacked (loopback redirect, `state`/PKCE handling)
- Links or files from emails opening something other than the browser or mail app

How the app protects against these is described in [docs/ui.md](docs/ui.md#sandbox) and [docs/auth.md](docs/auth.md).
