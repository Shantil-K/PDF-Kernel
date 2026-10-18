//! Local folder backend for the "This device" account. Same model as
//! Drive: files with metadata properties, stored under one directory.

use crate::error::{CloudError, Result};
use crate::store::{Props, RemoteFile, RemoteStore, KIND};
use crate::time::{now_ms, rfc3339};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    file: RemoteFile,
    parent: Option<String>,
    trashed: bool,
}

pub struct LocalStore {
    dir: PathBuf,
    index: Mutex<BTreeMap<String, Entry>>,
}

impl LocalStore {
    pub fn open(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(dir.join("blobs"))?;
        let index = match std::fs::read(dir.join("index.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { dir, index: Mutex::new(index) })
    }

    fn blob(&self, id: &str) -> PathBuf {
        self.dir.join("blobs").join(id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry>> {
        self.index.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn save_index(&self) -> Result<()> {
        let json = serde_json::to_vec(&*self.lock())?;
        crate::secrets::write_private(&self.dir.join("index.json"), &json).await
    }

    async fn insert(&self, name: &str, parent: Option<&str>, mime: &str, props: Props, data: Option<Vec<u8>>) -> Result<RemoteFile> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let size = data.as_ref().map(|d| d.len() as u64);
        if let Some(d) = data {
            tokio::fs::write(self.blob(&id), d).await?;
        }
        let file = RemoteFile {
            id: id.clone(),
            name: name.to_string(),
            mime_type: mime.to_string(),
            modified: rfc3339(now_ms()),
            size,
            props,
        };
        self.lock().insert(id, Entry { file: file.clone(), parent: parent.map(String::from), trashed: false });
        self.save_index().await?;
        Ok(file)
    }
}

impl RemoteStore for LocalStore {
    async fn list(&self, parent: Option<&str>, kind: &str) -> Result<Vec<RemoteFile>> {
        Ok(self
            .lock()
            .values()
            .filter(|e| !e.trashed && e.parent.as_deref() == parent && e.file.props.get(KIND).map(String::as_str) == Some(kind))
            .map(|e| e.file.clone())
            .collect())
    }

    async fn create_folder(&self, name: &str, parent: Option<&str>, props: Props) -> Result<RemoteFile> {
        self.insert(name, parent, "application/vnd.google-apps.folder", props, None).await
    }

    async fn create_file(&self, name: &str, parent: &str, mime: &str, props: Props, data: Vec<u8>) -> Result<RemoteFile> {
        self.insert(name, Some(parent), mime, props, Some(data)).await
    }

    async fn update_content(&self, id: &str, _mime: &str, data: Vec<u8>) -> Result<RemoteFile> {
        if !self.lock().contains_key(id) {
            return Err(CloudError::NotFound(id.into()));
        }
        let size = data.len() as u64;
        tokio::fs::write(self.blob(id), data).await?;
        let file = {
            let mut idx = self.lock();
            let e = idx.get_mut(id).ok_or_else(|| CloudError::NotFound(id.into()))?;
            e.file.size = Some(size);
            e.file.modified = rfc3339(now_ms());
            e.file.clone()
        };
        self.save_index().await?;
        Ok(file)
    }

    async fn download(&self, id: &str) -> Result<Vec<u8>> {
        match tokio::fs::read(self.blob(id)).await {
            Ok(d) => Ok(d),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(CloudError::NotFound(id.into())),
            Err(e) => Err(e.into()),
        }
    }

    async fn trash(&self, id: &str) -> Result<()> {
        if let Some(e) = self.lock().get_mut(id) {
            e.trashed = true;
        }
        self.save_index().await?;
        let _ = tokio::fs::remove_file(self.blob(id)).await;
        Ok(())
    }
}
