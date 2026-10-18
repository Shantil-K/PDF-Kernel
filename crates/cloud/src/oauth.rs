//! Google OAuth for installed apps: system browser + loopback redirect + PKCE.
//! https://developers.google.com/identity/protocols/oauth2/native-app
//!
//! Scopes are basic profile plus `drive.file`, which only reaches files this
//! app created. Google classes it as non-sensitive, so no security audit.

use crate::error::{CloudError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
pub const SCOPES: &str = "openid email profile https://www.googleapis.com/auth/drive.file";

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub struct GoogleOAuth {
    client_id: String,
    /// Google issues a "secret" for desktop clients but documents that it is
    /// not confidential in installed apps; PKCE is what protects the flow.
    client_secret: Option<String>,
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct Tokens {
    pub access_token: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub picture: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
    refresh_token: Option<String>,
    #[serde(default)]
    scope: String,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: String,
}

impl GoogleOAuth {
    pub fn new(client_id: impl Into<String>, client_secret: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("HTTP client");
        Self { client_id: client_id.into(), client_secret: client_secret.filter(|s| !s.is_empty()), http }
    }

    /// Client credentials from `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` at
    /// run time, falling back to `PDFK_GOOGLE_CLIENT_ID` / `PDFK_GOOGLE_CLIENT_SECRET`
    /// baked in at build time (used for packaged and Android builds).
    pub fn from_env() -> Option<Self> {
        let runtime = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let id = runtime("GOOGLE_CLIENT_ID").or_else(|| option_env!("PDFK_GOOGLE_CLIENT_ID").map(String::from))?;
        if id.trim().is_empty() {
            return None;
        }
        let secret = runtime("GOOGLE_CLIENT_SECRET").or_else(|| option_env!("PDFK_GOOGLE_CLIENT_SECRET").map(String::from));
        Some(Self::new(id.trim(), secret))
    }

    /// Run the browser sign-in. `open_url` must open the URL in the system
    /// browser (never an embedded webview; Google blocks those).
    pub async fn sign_in(&self, open_url: impl FnOnce(&str) -> Result<()>) -> Result<(Tokens, UserInfo)> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());

        let verifier = random_token(48);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_token(24);

        let mut url = Url::parse(AUTH_URL).expect("valid URL");
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPES)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("access_type", "offline")
            // Always show the account chooser (to add a second account) and
            // consent (so Google returns a refresh token every time).
            .append_pair("prompt", "select_account consent");

        open_url(url.as_str())?;

        let code = tokio::time::timeout(SIGN_IN_TIMEOUT, wait_for_code(&listener, &state))
            .await
            .map_err(|_| CloudError::Auth("timed out waiting for the browser".into()))??;

        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", self.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", verifier.as_str()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret));
        }
        let resp = self.token_request(&form).await?;
        if !resp.scope.split_whitespace().any(|s| s == DRIVE_SCOPE) {
            return Err(CloudError::Auth(
                "Google Drive access was not allowed. Sign in again and tick the Drive checkbox.".into(),
            ));
        }
        let tokens = Tokens { access_token: resp.access_token, expires_in: resp.expires_in, refresh_token: resp.refresh_token };
        let user = self.userinfo(&tokens.access_token).await?;
        Ok((tokens, user))
    }

    /// Get a fresh access token. `CloudError::Reauth` means the refresh token
    /// was revoked or expired and the user must sign in again.
    pub async fn refresh(&self, refresh_token: &str, account: &str) -> Result<Tokens> {
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.as_str()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret));
        }
        match self.token_request(&form).await {
            Ok(r) => Ok(Tokens { access_token: r.access_token, expires_in: r.expires_in, refresh_token: r.refresh_token }),
            Err(CloudError::Auth(msg)) if msg.starts_with("invalid_grant") => Err(CloudError::Reauth(account.to_string())),
            Err(e) => Err(e),
        }
    }

    pub async fn userinfo(&self, access_token: &str) -> Result<UserInfo> {
        let resp = self.http.get(USERINFO_URL).bearer_auth(access_token).send().await?;
        if !resp.status().is_success() {
            return Err(CloudError::Auth(format!("could not read profile (HTTP {})", resp.status().as_u16())));
        }
        Ok(resp.json().await?)
    }

    /// Best effort: tell Google to revoke the grant.
    pub async fn revoke(&self, token: &str) {
        let body = form_urlencoded::Serializer::new(String::new()).append_pair("token", token).finish();
        let _ = self
            .http
            .post(REVOKE_URL)
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await;
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<TokenResponse> {
        let body = form_urlencoded::Serializer::new(String::new()).extend_pairs(form).finish();
        let resp = self
            .http
            .post(TOKEN_URL)
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if status.is_success() {
            return Ok(serde_json::from_str(&text)?);
        }
        if status.is_server_error() {
            return Err(CloudError::Network(format!("Google token endpoint returned HTTP {}", status.as_u16())));
        }
        let err: TokenError = serde_json::from_str(&text)
            .unwrap_or(TokenError { error: format!("HTTP {}", status.as_u16()), error_description: String::new() });
        Err(CloudError::Auth(format!("{}: {}", err.error, err.error_description)))
    }
}

/// Accept connections on the loopback listener until the browser delivers
/// the authorization code.
async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let Some(target) = read_request_target(&mut stream).await else {
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            respond(&mut stream, 400, "Bad request").await;
            continue;
        };
        let param = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());

        if let Some(error) = param("error") {
            respond(&mut stream, 200, "Sign-in was cancelled. You can close this tab.").await;
            return Err(CloudError::Auth(error));
        }
        let Some(code) = param("code") else {
            // Favicon or other stray request.
            respond(&mut stream, 404, "Not found").await;
            continue;
        };
        if param("state").as_deref() != Some(expected_state) {
            respond(&mut stream, 400, "Sign-in request did not match. Please try again from the app.").await;
            return Err(CloudError::Auth("state mismatch".into()));
        }
        respond(&mut stream, 200, "Signed in to PDF Kernel. You can close this tab and return to the app.").await;
        return Ok(code);
    }
}

async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk)).await.ok()?.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let mut parts = text.lines().next()?.split_whitespace();
    (parts.next()? == "GET").then_some(())?;
    parts.next().filter(|t| t.starts_with('/')).map(String::from)
}

async fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>PDF Kernel</title>\
         <body style=\"font-family:system-ui,sans-serif;display:grid;place-items:center;height:90vh\">\
         <p>{message}</p></body>"
    );
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn browser_hit(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn loopback_receives_code_and_ignores_noise() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiter = tokio::spawn(async move { wait_for_code(&listener, "xyz").await });

        assert!(browser_hit(port, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
        let page = browser_hit(port, "/?state=xyz&code=4%2F0Abc&scope=email").await;
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("Signed in"));
        assert_eq!(waiter.await.unwrap().unwrap(), "4/0Abc");
    }

    #[tokio::test]
    async fn loopback_rejects_wrong_state_and_reports_denial() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiter = tokio::spawn(async move { wait_for_code(&listener, "good").await });
        browser_hit(port, "/?state=evil&code=x").await;
        assert!(matches!(waiter.await.unwrap(), Err(CloudError::Auth(m)) if m == "state mismatch"));

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiter = tokio::spawn(async move { wait_for_code(&listener, "s").await });
        browser_hit(port, "/?error=access_denied&state=s").await;
        assert!(matches!(waiter.await.unwrap(), Err(CloudError::Auth(m)) if m == "access_denied"));
    }

    #[test]
    fn pkce_tokens_are_url_safe() {
        let t = random_token(48);
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }
}
