//! pdf-kernel core.
//!
//! Everything lives here: PDF extraction, chunking, retrieval, prompting,
//! provider routing and API-key rotation. It has no UI and no server.
//! Front-ends (CLI, stdio, HTTP, C FFI, Tauri) are thin layers over [`Engine`]
//! and the JSON protocol in [`rpc`].

pub mod chunk;
pub mod config;
pub mod engine;
pub mod error;
pub mod keypool;
pub mod pdf;
pub mod provider;
pub mod rpc;
pub mod search;

pub use config::{Config, ProviderConfig};
pub use engine::{Answer, AskOptions, DocumentInfo, Engine, Source, Summary, SummaryStyle};
pub use error::{Error, Result};
pub use provider::{ChatMessage, ProviderStatus};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
