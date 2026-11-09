use std::sync::Arc;

mod worker;
use worker::Procs;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let procs = Arc::new(Procs::default());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(procs.clone())
        .invoke_handler(tauri::generate_handler![
            worker::prereqs,
            worker::hardware,
            worker::download_worker,
            worker::setup_ollama,
            worker::ollama_ensure,
            worker::ollama_models,
            worker::ollama_pull,
            worker::worker_start,
            worker::worker_stop,
            worker::worker_running,
            worker::models_dir_get,
            worker::models_dir_set,
            worker::chats_read,
            worker::chats_write,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(move |_app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // Make sure no worker/ollama process is left running.
                let p = procs.clone();
                tauri::async_runtime::block_on(async move {
                    worker::kill_all(&p).await;
                });
            }
        });
}
