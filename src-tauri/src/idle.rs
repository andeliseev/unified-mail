//! Instant new-mail notifications: one IMAP IDLE connection per account.
//!
//! The server (Gmail or Yandex) writes a line to an idle IMAP connection as soon as the inbox
//! changes (new mail, or mail archived/read elsewhere). We then tell the UI, which runs the
//! normal sync. Here IMAP is used only as this signal.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_imap::extensions::idle::IdleResponse;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::sleep;

use crate::imap::{self, step, Session};
use crate::state::AppState;

/// UI event fired when an account's inbox changed; payload is the account email.
pub const MAIL_CHANGED: &str = "mail-changed";

/// Re-issue IDLE well within Gmail's ~29 minute limit. A connection that died
/// silently (e.g. while the computer slept) is noticed at the next cycle.
const IDLE_CYCLE: Duration = Duration::from_secs(5 * 60);
const MIN_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

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
    let provider = state.provider(account)?;
    let secret = state.credential(account).await?;
    // The cached token may be stale; on any failure the next attempt fetches a fresh one.
    let mut session = imap::login(provider.imap_server(), account, provider.imap_auth(&secret))
        .await
        .inspect_err(|_| state.invalidate_token(account))?;
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
