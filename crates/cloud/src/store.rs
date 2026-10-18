//! Storage interface the library is written against. Google Drive is one
//! implementation; the local folder store is another (and is used in tests).

use crate::drive::DriveStore;
use crate::error::Result;
use crate::local::LocalStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;

/// App metadata keys stored on each file (Drive `appProperties`).
pub const KIND: &str = "pdfk_kind";
pub const DOC: &str = "pdfk_doc";

pub type Props = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    /// RFC 3339 timestamp.
    pub modified: String,
    pub size: Option<u64>,
    pub props: Props,
}

pub trait RemoteStore: Send + Sync {
    /// Non-trashed children of `parent` (top level when `None`) whose
    /// `pdfk_kind` property equals `kind`.
    fn list(&self, parent: Option<&str>, kind: &str) -> impl Future<Output = Result<Vec<RemoteFile>>> + Send;
    fn create_folder(&self, name: &str, parent: Option<&str>, props: Props) -> impl Future<Output = Result<RemoteFile>> + Send;
    fn create_file(
        &self,
        name: &str,
        parent: &str,
        mime: &str,
        props: Props,
        data: Vec<u8>,
    ) -> impl Future<Output = Result<RemoteFile>> + Send;
    fn update_content(&self, id: &str, mime: &str, data: Vec<u8>) -> impl Future<Output = Result<RemoteFile>> + Send;
    fn download(&self, id: &str) -> impl Future<Output = Result<Vec<u8>>> + Send;
    fn trash(&self, id: &str) -> impl Future<Output = Result<()>> + Send;
}

/// Either backend, so front-ends can hold one concrete type.
pub enum AnyStore {
    Drive(DriveStore),
    Local(LocalStore),
}

impl AnyStore {
    pub fn is_local(&self) -> bool {
        matches!(self, AnyStore::Local(_))
    }
}

macro_rules! delegate {
    ($self:ident, $s:ident => $call:expr) => {
        match $self {
            AnyStore::Drive($s) => $call.await,
            AnyStore::Local($s) => $call.await,
        }
    };
}

impl RemoteStore for AnyStore {
    async fn list(&self, parent: Option<&str>, kind: &str) -> Result<Vec<RemoteFile>> {
        delegate!(self, s => s.list(parent, kind))
    }
    async fn create_folder(&self, name: &str, parent: Option<&str>, props: Props) -> Result<RemoteFile> {
        delegate!(self, s => s.create_folder(name, parent, props))
    }
    async fn create_file(&self, name: &str, parent: &str, mime: &str, props: Props, data: Vec<u8>) -> Result<RemoteFile> {
        delegate!(self, s => s.create_file(name, parent, mime, props, data))
    }
    async fn update_content(&self, id: &str, mime: &str, data: Vec<u8>) -> Result<RemoteFile> {
        delegate!(self, s => s.update_content(id, mime, data))
    }
    async fn download(&self, id: &str) -> Result<Vec<u8>> {
        delegate!(self, s => s.download(id))
    }
    async fn trash(&self, id: &str) -> Result<()> {
        delegate!(self, s => s.trash(id))
    }
}
