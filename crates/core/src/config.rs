use serde::{Deserialize, Serialize};
use std::env;

pub const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
pub const LMSTUDIO_BASE_URL: &str = "http://localhost:1234/v1";

pub const DEFAULT_GROQ_MODEL: &str = "llama-3.3-70b-versatile";
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-flash";

/// One OpenAI-compatible endpoint. Groq, Gemini and LM Studio all speak
/// `POST {base_url}/chat/completions`, so any similar provider can be added
/// from config alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub base_url: String,
    /// Model id. When empty, the first model from `GET {base_url}/models` is
    /// used (handy for LM Studio, which serves whatever model is loaded).
    #[serde(default)]
    pub model: Option<String>,
    /// Keys used in rotation. Never serialized back out.
    #[serde(default, skip_serializing)]
    pub api_keys: Vec<String>,
    #[serde(default = "yes")]
    pub requires_key: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Tried in this order; later ones are fallbacks.
    pub providers: Vec<ProviderConfig>,
    /// Most document text sent in one request (about 4 chars per token).
    pub max_context_chars: usize,
    /// Size of retrieval chunks used for Q&A.
    pub chunk_chars: usize,
    pub chunk_overlap: usize,
    /// Parallel requests while summarizing long documents.
    pub concurrency: usize,
    pub request_timeout_secs: u64,
    /// How long a rate-limited key rests when the provider gives no Retry-After.
    pub key_cooldown_secs: u64,
    pub max_pdf_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            max_context_chars: 16_000,
            chunk_chars: 2_000,
            chunk_overlap: 200,
            concurrency: 3,
            request_timeout_secs: 120,
            key_cooldown_secs: 60,
            max_pdf_bytes: 64 * 1024 * 1024,
        }
    }
}

impl Config {
    /// Build config from environment variables. `.env` files are read from,
    /// in priority order: `$PDFK_ENV_FILE`, the current directory, and the
    /// user config dir (`~/.config/pdf-kernel/.env`, `%APPDATA%\pdf-kernel\.env`,
    /// `~/Library/Application Support/pdf-kernel/.env`). Real environment
    /// variables always win.
    pub fn from_env() -> Self {
        load_env_files();
        let mut cfg = Self::default();

        set_num("PDFK_MAX_CONTEXT_CHARS", &mut cfg.max_context_chars);
        set_num("PDFK_CHUNK_CHARS", &mut cfg.chunk_chars);
        set_num("PDFK_CHUNK_OVERLAP", &mut cfg.chunk_overlap);
        set_num("PDFK_CONCURRENCY", &mut cfg.concurrency);
        set_num("PDFK_REQUEST_TIMEOUT_SECS", &mut cfg.request_timeout_secs);
        set_num("PDFK_KEY_COOLDOWN_SECS", &mut cfg.key_cooldown_secs);
        set_num("PDFK_MAX_PDF_BYTES", &mut cfg.max_pdf_bytes);

        let groq = list(&["GROQ_API_KEYS", "GROQ_API_KEY"]);
        if !groq.is_empty() {
            cfg.providers.push(ProviderConfig {
                name: "groq".into(),
                base_url: var_or("GROQ_BASE_URL", GROQ_BASE_URL),
                model: Some(var_or("GROQ_MODEL", DEFAULT_GROQ_MODEL)),
                api_keys: groq,
                requires_key: true,
            });
        }

        let gemini = list(&["GEMINI_API_KEYS", "GEMINI_API_KEY"]);
        if !gemini.is_empty() {
            cfg.providers.push(ProviderConfig {
                name: "gemini".into(),
                base_url: var_or("GEMINI_BASE_URL", GEMINI_BASE_URL),
                model: Some(var_or("GEMINI_MODEL", DEFAULT_GEMINI_MODEL)),
                api_keys: gemini,
                requires_key: true,
            });
        }

        if flag("LMSTUDIO_ENABLED") {
            cfg.providers.push(ProviderConfig {
                name: "lmstudio".into(),
                base_url: var_or("LMSTUDIO_BASE_URL", LMSTUDIO_BASE_URL),
                model: env::var("LMSTUDIO_MODEL").ok().filter(|m| !m.trim().is_empty()),
                api_keys: Vec::new(),
                requires_key: false,
            });
        }

        let order = list(&["PDFK_PROVIDER_ORDER"]);
        if !order.is_empty() {
            cfg.providers.sort_by_key(|p| {
                order
                    .iter()
                    .position(|o| o.eq_ignore_ascii_case(&p.name))
                    .unwrap_or(usize::MAX)
            });
        }
        cfg
    }

    /// Clamp values into ranges the engine can work with.
    pub fn normalize(&mut self) {
        self.max_context_chars = self.max_context_chars.max(2_000);
        self.chunk_chars = self.chunk_chars.clamp(200, self.max_context_chars / 2);
        self.chunk_overlap = self.chunk_overlap.min(self.chunk_chars / 2);
        self.concurrency = self.concurrency.clamp(1, 16);
        self.request_timeout_secs = self.request_timeout_secs.max(5);
        self.key_cooldown_secs = self.key_cooldown_secs.max(1);
    }
}

fn load_env_files() {
    if let Ok(path) = env::var("PDFK_ENV_FILE") {
        let _ = dotenvy::from_path(path);
    }
    let _ = dotenvy::dotenv();
    if let Some(dir) = dirs::config_dir() {
        let _ = dotenvy::from_path(dir.join("pdf-kernel").join(".env"));
    }
}

fn var_or(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Comma-separated values merged from several variables, de-duplicated.
fn list(names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        if let Ok(v) = env::var(name) {
            for item in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if !out.iter().any(|o| o == item) {
                    out.push(item.to_string());
                }
            }
        }
    }
    out
}

fn flag(name: &str) -> bool {
    matches!(
        env::var(name).map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
}

fn set_num<T: std::str::FromStr>(name: &str, target: &mut T) {
    if let Some(v) = env::var(name).ok().and_then(|v| v.trim().parse().ok()) {
        *target = v;
    }
}
