//! One account's documents and chats.
//!
//! Layout in the store (Drive or local):
//!
//! ```text
//! PDF Kernel/                      folder   pdfk_kind=root
//!   report.pdf                     file     pdfk_kind=pdf   pdfk_doc=<doc id>
//!   report.pdf.chat.json           file     pdfk_kind=chat  pdfk_doc=<doc id>
//! ```
//!
//! Chat sync: new entries are written to a local "pending" file first, then
//! merged into the remote chat (download → merge → upload → verify). If the
//! network is down they stay pending and go up on the next sync, so nothing
//! typed offline is lost. Merging is a union by entry id, so two devices
//! writing at the same time both keep their messages.

use crate::chat::{ChatEntry, ChatLog};
use crate::error::{CloudError, Result};
use crate::store::{Props, RemoteFile, RemoteStore, DOC, KIND};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};

pub const ROOT_FOLDER: &str = "PDF Kernel";
const SAVE_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocEntry {
    pub doc_id: String,
    pub name: String,
    pub pdf_file_id: String,
    pub size: Option<u64>,
    pub modified: String,
}

impl DocEntry {
    fn from_file(f: RemoteFile) -> Option<Self> {
        Some(Self {
            doc_id: f.props.get(DOC)?.clone(),
            name: f.name,
            pdf_file_id: f.id,
            size: f.size,
            modified: f.modified,
        })
    }
}

/// A result plus whether it came from the offline cache.
#[derive(Debug, Clone, Serialize)]
pub struct Synced<T> {
    pub value: T,
    pub offline: bool,
    /// Chat entries saved locally but not yet uploaded.
    pub pending: usize,
}

pub struct Library<S> {
    store: S,
    cache: PathBuf,
    device: String,
    root: OnceCell<String>,
    chat_lock: Mutex<()>,
}

impl<S: RemoteStore> Library<S> {
    pub fn new(store: S, cache: PathBuf, device: String) -> Self {
        Self { store, cache, device, root: OnceCell::new(), chat_lock: Mutex::new(()) }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    async fn root(&self) -> Result<String> {
        self.root
            .get_or_try_init(|| async {
                // If two devices raced to create it, the oldest wins (list is oldest first).
                if let Some(existing) = self.store.list(None, "root").await?.into_iter().next() {
                    return Ok(existing.id);
                }
                let props = Props::from([(KIND.to_string(), "root".to_string())]);
                Ok(self.store.create_folder(ROOT_FOLDER, None, props).await?.id)
            })
            .await
            .cloned()
    }

    /// All documents, newest first. Falls back to the cache when offline.
    pub async fn documents(&self) -> Result<Synced<Vec<DocEntry>>> {
        let remote = async {
            let root = self.root().await?;
            let mut docs: Vec<DocEntry> =
                self.store.list(Some(&root), "pdf").await?.into_iter().filter_map(DocEntry::from_file).collect();
            docs.sort_by(|a, b| b.modified.cmp(&a.modified));
            Ok::<_, CloudError>(docs)
        };
        match remote.await {
            Ok(docs) => {
                self.write_cache("documents.json", &docs).await;
                Ok(Synced { value: docs, offline: false, pending: 0 })
            }
            Err(e) if e.is_offline() => match self.read_cache::<Vec<DocEntry>>("documents.json").await {
                Some(docs) => Ok(Synced { value: docs, offline: true, pending: 0 }),
                None => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    /// Upload a PDF. Needs a connection.
    pub async fn upload(&self, name: &str, data: Vec<u8>) -> Result<DocEntry> {
        let root = self.root().await?;
        let doc_id = uuid::Uuid::new_v4().simple().to_string();
        let props = Props::from([(KIND.to_string(), "pdf".to_string()), (DOC.to_string(), doc_id.clone())]);
        let file = self.store.create_file(name, &root, "application/pdf", props, data.clone()).await?;
        let doc = DocEntry::from_file(file).ok_or_else(|| CloudError::Storage("uploaded file has no id".into()))?;
        self.write_bytes(&pdf_cache_name(&doc.pdf_file_id), &data).await;
        let mut docs = self.read_cache::<Vec<DocEntry>>("documents.json").await.unwrap_or_default();
        docs.insert(0, doc.clone());
        self.write_cache("documents.json", &docs).await;
        Ok(doc)
    }

    /// The PDF's bytes, from the local cache when available.
    pub async fn pdf_bytes(&self, doc: &DocEntry) -> Result<Vec<u8>> {
        let name = pdf_cache_name(&doc.pdf_file_id);
        if let Ok(bytes) = tokio::fs::read(self.cache.join(&name)).await {
            return Ok(bytes);
        }
        let bytes = self.store.download(&doc.pdf_file_id).await?;
        self.write_bytes(&name, &bytes).await;
        Ok(bytes)
    }

    /// Latest chat for a document (also uploads anything pending).
    pub async fn chat(&self, doc: &DocEntry) -> Result<Synced<ChatLog>> {
        self.sync_chat(doc, Vec::new()).await
    }

    /// Add entries to a document's chat and sync. Works offline: entries are
    /// kept locally and uploaded on a later sync.
    pub async fn sync_chat(&self, doc: &DocEntry, new: Vec<ChatEntry>) -> Result<Synced<ChatLog>> {
        let _guard = self.chat_lock.lock().await;
        let pending_name = format!("pending/{}.json", doc.doc_id);
        let mut pending: Vec<ChatEntry> = self.read_cache(&pending_name).await.unwrap_or_default();
        for e in new {
            if !pending.iter().any(|p| p.id == e.id) {
                pending.push(e);
            }
        }
        if !pending.is_empty() {
            // Durable before touching the network.
            self.write_cache_strict(&pending_name, &pending).await?;
        }

        match self.push_chat(doc, &pending).await {
            Ok(log) => {
                let _ = tokio::fs::remove_file(self.cache.join(&pending_name)).await;
                self.write_cache(&chat_cache_name(&doc.doc_id), &log).await;
                Ok(Synced { value: log, offline: false, pending: 0 })
            }
            Err(e) if e.is_offline() => {
                let mut log = self
                    .read_cache::<ChatLog>(&chat_cache_name(&doc.doc_id))
                    .await
                    .unwrap_or_else(|| ChatLog::new(&doc.doc_id));
                log.merge(pending.clone());
                Ok(Synced { value: log, offline: true, pending: pending.len() })
            }
            Err(e) => Err(e),
        }
    }

    async fn push_chat(&self, doc: &DocEntry, pending: &[ChatEntry]) -> Result<ChatLog> {
        let root = self.root().await?;
        let ids: Vec<String> = pending.iter().map(|e| e.id.clone()).collect();
        for _ in 0..SAVE_ATTEMPTS {
            let files: Vec<RemoteFile> = self
                .store
                .list(Some(&root), "chat")
                .await?
                .into_iter()
                .filter(|f| f.props.get(DOC) == Some(&doc.doc_id))
                .collect();
            // Normally one file; if two devices created one at once, read both
            // and keep writing to the oldest.
            let mut log = ChatLog::new(&doc.doc_id);
            for f in &files {
                log.merge(self.download_chat(&f.id).await?.messages);
            }
            if log.merge(pending.iter().cloned()) == 0 {
                return Ok(log);
            }
            let data = serde_json::to_vec(&log)?;
            let target = match files.first() {
                Some(f) => self.store.update_content(&f.id, "application/json", data).await?,
                None => {
                    let props = Props::from([(KIND.to_string(), "chat".to_string()), (DOC.to_string(), doc.doc_id.clone())]);
                    let name = format!("{}.chat.json", doc.name);
                    self.store.create_file(&name, &root, "application/json", props, data).await?
                }
            };
            // Another device may have written between our read and write.
            let saved = self.download_chat(&target.id).await?;
            if saved.contains_all(&ids) {
                return Ok(log);
            }
        }
        Err(CloudError::Storage("chat changed on another device during save; it will retry on the next sync".into()))
    }

    async fn download_chat(&self, file_id: &str) -> Result<ChatLog> {
        let bytes = self.store.download(file_id).await?;
        // A damaged file should not block new messages; treat it as empty.
        Ok(serde_json::from_slice(&bytes).unwrap_or_default())
    }

    /// Move the PDF and its chat to the trash (Drive keeps trash for 30 days).
    pub async fn delete(&self, doc: &DocEntry) -> Result<()> {
        let root = self.root().await?;
        for f in self.store.list(Some(&root), "chat").await? {
            if f.props.get(DOC) == Some(&doc.doc_id) {
                self.store.trash(&f.id).await?;
            }
        }
        self.store.trash(&doc.pdf_file_id).await?;
        for name in [pdf_cache_name(&doc.pdf_file_id), chat_cache_name(&doc.doc_id), format!("pending/{}.json", doc.doc_id)] {
            let _ = tokio::fs::remove_file(self.cache.join(name)).await;
        }
        let mut docs = self.read_cache::<Vec<DocEntry>>("documents.json").await.unwrap_or_default();
        docs.retain(|d| d.doc_id != doc.doc_id);
        self.write_cache("documents.json", &docs).await;
        Ok(())
    }

    // ---------- local cache ----------

    async fn read_cache<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let bytes = tokio::fs::read(self.cache.join(name)).await.ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Cache writes are best effort; a failure only costs a re-download.
    async fn write_cache<T: Serialize>(&self, name: &str, value: &T) {
        let _ = self.write_cache_strict(name, value).await;
    }

    async fn write_cache_strict<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let path = self.cache.join(name);
        ensure_parent(&path).await?;
        crate::secrets::write_private(&path, &serde_json::to_vec(value)?).await
    }

    async fn write_bytes(&self, name: &str, data: &[u8]) {
        let path = self.cache.join(name);
        if ensure_parent(&path).await.is_ok() {
            let _ = crate::secrets::write_private(&path, data).await;
        }
    }
}

async fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    Ok(())
}

fn pdf_cache_name(file_id: &str) -> String {
    format!("pdf/{}.pdf", crate::safe_name(file_id))
}

fn chat_cache_name(doc_id: &str) -> String {
    format!("chat/{}.json", crate::safe_name(doc_id))
}

/// Lets several libraries share one store (tests, or multiple windows).
impl<T: RemoteStore> RemoteStore for Arc<T> {
    async fn list(&self, parent: Option<&str>, kind: &str) -> Result<Vec<RemoteFile>> {
        (**self).list(parent, kind).await
    }
    async fn create_folder(&self, name: &str, parent: Option<&str>, props: Props) -> Result<RemoteFile> {
        (**self).create_folder(name, parent, props).await
    }
    async fn create_file(&self, name: &str, parent: &str, mime: &str, props: Props, data: Vec<u8>) -> Result<RemoteFile> {
        (**self).create_file(name, parent, mime, props, data).await
    }
    async fn update_content(&self, id: &str, mime: &str, data: Vec<u8>) -> Result<RemoteFile> {
        (**self).update_content(id, mime, data).await
    }
    async fn download(&self, id: &str) -> Result<Vec<u8>> {
        (**self).download(id).await
    }
    async fn trash(&self, id: &str) -> Result<()> {
        (**self).trash(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalStore;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Shared store that can be switched "offline".
    struct Flaky {
        inner: LocalStore,
        offline: AtomicBool,
    }

    impl Flaky {
        fn check(&self) -> Result<()> {
            if self.offline.load(Ordering::SeqCst) {
                Err(CloudError::Network("offline".into()))
            } else {
                Ok(())
            }
        }
    }

    impl RemoteStore for Flaky {
        async fn list(&self, parent: Option<&str>, kind: &str) -> Result<Vec<RemoteFile>> {
            self.check()?;
            self.inner.list(parent, kind).await
        }
        async fn create_folder(&self, name: &str, parent: Option<&str>, props: Props) -> Result<RemoteFile> {
            self.check()?;
            self.inner.create_folder(name, parent, props).await
        }
        async fn create_file(&self, name: &str, parent: &str, mime: &str, props: Props, data: Vec<u8>) -> Result<RemoteFile> {
            self.check()?;
            self.inner.create_file(name, parent, mime, props, data).await
        }
        async fn update_content(&self, id: &str, mime: &str, data: Vec<u8>) -> Result<RemoteFile> {
            self.check()?;
            self.inner.update_content(id, mime, data).await
        }
        async fn download(&self, id: &str) -> Result<Vec<u8>> {
            self.check()?;
            self.inner.download(id).await
        }
        async fn trash(&self, id: &str) -> Result<()> {
            self.check()?;
            self.inner.trash(id).await
        }
    }

    fn entry(id: &str, t: i64, device: &str) -> ChatEntry {
        ChatEntry {
            id: id.into(),
            role: "user".into(),
            content: format!("msg {id}"),
            created_at: t,
            kind: None,
            device: Some(device.into()),
            provider: None,
            model: None,
            meta: None,
        }
    }

    #[tokio::test]
    async fn two_devices_share_documents_and_chat() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Flaky { inner: LocalStore::open(tmp.path().join("remote")).unwrap(), offline: AtomicBool::new(false) });
        let laptop = Library::new(store.clone(), tmp.path().join("laptop"), "laptop".into());
        let phone = Library::new(store.clone(), tmp.path().join("phone"), "phone".into());

        let doc = laptop.upload("report.pdf", b"%PDF-1.4 fake".to_vec()).await.unwrap();
        laptop.sync_chat(&doc, vec![entry("1", 10, "laptop")]).await.unwrap();

        // Phone sees the document, its bytes, and the chat so far.
        let docs = phone.documents().await.unwrap();
        assert_eq!(docs.value, vec![doc.clone()]);
        assert_eq!(phone.pdf_bytes(&doc).await.unwrap(), b"%PDF-1.4 fake");
        let chat = phone.chat(&doc).await.unwrap();
        assert_eq!(chat.value.messages.len(), 1);

        // Continue the conversation on the phone, then back on the laptop.
        phone.sync_chat(&doc, vec![entry("2", 20, "phone")]).await.unwrap();
        let chat = laptop.chat(&doc).await.unwrap().value;
        let ids: Vec<_> = chat.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["1", "2"]);
    }

    #[tokio::test]
    async fn offline_messages_are_kept_and_uploaded_later() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Flaky { inner: LocalStore::open(tmp.path().join("remote")).unwrap(), offline: AtomicBool::new(false) });
        let laptop = Library::new(store.clone(), tmp.path().join("laptop"), "laptop".into());
        let phone = Library::new(store.clone(), tmp.path().join("phone"), "phone".into());
        let doc = laptop.upload("a.pdf", b"%PDF-".to_vec()).await.unwrap();
        laptop.sync_chat(&doc, vec![entry("1", 10, "laptop")]).await.unwrap();
        laptop.documents().await.unwrap();

        store.offline.store(true, Ordering::SeqCst);
        let r = laptop.sync_chat(&doc, vec![entry("2", 20, "laptop")]).await.unwrap();
        assert!(r.offline);
        assert_eq!(r.pending, 1);
        assert_eq!(r.value.messages.len(), 2, "offline view shows cached + pending");
        let docs = laptop.documents().await.unwrap();
        assert!(docs.offline && docs.value.len() == 1);
        assert!(phone.documents().await.is_err(), "no cache on a device that never synced");

        store.offline.store(false, Ordering::SeqCst);
        let r = laptop.chat(&doc).await.unwrap();
        assert!(!r.offline && r.pending == 0);
        assert_eq!(phone.chat(&doc).await.unwrap().value.messages.len(), 2);
    }

    #[tokio::test]
    async fn concurrent_first_chats_are_merged() {
        // Both devices create the chat file before seeing the other's.
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalStore::open(tmp.path().join("remote")).unwrap());
        let a = Library::new(store.clone(), tmp.path().join("a"), "a".into());
        let doc = a.upload("x.pdf", b"%PDF-".to_vec()).await.unwrap();
        let root = a.root().await.unwrap();
        for (id, t) in [("p", 5), ("q", 6)] {
            let mut log = ChatLog::new(&doc.doc_id);
            log.merge([entry(id, t, id)]);
            let props = Props::from([(KIND.to_string(), "chat".to_string()), (DOC.to_string(), doc.doc_id.clone())]);
            store.create_file("x.chat.json", &root, "application/json", props, serde_json::to_vec(&log).unwrap()).await.unwrap();
        }
        let chat = a.sync_chat(&doc, vec![entry("r", 7, "a")]).await.unwrap().value;
        let ids: Vec<_> = chat.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["p", "q", "r"]);
    }

    #[tokio::test]
    async fn delete_removes_document_and_chat() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::new(LocalStore::open(tmp.path().join("r")).unwrap(), tmp.path().join("c"), "d".into());
        let doc = lib.upload("a.pdf", b"%PDF-".to_vec()).await.unwrap();
        lib.sync_chat(&doc, vec![entry("1", 1, "d")]).await.unwrap();
        lib.delete(&doc).await.unwrap();
        assert!(lib.documents().await.unwrap().value.is_empty());
        let root = lib.root().await.unwrap();
        assert!(lib.store().list(Some(&root), "chat").await.unwrap().is_empty());
    }
}
