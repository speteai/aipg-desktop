//! Commands for signing in through the browser. The protocol lives in `auth`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;

use crate::auth;

/// Cancel switch of the sign-in that is waiting for the browser, if any.
static WAITING: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("aipg-desktop")
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct AuthStatus {
    signed_in: bool,
    /// Unix time of the sign-in; the grid ends a session after 180 days.
    since: u64,
}

#[tauri::command]
pub fn auth_status(app: AppHandle) -> AuthStatus {
    let v = auth::load(&crate::worker::data_dir(&app));
    AuthStatus {
        signed_in: v.is_some(),
        since: v.map(|v| v["since"].as_u64().unwrap_or(0)).unwrap_or(0),
    }
}

/// Open the browser, wait until the user has approved, store the session.
#[tauri::command]
pub async fn auth_signin(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let http = http()?;
    let pending = auth::begin(&http, auth::BASE).await?;
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(old) = WAITING.lock().unwrap().replace(cancel.clone()) {
        old.store(true, Ordering::SeqCst);
    }
    app.opener()
        .open_url(pending.url.clone(), None::<&str>)
        .map_err(|e| format!("Could not open the browser: {e}"))?;
    let out = auth::finish(pending, &crate::worker::data_dir(&app), &http, auth::BASE, cancel.clone()).await;
    let mut waiting = WAITING.lock().unwrap();
    if waiting.as_ref().map(|w| Arc::ptr_eq(w, &cancel)).unwrap_or(false) {
        *waiting = None;
    }
    out
}

#[tauri::command]
pub fn auth_cancel() {
    if let Some(w) = WAITING.lock().unwrap().take() {
        w.store(true, Ordering::SeqCst);
    }
}

/// The current access token, renewed when needed; `None` when signed out.
#[tauri::command]
pub async fn auth_token(app: AppHandle) -> Result<Option<String>, String> {
    auth::bearer(&crate::worker::data_dir(&app), &http()?, auth::BASE).await
}

#[tauri::command]
pub async fn auth_signout(app: AppHandle) -> Result<(), String> {
    auth::sign_out(&crate::worker::data_dir(&app), &http()?, auth::BASE).await;
    Ok(())
}
