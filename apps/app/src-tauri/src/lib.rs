//! PDF Kernel app. Thin glue between the UI and two libraries:
//! `pdf-kernel` (summaries and Q&A) and `pdf-kernel-cloud` (accounts,
//! Google Drive storage, chat sync). Same code on desktop and Android.

use base64::Engine as _;
use pdf_kernel::{config, AskOptions, ChatMessage, Config, Engine, ProviderConfig, ProviderStatus, SummaryStyle};
use pdf_kernel_cloud::{Account, AnyStore, ChatEntry, ChatLog, Cloud, DocEntry, GoogleOAuth, Library, Synced};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::Mutex;

const AI_SETTINGS_KEY: &str = "ai-settings";

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The user's own AI keys and options, kept in the OS keychain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct AiSettings {
    groq_keys: Vec<String>,
    groq_model: String,
    gemini_keys: Vec<String>,
    gemini_model: String,
    lmstudio_enabled: bool,
    lmstudio_url: String,
    lmstudio_model: String,
    /// Provider names, first tried first.
    order: Vec<String>,
}

impl AiSettings {
    fn defaults() -> Self {
        Self {
            groq_model: config::DEFAULT_GROQ_MODEL.into(),
            gemini_model: config::DEFAULT_GEMINI_MODEL.into(),
            lmstudio_url: config::LMSTUDIO_BASE_URL.into(),
            order: vec!["groq".into(), "gemini".into(), "lmstudio".into()],
            ..Default::default()
        }
    }

    fn providers(&self) -> Vec<ProviderConfig> {
        fn keys(list: &[String]) -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for k in list.iter().flat_map(|k| k.split([',', '\n'])).map(str::trim).filter(|k| !k.is_empty()) {
                if !out.iter().any(|o| o == k) {
                    out.push(k.to_string());
                }
            }
            out
        }
        fn or(value: &str, default: &str) -> String {
            if value.trim().is_empty() { default.to_string() } else { value.trim().to_string() }
        }
        let mut list = Vec::new();
        let groq = keys(&self.groq_keys);
        if !groq.is_empty() {
            list.push(ProviderConfig {
                name: "groq".into(),
                base_url: config::GROQ_BASE_URL.into(),
                model: Some(or(&self.groq_model, config::DEFAULT_GROQ_MODEL)),
                api_keys: groq,
                requires_key: true,
            });
        }
        let gemini = keys(&self.gemini_keys);
        if !gemini.is_empty() {
            list.push(ProviderConfig {
                name: "gemini".into(),
                base_url: config::GEMINI_BASE_URL.into(),
                model: Some(or(&self.gemini_model, config::DEFAULT_GEMINI_MODEL)),
                api_keys: gemini,
                requires_key: true,
            });
        }
        if self.lmstudio_enabled {
            list.push(ProviderConfig {
                name: "lmstudio".into(),
                base_url: or(&self.lmstudio_url, config::LMSTUDIO_BASE_URL),
                model: Some(self.lmstudio_model.trim().to_string()).filter(|m| !m.is_empty()),
                api_keys: Vec::new(),
                requires_key: false,
            });
        }
        list.sort_by_key(|p| self.order.iter().position(|o| o == &p.name).unwrap_or(usize::MAX));
        list
    }
}

struct AppState {
    engine: Engine,
    cloud: Cloud,
    /// Drive/local file id → engine document id, so PDFs are parsed once.
    loaded: Mutex<HashMap<String, String>>,
}

impl AppState {
    async fn init(dir: std::path::PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        // Reads .env files on desktop (handy in development); stored settings win.
        let env_config = Config::from_env();
        let cloud = Cloud::open(dir, GoogleOAuth::from_env()).await?;
        let engine = Engine::new(env_config)?;
        if let Some(settings) = load_settings(&cloud).await? {
            let providers = settings.providers();
            if !providers.is_empty() {
                engine.set_providers(providers)?;
            }
        }
        Ok(Self { engine, cloud, loaded: Mutex::new(HashMap::new()) })
    }

    fn library(&self) -> CmdResult<Arc<Library<AnyStore>>> {
        self.cloud.active_library().map_err(err)
    }

    async fn ensure_loaded(&self, lib: &Library<AnyStore>, doc: &DocEntry) -> CmdResult<String> {
        if let Some(id) = self.loaded.lock().await.get(&doc.pdf_file_id).cloned() {
            if self.engine.document(&id).is_ok() {
                return Ok(id);
            }
        }
        let bytes = lib.pdf_bytes(doc).await.map_err(err)?;
        let info = self.engine.load_pdf_bytes(doc.name.clone(), bytes).await.map_err(err)?;
        self.loaded.lock().await.insert(doc.pdf_file_id.clone(), info.id.clone());
        Ok(info.id)
    }
}

async fn load_settings(cloud: &Cloud) -> pdf_kernel_cloud::Result<Option<AiSettings>> {
    Ok(match cloud.secrets().get(AI_SETTINGS_KEY).await? {
        Some(json) => serde_json::from_str(&json).ok(),
        None => None,
    })
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn entry(lib: &Library<AnyStore>, role: &str, content: String, created_at: i64) -> ChatEntry {
    ChatEntry {
        id: uuid::Uuid::new_v4().simple().to_string(),
        role: role.into(),
        content,
        created_at,
        kind: None,
        device: Some(lib.device().to_string()),
        provider: None,
        model: None,
        meta: None,
    }
}

// ---------- app & accounts ----------

#[tauri::command]
async fn app_info(state: State<'_, AppState>) -> CmdResult<Value> {
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "google_enabled": state.cloud.accounts().google_enabled(),
        "secret_storage": state.cloud.secrets().backend(),
        "device": state.cloud.device(),
    }))
}

#[derive(Serialize)]
struct AccountsView {
    accounts: Vec<Account>,
    active: String,
}

fn accounts_view(state: &AppState) -> AccountsView {
    let accounts = state.cloud.accounts();
    AccountsView { accounts: accounts.list(), active: accounts.active_id() }
}

#[tauri::command]
async fn accounts_list(state: State<'_, AppState>) -> CmdResult<AccountsView> {
    Ok(accounts_view(&state))
}

/// Opens the system browser for Google sign-in and waits for it to finish.
#[tauri::command]
async fn account_add(app: AppHandle, state: State<'_, AppState>) -> CmdResult<AccountsView> {
    state
        .cloud
        .accounts()
        .add_google(move |url| {
            app.opener()
                .open_url(url, None::<&str>)
                .map_err(|e| pdf_kernel_cloud::CloudError::Auth(format!("could not open the browser: {e}")))
        })
        .await
        .map_err(err)?;
    Ok(accounts_view(&state))
}

#[tauri::command]
async fn account_switch(state: State<'_, AppState>, id: String) -> CmdResult<AccountsView> {
    state.cloud.accounts().set_active(&id).await.map_err(err)?;
    Ok(accounts_view(&state))
}

#[tauri::command]
async fn account_remove(state: State<'_, AppState>, id: String) -> CmdResult<AccountsView> {
    state.cloud.remove_account(&id).await.map_err(err)?;
    Ok(accounts_view(&state))
}

// ---------- documents & chat ----------

#[tauri::command]
async fn documents(state: State<'_, AppState>) -> CmdResult<Synced<Vec<DocEntry>>> {
    state.library()?.documents().await.map_err(err)
}

/// Check the PDF locally first (so junk is never uploaded), then store it.
#[tauri::command]
async fn document_upload(state: State<'_, AppState>, name: String, data_base64: String) -> CmdResult<DocEntry> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data_base64.trim()).map_err(err)?;
    let info = state.engine.load_pdf_bytes(name.clone(), bytes.clone()).await.map_err(err)?;
    let lib = state.library()?;
    match lib.upload(&name, bytes).await {
        Ok(doc) => {
            state.loaded.lock().await.insert(doc.pdf_file_id.clone(), info.id);
            Ok(doc)
        }
        Err(e) => {
            state.engine.remove_document(&info.id);
            Err(err(e))
        }
    }
}

/// Load the PDF (from cache or storage) and return its synced chat.
#[tauri::command]
async fn document_open(state: State<'_, AppState>, doc: DocEntry) -> CmdResult<Synced<ChatLog>> {
    let lib = state.library()?;
    state.ensure_loaded(&lib, &doc).await?;
    lib.chat(&doc).await.map_err(err)
}

#[tauri::command]
async fn document_delete(state: State<'_, AppState>, doc: DocEntry) -> CmdResult<()> {
    state.library()?.delete(&doc).await.map_err(err)?;
    if let Some(id) = state.loaded.lock().await.remove(&doc.pdf_file_id) {
        state.engine.remove_document(&id);
    }
    Ok(())
}

/// Pull changes from other devices and push anything pending.
#[tauri::command]
async fn chat_sync(state: State<'_, AppState>, doc: DocEntry) -> CmdResult<Synced<ChatLog>> {
    state.library()?.chat(&doc).await.map_err(err)
}

#[tauri::command]
async fn ask(
    state: State<'_, AppState>,
    doc: DocEntry,
    question: String,
    history: Vec<ChatMessage>,
) -> CmdResult<Synced<ChatLog>> {
    let lib = state.library()?;
    let engine_id = state.ensure_loaded(&lib, &doc).await?;
    let asked_at = now_ms();
    let answer = state
        .engine
        .ask(&engine_id, &question, AskOptions { provider: None, history, top_k: None })
        .await
        .map_err(err)?;
    let mut reply = entry(&lib, "assistant", answer.text, now_ms().max(asked_at + 1));
    reply.provider = Some(answer.provider);
    reply.model = Some(answer.model);
    reply.meta = Some(json!({ "sources": answer.sources, "used_full_document": answer.used_full_document }));
    let entries = vec![entry(&lib, "user", question, asked_at), reply];
    lib.sync_chat(&doc, entries).await.map_err(err)
}

#[tauri::command]
async fn summarize(state: State<'_, AppState>, doc: DocEntry, style: SummaryStyle) -> CmdResult<Synced<ChatLog>> {
    let lib = state.library()?;
    let engine_id = state.ensure_loaded(&lib, &doc).await?;
    let s = state.engine.summarize(&engine_id, style, None).await.map_err(err)?;
    let mut e = entry(&lib, "assistant", s.text, now_ms());
    e.kind = Some("summary".into());
    e.provider = Some(s.provider);
    e.model = Some(s.model);
    e.meta = Some(json!({ "style": style, "sections": s.sections }));
    lib.sync_chat(&doc, vec![e]).await.map_err(err)
}

// ---------- AI settings ----------

#[tauri::command]
async fn settings_get(state: State<'_, AppState>) -> CmdResult<AiSettings> {
    Ok(load_settings(&state.cloud).await.map_err(err)?.unwrap_or_else(AiSettings::defaults))
}

#[tauri::command]
async fn settings_set(state: State<'_, AppState>, settings: AiSettings) -> CmdResult<Vec<ProviderStatus>> {
    let json = serde_json::to_string(&settings).map_err(err)?;
    state.cloud.secrets().set(AI_SETTINGS_KEY, &json).await.map_err(err)?;
    let mut providers = settings.providers();
    if providers.is_empty() {
        providers = Config::from_env().providers;
    }
    state.engine.set_providers(providers).map_err(err)?;
    Ok(state.engine.providers())
}

#[tauri::command]
async fn providers(state: State<'_, AppState>) -> CmdResult<Vec<ProviderStatus>> {
    Ok(state.engine.providers())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let state = tauri::async_runtime::block_on(AppState::init(dir))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            accounts_list,
            account_add,
            account_switch,
            account_remove,
            documents,
            document_upload,
            document_open,
            document_delete,
            chat_sync,
            ask,
            summarize,
            settings_get,
            settings_set,
            providers,
        ])
        .run(tauri::generate_context!())
        .expect("error while running PDF Kernel");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_build_providers_in_order() {
        let s = AiSettings {
            groq_keys: vec!["a, b".into(), "b".into(), " ".into()],
            gemini_keys: vec!["g".into()],
            lmstudio_enabled: true,
            order: vec!["lmstudio".into(), "gemini".into(), "groq".into()],
            ..AiSettings::defaults()
        };
        let p = s.providers();
        let names: Vec<_> = p.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["lmstudio", "gemini", "groq"]);
        assert_eq!(p[2].api_keys, ["a", "b"]);
        assert_eq!(p[0].model, None);
        assert!(AiSettings::defaults().providers().is_empty());
    }
}
