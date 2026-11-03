//! Accounts, tokens and the inbox cache.
//!
//! Files in the app data dir: `client.json` (OAuth client), `accounts.json` (emails),
//! `inbox-cache.json` (last inbox snapshot). Refresh tokens live in the OS keychain.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use futures_util::{future::join_all, stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::gmail::{self, FullMessage, MessageSummary};
use crate::oauth;

const KEYRING_SERVICE: &str = "unified-mail";
const CLIENT_FILE: &str = "client.json";
const ACCOUNTS_FILE: &str = "accounts.json";
const CACHE_FILE: &str = "inbox-cache.json";
/// Newest inbox messages shown per account.
const PAGE_SIZE: u32 = 50;
/// Parallel metadata requests per account (Gmail allows ~50/s per user).
const FETCH_CONCURRENCY: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCreds {
    pub client_id: String,
    pub client_secret: String,
}

impl ClientCreds {
    /// The client baked in at build time (see build.rs), if any.
    fn embedded() -> Option<Self> {
        Some(Self {
            client_id: option_env!("EMBEDDED_GOOGLE_CLIENT_ID")?.to_string(),
            client_secret: option_env!("EMBEDDED_GOOGLE_CLIENT_SECRET")?.to_string(),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Setup {
    /// True when the OAuth client is built in and users never need to configure one.
    pub built_in_client: bool,
    pub client_id: Option<String>,
    pub accounts: Vec<String>,
}

#[derive(Serialize)]
pub struct InboxResult {
    pub messages: Vec<MessageSummary>,
    pub errors: Vec<AccountError>,
}

#[derive(Serialize)]
pub struct AccountError {
    pub account: String,
    pub message: String,
}

struct AccessToken {
    value: String,
    // Wall clock rather than Instant: Instant stops while a Mac sleeps.
    expires_at: SystemTime,
}

pub struct AppState {
    http: reqwest::Client,
    dir: PathBuf,
    creds: Mutex<Option<ClientCreds>>,
    accounts: Mutex<Vec<String>>,
    tokens: Mutex<HashMap<String, AccessToken>>,
    /// Current inbox of every account, keyed by `account/id`.
    cache: Mutex<HashMap<String, MessageSummary>>,
    /// Messages whose metadata failed to load and were already reported (`account/id`).
    /// They are retried on every sync but reported only once.
    reported_failures: Mutex<HashSet<String>>,
}

/// One account's sync: its inbox, plus a warning if some messages couldn't be loaded.
struct AccountSync {
    inbox: Vec<MessageSummary>,
    warning: Option<String>,
}

impl AppState {
    pub fn load(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        let cached: Vec<MessageSummary> = read_json(&dir.join(CACHE_FILE)).unwrap_or_default();
        Self {
            http: reqwest::Client::builder()
                .user_agent("UnifiedMail/0.1")
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            creds: Mutex::new(read_json(&dir.join(CLIENT_FILE))),
            accounts: Mutex::new(read_json(&dir.join(ACCOUNTS_FILE)).unwrap_or_default()),
            tokens: Mutex::new(HashMap::new()),
            cache: Mutex::new(cached.into_iter().map(|m| (cache_key(&m.account, &m.id), m)).collect()),
            reported_failures: Mutex::new(HashSet::new()),
            dir,
        }
    }

    pub fn setup(&self) -> Setup {
        Setup {
            built_in_client: ClientCreds::embedded().is_some(),
            client_id: self.creds.lock().unwrap().as_ref().map(|c| c.client_id.clone()),
            accounts: self.accounts.lock().unwrap().clone(),
        }
    }

    pub fn save_client(&self, creds: ClientCreds) -> Result<(), String> {
        write_json(&self.dir.join(CLIENT_FILE), &creds)?;
        *self.creds.lock().unwrap() = Some(creds);
        // Tokens issued to a different client can't be refreshed with this one.
        self.tokens.lock().unwrap().clear();
        Ok(())
    }

    /// Runs the browser sign-in and returns the account's email address.
    /// Signing in to an existing account again just replaces its token.
    pub async fn add_account(&self) -> Result<String, String> {
        let creds = self.creds()?;
        let tokens = oauth::authorize(&self.http, &creds).await?;
        let refresh_token = tokens
            .refresh_token
            .ok_or("Google did not return a refresh token. Try adding the account again.")?;
        let email = gmail::profile_email(&self.http, &tokens.access_token).await?;
        keyring_entry(&email)?
            .set_password(&refresh_token)
            .map_err(|e| format!("Could not save the sign-in to the system keychain: {e}"))?;
        self.store_token(&email, tokens.access_token, tokens.expires_in);

        let accounts = {
            let mut accounts = self.accounts.lock().unwrap();
            if !accounts.contains(&email) {
                accounts.push(email.clone());
            }
            accounts.clone()
        };
        write_json(&self.dir.join(ACCOUNTS_FILE), &accounts)?;
        Ok(email)
    }

    pub fn remove_account(&self, email: &str) -> Result<(), String> {
        if let Ok(entry) = keyring_entry(email) {
            if let Ok(refresh_token) = entry.get_password() {
                let http = self.http.clone();
                tauri::async_runtime::spawn(async move { oauth::revoke(&http, &refresh_token).await });
            }
            let _ = entry.delete_credential();
        }
        self.tokens.lock().unwrap().remove(email);
        let accounts = {
            let mut accounts = self.accounts.lock().unwrap();
            accounts.retain(|a| a != email);
            accounts.clone()
        };
        write_json(&self.dir.join(ACCOUNTS_FILE), &accounts)?;
        self.edit_cache(|cache| cache.retain(|_, m| m.account != email));
        Ok(())
    }

    pub fn cached_inbox(&self) -> Vec<MessageSummary> {
        sorted(&self.cache.lock().unwrap())
    }

    /// Syncs every account in parallel. An account that fails keeps its cached
    /// messages and is reported in `errors`.
    pub async fn refresh_inbox(&self) -> InboxResult {
        let accounts = self.accounts.lock().unwrap().clone();
        let results = join_all(accounts.iter().map(|a| self.sync_account(a))).await;

        let mut errors = Vec::new();
        let messages = {
            let mut cache = self.cache.lock().unwrap();
            cache.retain(|_, m| accounts.contains(&m.account));
            for (account, result) in accounts.iter().zip(results) {
                match result {
                    Ok(AccountSync { inbox, warning }) => {
                        cache.retain(|_, m| &m.account != account);
                        cache.extend(inbox.into_iter().map(|m| (cache_key(&m.account, &m.id), m)));
                        if let Some(message) = warning {
                            errors.push(AccountError { account: account.clone(), message });
                        }
                    }
                    Err(message) => errors.push(AccountError { account: account.clone(), message }),
                }
            }
            sorted(&cache)
        };
        let _ = write_json(&self.dir.join(CACHE_FILE), &messages);
        InboxResult { messages, errors }
    }

    /// Lists the account's inbox and fetches metadata only for messages not seen before.
    /// A message that fails to load is skipped, so one odd message can't hide the whole inbox.
    async fn sync_account(&self, account: &str) -> Result<AccountSync, String> {
        let token = self.access_token(account).await?;
        let (inbox, unread) = tokio::try_join!(
            gmail::list_ids(&self.http, &token, &["INBOX"], PAGE_SIZE),
            // Any unread message among the newest PAGE_SIZE is also among the newest
            // PAGE_SIZE unread ones, so this one call keeps read state current.
            gmail::list_ids(&self.http, &token, &["INBOX", "UNREAD"], PAGE_SIZE),
        )?;
        let unread: HashSet<String> = unread.into_iter().collect();

        let mut known: HashMap<String, MessageSummary> = {
            let cache = self.cache.lock().unwrap();
            inbox
                .iter()
                .filter_map(|id| cache.get(&cache_key(account, id)).map(|m| (id.clone(), m.clone())))
                .collect()
        };
        let missing: Vec<String> = inbox.iter().filter(|id| !known.contains_key(*id)).cloned().collect();
        let (http, token) = (&self.http, token.as_str());
        let fetched: Vec<Result<MessageSummary, (String, String)>> = stream::iter(missing)
            .map(|id| async move { gmail::get_summary(http, token, account, &id).await.map_err(|e| (id, e)) })
            .buffer_unordered(FETCH_CONCURRENCY)
            .collect()
            .await;

        let mut new_failures = Vec::new();
        {
            let mut reported = self.reported_failures.lock().unwrap();
            for result in fetched {
                match result {
                    Ok(m) => {
                        reported.remove(&cache_key(account, &m.id));
                        known.insert(m.id.clone(), m);
                    }
                    Err((id, error)) => {
                        if reported.insert(cache_key(account, &id)) {
                            new_failures.push(error);
                        }
                    }
                }
            }
        }
        let warning = new_failures.first().map(|first| match new_failures.len() {
            1 => format!("1 message couldn't be loaded: {first}"),
            n => format!("{n} messages couldn't be loaded: {first}"),
        });

        let inbox = inbox
            .iter()
            .filter_map(|id| known.remove(id))
            .map(|mut m| {
                m.unread = unread.contains(&m.id);
                m
            })
            .collect();
        Ok(AccountSync { inbox, warning })
    }

    /// Opening a message marks it read, as in Gmail.
    pub async fn get_message(&self, account: &str, id: &str) -> Result<FullMessage, String> {
        let token = self.access_token(account).await?;
        let (message, unread) = gmail::get_full(&self.http, &token, id).await?;
        if unread {
            let (http, message_id) = (self.http.clone(), id.to_string());
            tauri::async_runtime::spawn(async move { gmail::mark_read(&http, &token, &message_id).await });
            if let Some(m) = self.cache.lock().unwrap().get_mut(&cache_key(account, id)) {
                m.unread = false;
            }
        }
        Ok(message)
    }

    pub async fn archive(&self, account: &str, id: &str) -> Result<(), String> {
        let token = self.access_token(account).await?;
        gmail::archive(&self.http, &token, id).await?;
        self.forget(account, id);
        Ok(())
    }

    pub async fn trash(&self, account: &str, id: &str) -> Result<(), String> {
        let token = self.access_token(account).await?;
        gmail::trash(&self.http, &token, id).await?;
        self.forget(account, id);
        Ok(())
    }

    fn forget(&self, account: &str, id: &str) {
        self.edit_cache(|cache| {
            cache.remove(&cache_key(account, id));
        });
    }

    fn edit_cache(&self, edit: impl FnOnce(&mut HashMap<String, MessageSummary>)) {
        let messages = {
            let mut cache = self.cache.lock().unwrap();
            edit(&mut cache);
            sorted(&cache)
        };
        let _ = write_json(&self.dir.join(CACHE_FILE), &messages);
    }

    fn creds(&self) -> Result<ClientCreds, String> {
        ClientCreds::embedded()
            .or_else(|| self.creds.lock().unwrap().clone())
            .ok_or_else(|| "Set up the Google OAuth client in Settings first.".to_string())
    }

    /// Drops the cached access token, e.g. after Google rejected it.
    pub fn invalidate_token(&self, account: &str) {
        self.tokens.lock().unwrap().remove(account);
    }

    pub async fn access_token(&self, account: &str) -> Result<String, String> {
        let cached = {
            let tokens = self.tokens.lock().unwrap();
            tokens
                .get(account)
                .filter(|t| SystemTime::now() < t.expires_at)
                .map(|t| t.value.clone())
        };
        if let Some(token) = cached {
            return Ok(token);
        }
        let creds = self.creds()?;
        let refresh_token = keyring_entry(account)?
            .get_password()
            .map_err(|_| format!("No saved sign-in for {account}. Use \"Add account\" to sign in again."))?;
        let tokens = oauth::refresh(&self.http, &creds, &refresh_token).await.map_err(|e| {
            if e == oauth::EXPIRED {
                format!("Sign-in for {account} has expired. Use \"Add account\" to sign in again.")
            } else {
                e
            }
        })?;
        self.store_token(account, tokens.access_token.clone(), tokens.expires_in);
        Ok(tokens.access_token)
    }

    fn store_token(&self, account: &str, value: String, expires_in: u64) {
        let expires_at = SystemTime::now() + Duration::from_secs(expires_in.saturating_sub(60));
        self.tokens
            .lock()
            .unwrap()
            .insert(account.to_string(), AccessToken { value, expires_at });
    }
}

fn cache_key(account: &str, id: &str) -> String {
    format!("{account}/{id}")
}

fn sorted(cache: &HashMap<String, MessageSummary>) -> Vec<MessageSummary> {
    let mut messages: Vec<MessageSummary> = cache.values().cloned().collect();
    messages.sort_by(|a, b| b.date.cmp(&a.date));
    messages
}

fn keyring_entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, account).map_err(|e| format!("System keychain error: {e}"))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Writes via a temp file so a crash mid-write can't leave a truncated file.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    fs::write(&tmp, bytes)
        .and_then(|_| fs::rename(&tmp, path))
        .map_err(|e| format!("Could not save {}: {e}", path.display()))
}
