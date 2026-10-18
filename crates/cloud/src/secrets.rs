//! Small secret store: OS keychain when available, otherwise files in the
//! app's private data directory (always the case on Android, where app data
//! is sandboxed per app).

use crate::error::Result;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretBackend {
    /// Windows Credential Manager, macOS Keychain or Linux Secret Service.
    Keychain,
    /// Files readable only by this app/user.
    AppStorage,
}

pub struct SecretStore {
    backend: SecretBackend,
    dir: PathBuf,
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos", target_os = "linux")), allow(dead_code))]
    service: String,
}

impl SecretStore {
    /// Use the keychain if it works on this machine, else app storage.
    pub async fn open(dir: PathBuf, service: &str) -> Self {
        let service = service.to_string();
        let backend = if keychain::probe(&service).await { SecretBackend::Keychain } else { SecretBackend::AppStorage };
        Self { backend, dir, service }
    }

    /// File-based store, for tests or when the keychain must not be used.
    pub fn app_storage(dir: PathBuf) -> Self {
        Self { backend: SecretBackend::AppStorage, dir, service: String::new() }
    }

    pub fn backend(&self) -> SecretBackend {
        self.backend
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        match self.backend {
            SecretBackend::Keychain => keychain::get(&self.service, key).await,
            SecretBackend::AppStorage => match tokio::fs::read_to_string(self.path(key)).await {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            },
        }
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        match self.backend {
            SecretBackend::Keychain => keychain::set(&self.service, key, value).await,
            SecretBackend::AppStorage => {
                tokio::fs::create_dir_all(&self.dir).await?;
                write_private(&self.path(key), value.as_bytes()).await
            }
        }
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        match self.backend {
            SecretBackend::Keychain => keychain::delete(&self.service, key).await,
            SecretBackend::AppStorage => match tokio::fs::remove_file(self.path(key)).await {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
        }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(crate::safe_name(key))
    }
}

/// Write a file only the current user can read, replacing it atomically.
pub(crate) async fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(&tmp).await?;
    tokio::io::AsyncWriteExt::write_all(&mut f, data).await?;
    f.sync_all().await?;
    drop(f);
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
mod keychain {
    use crate::error::{CloudError, Result};

    // keyring's API is blocking, so run it off the async threads.
    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
        tokio::task::spawn_blocking(f).await.map_err(|e| CloudError::Secrets(e.to_string()))?
    }

    fn entry(service: &str, key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(service, key).map_err(|e| CloudError::Secrets(e.to_string()))
    }

    pub async fn probe(service: &str) -> bool {
        let service = service.to_string();
        blocking(move || {
            let e = entry(&service, "probe")?;
            e.set_password("ok").map_err(|e| CloudError::Secrets(e.to_string()))?;
            let ok = e.get_password().map(|v| v == "ok").unwrap_or(false);
            let _ = e.delete_credential();
            Ok(ok)
        })
        .await
        .unwrap_or(false)
    }

    pub async fn get(service: &str, key: &str) -> Result<Option<String>> {
        let (service, key) = (service.to_string(), key.to_string());
        blocking(move || match entry(&service, &key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CloudError::Secrets(e.to_string())),
        })
        .await
    }

    pub async fn set(service: &str, key: &str, value: &str) -> Result<()> {
        let (service, key, value) = (service.to_string(), key.to_string(), value.to_string());
        blocking(move || entry(&service, &key)?.set_password(&value).map_err(|e| CloudError::Secrets(e.to_string()))).await
    }

    pub async fn delete(service: &str, key: &str) -> Result<()> {
        let (service, key) = (service.to_string(), key.to_string());
        blocking(move || match entry(&service, &key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CloudError::Secrets(e.to_string())),
        })
        .await
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod keychain {
    use crate::error::Result;

    pub async fn probe(_: &str) -> bool {
        false
    }
    pub async fn get(_: &str, _: &str) -> Result<Option<String>> {
        unreachable!("keychain not available on this platform")
    }
    pub async fn set(_: &str, _: &str, _: &str) -> Result<()> {
        unreachable!("keychain not available on this platform")
    }
    pub async fn delete(_: &str, _: &str) -> Result<()> {
        unreachable!("keychain not available on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_storage_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = SecretStore::app_storage(dir.path().join("s"));
        assert_eq!(s.get("google-refresh:123").await.unwrap(), None);
        s.set("google-refresh:123", "tok").await.unwrap();
        assert_eq!(s.get("google-refresh:123").await.unwrap().as_deref(), Some("tok"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(s.path("google-refresh:123")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        s.delete("google-refresh:123").await.unwrap();
        s.delete("google-refresh:123").await.unwrap();
        assert_eq!(s.get("google-refresh:123").await.unwrap(), None);
    }
}

#[cfg(all(test, any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod keychain_tests {
    /// Needs a desktop session with a keychain; run with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn keychain_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = super::SecretStore::open(dir.path().to_path_buf(), "pdf-kernel-test").await;
        assert_eq!(s.backend(), super::SecretBackend::Keychain);
        s.set("k", "v").await.unwrap();
        assert_eq!(s.get("k").await.unwrap().as_deref(), Some("v"));
        s.delete("k").await.unwrap();
        assert_eq!(s.get("k").await.unwrap(), None);
    }
}
