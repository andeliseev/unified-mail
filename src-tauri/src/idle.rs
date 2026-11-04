//! Instant new-mail notifications: one IMAP IDLE connection per account.
//!
//! Gmail writes a line to an idle IMAP connection as soon as the inbox changes
//! (new mail, or mail archived/read elsewhere). We then tell the UI, which runs
//! the normal Gmail API sync. IMAP is used only as this signal.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use async_imap::extensions::idle::IdleResponse;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_native_tls::{native_tls, TlsConnector, TlsStream};

use crate::state::AppState;

/// UI event fired when an account's inbox changed; payload is the account email.
pub const MAIL_CHANGED: &str = "mail-changed";

/// Re-issue IDLE well within Gmail's ~29 minute limit. A connection that died
/// silently (e.g. while the computer slept) is noticed at the next cycle.
const IDLE_CYCLE: Duration = Duration::from_secs(5 * 60);
/// Limit for every other IMAP step.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

type Session = async_imap::Session<TlsStream<TcpStream>>;

/// Running watcher task per account.
#[derive(Default)]
pub struct Watchers(Mutex<HashMap<String, JoinHandle<()>>>);

/// Starts push for `account`, replacing any watcher it already had.
pub fn start(app: &AppHandle, account: &str) {
    let task = tauri::async_runtime::spawn(watch(app.clone(), account.to_string()));
    let old = app.state::<Watchers>().0.lock().unwrap().insert(account.to_string(), task);
    if let Some(old) = old {
        old.abort();
    }
}

pub fn stop(app: &AppHandle, account: &str) {
    let task = app.state::<Watchers>().0.lock().unwrap().remove(account);
    if let Some(task) = task {
        task.abort();
    }
}

/// Keeps an IDLE connection open forever, reconnecting with backoff.
async fn watch(app: AppHandle, account: String) {
    let mut backoff = MIN_BACKOFF;
    let mut connected_before = false;
    loop {
        let error = match connect(&app, &account).await {
            Ok(session) => {
                if connected_before {
                    // Anything could have changed while we were disconnected.
                    let _ = app.emit(MAIL_CHANGED, &account);
                }
                connected_before = true;
                idle(&app, &account, session, &mut backoff).await
            }
            Err(e) => e,
        };
        eprintln!("[push] {account}: {error}; reconnecting in {}s", backoff.as_secs());
        sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn connect(app: &AppHandle, account: &str) -> Result<Session, String> {
    let state = app.state::<AppState>();
    let host = state.provider(account)?.imap_host();
    let token = state.access_token(account).await?;
    // The cached token may be stale; on any failure the next attempt fetches a fresh one.
    login(host, account, &token).await.inspect_err(|_| state.invalidate_token(account))
}

async fn login(host: &str, account: &str, token: &str) -> Result<Session, String> {
    let tcp = timeout(STEP_TIMEOUT, TcpStream::connect((host, 993)))
        .await
        .map_err(|_| "connection timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let tls = TlsConnector::from(native_tls::TlsConnector::new().map_err(|e| e.to_string())?);
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

    let auth = XOAuth2 { user: account, token, sent: false };
    let mut session = timeout(STEP_TIMEOUT, client.authenticate("XOAUTH2", auth))
        .await
        .map_err(|_| "sign-in timed out".to_string())?
        .map_err(|(e, _)| format!("IMAP sign-in failed: {e}"))?;
    step(session.select("INBOX")).await?;
    Ok(session)
}

/// IDLEs until the connection fails; returns why.
async fn idle(app: &AppHandle, account: &str, mut session: Session, backoff: &mut Duration) -> String {
    loop {
        let mut handle = session.idle();
        if let Err(e) = step(handle.init()).await {
            return e;
        }
        let response = {
            // Dropping `_stop` would interrupt the wait, so keep it alive.
            let (wait, _stop) = handle.wait_with_timeout(IDLE_CYCLE);
            wait.await
        };
        match response {
            Ok(IdleResponse::NewData(_)) => {
                let _ = app.emit(MAIL_CHANGED, account);
            }
            Ok(IdleResponse::Timeout) => {}
            // We never interrupt, so this means the server closed the stream.
            Ok(IdleResponse::ManualInterrupt) => return "connection closed".into(),
            Err(e) => return e.to_string(),
        }
        *backoff = MIN_BACKOFF;
        session = match step(handle.done()).await {
            Ok(session) => session,
            Err(e) => return e,
        };
    }
}

async fn step<T>(future: impl Future<Output = async_imap::error::Result<T>>) -> Result<T, String> {
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
        // On failure Gmail sends an error as a second challenge; an empty reply ends the exchange.
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
        let error = login("imap.gmail.com", "nobody@gmail.com", "not-a-real-token").await.err().unwrap();
        assert!(error.contains("Invalid credentials"), "{error}");
    }
}
