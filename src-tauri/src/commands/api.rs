use crate::managers::diarization::DiarizationManager;
use crate::managers::transcription::TranscriptionManager;
use crate::server::ApiServer;
use crate::settings::{get_settings, write_settings};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

#[tauri::command]
#[specta::specta]
pub fn get_local_api_enabled(app: AppHandle) -> bool {
    let settings = get_settings(&app);
    settings.local_api_enabled
}

#[tauri::command]
#[specta::specta]
pub fn set_local_api_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let api_server = app
        .try_state::<Mutex<ApiServer>>()
        .ok_or_else(|| "API server not initialized".to_string())?;

    let mut server = api_server
        .lock()
        .map_err(|e| format!("Failed to lock API server: {e}"))?;

    if enabled && !server.is_running() {
        let tm = app.state::<Arc<TranscriptionManager>>().inner().clone();
        let dm = app.state::<Arc<DiarizationManager>>().inner().clone();
        let app_handle = app.clone();
        server.start(tm, dm, move || get_settings(&app_handle));
    } else if !enabled && server.is_running() {
        server.stop();
    }

    drop(server);

    let mut settings = get_settings(&app);
    settings.local_api_enabled = enabled;
    write_settings(&app, settings);
    log::info!("Local API enabled: {}", enabled);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_local_api_port(app: AppHandle) -> u16 {
    let settings = get_settings(&app);
    settings.local_api_port
}

#[tauri::command]
#[specta::specta]
pub fn set_local_api_port(app: AppHandle, port: u16) -> Result<(), String> {
    if port == 0 {
        return Err("Port must be greater than 0".to_string());
    }

    let api_server = app
        .try_state::<Mutex<ApiServer>>()
        .ok_or_else(|| "API server not initialized".to_string())?;

    let mut server = api_server
        .lock()
        .map_err(|e| format!("Failed to lock API server: {e}"))?;

    let was_running = server.is_running();

    // Stop the server if it was running (port change requires restart)
    if was_running {
        server.stop();
    }

    server.set_port(port);

    // Restart if it was running
    if was_running {
        let tm = app.state::<Arc<TranscriptionManager>>().inner().clone();
        let dm = app.state::<Arc<DiarizationManager>>().inner().clone();
        let app_handle = app.clone();
        server.start(tm, dm, move || get_settings(&app_handle));
    }

    drop(server);

    let mut settings = get_settings(&app);
    settings.local_api_port = port;
    write_settings(&app, settings);
    log::info!("Local API port set to: {}", port);
    Ok(())
}
