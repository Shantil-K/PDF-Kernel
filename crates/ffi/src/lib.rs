//! C ABI. Four functions; everything else goes through the JSON protocol
//! (see docs/PROTOCOL.md), so bindings stay tiny in any language.
//!
//! Strings in and out are NUL-terminated UTF-8. Strings returned by this
//! library must be freed with `pk_string_free`.

use pdf_kernel::rpc::{self, Access};
use pdf_kernel::{Config, Engine};
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

pub struct PkEngine {
    runtime: tokio::runtime::Runtime,
    engine: Engine,
}

/// Create an engine.
///
/// `config_json` may be NULL to read config from the environment and `.env`
/// files, or a JSON object matching `Config` (see docs/PROTOCOL.md).
/// Returns NULL on failure; if `error_out` is not NULL it receives an error
/// message that must be freed with `pk_string_free`.
///
/// # Safety
/// `config_json` must be NULL or a valid NUL-terminated string.
/// `error_out` must be NULL or a valid pointer to writable storage.
#[no_mangle]
pub unsafe extern "C" fn pk_engine_new(config_json: *const c_char, error_out: *mut *mut c_char) -> *mut PkEngine {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<PkEngine, String> {
        let cfg = if config_json.is_null() {
            Config::from_env()
        } else {
            let s = CStr::from_ptr(config_json).to_str().map_err(|e| e.to_string())?;
            serde_json::from_str::<Config>(s).map_err(|e| format!("invalid config: {e}"))?
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("pdf-kernel")
            .build()
            .map_err(|e| e.to_string())?;
        let engine = Engine::new(cfg).map_err(|e| e.to_string())?;
        Ok(PkEngine { runtime, engine })
    }));
    let err = match result {
        Ok(Ok(engine)) => return Box::into_raw(Box::new(engine)),
        Ok(Err(e)) => e,
        Err(_) => "panic while creating engine".to_string(),
    };
    if !error_out.is_null() {
        *error_out = to_c_string(err);
    }
    ptr::null_mut()
}

/// Run one JSON request and return the JSON response.
///
/// Blocks until done, so call it off your UI thread. Safe to call from
/// several threads at once. Always returns a string (errors are JSON too)
/// unless allocation fails.
///
/// # Safety
/// `engine` must come from `pk_engine_new` and not be freed yet.
/// `request_json` must be a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn pk_engine_call(engine: *const PkEngine, request_json: *const c_char) -> *mut c_char {
    if engine.is_null() || request_json.is_null() {
        return to_c_string(error_json("invalid_request", "null engine or request"));
    }
    let engine = &*engine;
    let out = catch_unwind(AssertUnwindSafe(|| {
        let input = String::from_utf8_lossy(CStr::from_ptr(request_json).to_bytes());
        engine
            .runtime
            .block_on(rpc::handle_json(&engine.engine, &input, Access { allow_file_paths: true }))
    }))
    .unwrap_or_else(|_| error_json("internal", "panic while handling request"));
    to_c_string(out)
}

/// Free a string returned by this library. NULL is ignored.
///
/// # Safety
/// `s` must be NULL or a pointer returned by this library, freed only once.
#[no_mangle]
pub unsafe extern "C" fn pk_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Destroy an engine. NULL is ignored. No calls may be in flight.
///
/// # Safety
/// `engine` must be NULL or a pointer from `pk_engine_new`, freed only once.
#[no_mangle]
pub unsafe extern "C" fn pk_engine_free(engine: *mut PkEngine) {
    if !engine.is_null() {
        drop(Box::from_raw(engine));
    }
}

/// Library version, e.g. "0.1.0". Static; do not free.
#[no_mangle]
pub extern "C" fn pk_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

fn error_json(code: &str, message: &str) -> String {
    serde_json::json!({ "id": null, "error": { "code": code, "message": message } }).to_string()
}

fn to_c_string(s: String) -> *mut c_char {
    // Interior NULs cannot cross the C boundary; JSON never contains raw NULs.
    CString::new(s.replace('\0', "")).map(CString::into_raw).unwrap_or(ptr::null_mut())
}
