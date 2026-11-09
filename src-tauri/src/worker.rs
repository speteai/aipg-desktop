//! All-in-one worker orchestration: verified download of the official
//! grid-inference-worker binary, Ollama lifecycle, and worker process
//! supervision with log streaming to the UI.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const WORKER_TAG: &str = "v0.3.12";
const REPO: &str = "AIPowerGrid/grid-text-worker";
const OLLAMA: &str = "http://127.0.0.1:11434";

/// Running child processes, owned by the app and killed on exit.
#[derive(Default)]
pub struct Procs {
    worker: Mutex<Option<Child>>,
    ollama: Mutex<Option<Child>>,
}

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

/// (asset file name, is_zip). None => this OS/arch has no official binary.
fn asset_for_target() -> Option<(&'static str, bool)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some(("grid-inference-worker-linux-x64", false)),
        ("linux", "aarch64") => Some(("grid-inference-worker-linux-arm64", false)),
        ("macos", "aarch64") => Some(("grid-inference-worker-macos-arm64.zip", true)),
        ("windows", "x86_64") => Some(("grid-inference-worker-windows-x64.exe", false)),
        _ => None,
    }
}

fn worker_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("worker")
}

fn worker_bin(app: &AppHandle) -> PathBuf {
    let name = if cfg!(windows) {
        "grid-inference-worker.exe"
    } else {
        "grid-inference-worker"
    };
    worker_dir(app).join(name)
}

async fn ollama_up() -> bool {
    match reqwest::Client::new()
        .get(format!("{OLLAMA}/api/version"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// Is the `ollama` command on PATH? Checked by trying to run it.
async fn ollama_installed() -> bool {
    Command::new("ollama")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Serialize)]
pub struct Prereqs {
    os: String,
    arch: String,
    supported: bool,
    worker_downloaded: bool,
    ollama_installed: bool,
    ollama_running: bool,
}

#[tauri::command]
pub async fn prereqs(app: AppHandle) -> Prereqs {
    let running = ollama_up().await;
    Prereqs {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        supported: asset_for_target().is_some(),
        worker_downloaded: worker_bin(&app).exists(),
        ollama_installed: running || ollama_installed().await,
        ollama_running: running,
    }
}

/// Download the official worker binary for this platform, verify its SHA-256
/// against the release manifest, unpack if needed, and make it executable.
#[tauri::command]
pub async fn download_worker(app: AppHandle) -> Result<String, String> {
    let (asset, is_zip) = asset_for_target()
        .ok_or("Dein Betriebssystem/Architektur wird vom offiziellen Worker (v0.3.12) nicht abgedeckt.")?;

    let base = format!("https://github.com/{REPO}/releases/download/{WORKER_TAG}");
    let client = reqwest::Client::builder()
        .user_agent("aipg-desktop")
        .build()
        .map_err(e)?;

    // 1. manifest with per-asset sha256 + size
    let manifest: serde_json::Value = client
        .get(format!("{base}/worker-release.json"))
        .send()
        .await
        .map_err(e)?
        .json()
        .await
        .map_err(e)?;
    let entry = manifest["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == asset))
        .ok_or("Asset nicht im Release-Manifest gefunden.")?;
    let want_sha = entry["sha256"].as_str().unwrap_or("").to_lowercase();
    let total = entry["bytes"].as_u64().unwrap_or(0);
    if want_sha.is_empty() {
        return Err("Kein SHA256 im Manifest — Abbruch aus Sicherheitsgründen.".into());
    }

    // 2. stream download, hashing as we go
    let resp = client.get(format!("{base}/{asset}")).send().await.map_err(e)?;
    if !resp.status().is_success() {
        return Err(format!("Download fehlgeschlagen: HTTP {}", resp.status()));
    }
    let mut hasher = Sha256::new();
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut stream = resp.bytes_stream();
    let mut received: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(e)?;
        hasher.update(&chunk);
        buf.extend_from_slice(&chunk);
        received += chunk.len() as u64;
        let _ = app.emit(
            "worker-dl",
            serde_json::json!({ "received": received, "total": total }),
        );
    }

    // 3. verify
    let got = hex::encode(hasher.finalize());
    if got != want_sha {
        return Err(format!(
            "SHA256 stimmt nicht überein (erwartet {}…, erhalten {}…) — Datei verworfen.",
            &want_sha[..12.min(want_sha.len())],
            &got[..12.min(got.len())]
        ));
    }

    // 4. place binary
    let dir = worker_dir(&app);
    std::fs::create_dir_all(&dir).map_err(e)?;
    let bin = worker_bin(&app);
    if is_zip {
        extract_binary_from_zip(&buf, &bin)?;
    } else {
        std::fs::write(&bin, &buf).map_err(e)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).map_err(e)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).map_err(e)?;
    }
    Ok(bin.to_string_lossy().into_owned())
}

/// Extract the single executable from a release .zip (largest regular file).
fn extract_binary_from_zip(bytes: &[u8], dest: &PathBuf) -> Result<(), String> {
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(e)?;
    let mut best: Option<(String, u64)> = None;
    for i in 0..zip.len() {
        let f = zip.by_index(i).map_err(e)?;
        if f.is_file() {
            let name = f.name().to_string();
            let size = f.size();
            if best.as_ref().map(|(_, s)| size > *s).unwrap_or(true) {
                best = Some((name, size));
            }
        }
    }
    let (name, _) = best.ok_or("Zip enthält keine Datei.")?;
    let mut f = zip.by_name(&name).map_err(e)?;
    let mut out = std::fs::File::create(dest).map_err(e)?;
    std::io::copy(&mut f, &mut out).map_err(e)?;
    Ok(())
}

/// Start `ollama serve` if it is not already answering, and wait until it is.
#[tauri::command]
pub async fn ollama_ensure(app: AppHandle, state: State<'_, Arc<Procs>>) -> Result<(), String> {
    if ollama_up().await {
        return Ok(());
    }
    if !ollama_installed().await {
        return Err("Ollama ist nicht installiert. Installiere es von https://ollama.com/download und starte neu.".into());
    }
    let child = Command::new("ollama")
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|x| format!("Konnte 'ollama serve' nicht starten: {x}"))?;
    *state.ollama.lock().await = Some(child);
    // poll up to ~20s
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if ollama_up().await {
            let _ = app.emit("ollama-ready", true);
            return Ok(());
        }
    }
    Err("Ollama gestartet, antwortet aber nicht auf Port 11434.".into())
}

/// Models already pulled into the local Ollama.
#[tauri::command]
pub async fn ollama_models() -> Result<Vec<String>, String> {
    let v: serde_json::Value = reqwest::Client::new()
        .get(format!("{OLLAMA}/api/tags"))
        .send()
        .await
        .map_err(e)?
        .json()
        .await
        .map_err(e)?;
    Ok(v["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

/// Pull a model, streaming progress to the UI via `ollama-pull` events.
#[tauri::command]
pub async fn ollama_pull(app: AppHandle, model: String) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .post(format!("{OLLAMA}/api/pull"))
        .json(&serde_json::json!({ "name": model, "stream": true }))
        .send()
        .await
        .map_err(e)?;
    let mut stream = resp.bytes_stream();
    let mut pending = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(e)?;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(nl) = pending.find('\n') {
            let line: String = pending.drain(..=nl).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(j) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(err) = j["error"].as_str() {
                    return Err(err.to_string());
                }
                let _ = app.emit("ollama-pull", j);
            }
        }
    }
    let _ = app.emit("ollama-pull", serde_json::json!({ "status": "success" }));
    Ok(())
}

#[derive(Deserialize)]
pub struct WorkerCfg {
    api_key: String,
    model: String,
    worker_name: String,
}

/// Spawn the worker binary headless and stream its stdout/stderr as `worker-log`.
#[tauri::command]
pub async fn worker_start(
    app: AppHandle,
    state: State<'_, Arc<Procs>>,
    cfg: WorkerCfg,
) -> Result<(), String> {
    if state.worker.lock().await.is_some() {
        return Err("Worker läuft bereits.".into());
    }
    let bin = worker_bin(&app);
    if !bin.exists() {
        return Err("Worker-Binary nicht gefunden — zuerst herunterladen.".into());
    }
    if cfg.api_key.trim().is_empty() || cfg.model.trim().is_empty() {
        return Err("API-Key und Modell sind erforderlich.".into());
    }
    let name = if cfg.worker_name.trim().is_empty() {
        "AIPG-Desktop-Worker".to_string()
    } else {
        cfg.worker_name.trim().to_string()
    };

    let mut cmd = Command::new(&bin);
    cmd.args(["--no-gui", "--no-browser"])
        .arg("--api-key")
        .arg(cfg.api_key.trim())
        .arg("--model")
        .arg(cfg.model.trim())
        .arg("--backend-url")
        .arg(OLLAMA)
        .arg("--worker-name")
        .arg(&name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|x| format!("Worker-Start fehlgeschlagen: {x}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *state.worker.lock().await = Some(child);

    if let Some(out) = stdout {
        pump(app.clone(), out, false);
    }
    if let Some(err) = stderr {
        pump(app.clone(), err, true);
    }
    let _ = app.emit("worker-log", "[desktop] worker gestartet.");
    Ok(())
}

/// Read a child stream line by line and forward to the UI; emit exit on EOF.
fn pump<R>(app: AppHandle, reader: R, is_err: bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = app.emit("worker-log", line);
        }
        // stdout EOF means the process ended
        if !is_err {
            let _ = app.emit("worker-exit", true);
        }
    });
}

#[tauri::command]
pub async fn worker_stop(state: State<'_, Arc<Procs>>) -> Result<(), String> {
    if let Some(mut c) = state.worker.lock().await.take() {
        let _ = c.start_kill();
    }
    Ok(())
}

#[tauri::command]
pub async fn worker_running(state: State<'_, Arc<Procs>>) -> Result<bool, String> {
    Ok(state.worker.lock().await.is_some())
}

/// Kill every child we started (called on app exit).
pub async fn kill_all(procs: &Procs) {
    if let Some(mut c) = procs.worker.lock().await.take() {
        let _ = c.start_kill();
    }
    if let Some(mut c) = procs.ollama.lock().await.take() {
        let _ = c.start_kill();
    }
}
