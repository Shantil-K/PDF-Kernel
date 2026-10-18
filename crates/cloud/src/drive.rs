//! Google Drive v3 backend. With the `drive.file` scope the app only sees
//! files it created, under the same OAuth client, on any device.

use crate::accounts::AccountManager;
use crate::error::{CloudError, Result};
use crate::store::{Props, RemoteFile, RemoteStore, KIND};
use reqwest::{header, Method, RequestBuilder, Response, StatusCode, Url};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const API: &str = "https://www.googleapis.com";
const FIELDS: &str = "id,name,mimeType,modifiedTime,size,appProperties";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

pub struct DriveStore {
    http: reqwest::Client,
    accounts: Arc<AccountManager>,
    account_id: String,
    api: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFile {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    modified_time: String,
    size: Option<String>,
    #[serde(default)]
    app_properties: Props,
}

impl From<DriveFile> for RemoteFile {
    fn from(f: DriveFile) -> Self {
        RemoteFile {
            id: f.id,
            name: f.name,
            mime_type: f.mime_type,
            modified: f.modified_time,
            size: f.size.and_then(|s| s.parse().ok()),
            props: f.app_properties,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
}

impl DriveStore {
    pub fn new(accounts: Arc<AccountManager>, account_id: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("HTTP client");
        Self { http, accounts, account_id, api: API.to_string() }
    }

    fn url(&self, path: &str) -> Url {
        Url::parse(&format!("{}{path}", self.api)).expect("valid Drive URL")
    }

    /// Send with the account's token; on 401 refresh the token and retry once.
    async fn send(&self, make: impl Fn(&reqwest::Client) -> RequestBuilder) -> Result<Response> {
        for attempt in 0..2 {
            let token = self.accounts.access_token(&self.account_id).await?;
            let resp = make(&self.http).bearer_auth(&token).send().await?;
            match resp.status() {
                s if s.is_success() => return Ok(resp),
                StatusCode::UNAUTHORIZED if attempt == 0 => self.accounts.invalidate(&self.account_id).await,
                StatusCode::NOT_FOUND => return Err(CloudError::NotFound("Drive file".into())),
                s if s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS => {
                    return Err(CloudError::Network(format!("Google Drive is busy (HTTP {})", s.as_u16())))
                }
                s => {
                    let body = resp.text().await.unwrap_or_default();
                    let msg: String = body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect();
                    return Err(CloudError::Storage(format!("Drive HTTP {}: {msg}", s.as_u16())));
                }
            }
        }
        Err(CloudError::Reauth(self.account_id.clone()))
    }

    async fn file_json(&self, resp: Response) -> Result<RemoteFile> {
        Ok(resp.json::<DriveFile>().await?.into())
    }
}

impl RemoteStore for DriveStore {
    async fn list(&self, parent: Option<&str>, kind: &str) -> Result<Vec<RemoteFile>> {
        let parent = parent.unwrap_or("root");
        let q = format!(
            "'{}' in parents and trashed = false and appProperties has {{ key='{KIND}' and value='{}' }}",
            escape(parent),
            escape(kind)
        );
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut url = self.url("/drive/v3/files");
            url.query_pairs_mut()
                .append_pair("q", &q)
                .append_pair("fields", &format!("nextPageToken,files({FIELDS})"))
                .append_pair("pageSize", "1000")
                .append_pair("spaces", "drive")
                .append_pair("orderBy", "createdTime");
            if let Some(p) = &page {
                url.query_pairs_mut().append_pair("pageToken", p);
            }
            let list: FileList = self.send(|c| c.get(url.clone())).await?.json().await?;
            out.extend(list.files.into_iter().map(RemoteFile::from));
            match list.next_page_token {
                Some(t) => page = Some(t),
                None => return Ok(out),
            }
        }
    }

    async fn create_folder(&self, name: &str, parent: Option<&str>, props: Props) -> Result<RemoteFile> {
        let mut url = self.url("/drive/v3/files");
        url.query_pairs_mut().append_pair("fields", FIELDS);
        let mut meta = json!({ "name": name, "mimeType": FOLDER_MIME, "appProperties": props });
        if let Some(p) = parent {
            meta["parents"] = json!([p]);
        }
        let resp = self.send(|c| c.post(url.clone()).json(&meta)).await?;
        self.file_json(resp).await
    }

    /// Resumable upload (works for any size; simple uploads cap at 5 MB).
    async fn create_file(&self, name: &str, parent: &str, mime: &str, props: Props, data: Vec<u8>) -> Result<RemoteFile> {
        let mut url = self.url("/upload/drive/v3/files");
        url.query_pairs_mut().append_pair("uploadType", "resumable").append_pair("fields", FIELDS);
        let meta = json!({ "name": name, "parents": [parent], "mimeType": mime, "appProperties": props });
        let len = data.len().to_string();
        let start = self
            .send(|c| {
                c.post(url.clone())
                    .header("X-Upload-Content-Type", mime)
                    .header("X-Upload-Content-Length", &len)
                    .json(&meta)
            })
            .await?;
        let session = start
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| CloudError::Storage("Drive did not return an upload session".into()))?
            .to_string();
        let resp = self
            .send(|c| c.put(&session).header(header::CONTENT_TYPE, mime).body(data.clone()))
            .await?;
        self.file_json(resp).await
    }

    async fn update_content(&self, id: &str, mime: &str, data: Vec<u8>) -> Result<RemoteFile> {
        let mut url = self.url(&format!("/upload/drive/v3/files/{id}"));
        url.query_pairs_mut().append_pair("uploadType", "media").append_pair("fields", FIELDS);
        let resp = self
            .send(|c| c.request(Method::PATCH, url.clone()).header(header::CONTENT_TYPE, mime).body(data.clone()))
            .await?;
        self.file_json(resp).await
    }

    async fn download(&self, id: &str) -> Result<Vec<u8>> {
        let mut url = self.url(&format!("/drive/v3/files/{id}"));
        url.query_pairs_mut().append_pair("alt", "media");
        Ok(self.send(|c| c.get(url.clone())).await?.bytes().await?.to_vec())
    }

    async fn trash(&self, id: &str) -> Result<()> {
        let url = self.url(&format!("/drive/v3/files/{id}"));
        let body = json!({ "trashed": true });
        match self.send(|c| c.request(Method::PATCH, url.clone()).json(&body)).await {
            Ok(_) | Err(CloudError::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Escape a value for a Drive query string literal.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_query_literals() {
        assert_eq!(super::escape(r"it's a\b"), r"it\'s a\\b");
    }
}
