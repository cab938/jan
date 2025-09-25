mod core;
use core::{
    app::commands::get_jan_data_folder_path,
    downloads::models::DownloadManagerState,
    mcp::helpers::clean_up_mcp_servers,
    setup::{self, setup_mcp},
    state::AppState,
};
use jan_utils::generate_app_token;
use log::LevelFilter;
use std::{collections::HashMap, env, sync::Arc};
use tauri::{Emitter, Manager, RunEvent};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_llamacpp::cleanup_llama_processes;
use tokio::sync::Mutex;

use crate::core::setup::setup_tray;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (log_level, log_level_source) = resolve_log_level_from_args();
    let mut builder = tauri::Builder::default();
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|_app, argv, _cwd| {
          println!("a new app instance was opened with {argv:?} and the deep link event was already triggered");
          // when defining deep link schemes at runtime, you must also check `argv` here
          let arg = argv.iter().find(|arg| arg.starts_with("jan://"));
            if let Some(deep_link) = arg {
                println!("deep link: {deep_link}");
                // handle the deep link, e.g., emit an event to the webview
                _app.app_handle().emit("deep-link", deep_link).unwrap();
                if let Some(window) = _app.app_handle().get_webview_window("main") {
                    let _ = window.set_focus();
                }
            }
        }));
    }

    let app = builder
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_llamacpp::init())
        .plugin(tauri_plugin_hardware::init())
        .invoke_handler(tauri::generate_handler![
            // FS commands - Deperecate soon
            core::filesystem::commands::join_path,
            core::filesystem::commands::mkdir,
            core::filesystem::commands::exists_sync,
            core::filesystem::commands::readdir_sync,
            core::filesystem::commands::read_file_sync,
            core::filesystem::commands::rm,
            core::filesystem::commands::file_stat,
            core::filesystem::commands::write_file_sync,
            core::filesystem::commands::write_yaml,
            core::filesystem::commands::read_yaml,
            core::filesystem::commands::decompress,
            // App configuration commands
            core::app::commands::get_app_configurations,
            core::app::commands::get_user_home_path,
            core::app::commands::update_app_configuration,
            core::app::commands::get_jan_data_folder_path,
            core::app::commands::get_configuration_file_path,
            core::app::commands::default_data_folder_path,
            core::app::commands::change_app_data_folder,
            core::app::commands::app_token,
            // Extension commands
            core::extensions::commands::get_jan_extensions_path,
            core::extensions::commands::install_extensions,
            core::extensions::commands::get_active_extensions,
            // System commands
            core::system::commands::relaunch,
            core::system::commands::open_app_directory,
            core::system::commands::open_file_explorer,
            core::system::commands::factory_reset,
            core::system::commands::read_logs,
            core::system::commands::is_library_available,
            // Server commands
            core::server::commands::start_server,
            core::server::commands::stop_server,
            core::server::commands::get_server_status,
            // MCP commands
            core::mcp::commands::get_tools,
            core::mcp::commands::call_tool,
            core::mcp::commands::cancel_tool_call,
            core::mcp::commands::restart_mcp_servers,
            core::mcp::commands::get_connected_servers,
            core::mcp::commands::save_mcp_configs,
            core::mcp::commands::get_mcp_configs,
            core::mcp::commands::activate_mcp_server,
            core::mcp::commands::deactivate_mcp_server,
            core::mcp::commands::reset_mcp_restart_count,
            // Threads
            core::threads::commands::list_threads,
            core::threads::commands::create_thread,
            core::threads::commands::modify_thread,
            core::threads::commands::delete_thread,
            core::threads::commands::list_messages,
            core::threads::commands::create_message,
            core::threads::commands::modify_message,
            core::threads::commands::delete_message,
            core::threads::commands::get_thread_assistant,
            core::threads::commands::create_thread_assistant,
            core::threads::commands::modify_thread_assistant,
            // Download
            core::downloads::commands::download_files,
            core::downloads::commands::cancel_download_task,
        ])
        .manage(AppState {
            app_token: Some(generate_app_token()),
            mcp_servers: Arc::new(Mutex::new(HashMap::new())),
            download_manager: Arc::new(Mutex::new(DownloadManagerState::default())),
            mcp_restart_counts: Arc::new(Mutex::new(HashMap::new())),
            mcp_active_servers: Arc::new(Mutex::new(HashMap::new())),
            mcp_successfully_connected: Arc::new(Mutex::new(HashMap::new())),
            server_handle: Arc::new(Mutex::new(None)),
            tool_call_cancellations: Arc::new(Mutex::new(HashMap::new())),
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if option_env!("ENABLE_SYSTEM_TRAY_ICON").unwrap_or("false") == "true" {
                    #[cfg(target_os = "macos")]
                    window
                        .app_handle()
                        .set_activation_policy(tauri::ActivationPolicy::Accessory)
                        .unwrap();

                    window.hide().unwrap();
                    api.prevent_close();
                }
            }
            _ => {}
        })
        .setup(move |app| {
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log_level)
                    .targets([
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Folder {
                            path: get_jan_data_folder_path(app.handle().clone()).join("logs"),
                            file_name: Some("app".to_string()),
                        }),
                    ])
                    .build(),
            )?;
            match log_level_source {
                Some(source) => {
                    log::info!("Initialized logging at level {:?} via {source}", log_level);
                }
                None => {
                    log::info!("Initialized logging at default level {:?}", log_level);
                }
            }
            app.handle()
                .plugin(tauri_plugin_updater::Builder::new().build())?;
            // Install extensions
            if let Err(e) = setup::install_extensions(app.handle().clone(), false) {
                log::error!("Failed to install extensions: {}", e);
            }

            if option_env!("ENABLE_SYSTEM_TRAY_ICON").unwrap_or("false") == "true" {
                log::info!("Enabling system tray icon");
                let _ = setup_tray(app);
            }

            #[cfg(any(windows, target_os = "linux"))]
            {
                app.deep_link().register_all()?;
            }
            setup_mcp(app);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    // Handle app lifecycle events
    app.run(|app, event| match event {
        RunEvent::Exit => {
            // This is called when the app is actually exiting (e.g., macOS dock quit)
            // We can't prevent this, so run cleanup quickly
            let app_handle = app.clone();
            // Hide window immediately
            if let Some(window) = app_handle.get_webview_window("main") {
                let _ = window.hide();
            }
            tokio::task::block_in_place(|| {
                tauri::async_runtime::block_on(async {
                    // Quick cleanup with shorter timeout
                    let state = app_handle.state::<AppState>();
                    let _ = clean_up_mcp_servers(state).await;
                    let _ = cleanup_llama_processes(app.clone()).await;
                });
            });
        }
        _ => {}
    });
}

fn resolve_log_level_from_args() -> (LevelFilter, Option<String>) {
    let mut args = env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--log-level=") {
            return match parse_log_level(value) {
                Ok(level) => (level, Some("--log-level".to_string())),
                Err(err) => {
                    eprintln!("{err}");
                    (
                        LevelFilter::Debug,
                        Some("--log-level (invalid value)".to_string()),
                    )
                }
            };
        }

        if arg == "--log-level" {
            if let Some(value) = args.next() {
                return match parse_log_level(&value) {
                    Ok(level) => (level, Some("--log-level".to_string())),
                    Err(err) => {
                        eprintln!("{err}");
                        (
                            LevelFilter::Debug,
                            Some("--log-level (invalid value)".to_string()),
                        )
                    }
                };
            } else {
                eprintln!(
                    "--log-level flag requires a value (error, warn, info, debug, trace, off)"
                );
                return (LevelFilter::Debug, Some("--log-level".to_string()));
            }
        }
    }

    (LevelFilter::Debug, None)
}

fn parse_log_level(value: &str) -> Result<LevelFilter, String> {
    match value.to_ascii_lowercase().as_str() {
        "error" => Ok(LevelFilter::Error),
        "warn" | "warning" => Ok(LevelFilter::Warn),
        "info" => Ok(LevelFilter::Info),
        "debug" => Ok(LevelFilter::Debug),
        "trace" => Ok(LevelFilter::Trace),
        "off" => Ok(LevelFilter::Off),
        other => Err(format!(
            "Unrecognized log level '{other}'. Expected one of: error, warn, info, debug, trace, off"
        )),
    }
}
