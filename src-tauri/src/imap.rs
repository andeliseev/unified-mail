//! IMAP connections over TLS. Gmail accounts sign in with an OAuth access token (SASL XOAUTH2),
//! Yandex accounts with an app password (IMAP LOGIN). Used by push (`idle.rs`) for every account,
//! and for reading Yandex Mail (`yandex.rs`).

use std::future::Future;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_native_tls::{native_tls, TlsConnector, TlsStream};

pub type Session = async_imap::Session<TlsStream<TcpStream>>;

/// Limit for each IMAP step.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// Starts the error when the server turned the sign-in down (as opposed to a network problem),
/// so callers can tell an expired or wrong password apart (`is_rejection`).
const REJECTED: &str = "IMAP sign-in failed";

/// An IMAP server, and how this app talks TLS to it.
pub struct Server {
    pub host: &'static str,
    /// Stay on TLS 1.2. Windows' own TLS (SChannel) drops some servers' long-lived TLS 1.3
    /// connections mid-session with SEC_E_DECRYPT_FAILURE ("the specified data could not be
    /// decrypted"), which broke push for Yandex: https://github.com/curl/curl/issues/9879
    pub tls12_only: bool,
}

pub const GMAIL: Server = Server { host: "imap.gmail.com", tls12_only: false };

/// How an account signs in to its IMAP server.
#[derive(Clone, Copy)]
pub enum Auth<'a> {
    /// An OAuth access token (Gmail).
    OAuth(&'a str),
    /// A password: for Yandex, an app password created in Yandex ID.
    Password(&'a str),
}

/// Whether `error` (from [`login`]) means the server rejected the sign-in itself.
pub fn is_rejection(error: &str) -> bool {
    error.starts_with(REJECTED)
}

/// Connects to the server's port 993 over TLS and signs in as `account`.
pub async fn login(server: &Server, account: &str, auth: Auth<'_>) -> Result<Session, String> {
    let host = server.host;
    let tcp = timeout(STEP_TIMEOUT, TcpStream::connect((host, 993)))
        .await
        .map_err(|_| "connection timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let mut builder = native_tls::TlsConnector::builder();
    if server.tls12_only {
        builder.max_protocol_version(Some(native_tls::Protocol::Tlsv12));
    }
    let tls = TlsConnector::from(builder.build().map_err(|e| e.to_string())?);
    let stream = timeout(STEP_TIMEOUT, tls.connect(host, tcp))
        .await
        .map_err(|_| "TLS handshake timed out".to_string())?
        .map_err(|e| e.to_string())?;

    let mut client = async_imap::Client::new(stream);
    timeout(STEP_TIMEOUT, client.read_response())
        .await
        .map_err(|_| "no greeting from server".to_string())?
        .ok_or("connection closed before greeting")?
        .map_err(|e| e.to_string())?;

    let signed_in = match auth {
        Auth::OAuth(token) => {
            let xoauth2 = XOAuth2 { user: account, token, sent: false };
            timeout(STEP_TIMEOUT, client.authenticate("XOAUTH2", xoauth2)).await
        }
        Auth::Password(password) => timeout(STEP_TIMEOUT, client.login(account, password)).await,
    };
    signed_in
        .map_err(|_| "sign-in timed out".to_string())?
        .map_err(|(e, _)| format!("{REJECTED}: {e}"))
}

/// Runs one IMAP command with the step timeout, turning its error into text.
pub async fn step<T>(future: impl Future<Output = async_imap::error::Result<T>>) -> Result<T, String> {
    timeout(STEP_TIMEOUT, future)
        .await
        .map_err(|_| "IMAP command timed out".to_string())?
        .map_err(|e| e.to_string())
}

/// SASL XOAUTH2: https://developers.google.com/gmail/imap/xoauth2-protocol
struct XOAuth2<'a> {
    user: &'a str,
    token: &'a str,
    sent: bool,
}

impl async_imap::Authenticator for XOAuth2<'_> {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> String {
        // On failure the server sends an error as a second challenge; an empty reply ends the exchange.
        if std::mem::replace(&mut self.sent, true) {
            return String::new();
        }
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to the real imap.gmail.com: run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn gmail_rejects_bad_token_without_hanging() {
        let error = login(&GMAIL, "nobody@gmail.com", Auth::OAuth("not-a-real-token")).await.err().unwrap();
        assert!(error.contains("Invalid credentials"), "{error}");
        assert!(is_rejection(&error));
    }

    /// Talks to the real imap.yandex.com: run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn yandex_rejects_bad_password_without_hanging() {
        let error = login(&crate::yandex::SERVER, "nobody@yandex.ru", Auth::Password("not-a-real-password")).await.err().unwrap();
        assert!(is_rejection(&error), "{error}");
    }
}
