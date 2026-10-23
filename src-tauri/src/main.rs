#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod store;

use serde::Serialize;
use std::sync::OnceLock;
use store::{Store, Summary};
use tauri::ipc::Response;
use tauri::{Manager, State};

struct AppState {
    store: Store,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    /// This build has no recorder yet, so the UI hides everything that starts one.
    can_record: bool,
    recording: bool,
}

#[tauri::command]
fn capabilities() -> Capabilities {
    Capabilities { can_record: false, recording: false }
}

#[tauri::command]
async fn list_sessions(state: State<'_, AppState>) -> Result<Vec<Summary>, String> {
    Ok(state.store.list())
}

/// Sends the saved file as raw bytes, so a report is never decoded and re-encoded on the way.
#[tauri::command]
async fn get_session(state: State<'_, AppState>, id: String) -> Result<Response, String> {
    state.store.read(&id).map(Response::new)
}

#[tauri::command]
async fn delete_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.store.delete(&id)
}

/// The example report, worked out once on first use.
#[tauri::command]
async fn example_session() -> Result<Response, String> {
    static EXAMPLE: OnceLock<Vec<u8>> = OnceLock::new();
    let bytes = tauri::async_runtime::spawn_blocking(|| {
        EXAMPLE
            .get_or_init(|| {
                let session = culpritcore::summarize(&culpritcore::demo::defender());
                serde_json::to_vec(&session).unwrap_or_default()
            })
            .clone()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

const NO_RECORDER: &str = "This build can't record yet.";

#[tauri::command]
fn start_recording() -> Result<(), String> {
    Err(NO_RECORDER.into())
}

#[tauri::command]
fn stop_recording() -> Result<Option<String>, String> {
    Err(NO_RECORDER.into())
}

#[tauri::command]
fn live() -> Result<(), String> {
    Err(NO_RECORDER.into())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let dir = app.path().app_data_dir()?.join("sessions");
            app.manage(AppState { store: Store::new(dir) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            capabilities,
            list_sessions,
            get_session,
            delete_session,
            example_session,
            start_recording,
            stop_recording,
            live
        ])
        .run(tauri::generate_context!())
        .expect("error while running culprit");
}
