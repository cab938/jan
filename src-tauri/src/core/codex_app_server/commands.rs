use tauri::{AppHandle, Runtime, State};

use crate::core::{
    codex_app_server::shim::{self, CodexShimConfig},
    state::AppState,
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartCodexShimConfig {
    pub host: String,
    pub port: u16,
    pub tool_mode: String,
    pub model_id: Option<String>,
    pub codex_app_server_path: Option<String>,
}

impl From<StartCodexShimConfig> for CodexShimConfig {
    fn from(config: StartCodexShimConfig) -> Self {
        CodexShimConfig {
            host: config.host,
            port: config.port,
            tool_mode: config.tool_mode,
            model_id: config.model_id,
            codex_app_server_path: config.codex_app_server_path,
        }
    }
}

#[tauri::command]
pub async fn start_codex_app_server_shim<R: Runtime>(
    _app_handle: AppHandle<R>,
    state: State<'_, AppState>,
    config: StartCodexShimConfig,
) -> Result<u16, String> {
    let handle = state.codex_shim_handle.clone();
    let shim_state = state.codex_shim_state.clone();
    shim::start_server(handle, shim_state, config.into()).await
}

#[tauri::command]
pub async fn stop_codex_app_server_shim(state: State<'_, AppState>) -> Result<(), String> {
    let handle = state.codex_shim_handle.clone();
    let shim_state = state.codex_shim_state.clone();
    shim::stop_server(handle, shim_state).await
}

#[tauri::command]
pub async fn get_codex_app_server_shim_status(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let handle = state.codex_shim_handle.clone();
    Ok(shim::is_running(handle).await)
}
