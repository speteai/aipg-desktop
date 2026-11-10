use std::sync::Arc;

use tauri::menu::{MenuBuilder, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;

mod connector;
mod mcp;
mod worker;
use worker::Procs;

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Tray tooltip mirrors the worker state ("serving 1 grid job"). Linux trays
/// have no tooltip, so this is a no-op there.
#[tauri::command]
fn tray_status(app: tauri::AppHandle, text: String) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(text));
    }
}

/// `--mcp <connector.json>`: run as a local MCP server on stdio instead of
/// opening the app (launched by Claude Desktop, Claude Code, …).
pub fn run_connector(key_file: String) {
    mcp::run_stdio(key_file.into());
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let procs = Arc::new(Procs::default());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(procs.clone())
        .invoke_handler(tauri::generate_handler![
            worker::prereqs,
            worker::hardware,
            worker::download_worker,
            worker::setup_ollama,
            worker::ollama_ensure,
            worker::ollama_models,
            worker::ollama_list,
            worker::ollama_delete,
            worker::update_check,
            worker::update_install,
            worker::ollama_pull,
            worker::worker_start,
            worker::worker_stop,
            worker::worker_running,
            worker::models_dir_get,
            worker::models_dir_set,
            worker::config_get,
            worker::config_set,
            worker::image_store,
            worker::images_index,
            worker::image_data,
            worker::image_remove,
            worker::image_export,
            worker::chats_read,
            worker::chats_write,
            tray_status,
            connector::connector_status,
            connector::connector_enable,
            connector::connector_disable,
            connector::connector_claude_add,
            connector::connector_claude_remove,
        ])
        .setup(|app| {
            // System tray: keep the worker earning while the window is closed.
            let handle = app.handle().clone();
            let show = MenuItem::with_id(&handle, "show", "Open AIPG Desktop", true, None::<&str>)?;
            let quit = MenuItem::with_id(&handle, "quit", "Quit", true, None::<&str>)?;
            let menu = MenuBuilder::new(&handle).item(&show).item(&quit).build()?;
            let icon = app.default_window_icon().unwrap().clone();
            let _ = TrayIconBuilder::with_id("main")
                .icon(icon)
                .tooltip("AIPG Desktop")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(&handle)?;
            // Launched at login (autostart passes --minimized): stay in the tray.
            if std::env::args().any(|a| a == "--minimized") {
                if let Some(w) = handle.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Default: hide to tray instead of quitting, so a running worker
                // keeps earning. Disable via Settings → "close to tray".
                if worker::config_bool(window.app_handle(), "close_to_tray", true) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(move |_app, event| {
            // Make sure no worker/ollama process is left running, whether the app
            // exits by closing the last window (ExitRequested) or via the tray's
            // "Quit" → app.exit(0) (Exit). kill_all is idempotent.
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                let p = procs.clone();
                tauri::async_runtime::block_on(async move {
                    worker::kill_all(&p).await;
                });
            }
        });
}
