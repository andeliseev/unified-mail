//! The mail services accounts can belong to, and what the rest of the app asks of them.
//!
//! `AppState` keeps accounts, sign-ins and the caches. Everything that depends on an account's
//! service goes through its [`Mailbox`]: listing a folder, loading messages, marking and moving
//! them. Gmail ([`crate::gmail`]) is the only service so far. Another one (docs/todo.md has the
//! plan for Yandex Mail) adds a [`Provider`] variant and its arm in each `Mailbox` method, and
//! the compiler points at every place that needs one.

use std::collections::HashSet;

use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::gmail;

/// Newest messages shown per account in each folder, and the size of each older page.
pub const PAGE_SIZE: u32 = 50;

/// A mail list the app shows, unified across accounts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Folder {
    Inbox,
    /// Everything outside Spam and Trash (Gmail's All Mail).
    All,
    Trash,
}

impl Folder {
    pub const EVERY: [Folder; 3] = [Folder::Inbox, Folder::All, Folder::Trash];
}

/// One row of a message list. Cached on disk so the app can paint before the first sync.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub account: String,
    pub id: String,
    pub thread_id: String,
    pub from: String,
    pub subject: String,
    pub snippet: String,
    /// Milliseconds since the Unix epoch.
    pub date: i64,
    pub unread: bool,
}

/// A message to show in the reader.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FullMessage {
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub date: i64,
    pub html: Option<Body>,
    pub text: Option<Body>,
}

/// A body part as base64url bytes, plus the charset its header declares. The bytes may already
/// be UTF-8 despite that header (Gmail converts some), so the webview tries UTF-8 first, then
/// the declared charset (`decodeBody` in app.js).
#[derive(Serialize)]
pub struct Body {
    pub data: String,
    pub charset: String,
}

/// One page of a folder's message ids, newest first.
pub struct Page {
    pub ids: Vec<String>,
    /// More messages exist beyond this page.
    pub more: bool,
}

/// The mail service an account belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Gmail,
}

impl Provider {
    /// The IMAP server push (IDLE) connects to.
    pub fn imap_host(self) -> &'static str {
        match self {
            Provider::Gmail => "imap.gmail.com",
        }
    }
}

/// An added account. In `accounts.json` a Gmail account is just its email address, as it was
/// before there were providers, so older files load and Gmail-only files are written unchanged.
/// Accounts of other services are saved as `{ "email": ..., "provider": ... }`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Account {
    pub email: String,
    pub provider: Provider,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum StoredAccount {
    Gmail(String),
    WithProvider { email: String, provider: Provider },
}

impl Serialize for Account {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let stored = match self.provider {
            Provider::Gmail => StoredAccount::Gmail(self.email.clone()),
        };
        stored.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Account {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match StoredAccount::deserialize(deserializer)? {
            StoredAccount::Gmail(email) => Account { email, provider: Provider::Gmail },
            StoredAccount::WithProvider { email, provider } => Account { email, provider },
        })
    }
}

/// One account's mail at its service, signed in with `token`.
pub struct Mailbox<'a> {
    pub provider: Provider,
    pub http: &'a Client,
    pub token: &'a str,
    pub account: &'a str,
}

impl Mailbox<'_> {
    /// One page of `folder`: its newest messages, or with `before` (ms) the newest ones older
    /// than that. Such a page may also hold messages from that same second, so callers drop
    /// the ones they already have.
    pub async fn list(&self, folder: Folder, before: Option<i64>) -> Result<Page, String> {
        match self.provider {
            Provider::Gmail => gmail::list(self.http, self.token, folder, before).await,
        }
    }

    /// The unread messages of the same page: every unread message in `list`'s page is among them.
    pub async fn unread(&self, folder: Folder, before: Option<i64>) -> Result<HashSet<String>, String> {
        match self.provider {
            Provider::Gmail => gmail::unread(self.http, self.token, folder, before).await,
        }
    }

    /// Summaries of these messages, for the list. One that fails comes back as `(id, error)`.
    pub async fn summaries(&self, ids: Vec<String>) -> Vec<Result<MessageSummary, (String, String)>> {
        match self.provider {
            Provider::Gmail => gmail::summaries(self.http, self.token, self.account, ids).await,
        }
    }

    pub async fn message(&self, id: &str) -> Result<FullMessage, String> {
        match self.provider {
            Provider::Gmail => gmail::get_full(self.http, self.token, id).await,
        }
    }

    pub async fn set_unread(&self, id: &str, unread: bool) -> Result<(), String> {
        match self.provider {
            Provider::Gmail => gmail::set_unread(self.http, self.token, id, unread).await,
        }
    }

    /// Takes the message out of the Inbox, keeping it in All Mail.
    pub async fn archive(&self, id: &str) -> Result<(), String> {
        match self.provider {
            Provider::Gmail => gmail::archive(self.http, self.token, id).await,
        }
    }

    /// Moves the message to Trash. Returns whether it was in the Inbox, so Undo can put it back.
    pub async fn trash(&self, id: &str) -> Result<bool, String> {
        match self.provider {
            Provider::Gmail => gmail::trash(self.http, self.token, id).await,
        }
    }

    /// Undoes Archive or Delete, or restores from Trash: takes the message out of Trash
    /// (`from_trash`), then puts it back in the Inbox (`to_inbox`).
    pub async fn restore(&self, id: &str, from_trash: bool, to_inbox: bool) -> Result<(), String> {
        match self.provider {
            Provider::Gmail => gmail::restore(self.http, self.token, id, from_trash, to_inbox).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_match_the_ui() {
        let names: Vec<String> = Folder::EVERY.iter().map(|f| serde_json::to_string(f).unwrap()).collect();
        assert_eq!(names, [r#""inbox""#, r#""all""#, r#""trash""#]);
        assert_eq!(serde_json::from_str::<Folder>(r#""trash""#).unwrap(), Folder::Trash);
    }

    #[test]
    fn gmail_accounts_stay_plain_emails() {
        // accounts.json from before providers: a list of email addresses.
        let old = r#"["a@gmail.com","b@example.com"]"#;
        let accounts: Vec<Account> = serde_json::from_str(old).unwrap();
        assert_eq!(accounts[0], Account { email: "a@gmail.com".into(), provider: Provider::Gmail });
        assert_eq!(accounts[1].provider, Provider::Gmail);
        // Written back exactly as it was.
        assert_eq!(serde_json::to_string(&accounts).unwrap(), old);

        let tagged: Vec<Account> = serde_json::from_str(r#"[{"email":"c@gmail.com","provider":"gmail"}]"#).unwrap();
        assert_eq!(tagged[0], Account { email: "c@gmail.com".into(), provider: Provider::Gmail });
        assert!(serde_json::from_str::<Vec<Account>>(r#"[{"email":"d@x.ru","provider":"nope"}]"#).is_err());
    }
}
