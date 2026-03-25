/// Check if an Ollama instance is reachable at the given base_url.
/// Returns true if the server responds, false otherwise.
#[specta::specta]
#[tauri::command]
pub async fn check_ollama_status(base_url: String) -> bool {
    crate::llm_client::check_ollama_status(&base_url).await
}
