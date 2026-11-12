//! Accounts, tokens and the per-folder message caches.
//!
//! Files in the app data dir: `client.json` (OAuth client), `accounts.json` (emails),
//! `settings.json` (appearance), `<folder>-cache.json` (last snapshot of each folder).
//! Refresh tokens live in the OS keychain.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures_util::{future::join_all, stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::gmail::{self, FullMessage, MessageSummary};
use crate::{mime, oauth};

const KEYRING_SERVICE: &str = "unified-mail";
const CLIENT_FILE: &str = "client.json";
const ACCOUNTS_FILE: &str = "accounts.json";
const SETTINGS_FILE: &str = "settings.json";
/// Newest messages shown per account in each folder.
const PAGE_SIZE: u32 = 50;
/// Parallel metadata requests per account (Gmail allows ~50/s per user).
const FETCH_CONCURRENCY: usize = 8;
/// Summaries kept in memory for pages the caches don't hold (`AppState::recent`); past this
/// many, they're all dropped and collected again.
const RECENT_MAX: usize = 2000;
/// Bytes of parts kept for emails being written (`AppState::parts`); past this, they're dropped.
const PARTS_MAX: usize = 64 * 1024 * 1024;

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
    pub theme: Theme,
    /// The message list's width in px, set by dragging its edge; None for the default.
    pub list_width: Option<u32>,
}

/// Settings → Appearance.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the system's light or dark mode.
    #[default]
    System,
    Light,
    Dark,
}

/// Choices kept in `settings.json`. Missing fields get their defaults, so new ones can be added.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    theme: Theme,
    list_width: Option<u32>,
}

/// A mail list the app shows, unified across accounts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Folder {
    Inbox,
    Sent,
    /// Gmail's drafts, of every account: each row also says which draft it is (`draft`).
    Drafts,
    /// Gmail's All Mail: everything outside Spam and Trash, which is what an unfiltered list
    /// returns, without drafts.
    All,
    Trash,
}

impl Folder {
    const EVERY: [Folder; 5] = [Folder::Inbox, Folder::Sent, Folder::Drafts, Folder::All, Folder::Trash];

    /// Labels a message must carry to be in this folder.
    fn labels(self) -> &'static [&'static str] {
        match self {
            Folder::Inbox => &["INBOX"],
            Folder::Sent => &["SENT"],
            Folder::Drafts => &["DRAFT"],
            Folder::All => &[],
            Folder::Trash => &["TRASH"],
        }
    }

    /// Labels of this folder's unread messages; none for drafts, which are never unread.
    fn unread_labels(self) -> Option<&'static [&'static str]> {
        match self {
            Folder::Inbox => Some(&["INBOX", "UNREAD"]),
            Folder::Sent => Some(&["SENT", "UNREAD"]),
            Folder::Drafts => None,
            Folder::All => Some(&["UNREAD"]),
            Folder::Trash => Some(&["TRASH", "UNREAD"]),
        }
    }

    /// The Gmail search a list of this folder uses, given the one asked for (`q`): All Mail
    /// leaves drafts out, as Gmail's own All Mail does. A draft is a message too, and saving one
    /// as it's written replaces it again and again.
    fn query(self, q: Option<&str>) -> Option<String> {
        match (self, q) {
            (Folder::All, Some(q)) => Some(format!("({q}) -in:drafts")),
            (Folder::All, None) => Some("-in:drafts".into()),
            (_, q) => q.map(str::to_string),
        }
    }

    /// The folder's rows show whom each message went to (`MessageSummary::to`).
    fn shows_recipients(self) -> bool {
        matches!(self, Folder::Sent | Folder::Drafts)
    }

    fn cache_file(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox-cache.json",
            Folder::Sent => "sent-cache.json",
            Folder::Drafts => "drafts-cache.json",
            Folder::All => "all-cache.json",
            Folder::Trash => "trash-cache.json",
        }
    }
}

/// One folder's messages, keyed by `account/id`.
type FolderCache = HashMap<String, MessageSummary>;

/// A part of another message that goes along with an email: its name, type and bytes.
type CarriedFile = (String, String, Arc<Vec<u8>>);

#[derive(Serialize)]
pub struct FolderResult {
    pub messages: Vec<MessageSummary>,
    pub errors: Vec<AccountError>,
    /// For each account that synced: whether it has older messages than its newest page.
    pub more: HashMap<String, bool>,
}

/// One account's messages older than a date, loaded while scrolling.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderPage {
    pub messages: Vec<MessageSummary>,
    /// Even older messages exist.
    pub more: bool,
    /// All Mail only: ids of these messages that are in the Inbox.
    pub inbox: Vec<String>,
    pub warning: Option<String>,
}

/// What Undo needs to know about a message moved to Trash.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Trashed {
    pub was_in_inbox: bool,
    pub was_sent: bool,
}

/// An email written in the app, to send from `account` (`AppState::send`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub account: String,
    #[serde(default)]
    pub to: Vec<mime::Address>,
    #[serde(default)]
    pub cc: Vec<mime::Address>,
    #[serde(default)]
    pub bcc: Vec<mime::Address>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub text: String,
    /// The text as HTML, with the quoted or forwarded email in its own formatting.
    #[serde(default)]
    pub html: Option<String>,
    /// Replying or forwarding: the email answered, so this one joins its conversation.
    #[serde(default)]
    pub answers: Option<Answered>,
    /// The Gmail draft it's saved as, if it is.
    #[serde(default)]
    pub draft_id: Option<String>,
    /// Files picked on this computer.
    #[serde(default)]
    pub files: Vec<PickedFile>,
    /// Parts of the email answered that go along: its files when forwarding, and the pictures
    /// its quoted HTML shows in place (with their `contentId`).
    #[serde(default)]
    pub parts: Vec<CarriedPart>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answered {
    pub id: String,
    pub thread_id: String,
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub references: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedFile {
    pub name: String,
    #[serde(default)]
    pub mime_type: String,
    /// The file's bytes, as base64.
    pub data: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CarriedPart {
    pub part: String,
    #[serde(default)]
    pub content_id: Option<String>,
}

#[derive(Serialize)]
pub struct AccountError {
    pub account: String,
    pub message: String,
    /// Google no longer accepts the account's sign-in: the UI offers Reconnect instead of
    /// showing `message`.
    pub expired: bool,
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
    settings: Mutex<Settings>,
    tokens: Mutex<HashMap<String, AccessToken>>,
    /// Each account's refresh token once read from the keychain, so it's read once per launch.
    /// On macOS a new build of the app (every update) asks for the keychain password on its
    /// first read of each one.
    refresh_tokens: Mutex<HashMap<String, String>>,
    /// Accounts whose keychain read failed (on macOS: the password prompt was refused). They
    /// aren't read again until the app restarts or they sign in again, so the prompt doesn't
    /// keep coming back.
    unreadable: Mutex<HashSet<String>>,
    /// One token refresh at a time per account. Push and every folder's sync start together,
    /// and each would otherwise read the keychain (and ask for the password) and ask Google.
    refreshing: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Accounts whose sign-in Google rejected (or whose keychain entry is gone), until they
    /// sign in again.
    expired: Mutex<HashSet<String>>,
    /// Current contents of each folder, for every account.
    cache: Mutex<HashMap<Folder, FolderCache>>,
    /// Summaries of messages on pages the caches don't hold (search results, older pages), so
    /// searching again or scrolling back doesn't fetch them again. In memory only, keyed by
    /// `account/id`. Locked after `cache` or `reported_failures`, never before them.
    recent: Mutex<FolderCache>,
    /// The name each account sends under, once read (`display_name`); None when none is set.
    names: Mutex<HashMap<String, Option<String>>>,
    /// Parts of other messages that went along with an email, by `account/id/part` (`carried`).
    parts: Mutex<HashMap<String, CarriedFile>>,
    /// Messages whose metadata failed to load and were already reported (`account/id`).
    /// They are retried on every sync but reported only once.
    reported_failures: Mutex<HashSet<String>>,
}

/// One page of an account's folder: its messages, plus a warning if some couldn't be loaded.
struct AccountSync {
    messages: Vec<MessageSummary>,
    warning: Option<String>,
    more: bool,
}

impl AppState {
    pub fn load(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        let cache = Folder::EVERY
            .into_iter()
            .map(|folder| {
                let cached: Vec<MessageSummary> = read_json(&dir.join(folder.cache_file())).unwrap_or_default();
                (folder, cached.into_iter().map(|m| (cache_key(&m.account, &m.id), m)).collect())
            })
            .collect();
        Self {
            http: reqwest::Client::builder()
                .user_agent("UnifiedMail/0.1")
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            creds: Mutex::new(read_json(&dir.join(CLIENT_FILE))),
            accounts: Mutex::new(read_json(&dir.join(ACCOUNTS_FILE)).unwrap_or_default()),
            settings: Mutex::new(read_json(&dir.join(SETTINGS_FILE)).unwrap_or_default()),
            tokens: Mutex::new(HashMap::new()),
            refresh_tokens: Mutex::new(HashMap::new()),
            unreadable: Mutex::new(HashSet::new()),
            refreshing: Mutex::new(HashMap::new()),
            expired: Mutex::new(HashSet::new()),
            cache: Mutex::new(cache),
            recent: Mutex::new(HashMap::new()),
            names: Mutex::new(HashMap::new()),
            parts: Mutex::new(HashMap::new()),
            reported_failures: Mutex::new(HashSet::new()),
            dir,
        }
    }

    pub fn setup(&self) -> Setup {
        let settings = self.settings.lock().unwrap();
        Setup {
            built_in_client: ClientCreds::embedded().is_some(),
            client_id: self.creds.lock().unwrap().as_ref().map(|c| c.client_id.clone()),
            accounts: self.accounts.lock().unwrap().clone(),
            theme: settings.theme,
            list_width: settings.list_width,
        }
    }

    pub fn save_theme(&self, theme: Theme) -> Result<(), String> {
        let mut settings = self.settings.lock().unwrap();
        settings.theme = theme;
        write_json(&self.dir.join(SETTINGS_FILE), &*settings)
    }

    pub fn save_list_width(&self, width: Option<u32>) -> Result<(), String> {
        let mut settings = self.settings.lock().unwrap();
        settings.list_width = width;
        write_json(&self.dir.join(SETTINGS_FILE), &*settings)
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
        self.sign_in(None).await
    }

    /// Signs in to `account` again after its sign-in expired, with Google's chooser preselecting
    /// it. Choosing a different account there changes nothing.
    pub async fn reconnect(&self, account: &str) -> Result<String, String> {
        self.sign_in(Some(account)).await
    }

    async fn sign_in(&self, expected: Option<&str>) -> Result<String, String> {
        let creds = self.creds()?;
        let tokens = oauth::authorize(&self.http, &creds, expected).await?;
        let refresh_token = tokens
            .refresh_token
            .ok_or("Google did not return a refresh token. Try adding the account again.")?;
        let email = gmail::profile_email(&self.http, &tokens.access_token).await?;
        if let Some(expected) = expected.filter(|expected| !expected.eq_ignore_ascii_case(&email)) {
            return Err(format!(
                "You signed in to {email}, not {expected}. To reconnect {expected}, choose it in Google's sign-in."
            ));
        }
        keyring_entry(&email)?
            .set_password(&refresh_token)
            .map_err(|e| format!("Could not save the sign-in to the system keychain: {e}"))?;
        self.refresh_tokens.lock().unwrap().insert(email.clone(), refresh_token);
        self.unreadable.lock().unwrap().remove(&email);
        self.store_token(&email, tokens.access_token, tokens.expires_in);
        self.expired.lock().unwrap().remove(&email);

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
        let known = self.refresh_tokens.lock().unwrap().remove(email);
        self.unreadable.lock().unwrap().remove(email);
        if let Ok(entry) = keyring_entry(email) {
            if let Some(refresh_token) = known.or_else(|| entry.get_password().ok()) {
                let http = self.http.clone();
                tauri::async_runtime::spawn(async move { oauth::revoke(&http, &refresh_token).await });
            }
            let _ = entry.delete_credential();
        }
        self.tokens.lock().unwrap().remove(email);
        self.expired.lock().unwrap().remove(email);
        let accounts = {
            let mut accounts = self.accounts.lock().unwrap();
            accounts.retain(|a| a != email);
            accounts.clone()
        };
        write_json(&self.dir.join(ACCOUNTS_FILE), &accounts)?;
        for folder in Folder::EVERY {
            self.edit_cache(folder, |cache| cache.retain(|_, m| m.account != email));
        }
        self.recent.lock().unwrap().retain(|_, m| m.account != email);
        Ok(())
    }

    pub fn cached(&self, folder: Folder) -> Vec<MessageSummary> {
        sorted(&self.cache.lock().unwrap()[&folder])
    }

    /// Syncs one folder of every account in parallel. An account that fails keeps its
    /// cached messages and is reported in `errors`.
    pub async fn refresh(&self, folder: Folder) -> FolderResult {
        let accounts = self.accounts.lock().unwrap().clone();
        let results = join_all(accounts.iter().map(|a| self.sync_account(a, folder, None))).await;
        let expired = self.expired.lock().unwrap().clone();

        let mut errors = Vec::new();
        let mut more = HashMap::new();
        let messages = {
            let mut caches = self.cache.lock().unwrap();
            let cache = caches.get_mut(&folder).expect("every folder has a cache");
            cache.retain(|_, m| accounts.contains(&m.account));
            for (account, result) in accounts.iter().zip(results) {
                match result {
                    Ok(page) => {
                        more.insert(account.clone(), page.more);
                        cache.retain(|_, m| &m.account != account);
                        cache.extend(page.messages.into_iter().map(|m| (cache_key(&m.account, &m.id), m)));
                        if let Some(message) = page.warning {
                            errors.push(AccountError { account: account.clone(), message, expired: false });
                        }
                    }
                    Err(message) => errors.push(AccountError {
                        account: account.clone(),
                        message,
                        expired: expired.contains(account),
                    }),
                }
            }
            sorted(cache)
        };
        let _ = write_json(&self.dir.join(folder.cache_file()), &messages);
        FolderResult { messages, errors, more }
    }

    /// The account's next page of messages older than `before` (ms), for scrolling.
    /// Older pages aren't cached; the caches hold only each folder's newest page.
    pub async fn older(&self, folder: Folder, account: &str, before: i64) -> Result<OlderPage, String> {
        self.page(folder, account, &gmail::before_query(before)).await
    }

    /// Gmail search in the account's All Mail, which leaves out Spam and Trash as Gmail's own
    /// search does: the newest messages matching `query`, or those older than `before` (ms) as
    /// the results scroll. Results aren't cached on disk.
    pub async fn search(&self, account: &str, query: &str, before: Option<i64>) -> Result<OlderPage, String> {
        if query.trim().is_empty() {
            return Err("Type what to search for.".into());
        }
        self.page(Folder::All, account, &gmail::search_query(query, before)).await
    }

    /// One page of the account's folder matching the Gmail search `q`. In All Mail it also says
    /// which of the page's messages are in the Inbox.
    async fn page(&self, folder: Folder, account: &str, q: &str) -> Result<OlderPage, String> {
        let inbox_ids = async {
            if folder != Folder::All {
                return Ok(Vec::new());
            }
            let token = self.access_token(account).await?;
            // As with unread: any Inbox message in this page is among the newest PAGE_SIZE
            // Inbox messages matching the same search.
            Ok(gmail::list_ids(&self.http, &token, &["INBOX"], false, Some(q), PAGE_SIZE).await?.ids)
        };
        let (page, inbox) = tokio::try_join!(self.sync_account(account, folder, Some(q)), inbox_ids)?;
        Ok(OlderPage { messages: page.messages, more: page.more, inbox, warning: page.warning })
    }

    /// Lists one page of the account's folder (the newest, or those matching `q`) and fetches
    /// metadata only for messages not seen before. A message that fails to load is skipped,
    /// so one odd message can't hide the whole folder.
    async fn sync_account(&self, account: &str, folder: Folder, q: Option<&str>) -> Result<AccountSync, String> {
        let token = self.access_token(account).await?;
        let trash = folder == Folder::Trash;
        let query = folder.query(q);
        let unread_ids = async {
            match folder.unread_labels() {
                // Any unread message in this page is also among the PAGE_SIZE newest unread
                // ones of the same query, so this one call keeps read state current.
                Some(labels) => gmail::list_ids(&self.http, &token, labels, trash, query.as_deref(), PAGE_SIZE).await,
                None => Ok(gmail::Page { ids: Vec::new(), more: false }),
            }
        };
        let (page, unread) = tokio::try_join!(
            gmail::list_ids(&self.http, &token, folder.labels(), trash, query.as_deref(), PAGE_SIZE),
            unread_ids,
        )?;
        let unread: HashSet<String> = unread.ids.into_iter().collect();
        let ids = page.ids;

        let mut known: HashMap<String, MessageSummary> = {
            let caches = self.cache.lock().unwrap();
            let recent = self.recent.lock().unwrap();
            ids.iter()
                .filter_map(|id| {
                    let key = cache_key(account, id);
                    // Summaries don't depend on the folder, so another folder's copy will do.
                    caches[&folder]
                        .get(&key)
                        .or_else(|| caches.values().find_map(|cache| cache.get(&key)))
                        .or_else(|| recent.get(&key))
                        // Not known yet whether it has files (`has_files`), or whom it went to where
                        // the rows show it (`to`): fetched again.
                        .filter(|m| m.has_files.is_some() && (m.to.is_some() || !folder.shows_recipients()))
                        .map(|m| (id.clone(), m.clone()))
                })
                .collect()
        };
        let missing: Vec<String> = ids.iter().filter(|id| !known.contains_key(*id)).cloned().collect();
        let (http, token) = (&self.http, token.as_str());
        let fetched: Vec<Result<MessageSummary, (String, String)>> = stream::iter(missing)
            .map(|id| async move { gmail::get_summary(http, token, account, &id).await.map_err(|e| (id, e)) })
            .buffer_unordered(FETCH_CONCURRENCY)
            .collect()
            .await;

        let mut new_failures = Vec::new();
        {
            let mut reported = self.reported_failures.lock().unwrap();
            let mut recent = self.recent.lock().unwrap();
            for result in fetched {
                match result {
                    Ok(m) => {
                        let key = cache_key(account, &m.id);
                        reported.remove(&key);
                        // A folder's newest page goes into its cache; other pages are kept here.
                        if q.is_some() {
                            if recent.len() >= RECENT_MAX {
                                recent.clear();
                            }
                            recent.insert(key, m.clone());
                        }
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

        let mut messages: Vec<MessageSummary> = ids
            .iter()
            .filter_map(|id| known.remove(id))
            .map(|mut m| {
                m.unread = unread.contains(&m.id);
                m
            })
            .collect();
        // Which draft each draft message is: a draft keeps its id while every save gives it a new
        // message.
        if folder == Folder::Drafts {
            let drafts = gmail::draft_ids(http, token).await?;
            for m in &mut messages {
                m.draft = drafts.get(&m.id).cloned();
            }
        }
        Ok(AccountSync { messages, warning, more: page.more })
    }

    /// Fetches a message to show. The UI marks it read separately (`set_unread`).
    pub async fn get_message(&self, account: &str, id: &str) -> Result<FullMessage, String> {
        let token = self.access_token(account).await?;
        gmail::get_full(&self.http, &token, id).await
    }

    /// Attachments `parts` of a message, each with the name to save it under. `arrived` hears how
    /// many bytes of them have arrived, as they come.
    pub async fn attachments(
        &self,
        account: &str,
        id: &str,
        parts: &[String],
        arrived: &(dyn Fn(u64) + Sync),
    ) -> Result<Vec<(String, Vec<u8>)>, String> {
        let token = self.access_token(account).await?;
        gmail::get_attachments(&self.http, &token, id, parts, arrived).await
    }

    /// Pictures `parts` that a message shows in place (`cid:` links), with their data.
    pub async fn inline_images(&self, account: &str, id: &str, parts: &[String]) -> Result<Vec<gmail::PartData>, String> {
        let token = self.access_token(account).await?;
        gmail::get_inline_images(&self.http, &token, id, parts).await
    }

    /// Marks a message read (opening it, "Mark as read") or unread ("Mark as unread").
    pub async fn set_unread(&self, account: &str, id: &str, unread: bool) -> Result<(), String> {
        let token = self.access_token(account).await?;
        gmail::set_unread(&self.http, &token, id, unread).await?;
        // The same message can be in several folders (Inbox and All Mail).
        let key = cache_key(account, id);
        for cache in self.cache.lock().unwrap().values_mut() {
            if let Some(m) = cache.get_mut(&key) {
                m.unread = unread;
            }
        }
        Ok(())
    }

    pub async fn archive(&self, account: &str, id: &str) -> Result<(), String> {
        let token = self.access_token(account).await?;
        gmail::archive(&self.http, &token, id).await?;
        self.forget(account, id, &[Folder::Inbox]);
        Ok(())
    }

    /// Moves to Trash and reports whether the message was in the Inbox, so Undo can put it back there.
    pub async fn trash(&self, account: &str, id: &str) -> Result<Trashed, String> {
        let token = self.access_token(account).await?;
        let labels = gmail::labels(&self.http, &token, id).await?;
        let was_in_inbox = labels.iter().any(|l| l == "INBOX");
        let was_sent = labels.iter().any(|l| l == "SENT");
        gmail::trash(&self.http, &token, id).await?;
        self.forget(account, id, &[Folder::Inbox, Folder::Sent, Folder::All]);
        Ok(Trashed { was_in_inbox, was_sent })
    }

    /// Undoes Archive or Delete, or restores from Trash: optionally takes the message out of
    /// Trash, then optionally puts it back in the Inbox.
    pub async fn restore(&self, account: &str, id: &str, from_trash: bool, to_inbox: bool) -> Result<(), String> {
        let token = self.access_token(account).await?;
        if from_trash {
            gmail::untrash(&self.http, &token, id).await?;
            self.forget(account, id, &[Folder::Trash]);
        }
        if to_inbox {
            gmail::move_to_inbox(&self.http, &token, id).await?;
        }
        Ok(())
    }

    /// Sends an email written in the app and returns its id. A draft saved in Gmail
    /// (`draft_id`) is saved once more with what was last written and sent as itself, which
    /// moves it to Sent; anything else goes as a new message. Errors are for people.
    pub async fn send(&self, draft: Draft) -> Result<String, String> {
        let (account, draft_id) = (draft.account.clone(), draft.draft_id.clone());
        let (raw, thread, token) = self.write_email(draft, true).await?;
        let Some(id) = draft_id else {
            return gmail::send_email(&self.http, &token, &raw, thread.as_deref()).await;
        };
        let saved = gmail::save_draft(&self.http, &token, Some(&id), &raw, thread.as_deref()).await?;
        let sent = gmail::send_draft(&self.http, &token, &saved.id).await?;
        self.forget_draft(&account, &[&id, &saved.id]);
        Ok(sent)
    }

    /// Saves what's written as a Gmail draft of its account: a new one, or `draft_id` updated.
    /// It may still lack recipients, or have one half typed.
    pub async fn save_draft(&self, draft: Draft) -> Result<gmail::SavedDraft, String> {
        let draft_id = draft.draft_id.clone();
        let (raw, thread, token) = self.write_email(draft, false).await?;
        gmail::save_draft(&self.http, &token, draft_id.as_deref(), &raw, thread.as_deref()).await
    }

    /// Deletes a Gmail draft for good (Discard).
    pub async fn delete_draft(&self, account: &str, id: &str) -> Result<(), String> {
        let token = self.access_token(account).await?;
        gmail::delete_draft(&self.http, &token, id).await?;
        self.forget_draft(account, &[id]);
        Ok(())
    }

    /// A Gmail draft to go on writing, with the email it answers when that's still in its
    /// conversation.
    pub async fn open_draft(&self, account: &str, id: &str) -> Result<gmail::OpenedDraft, String> {
        let token = self.access_token(account).await?;
        let mut draft = gmail::get_draft(&self.http, &token, id).await?;
        if !draft.in_reply_to.trim().is_empty() && !draft.thread_id.is_empty() {
            if let Ok(Some(answered)) = gmail::find_in_thread(&self.http, &token, &draft.thread_id, &draft.in_reply_to).await {
                draft.answered = gmail::get_summary(&self.http, &token, account, &answered).await.ok();
            }
        }
        Ok(draft)
    }

    /// Drafts sent or deleted leave the Drafts folder's cache.
    fn forget_draft(&self, account: &str, ids: &[&str]) {
        self.edit_cache(Folder::Drafts, |cache| {
            cache.retain(|_, m| m.account != account || !m.draft.as_deref().is_some_and(|d| ids.contains(&d)));
        });
    }

    /// Writes out an email from the app, to send (`check`: someone to send it to, every
    /// address right, checked before anything downloads) or as a draft. From gets the account's
    /// name, and the parts of the email answered that go along are fetched. Returns the email,
    /// the conversation it goes in, and the account's access token.
    async fn write_email(&self, draft: Draft, check: bool) -> Result<(Vec<u8>, Option<String>, String), String> {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let mut files = Vec::new();
        for file in &draft.files {
            let data = STANDARD
                .decode(&file.data)
                .map_err(|_| format!("{} couldn't be read. Attach it again.", file.name))?;
            files.push(mime::File { name: file.name.clone(), mime_type: file.mime_type.clone(), content_id: None, data });
        }
        let (in_reply_to, references) = match &draft.answers {
            Some(answered) if !answered.message_id.trim().is_empty() => {
                let mut references: Vec<String> = answered.references.split_whitespace().map(String::from).collect();
                references.push(answered.message_id.trim().to_string());
                (Some(answered.message_id.clone()), references)
            }
            _ => (None, Vec::new()),
        };
        let mut email = mime::Email {
            from: None,
            to: draft.to,
            cc: draft.cc,
            bcc: draft.bcc,
            subject: draft.subject,
            text: draft.text,
            html: draft.html,
            in_reply_to,
            references,
            files,
        };
        if check {
            email.check()?;
        }
        let token = self.access_token(&draft.account).await?;
        // The account's name, as Gmail's own sending shows it. If it can't be read, the email still
        // goes, From filled in by Gmail.
        if let Some(name) = self.display_name(&token, &draft.account).await {
            email.from = Some(mime::Address { name, email: draft.account.clone() });
        }
        if let (Some(answered), false) = (&draft.answers, draft.parts.is_empty()) {
            email.files.extend(self.carried(&token, &draft.account, &answered.id, &draft.parts).await?);
        }
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let raw = if check { email.build(now)? } else { email.write(now) };
        let thread = draft.answers.map(|a| a.thread_id).filter(|t| !t.is_empty());
        Ok((raw, thread, token))
    }

    /// The name Gmail sends `account` under (`gmail::display_name`), read once per launch.
    async fn display_name(&self, token: &str, account: &str) -> Option<String> {
        if let Some(name) = self.names.lock().unwrap().get(account) {
            return name.clone();
        }
        // Not kept if it can't be read, so the next email asks again.
        let name = gmail::display_name(&self.http, token, account).await.ok()?;
        self.names.lock().unwrap().insert(account.to_string(), name.clone());
        name
    }

    /// Parts of message `id` that go along with an email: a forward's files, and the pictures its
    /// quoted HTML shows in place (with their Content-ID). Downloaded once, then kept in memory
    /// (`parts`), since a draft is saved again and again with them.
    async fn carried(&self, token: &str, account: &str, id: &str, parts: &[CarriedPart]) -> Result<Vec<mime::File>, String> {
        let key = |part: &str| format!("{account}/{id}/{part}");
        let cached: HashMap<String, CarriedFile> = {
            let cache = self.parts.lock().unwrap();
            parts.iter().filter_map(|p| cache.get(&key(&p.part)).map(|f| (p.part.clone(), f.clone()))).collect()
        };
        let missing: Vec<String> = parts.iter().map(|p| p.part.clone()).filter(|p| !cached.contains_key(p)).collect();
        let mut found = cached;
        if !missing.is_empty() {
            let fetched = gmail::get_files(&self.http, token, id, &missing).await?;
            let mut cache = self.parts.lock().unwrap();
            let fetched_size: usize = fetched.iter().map(|(_, _, data)| data.len()).sum();
            if cache.values().map(|(_, _, data)| data.len()).sum::<usize>() + fetched_size > PARTS_MAX {
                cache.clear();
            }
            for (part, (name, mime_type, data)) in missing.into_iter().zip(fetched) {
                let file: CarriedFile = (name, mime_type, Arc::new(data));
                cache.insert(key(&part), file.clone());
                found.insert(part, file);
            }
        }
        parts
            .iter()
            .map(|p| {
                let (name, mime_type, data) = found.get(&p.part).ok_or("A file to send along is missing. Open the email again.")?;
                Ok(mime::File { name: name.clone(), mime_type: mime_type.clone(), content_id: p.content_id.clone(), data: data.to_vec() })
            })
            .collect()
    }

    /// Drops a message from folders it just left. Folders it joined pick it up on their next sync.
    fn forget(&self, account: &str, id: &str, folders: &[Folder]) {
        let key = cache_key(account, id);
        for &folder in folders {
            self.edit_cache(folder, |cache| {
                cache.remove(&key);
            });
        }
    }

    fn edit_cache(&self, folder: Folder, edit: impl FnOnce(&mut FolderCache)) {
        let messages = {
            let mut caches = self.cache.lock().unwrap();
            let cache = caches.get_mut(&folder).expect("every folder has a cache");
            edit(cache);
            sorted(cache)
        };
        let _ = write_json(&self.dir.join(folder.cache_file()), &messages);
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
        if let Some(token) = self.cached_token(account) {
            return Ok(token);
        }
        // Calls for the same account wait here for the first one, then find its token.
        let refreshing = self.refreshing.lock().unwrap().entry(account.to_string()).or_default().clone();
        let _refreshing = refreshing.lock().await;
        if let Some(token) = self.cached_token(account) {
            return Ok(token);
        }
        let creds = self.creds()?;
        let refresh_token = self.refresh_token(account)?;
        let tokens = match oauth::refresh(&self.http, &creds, &refresh_token).await {
            Ok(tokens) => tokens,
            Err(e) if e == oauth::EXPIRED => {
                self.refresh_tokens.lock().unwrap().remove(account);
                return Err(self.mark_expired(account, "The sign-in has expired for"));
            }
            Err(e) => return Err(e),
        };
        self.expired.lock().unwrap().remove(account);
        self.store_token(account, tokens.access_token.clone(), tokens.expires_in);
        Ok(tokens.access_token)
    }

    fn cached_token(&self, account: &str) -> Option<String> {
        let tokens = self.tokens.lock().unwrap();
        tokens.get(account).filter(|t| SystemTime::now() < t.expires_at).map(|t| t.value.clone())
    }

    /// The account's refresh token: from memory, or else from the keychain, once per launch.
    fn refresh_token(&self, account: &str) -> Result<String, String> {
        if let Some(token) = self.refresh_tokens.lock().unwrap().get(account) {
            return Ok(token.clone());
        }
        let unreadable = || {
            format!(
                "Couldn't read the saved sign-in for {account} from the system keychain. To try again, quit and \
                 reopen Unified Mail. On a Mac, when it asks for your password, enter it and click Always Allow."
            )
        };
        if self.unreadable.lock().unwrap().contains(account) {
            return Err(unreadable());
        }
        match keyring_entry(account)?.get_password() {
            Ok(token) => {
                self.refresh_tokens.lock().unwrap().insert(account.to_string(), token.clone());
                Ok(token)
            }
            Err(keyring::Error::NoEntry) => Err(self.mark_expired(account, "There's no saved sign-in for")),
            Err(e) => {
                eprintln!("[keychain] {account}: {e}");
                self.unreadable.lock().unwrap().insert(account.to_string());
                Err(unreadable())
            }
        }
    }

    /// Remembers that `account` needs signing in again, and returns the error to show for it.
    fn mark_expired(&self, account: &str, problem: &str) -> String {
        self.expired.lock().unwrap().insert(account.to_string());
        format!("{problem} {account}. Click Reconnect above the message list to sign in again.")
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

fn sorted(cache: &FolderCache) -> Vec<MessageSummary> {
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

/// The identifiers of the app and the test app up to 0.1.11, by the ones they have now.
const EARLIER_IDENTIFIERS: [(&str, &str); 2] =
    [("com.andeliseev.unified-mail", "io.unifiedmail.desktop"), ("com.andeliseev.unified-mail.devtest", "io.unifiedmail.devtest")];

/// Moves the data folder of the earlier identifier (accounts, settings, cached message lists) to `dir`,
/// this identifier's, unless `dir` already has files. Then deletes what the webview kept for the earlier
/// identifier next to each of `webview_dirs`. Sign-ins are in the keychain and don't move.
pub fn move_earlier_data(identifier: &str, dir: &Path, webview_dirs: &[PathBuf]) {
    let Some(&(_, earlier)) = EARLIER_IDENTIFIERS.iter().find(|(now, _)| *now == identifier) else {
        return;
    };
    let old = dir.with_file_name(earlier);
    let has_files = fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some());
    if !old.is_dir() || has_files {
        return;
    }
    let _ = fs::remove_dir(dir); // an empty folder would be in the way
    if fs::rename(&old, dir).is_err() {
        // Windows refuses to move a folder while a file in it is open: copy instead, and keep the
        // earlier webview data, since that app may still be running.
        let _ = fs::create_dir_all(dir);
        for entry in fs::read_dir(&old).into_iter().flatten().flatten() {
            let _ = fs::copy(entry.path(), dir.join(entry.file_name()));
        }
        return;
    }
    for webview in webview_dirs {
        let _ = fs::remove_dir_all(webview.with_file_name(earlier));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty folder under the system's temp folder, for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unified-mail-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn earlier_data_moves_to_the_new_identifier() {
        let base = scratch("move");
        let (old, new) = (base.join("io.unifiedmail.desktop"), base.join("com.andeliseev.unified-mail"));
        let old_web = base.join("web").join("io.unifiedmail.desktop");
        for dir in [&old, &new, &old_web] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(old.join(ACCOUNTS_FILE), r#"["a@example.com"]"#).unwrap();

        move_earlier_data("com.andeliseev.unified-mail", &new, &[base.join("web").join("com.andeliseev.unified-mail")]);

        assert_eq!(fs::read_to_string(new.join(ACCOUNTS_FILE)).unwrap(), r#"["a@example.com"]"#);
        assert!(!old.exists(), "the earlier folder is moved, not copied");
        assert!(!old_web.exists(), "the earlier webview data is deleted");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn data_under_the_new_identifier_stays() {
        let base = scratch("keep");
        let (old, new) = (base.join("io.unifiedmail.devtest"), base.join("com.andeliseev.unified-mail.devtest"));
        let old_web = base.join("web").join("io.unifiedmail.devtest");
        for dir in [&old, &new, &old_web] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(old.join(ACCOUNTS_FILE), "[]").unwrap();
        fs::write(new.join(SETTINGS_FILE), "{}").unwrap();

        move_earlier_data("com.andeliseev.unified-mail.devtest", &new, &[base.join("web").join("com.andeliseev.unified-mail.devtest")]);
        assert!(old.join(ACCOUNTS_FILE).exists() && old_web.exists());
        assert!(!new.join(ACCOUNTS_FILE).exists());

        // Other identifiers have nothing to move.
        move_earlier_data("org.example.other", &base.join("org.example.other"), &[]);
        assert!(!base.join("org.example.other").exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn folder_names_match_the_ui() {
        let names: Vec<String> = Folder::EVERY.iter().map(|f| serde_json::to_string(f).unwrap()).collect();
        assert_eq!(names, [r#""inbox""#, r#""sent""#, r#""drafts""#, r#""all""#, r#""trash""#]);
        assert_eq!(serde_json::from_str::<Folder>(r#""trash""#).unwrap(), Folder::Trash);
    }

    #[test]
    fn all_mail_leaves_drafts_out() {
        assert_eq!(Folder::All.query(None).as_deref(), Some("-in:drafts"));
        assert_eq!(Folder::All.query(Some("from:a OR from:b")).as_deref(), Some("(from:a OR from:b) -in:drafts"));
        assert_eq!(Folder::Inbox.query(None), None);
        assert_eq!(Folder::Drafts.query(Some("before:1")).as_deref(), Some("before:1"));
    }

    #[test]
    fn unread_lists_narrow_the_folder() {
        for folder in Folder::EVERY {
            let Some(unread) = folder.unread_labels() else { continue };
            assert!(folder.labels().iter().all(|l| unread.contains(l)), "{folder:?}");
            assert!(unread.contains(&"UNREAD"), "{folder:?}");
        }
        assert!(Folder::Drafts.unread_labels().is_none());
        let files: HashSet<_> = Folder::EVERY.iter().map(|f| f.cache_file()).collect();
        assert_eq!(files.len(), 5);
        // The Inbox keeps its old file name, so existing caches still load.
        assert_eq!(Folder::Inbox.cache_file(), "inbox-cache.json");
    }
}
