//! Sign-ins (each account's refresh token) in the system keychain: the macOS keychain or Windows
//! Credential Manager, under one service, keyed by email address.
//!
//! macOS keeps two lists on each keychain item: the apps that may read it, by designated
//! requirement, and the builds that may (its partition list). An app without an Apple Developer ID
//! is on the second by the hash of its exact build (`cdhash:…`), which the self-signed certificate
//! doesn't change, so to the keychain every update is a new build: reading an item an earlier
//! build saved asks for the user's password. Deleting an item and saving a new one don't ask. So
//! `save` replaces the item with one of this build's own, which it then reads without asking, and
//! an update passes the sign-ins on to the new version that way (`AppState::hand_over_sign_ins`,
//! docs/auth.md).

/// The keychain items of one service.
pub struct Keychain {
    service: &'static str,
}

impl Keychain {
    pub const fn new(service: &'static str) -> Self {
        Self { service }
    }
}

#[cfg(target_os = "macos")]
use security_framework::{
    item::{ItemClass, ItemSearchOptions, Limit, Reference, SearchResult},
    os::macos::{keychain::SecKeychain, keychain_item::SecKeychainItem, passwords::find_generic_password},
};

/// `errSecItemNotFound`: there's no such item.
#[cfg(target_os = "macos")]
const NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
impl Keychain {
    /// The account's sign-in, or None when there's none. Reading an item an earlier build saved
    /// asks for the user's password, and refusing is an error.
    pub fn read(&self, account: &str) -> Result<Option<String>, String> {
        match find_generic_password(Some(&[login()?]), self.service, account) {
            Ok((password, _)) => {
                String::from_utf8(password.to_vec()).map(Some).map_err(|_| "The saved sign-in isn't text.".to_string())
            }
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Saves the account's sign-in as a new item of this build's, replacing any other, without
    /// asking. Changing the old item instead would keep its lists, and keychain libraries that
    /// save over an item read it first, which asks.
    pub fn save(&self, account: &str, token: &str) -> Result<(), String> {
        self.delete(account)?;
        login()?.add_generic_password(self.service, account, token.as_bytes()).map_err(|e| e.to_string())
    }

    /// Deletes the account's sign-in, if there is one, without reading it, so without asking.
    /// (`SecItemDelete` won't delete an item another build saved: "Invalid attempt to change the
    /// owner of this item". Deleting the item itself does.)
    pub fn delete(&self, account: &str) -> Result<(), String> {
        for item in self.items(account)? {
            item.delete();
        }
        match self.items(account)?.is_empty() {
            true => Ok(()),
            false => Err("The keychain kept the earlier sign-in.".to_string()),
        }
    }

    /// The account's items, found without reading them.
    fn items(&self, account: &str) -> Result<Vec<SecKeychainItem>, String> {
        let found = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .keychains(&[login()?])
            .service(self.service)
            .account(account)
            .load_refs(true)
            .limit(Limit::All)
            .search();
        match found {
            Ok(found) => Ok(found
                .into_iter()
                .filter_map(|found| match found {
                    SearchResult::Ref(Reference::KeychainItem(item)) => Some(item),
                    _ => None,
                })
                .collect()),
            Err(e) if e.code() == NOT_FOUND => Ok(Vec::new()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// The user's default keychain (the login keychain), where sign-ins have always been saved.
#[cfg(target_os = "macos")]
fn login() -> Result<SecKeychain, String> {
    SecKeychain::default().map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
impl Keychain {
    /// The account's sign-in, or None when there's none.
    pub fn read(&self, account: &str) -> Result<Option<String>, String> {
        match self.entry(account)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn save(&self, account: &str, token: &str) -> Result<(), String> {
        self.entry(account)?.set_password(token).map_err(|e| e.to_string())
    }

    /// Deletes the account's sign-in, if there is one.
    pub fn delete(&self, account: &str) -> Result<(), String> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(self.service, account).map_err(|e| e.to_string())
    }
}
