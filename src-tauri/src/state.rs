//! Accounts, tokens and the per-folder message caches.
//!
//! Files in the app data dir: `client.json` (OAuth client), `accounts.json` (emails),
//! `settings.json` (appearance), `<folder>-cache.json` (last snapshot of each folder).
//! Refresh tokens live in the OS keychain (keychain.rs). On macOS an update passes them to the
//! new version in `sign-ins-for-update.json`, for a moment (`AppState::hand_over_sign_ins`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures_util::{future::join_all, stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::gmail::{self, FullMessage, MessageSummary, Thread};
use crate::keychain::Keychain;
use crate::{mime, oauth};

/// Sign-ins, under the service name every version has used.
const KEYCHAIN: Keychain = Keychain::new("unified-mail");
/// macOS: the sign-ins a version hands to the one its update installs (`AppState::hand_over_sign_ins`).
#[cfg(target_os = "macos")]
const HAND_OVER_FILE: &str = "sign-ins-for-update.json";
const CLIENT_FILE: &str = "client.json";
const ACCOUNTS_FILE: &str = "accounts.json";
const SETTINGS_FILE: &str = "settings.json";
/// The conversations of the folders' messages (`AppState::threads`), for the rows' first paint.
const THREADS_FILE: &str = "threads-cache.json";
/// Newest messages shown per account in each folder.
const PAGE_SIZE: u32 = 50;
/// Parallel metadata requests per account (Gmail allows ~50/s per user).
const FETCH_CONCURRENCY: usize = 8;
/// Summaries kept in memory for pages the caches don't hold (`AppState::recent`); past this
/// many, they're all dropped and collected again.
const RECENT_MAX: usize = 2000;
/// Parallel conversation requests per account: each costs Gmail twice a message's metadata.
const THREAD_CONCURRENCY: usize = 4;
/// Conversations listed with their history ids at a folder's sync (`gmail::thread_history`):
/// more than a page of messages can be in, since Gmail may order conversations by their newest
/// message anywhere, not their newest in the folder.
const HISTORY_SIZE: u32 = 2 * PAGE_SIZE;
/// Parallel requests when a conversation's messages go to Trash or leave it one by one
/// (`AppState::move_trash`).
const MOVE_CONCURRENCY: usize = 8;
/// Conversations kept in memory (`AppState::threads`); past this many, those the folders don't
/// show are dropped.
const THREADS_MAX: usize = 3000;
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
    /// The conversations the messages are in, for their rows; one that couldn't be loaded is missing.
    pub threads: Vec<Thread>,
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
    /// The conversations the messages are in, as far as they're known already.
    pub threads: Vec<Thread>,
    /// The ids of the others, and of those known that are behind: the rows don't wait for them,
    /// and the UI asks for them next (`AppState::load_threads`).
    pub pending: Vec<String>,
    pub warning: Option<String>,
}

/// What Undo needs to know about a conversation an action moved: its messages (`ids`), and
/// which of them were in the Inbox, in Sent and in Trash before.
#[derive(Serialize)]
pub struct ThreadMoved {
    pub ids: Vec<String>,
    pub inbox: Vec<String>,
    pub sent: Vec<String>,
    pub trash: Vec<String>,
}

/// Where Undo puts a conversation's messages back (`AppState::restore_thread`): out of Trash,
/// into Trash, into the Inbox and out of it. A list left out moves nothing.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutBack {
    #[serde(default)]
    pub untrash: Vec<String>,
    #[serde(default)]
    pub trash: Vec<String>,
    #[serde(default)]
    pub inbox: Vec<String>,
    #[serde(default)]
    pub archive: Vec<String>,
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
    /// Each account's refresh token once read from the keychain (or handed over by the version
    /// before an update), so it's read once per launch: on macOS, reading one an earlier build
    /// saved asks for the keychain password (keychain.rs).
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
    /// The conversations of messages listed, keyed by `account/thread id`, so a sync fetches
    /// only those that changed (`threads_of`). Locked after `cache`, never before it.
    threads: Mutex<HashMap<String, Thread>>,
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
    threads: Vec<Thread>,
    /// Some of `threads` were fetched, so the conversations on disk are behind.
    threads_changed: bool,
    /// Conversations left to fetch (`OlderPage::pending`).
    pending: Vec<String>,
    warning: Option<String>,
    more: bool,
}

/// The conversations of a page's messages (`AppState::threads_of`).
struct PageThreads {
    /// The copies there are, in the messages' order.
    threads: Vec<Thread>,
    /// Some were fetched just now.
    fetched: bool,
    /// The ids of those still to fetch, when the page didn't wait for them.
    pending: Vec<String>,
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
        let threads: Vec<Thread> = read_json(&dir.join(THREADS_FILE)).unwrap_or_default();
        let state = Self {
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
            threads: Mutex::new(threads.into_iter().map(|t| (cache_key(&t.account, &t.id), t)).collect()),
            recent: Mutex::new(HashMap::new()),
            names: Mutex::new(HashMap::new()),
            parts: Mutex::new(HashMap::new()),
            reported_failures: Mutex::new(HashSet::new()),
            dir,
        };
        #[cfg(target_os = "macos")]
        state.take_over_sign_ins();
        state
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
        KEYCHAIN
            .save(&email, &refresh_token)
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
        // The sign-in is only deleted here, not revoked at Google: Google revokes the app's whole
        // grant, which would sign the account out of the app on the person's other computers too.
        self.refresh_tokens.lock().unwrap().remove(email);
        self.unreadable.lock().unwrap().remove(email);
        if let Err(e) = KEYCHAIN.delete(email) {
            eprintln!("[keychain] {email}: {e}");
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
        self.threads.lock().unwrap().retain(|_, t| t.account != email);
        self.save_threads();
        Ok(())
    }

    /// macOS, just before an update installs: writes the sign-ins this launch has read to a file
    /// only this user can read. The new version saves them to the keychain as its own and deletes
    /// the file as it starts (`take_over_sign_ins`): it couldn't read the items this version saved
    /// without asking for the password (keychain.rs). Sign-ins not read yet stay as they are.
    #[cfg(target_os = "macos")]
    pub fn hand_over_sign_ins(&self) {
        let accounts = self.accounts.lock().unwrap().clone();
        let tokens: HashMap<String, String> = {
            let known = self.refresh_tokens.lock().unwrap();
            accounts.into_iter().filter_map(|account| known.get(&account).cloned().map(|token| (account, token))).collect()
        };
        if tokens.is_empty() {
            return;
        }
        if let Err(e) = write_private(&self.dir.join(HAND_OVER_FILE), &tokens) {
            eprintln!("[keychain] couldn't hand the sign-ins over to the update: {e}");
        }
    }

    /// The update didn't install after all: deletes what `hand_over_sign_ins` wrote.
    #[cfg(target_os = "macos")]
    pub fn cancel_hand_over(&self) {
        let _ = fs::remove_file(self.dir.join(HAND_OVER_FILE));
    }

    /// macOS, at launch: saves the sign-ins the version before an update handed over
    /// (`hand_over_sign_ins`) as this build's own keychain items, so reading them doesn't ask,
    /// and keeps them in memory. The file is deleted first, whatever happens next.
    #[cfg(target_os = "macos")]
    fn take_over_sign_ins(&self) {
        let Some(tokens) = take_private::<HashMap<String, String>>(&self.dir.join(HAND_OVER_FILE)) else {
            return;
        };
        let accounts = self.accounts.lock().unwrap().clone();
        for (account, token) in tokens.into_iter().filter(|(account, _)| accounts.contains(account)) {
            if let Err(e) = KEYCHAIN.save(&account, &token) {
                eprintln!("[keychain] {account}: {e}");
            }
            self.refresh_tokens.lock().unwrap().insert(account, token);
        }
    }

    pub fn cached(&self, folder: Folder) -> Vec<MessageSummary> {
        sorted(&self.cache.lock().unwrap()[&folder])
    }

    /// The conversations saved with the folders, for the rows' first paint.
    pub fn cached_threads(&self) -> Vec<Thread> {
        self.threads.lock().unwrap().values().cloned().collect()
    }

    /// Syncs one folder of every account in parallel. An account that fails keeps its
    /// cached messages and is reported in `errors`.
    pub async fn refresh(&self, folder: Folder) -> FolderResult {
        let accounts = self.accounts.lock().unwrap().clone();
        let results = join_all(accounts.iter().map(|a| self.sync_account(a, folder, None))).await;
        let expired = self.expired.lock().unwrap().clone();

        let mut errors = Vec::new();
        let mut more = HashMap::new();
        let mut threads = Vec::new();
        let mut threads_changed = false;
        let messages = {
            let mut caches = self.cache.lock().unwrap();
            let cache = caches.get_mut(&folder).expect("every folder has a cache");
            cache.retain(|_, m| accounts.contains(&m.account));
            for (account, result) in accounts.iter().zip(results) {
                match result {
                    Ok(page) => {
                        more.insert(account.clone(), page.more);
                        threads.extend(page.threads);
                        threads_changed |= page.threads_changed;
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
        if threads_changed {
            self.save_threads();
        }
        FolderResult { messages, threads, errors, more }
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
        Ok(OlderPage {
            messages: page.messages,
            more: page.more,
            inbox,
            threads: page.threads,
            pending: page.pending,
            warning: page.warning,
        })
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
        // A folder's newest page also asks which of its conversations changed since their copies
        // were made, by anything, anywhere (`behind`). Without an answer they're still fetched
        // for the messages the page lists, so a failure here doesn't fail the sync.
        let history = async {
            if q.is_some() || folder == Folder::Drafts {
                return Ok(None);
            }
            let listed = gmail::thread_history(&self.http, &token, folder.labels(), trash, query.as_deref(), HISTORY_SIZE).await;
            Ok::<_, String>(listed.ok())
        };
        let (page, unread, history) = tokio::try_join!(
            gmail::list_ids(&self.http, &token, folder.labels(), trash, query.as_deref(), PAGE_SIZE),
            unread_ids,
            history,
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
                    // Gone between the list and this call (each save of a draft replaces its
                    // message, say): it's simply not listed.
                    Err((_, error)) if error.starts_with("Gmail error 404") => {}
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
            return Ok(AccountSync {
                messages,
                threads: Vec::new(),
                threads_changed: false,
                pending: Vec::new(),
                warning,
                more: page.more,
            });
        }
        // A folder's newest page waits for its conversations: after the first sync, only those
        // that changed. Search results and older pages don't (`threads_of`).
        let found = self.threads_of(account, token, &messages, history.as_ref(), q.is_none()).await;
        leave_out_drafts(&mut messages, &found.threads);
        Ok(AccountSync {
            messages,
            threads: found.threads,
            threads_changed: found.fetched,
            pending: found.pending,
            warning,
            more: page.more,
        })
    }

    /// The conversations `messages` are in, for their rows. One needs fetching when it isn't
    /// known yet, or when it's behind (`behind`): Gmail lists it with another history id than
    /// its copy's (`history`, a folder's newest page: something in it changed), or the page has
    /// a message it didn't have, or one it has as a draft. With `wait` they're fetched now; one
    /// that can't be loaded is left out, or kept as it was, and tried again at the next sync,
    /// and its row shows what the folder has of it. Without, the page would wait for up to
    /// PAGE_SIZE requests, so it comes with the copies there are, and says which are `pending`.
    async fn threads_of(
        &self,
        account: &str,
        token: &str,
        messages: &[MessageSummary],
        history: Option<&HashMap<String, String>>,
        wait: bool,
    ) -> PageThreads {
        let mut order: Vec<&str> = Vec::new();
        let mut listed: HashMap<&str, Vec<&str>> = HashMap::new();
        for m in messages.iter().filter(|m| !m.thread_id.is_empty()) {
            let ids = listed.entry(m.thread_id.as_str()).or_default();
            if ids.is_empty() {
                order.push(&m.thread_id);
            }
            ids.push(&m.id);
        }
        let (mut found, stale) = {
            let threads = self.threads.lock().unwrap();
            let mut found = HashMap::new();
            let mut stale = Vec::new();
            for &id in &order {
                let Some(thread) = threads.get(&cache_key(account, id)) else {
                    stale.push(id.to_string());
                    continue;
                };
                if behind(thread, &listed[id], history.and_then(|h| h.get(id)).map(String::as_str)) {
                    stale.push(id.to_string());
                }
                found.insert(id.to_string(), thread.clone());
            }
            (found, stale)
        };
        let (fetched, pending) = match wait {
            true => (self.fetch_threads(account, token, stale, history).await, Vec::new()),
            false => (Vec::new(), stale),
        };
        let any = !fetched.is_empty();
        for thread in fetched {
            found.insert(thread.id.clone(), thread);
        }
        PageThreads { threads: order.iter().filter_map(|id| found.remove(*id)).collect(), fetched: any, pending }
    }

    /// Fetches conversations `ids` of `account`, a few at a time, and keeps them; one that can't
    /// be loaded is left out. A copy is noted with the history id a listing just gave for it
    /// (`history`), not the one it came with, so it's behind only once the listing says
    /// something else: if the two ever disagree, that costs one more fetch, not one at every sync.
    async fn fetch_threads(
        &self,
        account: &str,
        token: &str,
        ids: Vec<String>,
        history: Option<&HashMap<String, String>>,
    ) -> Vec<Thread> {
        let http = &self.http;
        let fetched: Vec<Option<Thread>> = stream::iter(ids)
            .map(|id| async move { gmail::get_thread(http, token, account, &id).await.ok() })
            .buffer_unordered(THREAD_CONCURRENCY)
            .collect()
            .await;
        let mut fetched: Vec<Thread> = fetched.into_iter().flatten().collect();
        for thread in &mut fetched {
            if let Some(listed) = history.and_then(|h| h.get(&thread.id)) {
                thread.history_id = listed.clone();
            }
        }
        self.threads.lock().unwrap().extend(fetched.iter().map(|t| (cache_key(account, &t.id), t.clone())));
        fetched
    }

    /// Fetches conversations `ids`, which a page of search results or of older messages came
    /// without (`OlderPage::pending`): the UI asks for a few at a time, and stops once the page
    /// isn't on screen any more. Those that can't be loaded are left out. These are the ones
    /// that add up over a long session, so past THREADS_MAX the ones no folder shows are dropped
    /// (the UI has what it was given).
    pub async fn load_threads(&self, account: &str, ids: Vec<String>) -> Result<Vec<Thread>, String> {
        let token = self.access_token(account).await?;
        let fetched = self.fetch_threads(account, &token, ids, None).await;
        self.trim_threads();
        Ok(fetched)
    }

    /// Fetches conversation `id` to show, and keeps it for its row.
    pub async fn get_thread(&self, account: &str, id: &str) -> Result<Thread, String> {
        let token = self.access_token(account).await?;
        let thread = gmail::get_thread(&self.http, &token, account, id).await?;
        self.threads.lock().unwrap().insert(cache_key(account, id), thread.clone());
        Ok(thread)
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
        // The same message can be in several folders (Inbox and All Mail), and in its conversation.
        let key = cache_key(account, id);
        let thread = {
            let mut caches = self.cache.lock().unwrap();
            let mut thread = None;
            for m in caches.values_mut().filter_map(|cache| cache.get_mut(&key)) {
                m.unread = unread;
                thread = Some(m.thread_id.clone());
            }
            thread
        };
        let mut threads = self.threads.lock().unwrap();
        let conversations: Vec<&mut Thread> = match thread {
            Some(thread) => threads.get_mut(&cache_key(account, &thread)).into_iter().collect(),
            None => threads.values_mut().filter(|t| t.account == account).collect(),
        };
        for m in conversations.into_iter().flat_map(|t| t.messages.iter_mut()).filter(|m| m.id == id) {
            m.unread = unread;
        }
        Ok(())
    }

    /// Archives conversation `thread`: none of its messages stays in the Inbox, as with Gmail's
    /// Archive. Reports which were there, so Undo puts back just those.
    pub async fn archive_thread(&self, account: &str, thread: &str) -> Result<ThreadMoved, String> {
        let token = self.access_token(account).await?;
        let moved = ThreadMoved::from_labels(&gmail::thread_labels(&self.http, &token, thread).await?);
        gmail::modify_thread(&self.http, &token, thread, &[], &["INBOX"]).await?;
        self.forget(account, &moved.ids, &[Folder::Inbox]);
        self.edit_thread(account, thread, |t| relabel(t, None, &[], &["INBOX"]));
        Ok(moved)
    }

    /// Moves conversation `thread` to Trash, every message of it, and reports which were in the
    /// Inbox, in Sent and already in Trash, so Undo can put each back where it was.
    pub async fn trash_thread(&self, account: &str, thread: &str) -> Result<ThreadMoved, String> {
        let token = self.access_token(account).await?;
        let moved = ThreadMoved::from_labels(&gmail::thread_labels(&self.http, &token, thread).await?);
        gmail::trash_thread(&self.http, &token, thread).await?;
        self.forget(account, &moved.ids, &[Folder::Inbox, Folder::Sent, Folder::All]);
        self.edit_thread(account, thread, |t| relabel(t, None, &["TRASH"], &[]));
        Ok(moved)
    }

    /// Move to Inbox in Trash: the messages of conversation `thread` that are in Trash leave it
    /// (all of them do, so one request takes them out) for the Inbox (a draft among them only
    /// leaves Trash: `to_inbox`); the rest of it stays where it is. Reports where they were, for
    /// Undo.
    pub async fn untrash_thread(&self, account: &str, thread: &str) -> Result<ThreadMoved, String> {
        let token = self.access_token(account).await?;
        let labels = gmail::thread_labels(&self.http, &token, thread).await?;
        let moved = ThreadMoved::from_labels(&labels);
        let emails = to_inbox(&labels, true);
        gmail::untrash_thread(&self.http, &token, thread).await?;
        gmail::modify_messages(&self.http, &token, &emails, &["INBOX"], &[]).await?;
        self.forget(account, &moved.trash, &[Folder::Trash]);
        self.edit_thread(account, thread, |t| {
            relabel(t, None, &[], &["TRASH"]);
            relabel(t, Some(&emails), &["INBOX"], &[]);
        });
        Ok(moved)
    }

    /// Move to Inbox in All Mail: the emails of conversation `thread` outside Trash and Spam go
    /// to the Inbox, sent ones too, as with Gmail's own (`to_inbox`). Reports where its messages
    /// were, so Undo takes out of the Inbox just the ones this put there.
    pub async fn unarchive_thread(&self, account: &str, thread: &str) -> Result<ThreadMoved, String> {
        let token = self.access_token(account).await?;
        let labels = gmail::thread_labels(&self.http, &token, thread).await?;
        let emails = to_inbox(&labels, false);
        gmail::modify_messages(&self.http, &token, &emails, &["INBOX"], &[]).await?;
        self.edit_thread(account, thread, |t| relabel(t, Some(&emails), &["INBOX"], &[]));
        Ok(ThreadMoved::from_labels(&labels))
    }

    /// Puts a conversation's messages back where an action took them from (Undo): out of Trash
    /// or into it, into the Inbox or out of it. What was in Trash before the action stays there,
    /// and only the messages that moved leave the folders' caches.
    pub async fn restore_thread(&self, account: &str, thread: &str, to: PutBack) -> Result<(), String> {
        let token = self.access_token(account).await?;
        self.move_trash(&token, thread, &to.untrash, false).await?;
        self.move_trash(&token, thread, &to.trash, true).await?;
        gmail::modify_messages(&self.http, &token, &to.inbox, &["INBOX"], &[]).await?;
        gmail::modify_messages(&self.http, &token, &to.archive, &[], &["INBOX"]).await?;
        self.forget(account, &to.untrash, &[Folder::Trash]);
        self.forget(account, &to.trash, &[Folder::Inbox, Folder::Sent, Folder::All]);
        self.forget(account, &to.archive, &[Folder::Inbox]);
        self.edit_thread(account, thread, |t| {
            relabel(t, Some(&to.untrash), &[], &["TRASH"]);
            relabel(t, Some(&to.trash), &["TRASH"], &[]);
            relabel(t, Some(&to.inbox), &["INBOX"], &[]);
            relabel(t, Some(&to.archive), &[], &["INBOX"]);
        });
        Ok(())
    }

    /// Takes messages `ids` of conversation `thread` out of Trash, or moves them `into` it. One
    /// request does it for a single message, and for the whole conversation where that comes to
    /// the same (`whole_conversation`: nothing else of it is in Trash, or outside it), which is
    /// the usual case and moves all of it or none. Otherwise each message takes a request of its
    /// own, a few at a time; if one fails, others may have moved.
    async fn move_trash(&self, token: &str, thread: &str, ids: &[String], into: bool) -> Result<(), String> {
        async fn one(http: &reqwest::Client, token: &str, id: &str, into: bool) -> Result<(), String> {
            match into {
                true => gmail::trash_message(http, token, id).await,
                false => gmail::untrash_message(http, token, id).await,
            }
        }
        let http = &self.http;
        match ids {
            [] => return Ok(()),
            [id] => return one(http, token, id, into).await,
            _ => {}
        }
        if whole_conversation(&gmail::thread_labels(http, token, thread).await?, ids, into) {
            return match into {
                true => gmail::trash_thread(http, token, thread).await,
                false => gmail::untrash_thread(http, token, thread).await,
            };
        }
        let moved: Vec<Result<(), String>> = stream::iter(ids.to_vec())
            .map(|id| async move { one(http, token, &id, into).await })
            .buffer_unordered(MOVE_CONCURRENCY)
            .collect()
            .await;
        moved.into_iter().collect()
    }

    /// Sends an email written in the app and returns its id. A draft saved in Gmail
    /// (`draft_id`) is saved once more with what was last written and sent as itself, which
    /// moves it to Sent; anything else goes as a new message. Errors are for people.
    pub async fn send(&self, draft: Draft) -> Result<String, String> {
        let (account, draft_id) = (draft.account.clone(), draft.draft_id.clone());
        let (raw, thread, token) = self.write_email(draft, true).await?;
        let sent = match draft_id {
            None => gmail::send_email(&self.http, &token, &raw, thread.as_deref()).await?,
            Some(id) => {
                let saved = gmail::save_draft(&self.http, &token, Some(&id), &raw, thread.as_deref()).await?;
                let sent = gmail::send_draft(&self.http, &token, &saved.id).await?;
                self.forget_draft(&account, &[&id, &saved.id]);
                sent
            }
        };
        // The conversation it answers has one more message: its row counts it at the next sync.
        if let Some(thread) = thread {
            self.threads.lock().unwrap().remove(&cache_key(&account, &thread));
        }
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

    /// Drops messages `ids` from the caches of folders they just left. Folders they joined pick
    /// them up on their next sync.
    fn forget(&self, account: &str, ids: &[String], folders: &[Folder]) {
        if ids.is_empty() {
            return;
        }
        for &folder in folders {
            self.edit_cache(folder, |cache| {
                for id in ids {
                    cache.remove(&cache_key(account, id));
                }
            });
        }
    }

    /// Changes the copy kept of conversation `thread`, if there is one: an action notes there
    /// what it did to the messages' labels (`relabel`), so the rows go on counting them rightly
    /// until a sync fetches the conversation again (Gmail gave it a new history id).
    fn edit_thread(&self, account: &str, thread: &str, edit: impl FnOnce(&mut Thread)) {
        if let Some(thread) = self.threads.lock().unwrap().get_mut(&cache_key(account, thread)) {
            edit(thread);
        }
    }

    /// The keys of the conversations that the folders' messages are in.
    fn shown_threads(&self) -> HashSet<String> {
        let caches = self.cache.lock().unwrap();
        caches.values().flat_map(|cache| cache.values()).map(|m| cache_key(&m.account, &m.thread_id)).collect()
    }

    /// Past THREADS_MAX conversations in memory, drops those the folders don't show: search
    /// results' and older pages'.
    fn trim_threads(&self) {
        if self.threads.lock().unwrap().len() <= THREADS_MAX {
            return;
        }
        let shown = self.shown_threads();
        self.threads.lock().unwrap().retain(|key, _| shown.contains(key));
    }

    /// Writes the conversations of the folders' messages to disk, for the next launch's first
    /// paint. Others stay in memory, until there are more than THREADS_MAX.
    fn save_threads(&self) {
        let shown = self.shown_threads();
        let saved: Vec<Thread> = {
            let mut threads = self.threads.lock().unwrap();
            if threads.len() > THREADS_MAX {
                threads.retain(|key, _| shown.contains(key));
            }
            threads.iter().filter(|(key, _)| shown.contains(*key)).map(|(_, t)| t.clone()).collect()
        };
        let _ = write_json(&self.dir.join(THREADS_FILE), &saved);
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
        // An expired sign-in waits for Reconnect. Reading the keychain again could ask for the
        // password again and again (macOS), and Google would only turn the token down again.
        if self.expired.lock().unwrap().contains(account) {
            return Err(expired_message("The sign-in has expired for", account));
        }
        let creds = self.creds()?;
        let refresh_token = self.refresh_token(account).await?;
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

    /// The account's access token, unless it has run out or Google turned it down (401).
    fn cached_token(&self, account: &str) -> Option<String> {
        let tokens = self.tokens.lock().unwrap();
        tokens
            .get(account)
            .filter(|t| SystemTime::now() < t.expires_at && !gmail::was_rejected(&t.value))
            .map(|t| t.value.clone())
    }

    /// The account's refresh token: from memory, or else from the keychain, once per launch.
    async fn refresh_token(&self, account: &str) -> Result<String, String> {
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
        // On macOS the read may wait for the user to answer a password prompt: not on an async thread.
        let owned = account.to_string();
        let read = tauri::async_runtime::spawn_blocking(move || KEYCHAIN.read(&owned))
            .await
            .map_err(|e| format!("System keychain error: {e}"))?;
        match read {
            Ok(Some(token)) => {
                self.refresh_tokens.lock().unwrap().insert(account.to_string(), token.clone());
                Ok(token)
            }
            Ok(None) => Err(self.mark_expired(account, "There's no saved sign-in for")),
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
        expired_message(problem, account)
    }

    fn store_token(&self, account: &str, value: String, expires_in: u64) {
        let expires_at = SystemTime::now() + Duration::from_secs(expires_in.saturating_sub(60));
        self.tokens
            .lock()
            .unwrap()
            .insert(account.to_string(), AccessToken { value, expires_at });
    }
}

/// What a command for an account that needs signing in again says.
fn expired_message(problem: &str, account: &str) -> String {
    format!("{problem} {account}. Click Reconnect above the message list to sign in again.")
}

impl ThreadMoved {
    fn from_labels(messages: &[(String, Vec<String>)]) -> Self {
        let with = |label: &str| messages.iter().filter(|(_, labels)| labels.iter().any(|l| l == label)).map(|(id, _)| id.clone()).collect();
        Self { ids: messages.iter().map(|(id, _)| id.clone()).collect(), inbox: with("INBOX"), sent: with("SENT"), trash: with("TRASH") }
    }
}

/// The messages of a conversation that Move to Inbox puts in the Inbox, given each one's labels:
/// in Trash those there, elsewhere those outside Trash and Spam, sent ones too, as Gmail's does.
/// Never its drafts, which `threads.modify` would label as well: a folder lists what has its
/// label, so a draft with the Inbox's would be an email there (`leave_out_drafts`).
fn to_inbox(messages: &[(String, Vec<String>)], in_trash: bool) -> Vec<String> {
    let has = |labels: &[String], label: &str| labels.iter().any(|l| l == label);
    messages
        .iter()
        .filter(|(_, labels)| !has(labels, "DRAFT"))
        .filter(|(_, labels)| if in_trash { has(labels, "TRASH") } else { !has(labels, "TRASH") && !has(labels, "SPAM") })
        .map(|(id, _)| id.clone())
        .collect()
}

/// Whether the copy of a conversation is behind a page that lists `listed` of its messages.
/// Gmail lists the conversation with another history id than the copy's (`history`, when the
/// page asked): something in it changed, such as an answer sent from a phone, an email deleted
/// or read elsewhere. Or the copy lacks one of the messages (a new answer), or has one as a
/// draft: a folder other than Drafts lists a draft's message once it's been sent, or while it
/// has the folder's label; fetching the conversation again tells which, and `leave_out_drafts`
/// takes the second out of the page.
fn behind(thread: &Thread, listed: &[&str], history: Option<&str>) -> bool {
    history.is_some_and(|id| id != thread.history_id)
        || listed.iter().any(|id| !thread.messages.iter().any(|m| m.id == *id && !m.is_draft()))
}

/// Adds and removes Gmail labels in the copy of a conversation, on its messages `ids` (None:
/// all of them): what an action just did at Gmail. Only the labels the copy keeps count.
fn relabel(thread: &mut Thread, ids: Option<&[String]>, add: &[&str], remove: &[&str]) {
    for m in thread.messages.iter_mut().filter(|m| ids.is_none_or(|ids| ids.contains(&m.id))) {
        m.labels.retain(|l| !remove.contains(&l.as_str()));
        for label in add {
            if !m.labels.iter().any(|l| l == label) {
                m.labels.push(label.to_string());
            }
        }
    }
}

/// Whether taking messages `ids` of a conversation (`messages`, each with its labels) out of
/// Trash, or moving them `into` it, comes to the same as doing that to the whole conversation:
/// none of its other messages is in Trash, or outside it.
fn whole_conversation(messages: &[(String, Vec<String>)], ids: &[String], into: bool) -> bool {
    messages.iter().filter(|(_, labels)| labels.iter().any(|l| l == "TRASH") != into).all(|(id, _)| ids.contains(id))
}

/// Takes the drafts out of a folder's page of messages: a draft isn't one of the folder's
/// emails, even with its label (Move to Inbox in Gmail on the web labels a conversation's draft
/// too), and would show in its conversation as an email, counted and answered. `threads`, the
/// conversations of the page's messages, tell which they are. Drafts are listed in Drafts.
fn leave_out_drafts(messages: &mut Vec<MessageSummary>, threads: &[Thread]) {
    let drafts: HashSet<&str> =
        threads.iter().flat_map(|t| &t.messages).filter(|m| m.is_draft()).map(|m| m.id.as_str()).collect();
    messages.retain(|m| !drafts.contains(m.id.as_str()));
}

fn cache_key(account: &str, id: &str) -> String {
    format!("{account}/{id}")
}

fn sorted(cache: &FolderCache) -> Vec<MessageSummary> {
    let mut messages: Vec<MessageSummary> = cache.values().cloned().collect();
    messages.sort_by(|a, b| b.date.cmp(&a.date));
    messages
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Writes via a temp file so a crash mid-write can't leave a truncated file. Each write has a
/// temp file of its own: a sync and an action can save the same folder's cache at once.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    static WRITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tmp = path.with_extension(format!("{}.tmp", WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    fs::write(&tmp, bytes).and_then(|_| fs::rename(&tmp, path)).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Could not save {}: {e}", path.display())
    })
}

/// Writes `value` as JSON to a file only this user can read, through a new temp file, which a
/// failed write doesn't leave behind.
#[cfg(target_os = "macos")]
fn write_private<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let written = (|| {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Reads a file `write_private` wrote, and deletes it.
#[cfg(target_os = "macos")]
fn take_private<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    let _ = fs::remove_file(path);
    serde_json::from_slice(&bytes).ok()
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

    #[cfg(target_os = "macos")]
    #[test]
    fn handed_over_sign_ins_are_private_and_read_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("hand-over-file");
        let path = dir.join(HAND_OVER_FILE);
        let tokens = HashMap::from([("a@example.com".to_string(), "token-a".to_string())]);
        write_private(&path, &tokens).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!path.with_extension("tmp").exists());
        assert_eq!(take_private::<HashMap<String, String>>(&path), Some(tokens));
        assert!(!path.exists(), "the file is deleted once read");
        assert_eq!(take_private::<HashMap<String, String>>(&path), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Only the file: taking the sign-ins over would save them to the real keychain.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_update_gets_the_sign_ins_this_launch_has_read() {
        let dir = scratch("hand-over");
        let state = AppState::load(dir.clone());
        *state.accounts.lock().unwrap() = vec!["a@example.com".into(), "b@example.com".into()];
        state.refresh_tokens.lock().unwrap().extend([
            ("a@example.com".to_string(), "token-a".to_string()),
            ("removed@example.com".to_string(), "token-removed".to_string()),
        ]);
        state.hand_over_sign_ins();
        state.cancel_hand_over();
        assert!(!dir.join(HAND_OVER_FILE).exists(), "a failed install deletes it");

        state.hand_over_sign_ins();
        let handed: HashMap<String, String> = take_private(&dir.join(HAND_OVER_FILE)).unwrap();
        assert_eq!(handed, HashMap::from([("a@example.com".to_string(), "token-a".to_string())]));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Without reading the keychain, which could ask for the password on every sync (macOS).
    #[test]
    fn an_expired_sign_in_waits_for_reconnect() {
        let dir = scratch("expired");
        let state = AppState::load(dir.clone());
        state.expired.lock().unwrap().insert("a@example.com".into());
        let error = tauri::async_runtime::block_on(state.access_token("a@example.com")).unwrap_err();
        assert!(error.starts_with("The sign-in has expired for a@example.com"), "{error}");
        assert!(state.refresh_tokens.lock().unwrap().is_empty(), "the keychain wasn't read");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn undo_knows_where_a_conversation_was() {
        let moved = ThreadMoved::from_labels(&[
            ("a".into(), vec!["INBOX".into(), "UNREAD".into()]),
            ("b".into(), vec!["SENT".into()]),
            ("c".into(), vec!["INBOX".into(), "SENT".into()]),
        ]);
        assert_eq!(moved.ids, ["a", "b", "c"]);
        assert_eq!(moved.inbox, ["a", "c"]);
        assert_eq!(moved.sent, ["b", "c"]);
        assert!(moved.trash.is_empty());
    }

    #[test]
    fn move_to_inbox_leaves_drafts_alone() {
        let message = |id: &str, labels: &[&str]| (id.to_string(), labels.iter().map(|l| l.to_string()).collect::<Vec<_>>());
        let messages = [
            message("received", &[]),
            message("sent", &["SENT"]),
            message("draft", &["DRAFT"]),
            message("trashed", &["TRASH"]),
            message("trashed-draft", &["TRASH", "DRAFT"]),
            message("spam", &["SPAM"]),
        ];
        // From All Mail: the conversation's emails outside Trash and Spam, sent ones too.
        assert_eq!(to_inbox(&messages, false), ["received", "sent"]);
        // From Trash: its emails there.
        assert_eq!(to_inbox(&messages, true), ["trashed"]);
    }

    #[test]
    fn one_request_moves_a_conversation_when_all_of_it_moves() {
        let message = |id: &str, labels: &[&str]| (id.to_string(), labels.iter().map(|l| l.to_string()).collect::<Vec<_>>());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        // Undo of Delete: all of it is in Trash, and all of it comes out.
        let deleted = [message("a", &["TRASH"]), message("b", &["TRASH", "SENT"])];
        assert!(whole_conversation(&deleted, &ids(&["a", "b"]), false));
        // "a" was in Trash before the conversation was deleted, and stays there.
        assert!(!whole_conversation(&deleted, &ids(&["b"]), false));
        // Undo of Move to Inbox in Trash: all of it goes back, or a part of it does.
        let restored = [message("a", &["INBOX"]), message("b", &["SENT"])];
        assert!(whole_conversation(&restored, &ids(&["a", "b"]), true));
        assert!(!whole_conversation(&restored, &ids(&["a"]), true));
        // What's in Trash already doesn't have to be among those that go there.
        let partly = [message("a", &["TRASH"]), message("b", &["INBOX"])];
        assert!(whole_conversation(&partly, &ids(&["b"]), true));
    }

    #[test]
    fn an_action_leaves_the_rest_of_a_conversation_where_it_is() {
        let dir = scratch("forget");
        let state = AppState::load(dir.clone());
        let listed = |id: &str| MessageSummary {
            account: "a@example.com".into(),
            id: id.into(),
            thread_id: "t1".into(),
            from: String::new(),
            to: None,
            subject: String::new(),
            snippet: String::new(),
            date: 0,
            unread: false,
            draft: None,
            has_files: Some(false),
        };
        let message = |id: &str, labels: &[&str]| gmail::ThreadMessage {
            id: id.into(),
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            snippet: String::new(),
            date: 0,
            unread: false,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        };
        let key = |id: &str| cache_key("a@example.com", id);
        for folder in [Folder::Inbox, Folder::All] {
            state.cache.lock().unwrap().get_mut(&folder).unwrap().extend([(key("m1"), listed("m1")), (key("m2"), listed("m2"))]);
        }
        let thread = Thread {
            account: "a@example.com".into(),
            id: "t1".into(),
            history_id: "100".into(),
            messages: vec![message("m1", &["INBOX"]), message("m2", &["INBOX", "SENT"])],
        };
        state.threads.lock().unwrap().insert(key("t1"), thread);

        // Undo of Move to Inbox in Trash, where only m1 had been: it goes back there.
        let back = ["m1".to_string()];
        state.forget("a@example.com", &back, &[Folder::Inbox, Folder::Sent, Folder::All]);
        state.edit_thread("a@example.com", "t1", |t| {
            relabel(t, Some(&back), &["TRASH"], &[]);
            relabel(t, Some(&back), &[], &["INBOX"]);
        });
        // The email that never left stays in the folders' caches, and the copy is kept.
        let inbox: Vec<String> = state.cached(Folder::Inbox).into_iter().map(|m| m.id).collect();
        assert_eq!(inbox, ["m2"]);
        assert_eq!(state.cached(Folder::All).len(), 1);
        let labels: Vec<Vec<String>> = state.cached_threads().remove(0).messages.into_iter().map(|m| m.labels).collect();
        assert_eq!(labels, [vec!["TRASH"], vec!["INBOX", "SENT"]]);
        // Every message of it, and a label that's there already isn't added twice.
        state.edit_thread("a@example.com", "t1", |t| relabel(t, None, &["TRASH"], &["INBOX"]));
        let labels: Vec<Vec<String>> = state.cached_threads().remove(0).messages.into_iter().map(|m| m.labels).collect();
        assert_eq!(labels, [vec!["TRASH"], vec!["SENT", "TRASH"]]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_draft_with_a_folders_label_is_not_its_email() {
        let message = |id: &str, labels: &[&str]| gmail::ThreadMessage {
            id: id.into(),
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            snippet: String::new(),
            date: 0,
            unread: false,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        };
        let thread = Thread {
            account: "a@example.com".into(),
            id: "t1".into(),
            history_id: "100".into(),
            messages: vec![message("m1", &["INBOX"]), message("m2", &["DRAFT", "INBOX"])],
        };
        // The copy has what the page lists.
        assert!(!behind(&thread, &["m1"], None));
        // A new answer.
        assert!(behind(&thread, &["m1", "m3"], None));
        // The page lists what the copy has as a draft: it's fetched again, in case it was sent.
        assert!(behind(&thread, &["m1", "m2"], None));
        // Gmail lists the conversation as it was when the copy was made, or as changed since:
        // an answer sent from a phone, which the Inbox's page doesn't list.
        assert!(!behind(&thread, &["m1"], Some("100")));
        assert!(behind(&thread, &["m1"], Some("101")));

        let listed = |id: &str| MessageSummary {
            account: "a@example.com".into(),
            id: id.into(),
            thread_id: "t1".into(),
            from: String::new(),
            to: None,
            subject: String::new(),
            snippet: String::new(),
            date: 0,
            unread: false,
            draft: None,
            has_files: Some(false),
        };
        // Still a draft in the copy just fetched: the page leaves it out.
        let mut page = vec![listed("m1"), listed("m2")];
        leave_out_drafts(&mut page, &[thread]);
        assert_eq!(page.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["m1"]);
    }

    #[test]
    fn conversations_on_disk_are_the_folders_own() {
        let dir = scratch("threads");
        let state = AppState::load(dir.clone());
        let listed = MessageSummary {
            account: "a@example.com".into(),
            id: "m1".into(),
            thread_id: "t1".into(),
            from: String::new(),
            to: None,
            subject: String::new(),
            snippet: String::new(),
            date: 0,
            unread: false,
            draft: None,
            has_files: Some(false),
        };
        state.cache.lock().unwrap().get_mut(&Folder::Inbox).unwrap().insert(cache_key("a@example.com", "m1"), listed);
        let thread = |id: &str| {
            let thread = Thread { account: "a@example.com".into(), id: id.into(), history_id: String::new(), messages: Vec::new() };
            (cache_key("a@example.com", id), thread)
        };
        state.threads.lock().unwrap().extend([thread("t1"), thread("t2")]);
        state.save_threads();
        // A search's conversation (t2) stays in memory only.
        let saved: Vec<String> = AppState::load(dir.clone()).threads.lock().unwrap().keys().cloned().collect();
        assert_eq!(saved, ["a@example.com/t1"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_leaves_no_temp_files() {
        let dir = scratch("write");
        let path = dir.join(ACCOUNTS_FILE);
        write_json(&path, &["a@example.com"]).unwrap();
        write_json(&path, &["b@example.com"]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"["b@example.com"]"#);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "only the file itself");
        fs::remove_dir_all(&dir).unwrap();
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
