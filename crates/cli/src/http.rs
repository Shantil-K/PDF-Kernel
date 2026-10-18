//! Optional HTTP API. API only, no web UI. Binds to localhost by default.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use pdf_kernel::rpc::{self, Access};
use pdf_kernel::{AskOptions, Engine, Error, SummaryStyle};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
struct AppState {
    engine: Arc<Engine>,
    token: Option<Arc<str>>,
}

const NETWORK: Access = Access { allow_file_paths: false };

pub async fn serve(
    engine: Arc<Engine>,
    host: &str,
    port: u16,
    token: Option<String>,
    cors_origins: Vec<String>,
) -> std::io::Result<()> {
    let body_limit = engine.config().max_pdf_bytes + 1024 * 1024;
    let token = token.filter(|t| !t.is_empty()).map(Arc::<str>::from);
    let state = AppState { engine, token: token.clone() };

    let mut app = Router::new()
        .route("/v1/providers", get(providers))
        .route("/v1/documents", get(list_documents).post(upload))
        .route("/v1/documents/{id}", delete(remove_document))
        .route("/v1/documents/{id}/summary", post(summarize))
        .route("/v1/documents/{id}/ask", post(ask))
        .route("/v1/rpc", post(rpc_call))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        // Health stays open so supervisors can probe it.
        .route("/v1/health", get(|| async { Json(json!({ "ok": true, "version": pdf_kernel::VERSION })) }))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state);

    if !cors_origins.is_empty() {
        let origins = cors_origins
            .iter()
            .map(|o| o.parse::<HeaderValue>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        app = app.layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods(Any)
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        );
    }

    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    let addr = listener.local_addr()?;
    eprintln!("pdf-kernel HTTP API on http://{addr}/v1");
    if token.is_none() && !addr.ip().is_loopback() {
        eprintln!("warning: listening on a network address without --token; anyone who can reach it can use your API keys");
    }
    axum::serve(listener, app).await
}

async fn auth(State(s): State<AppState>, req: Request, next: Next) -> Response {
    if let Some(expected) = &s.token {
        let given = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if !given.is_some_and(|g| constant_time_eq(g.as_bytes(), expected.as_bytes())) {
            return ApiError(StatusCode::UNAUTHORIZED, "unauthorized".into(), "missing or wrong bearer token".into())
                .into_response();
        }
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct ApiError(StatusCode, String, String);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let status = match &e {
            Error::DocumentNotFound(_) => StatusCode::NOT_FOUND,
            Error::Pdf(_) | Error::EmptyDocument | Error::InvalidRequest(_) | Error::UnknownMethod(_) => {
                StatusCode::BAD_REQUEST
            }
            Error::NoProvider => StatusCode::SERVICE_UNAVAILABLE,
            Error::AllProvidersFailed(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(status, e.code().into(), e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": { "code": self.1, "message": self.2 } }))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn to_json<T: serde::Serialize>(v: T) -> ApiResult {
    serde_json::to_value(v).map(Json).map_err(|e| Error::from(e).into())
}

/// Parse an optional JSON body; an empty body means defaults.
fn body_or_default<T: serde::de::DeserializeOwned + Default>(body: &Bytes) -> Result<T, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| Error::InvalidRequest(e.to_string()).into())
}

async fn providers(State(s): State<AppState>) -> ApiResult {
    to_json(s.engine.providers())
}

async fn list_documents(State(s): State<AppState>) -> ApiResult {
    to_json(s.engine.list_documents())
}

#[derive(Deserialize)]
struct UploadQuery {
    name: Option<String>,
}

/// Upload raw PDF bytes: `curl --data-binary @file.pdf '…/v1/documents?name=file.pdf'`
async fn upload(State(s): State<AppState>, Query(q): Query<UploadQuery>, body: Bytes) -> ApiResult {
    let name = q.name.unwrap_or_else(|| "document.pdf".into());
    to_json(s.engine.load_pdf_bytes(name, body.to_vec()).await?)
}

async fn remove_document(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(json!({ "removed": s.engine.remove_document(&id) })))
}

#[derive(Deserialize, Default)]
struct SummaryBody {
    #[serde(default)]
    style: SummaryStyle,
    #[serde(default)]
    provider: Option<String>,
}

async fn summarize(State(s): State<AppState>, Path(id): Path<String>, body: Bytes) -> ApiResult {
    let b: SummaryBody = body_or_default(&body)?;
    to_json(s.engine.summarize(&id, b.style, b.provider).await?)
}

#[derive(Deserialize, Default)]
struct AskBody {
    #[serde(default)]
    question: String,
    #[serde(flatten)]
    options: AskOptions,
}

async fn ask(State(s): State<AppState>, Path(id): Path<String>, body: Bytes) -> ApiResult {
    let b: AskBody = body_or_default(&body)?;
    to_json(s.engine.ask(&id, &b.question, b.options).await?)
}

/// Same protocol as stdio and FFI, over HTTP.
async fn rpc_call(State(s): State<AppState>, body: Bytes) -> Response {
    let text = String::from_utf8_lossy(&body);
    let out = rpc::handle_json(&s.engine, &text, NETWORK).await;
    ([(header::CONTENT_TYPE, "application/json")], out).into_response()
}
