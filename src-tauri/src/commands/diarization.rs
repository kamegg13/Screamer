use crate::managers::diarization::DiarizationManager;
use crate::settings::{get_settings, write_settings};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[tauri::command]
#[specta::specta]
pub fn get_diarization_enabled(app: AppHandle) -> bool {
    let settings = get_settings(&app);
    settings.diarization_enabled
}

#[tauri::command]
#[specta::specta]
pub fn set_diarization_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let dm = app.state::<Arc<DiarizationManager>>();
    if enabled && !dm.models_available() {
        return Err("Diarization models not found. Please ensure segmentation-3.0.onnx and wespeaker_en_voxceleb_CAM++.onnx are in the models directory.".to_string());
    }

    let mut settings = get_settings(&app);
    settings.diarization_enabled = enabled;
    write_settings(&app, settings);
    log::info!("Diarization enabled: {}", enabled);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_diarization_max_speakers(app: AppHandle) -> u32 {
    let settings = get_settings(&app);
    settings.diarization_max_speakers
}

#[tauri::command]
#[specta::specta]
pub fn set_diarization_max_speakers(app: AppHandle, max_speakers: u32) -> Result<(), String> {
    if max_speakers < 2 || max_speakers > 20 {
        return Err("Max speakers must be between 2 and 20".to_string());
    }

    let mut settings = get_settings(&app);
    settings.diarization_max_speakers = max_speakers;
    write_settings(&app, settings);
    log::info!("Diarization max speakers set to: {}", max_speakers);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_diarization_models_available(app: AppHandle) -> bool {
    let dm = app.state::<Arc<DiarizationManager>>();
    dm.models_available()
}
