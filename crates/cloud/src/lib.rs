//! Optional sync layer for pdf-kernel front-ends.
//!
//! - [`oauth`]: "Sign in with Google" (loopback + PKCE), Drive access limited
//!   to files this app creates (`drive.file`).
//! - [`secrets`]: refresh tokens and API keys in the OS keychain.
//! - [`accounts`]: several Google accounts per install, plus a local
//!   "This device" account that needs no sign-in.
//! - [`library`]: each account's PDFs and chat history, stored in its Drive
//!   (or locally), with offline cache and conflict-free chat merging.
//!
//! The core engine does not depend on this crate.

pub mod accounts;
pub mod chat;
pub mod drive;
pub mod error;
pub mod library;
pub mod local;
pub mod oauth;
pub mod secrets;
pub mod store;
mod time;

pub use accounts::{Account, AccountKind, AccountManager, LOCAL_ACCOUNT};
pub use chat::{ChatEntry, ChatLog};
pub use error::{CloudError, Result};
pub use library::{DocEntry, Library, Synced};
pub use oauth::GoogleOAuth;
pub use secrets::{SecretBackend, SecretStore};
pub use store::AnyStore;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Everything a front-end needs: accounts, secrets and one library per account.
pub struct Cloud {
    dir: PathBuf,
    secrets: Arc<SecretStore>,
    accounts: Arc<AccountManager>,
    device: String,
    libraries: Mutex<HashMap<String, Arc<Library<AnyStore>>>>,
}

impl Cloud {
    /// `dir` is the app's private data directory.
    pub async fn open(dir: PathBuf, oauth: Option<GoogleOAuth>) -> Result<Self> {
        tokio::fs::create_dir_all(&dir).await?;
        let secrets = Arc::new(SecretStore::open(dir.join("secrets"), "pdf-kernel").await);
        let accounts = Arc::new(AccountManager::load(dir.join("accounts.json"), secrets.clone(), oauth).await?);
        let device = device_id(&dir).await?;
        Ok(Self { dir, secrets, accounts, device, libraries: Mutex::new(HashMap::new()) })
    }

    pub fn accounts(&self) -> &Arc<AccountManager> {
        &self.accounts
    }

    pub fn secrets(&self) -> &Arc<SecretStore> {
        &self.secrets
    }

    /// Short id for this install, recorded on chat messages.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The library for an account, created on first use.
    pub fn library(&self, account_id: &str) -> Result<Arc<Library<AnyStore>>> {
        let mut libs = self.libraries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lib) = libs.get(account_id) {
            return Ok(lib.clone());
        }
        let account = self.accounts.get(account_id)?;
        let store = match account.kind {
            AccountKind::Local => AnyStore::Local(local::LocalStore::open(self.dir.join("local-library"))?),
            AccountKind::Google => AnyStore::Drive(drive::DriveStore::new(self.accounts.clone(), account.id.clone())),
        };
        let cache = self.dir.join("cache").join(safe_name(&account.id));
        let lib = Arc::new(Library::new(store, cache, self.device.clone()));
        libs.insert(account_id.to_string(), lib.clone());
        Ok(lib)
    }

    pub fn active_library(&self) -> Result<Arc<Library<AnyStore>>> {
        self.library(&self.accounts.active_id())
    }

    /// Forget an account and its cached files.
    pub async fn remove_account(&self, account_id: &str) -> Result<()> {
        self.accounts.remove(account_id).await?;
        self.libraries.lock().unwrap_or_else(|e| e.into_inner()).remove(account_id);
        let cache = self.dir.join("cache").join(safe_name(account_id));
        let _ = tokio::fs::remove_dir_all(cache).await;
        Ok(())
    }
}

pub(crate) fn safe_name(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

async fn device_id(dir: &std::path::Path) -> Result<String> {
    let path = dir.join("device-id");
    if let Ok(id) = tokio::fs::read_to_string(&path).await {
        let id = id.trim();
        if !id.is_empty() {
            return Ok(id.to_string());
        }
    }
    let id = format!("{}-{}", std::env::consts::OS, &uuid::Uuid::new_v4().simple().to_string()[..8]);
    tokio::fs::write(&path, &id).await?;
    Ok(id)
}
