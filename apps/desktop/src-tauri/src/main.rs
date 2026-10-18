// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use pdf_kernel::rpc::{self, Access};
use pdf_kernel::Engine;
use serde_json::Value;

struct Kernel(Engine);

/// Single bridge command: the UI sends protocol requests, same as stdio/FFI.
#[tauri::command]
async fn kernel(state: tauri::State<'_, Kernel>, request: Value) -> Result<Value, String> {
    let req: rpc::Request = serde_json::from_value(request).map_err(|e| e.to_string())?;
    // The window only loads bundled local files, so file paths are allowed.
    let resp = rpc::dispatch_with(&state.0, req, Access { allow_file_paths: true }).await;
    serde_json::to_value(resp).map_err(|e| e.to_string())
}

fn main() {
    let engine = Engine::from_env().expect("failed to start pdf-kernel");
    tauri::Builder::default()
        .manage(Kernel(engine))
        .invoke_handler(tauri::generate_handler![kernel])
        .run(tauri::generate_context!())
        .expect("error while running the app");
}
