#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod store;

use culprit_capture::collector::{Live, Options};
use culprit_capture::log;
use culprit_capture::recorder::{self, Recorder};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use store::{Store, Summary};
use tauri::ipc::Response;
use tauri::{AppHandle, Manager, RunEvent, State};

struct AppState {
    store: Store,
    recorder: Mutex<Option<Recorder>>,
    /// Set while a recording is starting, so a second click doesn't start another.
    starting: AtomicBool,
}

impl AppState {
    fn recorder(&self) -> MutexGuard<'_, Option<Recorder>> {
        self.recorder.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    /// Only Windows can record; elsewhere the UI hides everything that starts a recording.
    can_record: bool,
    recording: bool,
}

#[tauri::command]
fn capabilities(state: State<'_, AppState>) -> Capabilities {
    Capabilities { can_record: recorder::supported(), recording: state.recorder().is_some() }
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

/// The example report, worked out once. Startup warms it in the background, so opening it
/// is instant.
fn example_bytes() -> &'static [u8] {
    static EXAMPLE: OnceLock<Vec<u8>> = OnceLock::new();
    EXAMPLE.get_or_init(|| {
        let session = culpritcore::summarize(&culpritcore::demo::defender());
        serde_json::to_vec(&session).unwrap_or_default()
    })
}

#[tauri::command]
async fn example_session() -> Result<Response, String> {
    let bytes = tauri::async_runtime::spawn_blocking(|| example_bytes().to_vec()).await.map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

/// Starts recording. Windows only lets administrators trace frames and network traffic, so
/// without those rights Culprit asks for them by starting again as administrator, and the
/// new window picks up recording (see `main`).
#[tauri::command]
async fn start_recording(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if !recorder::supported() {
        return Err("Culprit can only record on Windows.".into());
    }
    if state.recorder().is_some() || state.starting.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    if !recorder::elevated() {
        state.starting.store(false, Ordering::SeqCst);
        log!("app: asking Windows to start Culprit again as administrator");
        let pid = std::process::id().to_string();
        recorder::relaunch_elevated(&["--record", "--after", &pid]).inspect_err(|e| log!("app: {e}"))?;
        app.exit(0);
        return Ok(());
    }
    let started = tauri::async_runtime::spawn_blocking(|| Recorder::start(Options::default())).await;
    state.starting.store(false, Ordering::SeqCst);
    let rec = started.map_err(|e| e.to_string())?.inspect_err(|e| log!("app: recording didn't start: {e}"))?;
    *state.recorder() = Some(rec);
    Ok(())
}

/// Stops, works out the report, saves it and returns its id.
fn finish(rec: Recorder, store: &Store) -> Result<Option<String>, String> {
    let session = culpritcore::summarize(&rec.stop());
    store.save(&session).inspect_err(|e| log!("app: {e}"))?;
    let r = &session.report;
    log!(
        "app: saved session {}: {:?} / {} stutters, {} lag spikes, ping {:?} ms",
        session.id,
        r.headline,
        r.perf.as_ref().map_or(0, |p| p.stutters),
        r.net.as_ref().map_or(0, |n| n.lag_spikes),
        r.net.as_ref().and_then(|n| n.ping_ms).map(|p| p.round())
    );
    Ok(Some(session.id))
}

#[tauri::command]
async fn stop_recording(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let Some(rec) = state.recorder().take() else { return Ok(None) };
    let store = state.store.clone();
    tauri::async_runtime::spawn_blocking(move || finish(rec, &store)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn live(state: State<'_, AppState>) -> Result<Live, String> {
    state.recorder().as_ref().map(Recorder::live).ok_or_else(|| "Not recording.".into())
}

/// Marks this moment, like the hotkey.
#[tauri::command]
async fn mark(state: State<'_, AppState>) -> Result<(), String> {
    state.recorder().as_ref().map(Recorder::mark).ok_or_else(|| "Not recording.".into())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let record_now = args.iter().any(|a| a == "--record");
    // Started again as administrator: wait for the old window to close first, so this one
    // isn't turned away as a second copy of the app.
    let after = args.iter().position(|a| a == "--after").and_then(|i| args.get(i + 1)?.parse::<u32>().ok());
    if let Some(pid) = after {
        recorder::wait_for_exit(pid, Duration::from_secs(10));
    }
    // A crash goes in the log before the app closes.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log!("app: crashed: {info}");
        default_hook(info);
    }));

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(move |app| {
            let data = app.path().app_data_dir()?;
            match log::open(&data.join("logs")) {
                Ok(path) => log!(
                    "app: Culprit {} started, administrator: {}, arguments {:?}, log at {}",
                    env!("CARGO_PKG_VERSION"),
                    recorder::elevated(),
                    args.get(1..).unwrap_or_default(),
                    path.display()
                ),
                Err(e) => eprintln!("couldn't open the log: {e}"),
            }
            let state = AppState {
                store: Store::new(data.join("sessions")),
                recorder: Mutex::new(None),
                starting: AtomicBool::new(false),
            };
            // The UI asks whether a recording is running as soon as it loads, so start first.
            if record_now && recorder::elevated() {
                match Recorder::start(Options::default()) {
                    Ok(rec) => *state.recorder() = Some(rec),
                    Err(e) => log!("app: recording didn't start: {e}"),
                }
            }
            app.manage(state);
            std::thread::spawn(example_bytes);
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
            live,
            mark
        ])
        .build(tauri::generate_context!())
        .expect("error while running culprit")
        .run(|app, event| {
            // Closing the window mid-recording still saves the session.
            if let RunEvent::Exit = event {
                if let Some(state) = app.try_state::<AppState>() {
                    if let Some(rec) = state.recorder().take() {
                        log!("app: closing while recording, saving the session first");
                        let _ = finish(rec, &state.store);
                    }
                }
                log!("app: closed");
            }
        });
}

#[cfg(test)]
mod tests {
    #[test]
    fn example_report_is_a_valid_session() {
        let session: culpritcore::Session = serde_json::from_slice(super::example_bytes()).unwrap();
        assert_eq!(session.report.headline, "Microsoft Defender caused 9 of your 11 stutters.");
    }
}
