#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("Google sign-in is not set up in this build (missing OAuth client id)")]
    NotConfigured,
    #[error("sign-in failed: {0}")]
    Auth(String),
    #[error("please sign in to {0} again")]
    Reauth(String),
    #[error("unknown account: {0}")]
    UnknownAccount(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("secure storage error: {0}")]
    Secrets(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("data error: {0}")]
    Data(#[from] serde_json::Error),
}

impl CloudError {
    /// True when the failure means "can't reach the server", so cached data
    /// can be shown instead.
    pub fn is_offline(&self) -> bool {
        matches!(self, CloudError::Network(_))
    }
}

impl From<reqwest::Error> for CloudError {
    fn from(e: reqwest::Error) -> Self {
        CloudError::Network(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, CloudError>;
