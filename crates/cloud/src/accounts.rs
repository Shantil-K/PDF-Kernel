//! Accounts on this install: any number of Google accounts plus the local
//! "This device" account. Non-secret details live in `accounts.json`;
//! refresh tokens live in the [`SecretStore`].

use crate::error::{CloudError, Result};
use crate::oauth::GoogleOAuth;
use crate::secrets::SecretStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const LOCAL_ACCOUNT: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Local,
    Google,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// `local` or `google:<subject id>`.
    pub id: String,
    pub kind: AccountKind,
    pub name: Option<String>,
    pub email: Option<String>,
    pub picture: Option<String>,
}

impl Account {
    fn local() -> Self {
        Self {
            id: LOCAL_ACCOUNT.into(),
            kind: AccountKind::Local,
            name: Some("This device".into()),
            email: None,
            picture: None,
        }
    }

    pub fn label(&self) -> String {
        self.email.clone().or_else(|| self.name.clone()).unwrap_or_else(|| self.id.clone())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AccountsFile {
    accounts: Vec<Account>,
    active: Option<String>,
}

pub struct AccountManager {
    path: PathBuf,
    secrets: Arc<SecretStore>,
    oauth: Option<GoogleOAuth>,
    file: Mutex<AccountsFile>,
    /// Access tokens are short-lived and kept only in memory.
    tokens: tokio::sync::Mutex<HashMap<String, (String, Instant)>>,
}

fn refresh_key(account_id: &str) -> String {
    format!("refresh-token:{account_id}")
}

impl AccountManager {
    pub async fn load(path: PathBuf, secrets: Arc<SecretStore>, oauth: Option<GoogleOAuth>) -> Result<Self> {
        let mut file: AccountsFile = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AccountsFile::default(),
            Err(e) => return Err(e.into()),
        };
        if !file.accounts.iter().any(|a| a.kind == AccountKind::Local) {
            file.accounts.insert(0, Account::local());
        }
        Ok(Self { path, secrets, oauth, file: Mutex::new(file), tokens: Default::default() })
    }

    pub fn google_enabled(&self) -> bool {
        self.oauth.is_some()
    }

    pub fn list(&self) -> Vec<Account> {
        self.lock().accounts.clone()
    }

    pub fn get(&self, id: &str) -> Result<Account> {
        self.lock()
            .accounts
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or_else(|| CloudError::UnknownAccount(id.to_string()))
    }

    pub fn active_id(&self) -> String {
        let f = self.lock();
        f.active
            .clone()
            .filter(|id| f.accounts.iter().any(|a| &a.id == id))
            .unwrap_or_else(|| LOCAL_ACCOUNT.to_string())
    }

    pub async fn set_active(&self, id: &str) -> Result<()> {
        self.get(id)?;
        self.lock().active = Some(id.to_string());
        self.save().await
    }

    /// Open the browser, sign in, and add (or refresh) that Google account.
    /// The new account becomes active.
    pub async fn add_google(&self, open_url: impl FnOnce(&str) -> Result<()>) -> Result<Account> {
        let oauth = self.oauth.as_ref().ok_or(CloudError::NotConfigured)?;
        let (tokens, user) = oauth.sign_in(open_url).await?;
        let account = Account {
            id: format!("google:{}", user.sub),
            kind: AccountKind::Google,
            name: user.name,
            email: user.email,
            picture: user.picture,
        };
        match tokens.refresh_token {
            Some(rt) => self.secrets.set(&refresh_key(&account.id), &rt).await?,
            None if self.secrets.get(&refresh_key(&account.id)).await?.is_some() => {}
            None => return Err(CloudError::Auth("Google did not return a refresh token; please try again".into())),
        }
        self.tokens
            .lock()
            .await
            .insert(account.id.clone(), (tokens.access_token, expiry(tokens.expires_in)));
        {
            let mut f = self.lock();
            match f.accounts.iter_mut().find(|a| a.id == account.id) {
                Some(existing) => *existing = account.clone(),
                None => f.accounts.push(account.clone()),
            }
            f.active = Some(account.id.clone());
        }
        self.save().await?;
        Ok(account)
    }

    /// Sign out: revoke the grant, delete the token, forget the account.
    pub async fn remove(&self, id: &str) -> Result<()> {
        let account = self.get(id)?;
        if account.kind == AccountKind::Local {
            return Err(CloudError::Storage("the local account cannot be removed".into()));
        }
        if let (Some(oauth), Ok(Some(rt))) = (&self.oauth, self.secrets.get(&refresh_key(id)).await) {
            oauth.revoke(&rt).await;
        }
        self.secrets.delete(&refresh_key(id)).await?;
        self.tokens.lock().await.remove(id);
        {
            let mut f = self.lock();
            f.accounts.retain(|a| a.id != id);
            if f.active.as_deref() == Some(id) {
                f.active = Some(LOCAL_ACCOUNT.to_string());
            }
        }
        self.save().await
    }

    /// A valid access token for a Google account, refreshing if needed.
    pub async fn access_token(&self, id: &str) -> Result<String> {
        let mut tokens = self.tokens.lock().await;
        if let Some((token, expires)) = tokens.get(id) {
            if *expires > Instant::now() + Duration::from_secs(60) {
                return Ok(token.clone());
            }
        }
        let oauth = self.oauth.as_ref().ok_or(CloudError::NotConfigured)?;
        let label = self.get(id)?.label();
        let refresh = self.secrets.get(&refresh_key(id)).await?.ok_or_else(|| CloudError::Reauth(label.clone()))?;
        let fresh = oauth.refresh(&refresh, &label).await?;
        if let Some(rt) = &fresh.refresh_token {
            self.secrets.set(&refresh_key(id), rt).await?;
        }
        tokens.insert(id.to_string(), (fresh.access_token.clone(), expiry(fresh.expires_in)));
        Ok(fresh.access_token)
    }

    /// Drop a cached access token (after the server rejected it).
    pub async fn invalidate(&self, id: &str) {
        self.tokens.lock().await.remove(id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AccountsFile> {
        self.file.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn save(&self) -> Result<()> {
        let json = serde_json::to_vec_pretty(&*self.lock())?;
        if let Some(dir) = self.path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        crate::secrets::write_private(&self.path, &json).await
    }
}

fn expiry(expires_in: u64) -> Instant {
    Instant::now() + Duration::from_secs(expires_in.max(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_account_always_present_and_default() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(SecretStore::app_storage(dir.path().join("s")));
        let m = AccountManager::load(dir.path().join("accounts.json"), secrets.clone(), None).await.unwrap();
        assert_eq!(m.active_id(), LOCAL_ACCOUNT);
        assert_eq!(m.list().len(), 1);
        assert!(m.remove(LOCAL_ACCOUNT).await.is_err());
        assert!(matches!(m.add_google(|_| Ok(())).await, Err(CloudError::NotConfigured)));
        assert!(m.set_active("google:nope").await.is_err());
    }

    #[tokio::test]
    async fn persists_accounts_and_falls_back_when_active_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        let secrets = Arc::new(SecretStore::app_storage(dir.path().join("s")));
        {
            let m = AccountManager::load(path.clone(), secrets.clone(), None).await.unwrap();
            m.lock().accounts.push(Account {
                id: "google:1".into(),
                kind: AccountKind::Google,
                name: Some("A".into()),
                email: Some("a@example.com".into()),
                picture: None,
            });
            m.set_active("google:1").await.unwrap();
        }
        let m = AccountManager::load(path, secrets.clone(), None).await.unwrap();
        assert_eq!(m.active_id(), "google:1");
        assert_eq!(m.get("google:1").unwrap().label(), "a@example.com");
        // Token missing in secret store → must sign in again (but no oauth here).
        assert!(matches!(m.access_token("google:1").await, Err(CloudError::NotConfigured)));
        m.remove("google:1").await.unwrap();
        assert_eq!(m.active_id(), LOCAL_ACCOUNT);
    }
}
