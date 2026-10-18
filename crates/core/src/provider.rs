//! Routes chat requests across OpenAI-compatible providers with key
//! rotation and fallback.
//!
//! For each request: try providers in order. Within a provider, rotate keys:
//! a 429 rests that key and tries the next, a 401/403 disables it. If every
//! provider is only rate limited and a key frees up soon, wait and retry.

use crate::config::{Config, ProviderConfig};
use crate::error::{Error, Result};
use crate::keypool::{KeyPool, PoolStatus};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::OnceCell;

/// Longest the router will sleep waiting for a rate-limited key.
const MAX_WAIT: Duration = Duration::from_secs(20);
const ROUNDS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    /// Use only this provider (by name). `None` means all, in order.
    pub provider: Option<String>,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Completion {
    pub text: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub name: String,
    pub base_url: String,
    pub model: Option<String>,
    pub requires_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<PoolStatus>,
}

struct Provider {
    cfg: ProviderConfig,
    keys: KeyPool,
    discovered_model: OnceCell<String>,
}

enum CallError {
    RateLimited(Option<Duration>, String),
    BadKey(String),
    Retryable(String),
    Fatal(String),
}

impl CallError {
    fn message(self) -> String {
        match self {
            CallError::RateLimited(_, m)
            | CallError::BadKey(m)
            | CallError::Retryable(m)
            | CallError::Fatal(m) => m,
        }
    }
}

enum Failure {
    /// Worth retrying after this delay.
    Wait(Duration, String),
    Failed(String),
}

pub struct Router {
    http: reqwest::Client,
    providers: Vec<Provider>,
    cooldown: Duration,
}

impl Router {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.request_timeout_secs))
            .user_agent(concat!("pdf-kernel/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Config(e.to_string()))?;
        let providers = cfg
            .providers
            .iter()
            .map(|p| Provider {
                cfg: p.clone(),
                keys: KeyPool::new(p.api_keys.clone()),
                discovered_model: OnceCell::new(),
            })
            .collect();
        Ok(Self { http, providers, cooldown: Duration::from_secs(cfg.key_cooldown_secs) })
    }

    pub fn status(&self) -> Vec<ProviderStatus> {
        self.providers
            .iter()
            .map(|p| ProviderStatus {
                name: p.cfg.name.clone(),
                base_url: p.cfg.base_url.clone(),
                model: p.cfg.model.clone().or_else(|| p.discovered_model.get().cloned()),
                requires_key: p.cfg.requires_key,
                keys: p.cfg.requires_key.then(|| p.keys.status()),
            })
            .collect()
    }

    pub async fn chat(&self, messages: &[ChatMessage], opts: &ChatOptions) -> Result<Completion> {
        let selected: Vec<&Provider> = match &opts.provider {
            Some(name) => {
                let p = self
                    .providers
                    .iter()
                    .find(|p| p.cfg.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| Error::InvalidRequest(format!("provider '{name}' is not configured")))?;
                vec![p]
            }
            None => self.providers.iter().collect(),
        };
        if selected.is_empty() {
            return Err(Error::NoProvider);
        }

        let mut errors = Vec::new();
        for _ in 0..ROUNDS {
            errors.clear();
            let mut wait: Option<Duration> = None;
            for p in &selected {
                match self.try_provider(p, messages, opts).await {
                    Ok(c) => return Ok(c),
                    Err(Failure::Wait(d, msg)) => {
                        wait = Some(wait.map_or(d, |w| w.min(d)));
                        errors.push(format!("{}: {msg}", p.cfg.name));
                    }
                    Err(Failure::Failed(msg)) => errors.push(format!("{}: {msg}", p.cfg.name)),
                }
            }
            match wait {
                Some(d) if d <= MAX_WAIT => tokio::time::sleep(d + Duration::from_millis(250)).await,
                _ => break,
            }
        }
        Err(Error::AllProvidersFailed(errors.join("; ")))
    }

    async fn try_provider(
        &self,
        p: &Provider,
        messages: &[ChatMessage],
        opts: &ChatOptions,
    ) -> std::result::Result<Completion, Failure> {
        let model = self.model_for(p).await.map_err(Failure::Failed)?;
        let done = |text: String| Completion { text, provider: p.cfg.name.clone(), model: model.clone() };

        if !p.cfg.requires_key {
            return match self.send(p, None, &model, messages, opts).await {
                Ok(t) => Ok(done(t)),
                Err(CallError::RateLimited(d, m)) => Err(Failure::Wait(d.unwrap_or(Duration::from_secs(2)), m)),
                Err(e) => Err(Failure::Failed(e.message())),
            };
        }

        let mut last = String::from("no usable API key");
        for _ in 0..p.keys.len() {
            let Some(lease) = p.keys.acquire() else { break };
            let n = lease.slot + 1;
            match self.send(p, Some(&lease.key), &model, messages, opts).await {
                Ok(t) => return Ok(done(t)),
                Err(CallError::RateLimited(d, m)) => {
                    p.keys.cool_down(lease.slot, d.unwrap_or(self.cooldown));
                    last = format!("key #{n} rate limited ({m})");
                }
                Err(CallError::BadKey(m)) => {
                    p.keys.disable(lease.slot);
                    last = format!("key #{n} rejected ({m})");
                }
                Err(CallError::Retryable(m)) => last = m,
                Err(CallError::Fatal(m)) => return Err(Failure::Failed(m)),
            }
        }
        match p.keys.soonest_available() {
            Some(d) => Err(Failure::Wait(d, last)),
            None => Err(Failure::Failed(format!("{last}; every key is disabled"))),
        }
    }

    async fn model_for(&self, p: &Provider) -> std::result::Result<String, String> {
        if let Some(m) = p.cfg.model.as_ref().filter(|m| !m.trim().is_empty()) {
            return Ok(m.clone());
        }
        p.discovered_model
            .get_or_try_init(|| async {
                let url = format!("{}/models", p.cfg.base_url.trim_end_matches('/'));
                let mut req = self.http.get(&url);
                if let Some(lease) = p.keys.acquire() {
                    req = req.bearer_auth(lease.key);
                }
                let v: Value = req
                    .send()
                    .await
                    .map_err(|e| format!("cannot reach {url}: {e}"))?
                    .error_for_status()
                    .map_err(|e| e.to_string())?
                    .json()
                    .await
                    .map_err(|e| e.to_string())?;
                v["data"]
                    .as_array()
                    .and_then(|models| {
                        models
                            .iter()
                            .filter_map(|m| m["id"].as_str())
                            .find(|id| !id.contains("embed"))
                    })
                    .map(String::from)
                    .ok_or_else(|| format!("no chat model available at {url}; load one or set a model name"))
            })
            .await
            .cloned()
    }

    async fn send(
        &self,
        p: &Provider,
        key: Option<&str>,
        model: &str,
        messages: &[ChatMessage],
        opts: &ChatOptions,
    ) -> std::result::Result<String, CallError> {
        let url = format!("{}/chat/completions", p.cfg.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": model,
            "messages": messages,
            "temperature": opts.temperature,
            "stream": false,
        });
        if let Some(max) = opts.max_tokens {
            body["max_tokens"] = json!(max);
        }
        let mut req = self.http.post(&url).json(&body);
        if let Some(k) = key {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| CallError::Retryable(format!("request failed: {e}")))?;

        let status = resp.status();
        if status.is_success() {
            let v: Value = resp
                .json()
                .await
                .map_err(|e| CallError::Retryable(format!("unreadable response: {e}")))?;
            return v["choices"][0]["message"]["content"]
                .as_str()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CallError::Retryable("model returned an empty answer".into()));
        }

        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(Duration::from_secs_f64);
        let text = resp.text().await.unwrap_or_default();
        let msg = format!("HTTP {}: {}", status.as_u16(), snippet(&text, 240));
        Err(match status.as_u16() {
            429 => CallError::RateLimited(retry_after, msg),
            401 | 403 => CallError::BadKey(msg),
            // Gemini reports bad keys as 400 INVALID_ARGUMENT.
            400 if text.contains("API key") || text.contains("API_KEY") => CallError::BadKey(msg),
            408 | 409 | 500..=599 => CallError::Retryable(msg),
            _ => CallError::Fatal(msg),
        })
    }
}

fn snippet(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}
