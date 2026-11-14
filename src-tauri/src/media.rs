//! Image and video worker: the official grid-media-worker bridge in front of
//! a ComfyUI the user already runs.
//!
//! The app does not install ComfyUI or model files. It
//! - checks which of the grid's priced media models the user's ComfyUI can
//!   serve (nodes and files of the grid's own recipes),
//! - sets the bridge up in the app's data folder: `uv` and the bridge source
//!   at a fixed commit, both checked against SHA-256 values fixed here, and
//!   the Python packages exactly as the bridge's lock file names them,
//! - runs the bridge with preflight on, so it only offers a model after a
//!   test render on this machine has worked.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::worker::{self, Arch, Procs};

const GRID: &str = "https://api.aipowergrid.io/v1";
/// grid-media-worker commit this app version runs, and the SHA-256 of
/// GitHub's source archive of it.
const BRIDGE_COMMIT: &str = "ea8e7c73bcef180e6aae055615876e26374a9cd5";
const BRIDGE_SHA: &str = "090da915cfc3121dc4343a595adc5a34b587b5b32bc080722ed3a84d24ccb3f9";
const UV_TAG: &str = "0.11.28";

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

/// uv release asset for this platform: name, SHA-256, archive kind.
fn uv_asset() -> Option<(&'static str, &'static str, Arch)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some(("uv-x86_64-pc-windows-msvc.zip", "0a23463216d09c6a72ff80ef5dc5a795f07dc1575cb84d24596c2f124a441b7b", Arch::Zip)),
        ("windows", "aarch64") => Some(("uv-aarch64-pc-windows-msvc.zip", "3248109afad3ec59baad299d324ff53de17e2d9a3b3e21580ffd26744b11e036", Arch::Zip)),
        ("linux", "x86_64") => Some(("uv-x86_64-unknown-linux-gnu.tar.gz", "e490a6464492183c5d4534a5527fb4440f7f2bb2f228162ad7e4afe076dc0224", Arch::Tgz)),
        ("linux", "aarch64") => Some(("uv-aarch64-unknown-linux-gnu.tar.gz", "03e9fe0a81b0718d0bc84625de3885df6cc3f89a8b6af6121d6b9f6113fb6533", Arch::Tgz)),
        ("macos", "aarch64") => Some(("uv-aarch64-apple-darwin.tar.gz", "33540eb7c883ab857eff79bd5ac2aa31fe27b595abecb4a9c003a2c998447232", Arch::Tgz)),
        ("macos", "x86_64") => Some(("uv-x86_64-apple-darwin.tar.gz", "2ad79983127ffca7d77b77ce6a24278d7e4f7b817a1acf72fea5f8124b4aac5e", Arch::Tgz)),
        _ => None,
    }
}

/* ------------------------- paths ------------------------- */

fn root(app: &AppHandle) -> PathBuf {
    worker::data_dir(app).join("media")
}
fn src_dir(app: &AppHandle) -> PathBuf {
    root(app).join("src").join(format!("grid-media-worker-{BRIDGE_COMMIT}"))
}
fn venv_dir(app: &AppHandle) -> PathBuf {
    root(app).join("venv")
}
fn marker(app: &AppHandle) -> PathBuf {
    root(app).join("installed")
}
fn bridge_bin(app: &AppHandle) -> PathBuf {
    if cfg!(windows) {
        venv_dir(app).join("Scripts").join("comfy-bridge.exe")
    } else {
        venv_dir(app).join("bin").join("comfy-bridge")
    }
}
/// The uv binary: at the top of the extracted archive on Windows, one folder
/// down on the other systems.
fn uv_bin(app: &AppHandle) -> Option<PathBuf> {
    let dir = root(app).join("uv");
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let direct = dir.join(name);
    if direct.exists() {
        return Some(direct);
    }
    std::fs::read_dir(&dir).ok()?.flatten().map(|d| d.path().join(name)).find(|p| p.exists())
}
fn installed(app: &AppHandle) -> bool {
    bridge_bin(app).exists()
        && std::fs::read_to_string(marker(app)).map(|s| s.trim() == BRIDGE_COMMIT).unwrap_or(false)
}

/* ------------------------- setup ------------------------- */

fn say(app: &AppHandle, text: &str) {
    let _ = app.emit("media-setup", text.to_string());
}

/// Download to `dest`, refusing anything whose SHA-256 is not `sha`.
async fn fetch_verified(app: &AppHandle, url: &str, sha: &str, dest: &PathBuf, what: &str) -> Result<(), String> {
    let client = reqwest::Client::builder().user_agent("aipg-desktop").build().map_err(e)?;
    let resp = client.get(url).send().await.map_err(|x| format!("{what}: download failed ({x})"))?;
    if !resp.status().is_success() {
        return Err(format!("{what}: download failed (HTTP {})", resp.status().as_u16()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(dest).await.map_err(e)?;
    let mut hasher = Sha256::new();
    let (mut received, mut last) = (0u64, 0u64);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(e)?;
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(e)?;
        received += chunk.len() as u64;
        if received - last > 4_000_000 {
            last = received;
            let mb = |n: u64| n / 1_000_000;
            say(app, &if total > 0 { format!("Downloading {what}… {} / {} MB", mb(received), mb(total)) } else { format!("Downloading {what}… {} MB", mb(received)) });
        }
    }
    file.flush().await.map_err(e)?;
    drop(file);
    if hex::encode(hasher.finalize()) != sha {
        let _ = std::fs::remove_file(dest);
        return Err(format!("{what}: the download does not match the checksum fixed in this app version. It was discarded."));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct MediaStatus {
    supported: bool,
    installed: bool,
    running: bool,
    commit: String,
}

#[tauri::command]
pub async fn media_status(app: AppHandle, state: State<'_, Arc<Procs>>) -> Result<MediaStatus, String> {
    Ok(MediaStatus {
        supported: uv_asset().is_some(),
        installed: installed(&app),
        running: state.media.lock().await.is_some(),
        commit: BRIDGE_COMMIT[..7].to_string(),
    })
}

/// Download uv and the bridge source, then let uv install Python and the
/// bridge's packages as its lock file names them. Everything stays in the
/// app's data folder.
#[tauri::command]
pub async fn media_setup(app: AppHandle, state: State<'_, Arc<Procs>>) -> Result<(), String> {
    if state.media.lock().await.is_some() {
        return Err("Stop the media worker first.".into());
    }
    let (asset, uv_sha, kind) = uv_asset().ok_or("The media worker setup is not available for this system.")?;
    let base = root(&app);
    std::fs::create_dir_all(&base).map_err(e)?;
    let _ = std::fs::remove_file(marker(&app));

    if uv_bin(&app).is_none() {
        let tmp = base.join("uv.part");
        fetch_verified(&app, &format!("https://github.com/astral-sh/uv/releases/download/{UV_TAG}/{asset}"), uv_sha, &tmp, "the Python installer (uv)").await?;
        let (tmp2, dist) = (tmp.clone(), base.join("uv"));
        tokio::task::spawn_blocking(move || worker::extract_archive(&tmp2, &dist, kind)).await.map_err(e)??;
        let _ = std::fs::remove_file(&tmp);
    }
    let uv = uv_bin(&app).ok_or("The Python installer was unpacked, but its program was not found.")?;
    worker::make_executable(&uv)?;

    if !src_dir(&app).join("uv.lock").exists() {
        let tmp = base.join("src.part");
        fetch_verified(&app, &format!("https://codeload.github.com/AIPowerGrid/grid-media-worker/tar.gz/{BRIDGE_COMMIT}"), BRIDGE_SHA, &tmp, "the media worker").await?;
        let (tmp2, dist) = (tmp.clone(), base.join("src"));
        tokio::task::spawn_blocking(move || worker::extract_archive(&tmp2, &dist, Arch::Tgz)).await.map_err(e)??;
        let _ = std::fs::remove_file(&tmp);
    }
    let src = src_dir(&app);
    if !src.join("uv.lock").exists() {
        return Err("The media worker source was unpacked, but looks incomplete.".into());
    }

    say(&app, "Installing Python and the worker's packages (about 300 MB, one time)…");
    let mut cmd = Command::new(&uv);
    cmd.args(["sync", "--frozen", "--no-dev", "--python", "3.12"])
        .current_dir(&src)
        .env("UV_CACHE_DIR", base.join("uvcache"))
        .env("UV_PYTHON_INSTALL_DIR", base.join("python"))
        .env("UV_PROJECT_ENVIRONMENT", venv_dir(&app))
        .env("UV_PYTHON_PREFERENCE", "only-managed")
        .env("UV_NO_CONFIG", "1")
        .env("UV_NO_PROGRESS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    worker::quiet(&mut cmd);
    let mut child = cmd.spawn().map_err(|x| format!("The Python installer did not start: {x}"))?;
    let mut tail: Vec<String> = Vec::new();
    if let Some(err) = child.stderr.take() {
        let mut lines = BufReader::new(err).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            say(&app, &line.chars().take(160).collect::<String>());
            tail.push(line);
            if tail.len() > 6 {
                tail.remove(0);
            }
        }
    }
    let status = child.wait().await.map_err(e)?;
    if !status.success() || !bridge_bin(&app).exists() {
        return Err(format!("Installing the packages failed. Last lines: {}", tail.join(" | ")));
    }
    std::fs::write(marker(&app), BRIDGE_COMMIT).map_err(e)?;
    say(&app, "Media worker set up.");
    Ok(())
}

/* ------------------------- which models fit this ComfyUI ------------------------- */

fn comfy_base(url: &str) -> Result<String, String> {
    let u = url.trim().trim_end_matches('/');
    let rest = u
        .strip_prefix("http://")
        .or_else(|| u.strip_prefix("https://"))
        .ok_or("The ComfyUI address must start with http:// or https://")?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.contains('@') || u.chars().any(|c| c.is_whitespace()) {
        return Err("That is not a usable ComfyUI address.".into());
    }
    Ok(u.to_string())
}

#[derive(Serialize)]
pub struct MediaModel {
    /// the name the grid sends jobs under, when a worker serves it right now;
    /// otherwise the price-list name
    name: String,
    kind: String,
    price_usd: f64,
    unit: String,
    workers_online: u64,
    ready: bool,
    missing_nodes: Vec<String>,
    missing_files: Vec<String>,
}

#[derive(Serialize)]
pub struct ComfyReport {
    version: String,
    gpu: String,
    vram_gb: f64,
    models: Vec<MediaModel>,
}

/// Ask ComfyUI what it has, ask the grid what each priced media model needs,
/// and compare. The same two checks the bridge makes before its test render.
#[tauri::command]
pub async fn comfy_check(url: String, api_key: String) -> Result<ComfyReport, String> {
    let comfy = comfy_base(&url)?;
    if api_key.trim().is_empty() {
        return Err("An API key is needed to read the grid's recipes.".into());
    }
    let http = reqwest::Client::builder()
        .user_agent("aipg-desktop")
        .timeout(std::time::Duration::from_secs(40))
        .build()
        .map_err(e)?;
    let stats: serde_json::Value = http
        .get(format!("{comfy}/system_stats"))
        .send()
        .await
        .map_err(|_| format!("ComfyUI does not answer at {comfy}. Start it, or correct the address."))?
        .json()
        .await
        .map_err(|_| format!("Something answers at {comfy}, but it does not look like ComfyUI."))?;
    let info: serde_json::Value = http
        .get(format!("{comfy}/object_info"))
        .send()
        .await
        .map_err(|x| format!("ComfyUI did not list its nodes: {x}"))?
        .json()
        .await
        .map_err(|_| "ComfyUI's node list could not be read.".to_string())?;
    let nodes: HashSet<&str> = info.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    // every file name a loader node offers
    let mut files: HashSet<&str> = HashSet::new();
    if let Some(obj) = info.as_object() {
        for spec in obj.values() {
            for group in ["required", "optional"] {
                let Some(inputs) = spec["input"][group].as_object() else { continue };
                for opt in inputs.values() {
                    if let Some(list) = opt.get(0).and_then(|x| x.as_array()) {
                        files.extend(list.iter().filter_map(|x| x.as_str()));
                    }
                }
            }
        }
    }

    let pricing: serde_json::Value = http.get(format!("{GRID}/pricing")).send().await.map_err(e)?.json().await.map_err(e)?;
    let online: serde_json::Value = http.get(format!("{GRID}/status/models")).send().await.map_err(e)?.json().await.unwrap_or(serde_json::Value::Null);
    let mut models = Vec::new();
    for m in pricing["price_book"]["models"].as_array().cloned().unwrap_or_default() {
        let Some(key) = m["model"].as_str() else { continue };
        let rates = &m["rates"];
        let (kind, price, unit) = if let Some(p) = rates["per_image_usd"].as_f64() {
            ("image", p, "per image")
        } else if let Some(p) = rates["per_video_second_usd"].as_f64() {
            ("video", p, "per second of video")
        } else if let Some(p) = rates["per_3d_generation_usd"].as_f64() {
            ("3d", p, "per 3D model")
        } else {
            continue;
        };
        let live = online.as_array().and_then(|a| a.iter().find(|x| x["name"].as_str().map(|n| n.eq_ignore_ascii_case(key)).unwrap_or(false)));
        let name = live.and_then(|x| x["name"].as_str()).unwrap_or(key).to_string();
        let workers_online = live.and_then(|x| x["count"].as_u64()).unwrap_or(0);
        let r = http.get(format!("{GRID}/models/{}/recipe", urlencode(&name))).header("apikey", api_key.trim()).send().await;
        let recipes = match r {
            Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok().and_then(|v| v["recipes"].as_array().cloned()).unwrap_or_default(),
            _ => Vec::new(),
        };
        if recipes.is_empty() {
            continue; // nothing a ComfyUI worker could serve
        }
        // all variants (text-to-image, image-to-image, edit): a job for any of them can arrive
        let mut missing_nodes: Vec<String> = Vec::new();
        let mut missing_files: Vec<String> = Vec::new();
        for rec in &recipes {
            for n in rec["node_types"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                if !nodes.contains(n) && !missing_nodes.iter().any(|x| x == n) {
                    missing_nodes.push(n.to_string());
                }
            }
            for f in rec["model_files"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                if !files.contains(f) && !missing_files.iter().any(|x| x == f) {
                    missing_files.push(f.to_string());
                }
            }
        }
        models.push(MediaModel {
            name,
            kind: kind.into(),
            price_usd: price,
            unit: unit.into(),
            workers_online,
            ready: missing_nodes.is_empty() && missing_files.is_empty(),
            missing_nodes,
            missing_files,
        });
    }
    let dev = &stats["devices"][0];
    Ok(ComfyReport {
        version: stats["system"]["comfyui_version"].as_str().unwrap_or("").to_string(),
        gpu: dev["name"].as_str().unwrap_or("").to_string(),
        vram_gb: (dev["vram_total"].as_f64().unwrap_or(0.0) / 1_073_741_824.0 * 10.0).round() / 10.0,
        models,
    })
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/* ------------------------- run ------------------------- */

#[derive(Deserialize)]
pub struct MediaCfg {
    api_key: String,
    worker_name: String,
    comfy_url: String,
    models: Vec<String>,
}

fn emit_log(app: &AppHandle, line: &str) {
    let _ = app.emit("media-log", worker::redact(line));
}

#[tauri::command]
pub async fn media_start(app: AppHandle, state: State<'_, Arc<Procs>>, cfg: MediaCfg) -> Result<(), String> {
    if state.media.lock().await.is_some() {
        return Err("The media worker is already running.".into());
    }
    if !installed(&app) {
        return Err("Set the media worker up first.".into());
    }
    let comfy = comfy_base(&cfg.comfy_url)?;
    let models: Vec<&str> = cfg.models.iter().map(|m| m.trim()).filter(|m| !m.is_empty() && !m.contains(',')).collect();
    if cfg.api_key.trim().is_empty() || models.is_empty() {
        return Err("An API key and at least one model are needed.".into());
    }
    let name = if cfg.worker_name.trim().is_empty() { "AIPG-Desktop-Media" } else { cfg.worker_name.trim() };
    let port = std::net::TcpListener::bind("127.0.0.1:0").ok().and_then(|l| l.local_addr().ok()).map(|a| a.port()).unwrap_or(7860);

    let mut cmd = Command::new(bridge_bin(&app));
    // The key goes in through the environment, not the command line.
    cmd.current_dir(src_dir(&app))
        .env("GRID_API_KEY", cfg.api_key.trim())
        .env("GRID_WORKER_NAME", name)
        .env("GRID_API_URL", "https://api.aipowergrid.io")
        .env("COMFYUI_URL", &comfy)
        .env("GRID_MODEL", models.join(","))
        // offer a model only after a test render on this machine has worked
        .env("GRID_PREFLIGHT", "true")
        .env("GRID_TRUST_MODELS", "false")
        .env("GRID_THREADS", "1")
        .env("GRID_NSFW", "false")
        .env("BRIDGE_HOST", "127.0.0.1")
        .env("BRIDGE_PORT", port.to_string())
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(schedule) = worker::media_schedule(&app) {
        cmd.env("GRID_SCHEDULE", schedule);
    }
    worker::quiet(&mut cmd);
    worker::remember_key(cfg.api_key.trim());
    let mut child = cmd.spawn().map_err(|x| format!("The media worker did not start: {x}"))?;
    crate::job::adopt(&child);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let my_gen = state.media_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    *state.media.lock().await = Some(child);
    emit_log(&app, &format!("[desktop] media worker started (bridge {}) · {} · ComfyUI {comfy}", &BRIDGE_COMMIT[..7], models.join(", ")));

    if let Some(out) = stdout {
        let app2 = app.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                emit_log(&app2, &line);
            }
        });
    }
    if let Some(err) = stderr {
        // the bridge logs to stderr; when that closes, the process has ended
        let app2 = app.clone();
        let procs = state.inner().clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                emit_log(&app2, &line);
            }
            if procs.media_gen.load(std::sync::atomic::Ordering::SeqCst) == my_gen {
                if let Some(mut c) = procs.media.lock().await.take() {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), c.wait()).await;
                    emit_log(&app2, "[desktop] media worker process ended on its own.");
                }
                let _ = app2.emit("media-exit", true);
            }
        });
    }
    Ok(())
}

/// End the bridge and, on Windows, the Python process its launcher started.
pub async fn stop(procs: &Procs) {
    if let Some(mut c) = procs.media.lock().await.take() {
        #[cfg(windows)]
        {
            if let Some(pid) = c.id() {
                let mut k = Command::new("taskkill");
                k.args(["/PID", &pid.to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null());
                worker::quiet(&mut k);
                let _ = k.status().await;
            }
        }
        let _ = c.start_kill();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), c.wait()).await;
    }
}

#[tauri::command]
pub async fn media_stop(app: AppHandle, state: State<'_, Arc<Procs>>) -> Result<(), String> {
    state.media_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    stop(state.inner()).await;
    emit_log(&app, "[desktop] media worker stopped by the app.");
    Ok(())
}
