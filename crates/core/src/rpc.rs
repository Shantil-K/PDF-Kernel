//! The JSON protocol shared by every front-end (stdio, C FFI, HTTP `/v1/rpc`,
//! Tauri). One request in, one response out:
//!
//! ```json
//! {"id": 1, "method": "summarize", "params": {"document_id": "…", "style": "bullets"}}
//! {"id": 1, "result": {…}}
//! {"id": 1, "error": {"code": "document_not_found", "message": "…"}}
//! ```
//!
//! See `docs/PROTOCOL.md` for every method.

use crate::engine::{AskOptions, Engine, SummaryStyle};
use crate::error::{Error, Result};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl Response {
    pub fn error(id: Value, code: &str, message: impl Into<String>) -> Self {
        Self { id, result: None, error: Some(ErrorBody { code: code.into(), message: message.into() }) }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Access {
    /// Allow `load_pdf` with a local `path`. Turn off for network-facing
    /// front-ends so remote callers cannot read files on this machine.
    pub allow_file_paths: bool,
}

impl Default for Access {
    fn default() -> Self {
        Self { allow_file_paths: true }
    }
}

pub async fn dispatch(engine: &Engine, req: Request) -> Response {
    dispatch_with(engine, req, Access::default()).await
}

pub async fn dispatch_with(engine: &Engine, req: Request, access: Access) -> Response {
    let id = req.id;
    match call(engine, &req.method, req.params, access).await {
        Ok(result) => Response { id, result: Some(result), error: None },
        Err(e) => Response::error(id, e.code(), e.to_string()),
    }
}

/// Parse a JSON request string, run it, and return the JSON response string.
pub async fn handle_json(engine: &Engine, input: &str, access: Access) -> String {
    let resp = match serde_json::from_str::<Request>(input) {
        Ok(req) => dispatch_with(engine, req, access).await,
        Err(e) => Response::error(Value::Null, "parse_error", e.to_string()),
    };
    serde_json::to_string(&resp)
        .unwrap_or_else(|_| r#"{"id":null,"error":{"code":"internal","message":"could not encode response"}}"#.into())
}

#[derive(Deserialize)]
struct LoadPdf {
    path: Option<String>,
    name: Option<String>,
    data_base64: Option<String>,
}

#[derive(Deserialize)]
struct LoadText {
    name: String,
    pages: Vec<String>,
}

#[derive(Deserialize)]
struct DocId {
    document_id: String,
}

#[derive(Deserialize)]
struct Summarize {
    document_id: String,
    #[serde(default)]
    style: SummaryStyle,
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Deserialize)]
struct Ask {
    document_id: String,
    question: String,
    #[serde(flatten)]
    options: AskOptions,
}

fn params<T: DeserializeOwned>(v: Value) -> Result<T> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| Error::InvalidRequest(e.to_string()))
}

async fn call(engine: &Engine, method: &str, p: Value, access: Access) -> Result<Value> {
    Ok(match method {
        "version" => json!({
            "name": "pdf-kernel",
            "version": crate::VERSION,
            "protocol": PROTOCOL_VERSION,
        }),
        "providers" => serde_json::to_value(engine.providers())?,
        "load_pdf" => {
            let a: LoadPdf = params(p)?;
            let info = match (a.path, a.data_base64) {
                (Some(path), _) => {
                    if !access.allow_file_paths {
                        return Err(Error::InvalidRequest(
                            "'path' is not allowed on this transport; send 'data_base64'".into(),
                        ));
                    }
                    engine.load_pdf_file(path).await?
                }
                (None, Some(b64)) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(b64.trim())
                        .map_err(|e| Error::InvalidRequest(format!("bad base64: {e}")))?;
                    engine
                        .load_pdf_bytes(a.name.unwrap_or_else(|| "document.pdf".into()), bytes)
                        .await?
                }
                (None, None) => return Err(Error::InvalidRequest("load_pdf needs 'path' or 'data_base64'".into())),
            };
            serde_json::to_value(info)?
        }
        "load_text" => {
            let a: LoadText = params(p)?;
            serde_json::to_value(engine.add_pages(a.name, a.pages)?)?
        }
        "list_documents" => serde_json::to_value(engine.list_documents())?,
        "remove_document" => {
            let a: DocId = params(p)?;
            json!({ "removed": engine.remove_document(&a.document_id) })
        }
        "summarize" => {
            let a: Summarize = params(p)?;
            serde_json::to_value(engine.summarize(&a.document_id, a.style, a.provider).await?)?
        }
        "ask" => {
            let a: Ask = params(p)?;
            serde_json::to_value(engine.ask(&a.document_id, &a.question, a.options).await?)?
        }
        other => return Err(Error::UnknownMethod(other.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    async fn run(engine: &Engine, input: &str) -> Value {
        serde_json::from_str(&handle_json(engine, input, Access::default()).await).unwrap()
    }

    #[tokio::test]
    async fn protocol_round_trip() {
        let e = Engine::new(Config::default()).unwrap();
        let v = run(&e, r#"{"id":1,"method":"version"}"#).await;
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["protocol"], PROTOCOL_VERSION);

        let v = run(&e, r#"{"id":"a","method":"load_text","params":{"name":"n","pages":["alpha beta"]}}"#).await;
        let doc = v["result"]["id"].as_str().unwrap().to_string();
        let v = run(&e, r#"{"id":2,"method":"list_documents"}"#).await;
        assert_eq!(v["result"][0]["id"], doc.as_str());

        let v = run(&e, &format!(r#"{{"id":3,"method":"ask","params":{{"document_id":"{doc}","question":"alpha?"}}}}"#)).await;
        assert_eq!(v["error"]["code"], "no_provider");
    }

    #[tokio::test]
    async fn errors_are_structured() {
        let e = Engine::new(Config::default()).unwrap();
        assert_eq!(run(&e, "not json").await["error"]["code"], "parse_error");
        assert_eq!(run(&e, r#"{"id":1,"method":"nope"}"#).await["error"]["code"], "unknown_method");
        assert_eq!(
            run(&e, r#"{"id":1,"method":"summarize","params":{"document_id":"missing"}}"#).await["error"]["code"],
            "document_not_found"
        );
        let denied = handle_json(
            &e,
            r#"{"id":1,"method":"load_pdf","params":{"path":"/etc/passwd"}}"#,
            Access { allow_file_paths: false },
        )
        .await;
        assert!(denied.contains("invalid_request"));
    }
}
