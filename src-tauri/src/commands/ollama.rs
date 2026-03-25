/// Check if an Ollama instance is reachable at the given base_url.
/// Returns true if the server responds, false otherwise.
#[specta::specta]
#[tauri::command]
pub async fn check_ollama_status(base_url: String) -> bool {
    crate::llm_client::check_ollama_status(&base_url).await
}

/// Fetch available Ollama models with rich metadata (size, parameters, quantization).
#[specta::specta]
#[tauri::command]
pub async fn fetch_ollama_models_detailed(
    app: tauri::AppHandle,
) -> Result<Vec<crate::llm_client::OllamaModelInfo>, String> {
    let settings = crate::settings::get_settings(&app);
    let provider = settings
        .post_process_provider("ollama")
        .cloned()
        .ok_or_else(|| "Ollama provider not found in settings".to_string())?;
    crate::llm_client::fetch_ollama_models_with_metadata(&provider).await
}
