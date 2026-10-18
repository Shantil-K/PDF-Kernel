#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read PDF: {0}")]
    Pdf(String),
    #[error("no text could be extracted from the PDF (it may be scanned images)")]
    EmptyDocument,
    #[error("document not found: {0}")]
    DocumentNotFound(String),
    #[error("no AI provider is configured; set GROQ_API_KEYS, GEMINI_API_KEYS or LMSTUDIO_ENABLED=true")]
    NoProvider,
    #[error("all providers failed: {0}")]
    AllProvidersFailed(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Stable machine-readable code, used by every front-end.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Pdf(_) => "pdf_error",
            Error::EmptyDocument => "empty_document",
            Error::DocumentNotFound(_) => "document_not_found",
            Error::NoProvider => "no_provider",
            Error::AllProvidersFailed(_) => "providers_failed",
            Error::InvalidRequest(_) => "invalid_request",
            Error::UnknownMethod(_) => "unknown_method",
            Error::Config(_) => "config_error",
            Error::Io(_) => "io_error",
            Error::Internal(_) => "internal",
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Internal(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
