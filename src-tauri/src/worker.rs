//! All-in-one orchestration:
//! - verified download of the official grid-inference-worker binary
//! - managed, sandboxed Ollama (download + run, no system install)
//! - hardware detection + model recommendation
//! - worker process supervision with log streaming

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const WORKER_TAG: &str = "v0.3.13";
const WORKER_REPO: &str = "AIPowerGrid/grid-text-worker";
/// Port of an Ollama the user installed themselves.
const OLLAMA_SYSTEM_PORT: u16 = 11434;
/// The app's own Ollama listens here, so it never shares a port, a model
/// store or graphics memory bookkeeping with the user's Ollama.
const OLLAMA_MANAGED_PORT: u16 = 11511;
/// Set only when the user explicitly chose to use their own Ollama.
static USE_SYSTEM_OLLAMA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The API key of the running worker, so it can be blanked in log lines.
static REDACT_KEY: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

fn use_system_ollama() -> bool {
    USE_SYSTEM_OLLAMA.load(std::sync::atomic::Ordering::SeqCst)
}
fn ollama_url() -> String {
    let port = if use_system_ollama() { OLLAMA_SYSTEM_PORT } else { OLLAMA_MANAGED_PORT };
    format!("http://127.0.0.1:{port}")
}
const OLLAMA_TAG: &str = "v0.40.15";

#[derive(Default)]
pub struct Procs {
    worker: Mutex<Option<Child>>,
    ollama: Mutex<Option<Child>>,
    /// Bumped on every worker start, so the log reader of an older worker
    /// can never clear (and thereby kill) a newer one.
    worker_gen: std::sync::atomic::AtomicU64,
}

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

/// Prevent a console window from flashing up on Windows for any spawned child
/// process (worker, managed Ollama, hardware-detection probes like powershell).
/// No-op on other platforms.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/* ------------------------- paths ------------------------- */

pub(crate) fn data_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
}
fn worker_dir(app: &AppHandle) -> PathBuf {
    data_dir(app).join("worker")
}
fn worker_bin(app: &AppHandle) -> PathBuf {
    let name = if cfg!(windows) {
        "grid-inference-worker.exe"
    } else {
        "grid-inference-worker"
    };
    worker_dir(app).join(name)
}
/// Records which release the downloaded worker binary is, so an app update
/// that pins a newer worker fetches it instead of keeping the old file.
fn worker_version_file(app: &AppHandle) -> PathBuf {
    worker_dir(app).join("version.txt")
}
fn worker_current(app: &AppHandle) -> bool {
    worker_bin(app).exists()
        && std::fs::read_to_string(worker_version_file(app))
            .map(|s| s.trim() == WORKER_TAG)
            .unwrap_or(false)
}
fn ollama_root(app: &AppHandle) -> PathBuf {
    data_dir(app).join("ollama")
}
fn ollama_dist(app: &AppHandle) -> PathBuf {
    ollama_root(app).join("dist")
}
/// File holding an optional user-chosen models directory (one line, a path).
fn models_override_file(app: &AppHandle) -> PathBuf {
    data_dir(app).join("models_dir.txt")
}
fn ollama_models_dir(app: &AppHandle) -> PathBuf {
    if let Ok(s) = std::fs::read_to_string(models_override_file(app)) {
        let s = s.trim();
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    ollama_root(app).join("models")
}
/// First existing managed ollama executable, if any.
fn managed_ollama_bin(app: &AppHandle) -> Option<PathBuf> {
    let d = ollama_dist(app);
    let names: &[&str] = if cfg!(windows) {
        &["ollama.exe", "bin/ollama.exe"]
    } else {
        &["ollama", "bin/ollama"]
    };
    names.iter().map(|n| d.join(n)).find(|p| p.exists())
}

/* ------------------------- worker binary ------------------------- */

fn worker_asset() -> Option<(&'static str, bool)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some(("grid-inference-worker-linux-x64", false)),
        ("linux", "aarch64") => Some(("grid-inference-worker-linux-arm64", false)),
        ("macos", "aarch64") => Some(("grid-inference-worker-macos-arm64.zip", true)),
        ("windows", "x86_64") => Some(("grid-inference-worker-windows-x64.exe", false)),
        _ => None,
    }
}

async fn ollama_up() -> bool {
    matches!(
        reqwest::Client::new()
            .get(format!("{}/api/version", ollama_url()))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await,
        Ok(r) if r.status().is_success()
    )
}

async fn system_ollama() -> bool {
    quiet(&mut Command::new("ollama"))
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
    /// the worker release this app version pins
    worker_tag: String,
    ollama_running: bool,
    ollama_installed: bool,
    ollama_managed: bool,
}

#[tauri::command]
pub async fn prereqs(app: AppHandle) -> Prereqs {
    let running = ollama_up().await;
    let managed = managed_ollama_bin(&app).is_some();
    Prereqs {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        supported: worker_asset().is_some(),
        worker_downloaded: worker_current(&app),
        worker_tag: WORKER_TAG.into(),
        ollama_running: running,
        ollama_installed: running || managed || system_ollama().await,
        ollama_managed: managed,
    }
}

/// SHA-256 of the worker release assets, fixed here next to WORKER_TAG. A
/// tampered or replaced GitHub release therefore cannot supply its own
/// checksums: bumping the worker means changing these lines in a commit.
fn worker_sha(asset: &str) -> Option<&'static str> {
    match asset {
        "grid-inference-worker-linux-x64" => Some("48f2c9ee598f9947cb2193be81eaa0753dc7e3a90c83b24890fa9a1a1f5bd430"),
        "grid-inference-worker-linux-arm64" => Some("20aedf5fe4760ea2630293acd75ca3ba2d2775fe902e4b886eaa9021b1be0c55"),
        "grid-inference-worker-macos-arm64.zip" => Some("980b18d57fe1668a2bd58e4588f24ff62e3e6443246de7afa617c43198797c6b"),
        "grid-inference-worker-windows-x64.exe" => Some("7c2c262e9f08c6076f91a215039457ce436fabfbcbcc4c3beb8cc38320587d47"),
        _ => None,
    }
}

fn file_sha256(path: &std::path::Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher).ok()?;
    Some(hex::encode(hasher.finalize()))
}

#[tauri::command]
pub async fn download_worker(app: AppHandle) -> Result<String, String> {
    let (asset, is_zip) = worker_asset()
        .ok_or(format!("No official worker binary ({WORKER_TAG}) exists for this OS/architecture."))?;
    let want_sha = worker_sha(asset).ok_or("No pinned checksum for this worker asset — aborting for safety.")?;
    let dir = worker_dir(&app);
    std::fs::create_dir_all(&dir).map_err(e)?;
    let bin = worker_bin(&app);

    // Already the pinned file (e.g. marker missing after an older app version)?
    if !is_zip && bin.exists() {
        let b = bin.clone();
        let have = tokio::task::spawn_blocking(move || file_sha256(&b)).await.map_err(e)?;
        if have.as_deref() == Some(want_sha) {
            let _ = std::fs::write(worker_version_file(&app), WORKER_TAG);
            return Ok(bin.to_string_lossy().into_owned());
        }
    }

    let base = format!("https://github.com/{WORKER_REPO}/releases/download/{WORKER_TAG}");
    let client = reqwest::Client::builder()
        .user_agent("aipg-desktop")
        .build()
        .map_err(e)?;
    let resp = client.get(format!("{base}/{asset}")).send().await.map_err(e)?;
    if !resp.status().is_success() {
        return Err(format!("Download failed: HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut hasher = Sha256::new();
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut stream = resp.bytes_stream();
    let mut received: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(e)?;
        hasher.update(&chunk);
        buf.extend_from_slice(&chunk);
        received += chunk.len() as u64;
        let _ = app.emit("worker-dl", serde_json::json!({ "received": received, "total": total }));
    }
    let got = hex::encode(hasher.finalize());
    if got != want_sha {
        return Err(format!(
            "SHA-256 mismatch (expected {}…, got {}…) — discarded.",
            &want_sha[..12],
            &got[..12.min(got.len())]
        ));
    }

    // Write next to the target and swap it in, so a failed or interrupted
    // write never leaves a half-written executable behind.
    let tmp = dir.join("worker.download");
    if is_zip {
        extract_single_from_zip(&buf, &tmp)?;
    } else {
        std::fs::write(&tmp, &buf).map_err(e)?;
    }
    make_executable(&tmp)?;
    if let Err(err) = std::fs::rename(&tmp, &bin) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "The worker file could not be replaced ({err}). It is probably still running: stop the worker and try again."
        ));
    }
    let _ = std::fs::write(worker_version_file(&app), WORKER_TAG);
    Ok(bin.to_string_lossy().into_owned())
}

fn extract_single_from_zip(bytes: &[u8], dest: &PathBuf) -> Result<(), String> {
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(e)?;
    let mut best: Option<(String, u64)> = None;
    for i in 0..zip.len() {
        let f = zip.by_index(i).map_err(e)?;
        if f.is_file() && best.as_ref().map(|(_, s)| f.size() > *s).unwrap_or(true) {
            best = Some((f.name().to_string(), f.size()));
        }
    }
    let (name, _) = best.ok_or("Zip archive contains no file.")?;
    let mut f = zip.by_name(&name).map_err(e)?;
    let mut out = std::fs::File::create(dest).map_err(e)?;
    std::io::copy(&mut f, &mut out).map_err(e)?;
    Ok(())
}

fn make_executable(_p: &PathBuf) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(_p).map_err(e)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(_p, perms).map_err(e)?;
    }
    Ok(())
}

/* ------------------------- managed Ollama ------------------------- */

#[derive(Clone, Copy)]
enum Arch {
    Tgz,
    Zst,
    Zip,
}

fn ollama_sha(asset: &str) -> Option<&'static str> {
    match asset {
        "ollama-darwin.tgz" => Some("607c79d5d6315ef9d896db83adde736a4c0057196329843544e79b95afb260a8"),
        "ollama-linux-amd64.tar.zst" => Some("628c018404a4206493efd38e46251c154ede5141b8b8dc85c659b3b027c47379"),
        "ollama-linux-arm64.tar.zst" => Some("7f699ead5cdb4ae1ed89b888b148a238af188708800871e1889ab9dcee95f247"),
        "ollama-windows-amd64.zip" => Some("9ca74a7ab66ec86870b1eecac7df160df08dc4e3a291ab5d2d59bcf9310221c6"),
        "ollama-windows-arm64.zip" => Some("35ce5195b57496dbc9ecbaa6593807e6ac26cbb53cf1f8bb57ac58daafa58574"),
        _ => None,
    }
}

fn ollama_asset() -> Option<(&'static str, Arch)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => Some(("ollama-darwin.tgz", Arch::Tgz)),
        ("linux", "x86_64") => Some(("ollama-linux-amd64.tar.zst", Arch::Zst)),
        ("linux", "aarch64") => Some(("ollama-linux-arm64.tar.zst", Arch::Zst)),
        ("windows", "x86_64") => Some(("ollama-windows-amd64.zip", Arch::Zip)),
        ("windows", "aarch64") => Some(("ollama-windows-arm64.zip", Arch::Zip)),
        _ => None,
    }
}

/// Download the official Ollama for this platform (verified), unpack it into the
/// app's data dir, and make it runnable — no system install, no admin rights.
#[tauri::command]
pub async fn setup_ollama(app: AppHandle) -> Result<String, String> {
    if managed_ollama_bin(&app).is_some() {
        return Ok("already".into());
    }
    let (asset, kind) = ollama_asset().ok_or("Ollama is not available for this OS/architecture.")?;
    let base = format!("https://github.com/ollama/ollama/releases/download/{OLLAMA_TAG}");
    let client = reqwest::Client::builder()
        .user_agent("aipg-desktop")
        .build()
        .map_err(e)?;

    // expected SHA-256, fixed here next to OLLAMA_TAG (not read from the release)
    let want_sha = ollama_sha(asset)
        .ok_or("No pinned checksum for this Ollama asset — aborting for safety.")?
        .to_string();

    let root = ollama_root(&app);
    std::fs::create_dir_all(&root).map_err(e)?;
    let tmp = root.join("download.part");

    // stream to disk (archives are up to ~1.5 GB — never buffer in RAM)
    let resp = client.get(format!("{base}/{asset}")).send().await.map_err(e)?;
    if !resp.status().is_success() {
        return Err(format!("Ollama download failed: HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(&tmp).await.map_err(e)?;
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut last_emit: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(e)?;
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(e)?;
        received += chunk.len() as u64;
        if received - last_emit > 2_000_000 {
            last_emit = received;
            let _ = app.emit("ollama-dl", serde_json::json!({ "received": received, "total": total }));
        }
    }
    file.flush().await.map_err(e)?;
    drop(file);
    let _ = app.emit("ollama-dl", serde_json::json!({ "received": received, "total": total }));

    let got = hex::encode(hasher.finalize());
    if got != want_sha {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "Ollama SHA-256 mismatch (expected {}…) — discarded.",
            &want_sha[..12.min(want_sha.len())]
        ));
    }

    // extract on a blocking thread
    let dist = ollama_dist(&app);
    let tmp2 = tmp.clone();
    let _ = app.emit("ollama-dl", serde_json::json!({ "status": "extracting" }));
    tokio::task::spawn_blocking(move || extract_archive(&tmp2, &dist, kind))
        .await
        .map_err(e)??;
    let _ = std::fs::remove_file(&tmp);

    let bin = managed_ollama_bin(&app)
        .ok_or("Extracted, but no ollama binary was found.")?;
    make_executable(&bin)?;
    Ok(bin.to_string_lossy().into_owned())
}

fn extract_archive(archive: &PathBuf, dist: &PathBuf, kind: Arch) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dist);
    std::fs::create_dir_all(dist).map_err(e)?;
    let f = std::fs::File::open(archive).map_err(e)?;
    match kind {
        Arch::Tgz => {
            let gz = flate2::read::GzDecoder::new(f);
            tar::Archive::new(gz).unpack(dist).map_err(e)?;
        }
        Arch::Zst => {
            let zd = zstd::stream::read::Decoder::new(f).map_err(e)?;
            tar::Archive::new(zd).unpack(dist).map_err(e)?;
        }
        Arch::Zip => {
            let mut zip = zip::ZipArchive::new(f).map_err(e)?;
            zip.extract(dist).map_err(e)?;
        }
    }
    Ok(())
}

/// Start Ollama if it is not already answering (managed binary preferred).
#[tauri::command]
pub async fn ollama_ensure(app: AppHandle, state: State<'_, Arc<Procs>>) -> Result<(), String> {
    if ollama_up().await {
        return Ok(());
    }
    if use_system_ollama() {
        return Err("Your own Ollama does not answer on port 11434. Start it, or switch back to the app's Ollama under “Run a worker”.".into());
    }
    let bin = if let Some(b) = managed_ollama_bin(&app) {
        b
    } else if system_ollama().await {
        PathBuf::from("ollama")
    } else {
        return Err("Ollama is not set up. Click “Set up Ollama” (downloads it automatically).".into());
    };

    let models = ollama_models_dir(&app);
    std::fs::create_dir_all(&models).map_err(e)?;
    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .env("OLLAMA_HOST", format!("127.0.0.1:{OLLAMA_MANAGED_PORT}"))
        .env("OLLAMA_MODELS", &models)
        // the worker advertises this same value to the grid (see worker_start)
        .env("OLLAMA_CONTEXT_LENGTH", ollama_ctx(&app).to_string())
        // keep the model in memory between jobs, so a job after a quiet
        // spell does not start with a slow reload
        .env("OLLAMA_KEEP_ALIVE", keep_alive_env(ollama_keep_alive(&app)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "linux")]
    {
        let lib = ollama_dist(&app).join("lib/ollama");
        if lib.exists() {
            let prev = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
            cmd.env("LD_LIBRARY_PATH", format!("{}:{}", lib.display(), prev));
        }
    }
    quiet(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|x| format!("Could not start Ollama: {x}"))?;
    crate::job::adopt(&child);
    *state.ollama.lock().await = Some(child);

    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if ollama_up().await {
            let _ = app.emit("ollama-ready", true);
            return Ok(());
        }
    }
    Err(format!("Ollama was started but does not answer on port {OLLAMA_MANAGED_PORT}."))
}

#[tauri::command]
pub async fn ollama_models() -> Result<Vec<String>, String> {
    let v: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/api/tags", ollama_url()))
        .send()
        .await
        .map_err(e)?
        .json()
        .await
        .map_err(e)?;
    Ok(v["models"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect())
        .unwrap_or_default())
}

#[derive(Serialize)]
pub struct LocalModel {
    name: String,
    size: u64,
}

/// Installed local models with their on-disk size.
#[tauri::command]
pub async fn ollama_list() -> Result<Vec<LocalModel>, String> {
    let v: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/api/tags", ollama_url()))
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
                .filter_map(|m| {
                    Some(LocalModel {
                        name: m["name"].as_str()?.to_string(),
                        size: m["size"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

#[tauri::command]
pub async fn ollama_delete(model: String) -> Result<(), String> {
    let r = reqwest::Client::new()
        .delete(format!("{}/api/delete", ollama_url()))
        .json(&serde_json::json!({ "name": model, "model": model }))
        .send()
        .await
        .map_err(e)?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(format!("Could not delete model: HTTP {}", r.status()))
    }
}

#[tauri::command]
pub async fn ollama_pull(app: AppHandle, model: String) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/pull", ollama_url()))
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

/* ------------------------- hardware ------------------------- */

#[derive(Serialize)]
pub struct Hardware {
    ram_gb: f64,
    cpu: String,
    gpu: String,
    can_run_worker: bool,
    recommend: String,
    note: String,
    /// memory (GB) a model may use: VRAM on a real GPU, otherwise RAM
    budget_gb: f64,
    /// memory of the GPU a model can be loaded into (0 = no usable GPU);
    /// on Apple Silicon a share of the unified memory
    vram_gb: f64,
}

#[tauri::command]
pub async fn hardware() -> Hardware {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let ram_gb = sys.total_memory() as f64 / 1_073_741_824.0;
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::consts::ARCH.to_string());

    // GPU: NVIDIA via nvidia-smi, Apple Silicon = Metal
    let (gpu, vram_gb) = detect_gpu().await;

    // recommendation by the larger of VRAM (if a real GPU) or RAM
    let budget = if vram_gb >= 4.0 { vram_gb.max(ram_gb * 0.6) } else { ram_gb };
    let (recommend, can_run_worker, note) = if budget < 3.0 {
        (
            String::new(),
            false,
            "Not enough memory to run a local worker. Use Chat with “Grid / Auto” instead — inference then runs on the grid, not on this machine.".into(),
        )
    } else if budget < 6.0 {
        ("llama3.2:1b".into(), true, "Small model for tight memory.".into())
    } else if budget < 10.0 {
        ("llama3.2:3b".into(), true, "Solid small model.".into())
    } else if budget < 20.0 {
        ("qwen3:8b".into(), true, "Good all-rounder.".into())
    } else {
        ("qwen3:14b".into(), true, "Plenty of memory — a larger model is possible.".into())
    };

    Hardware {
        ram_gb: (ram_gb * 10.0).round() / 10.0,
        cpu,
        gpu,
        can_run_worker,
        recommend,
        note,
        budget_gb: (budget * 10.0).round() / 10.0,
        vram_gb: {
            let v = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                ram_gb * 0.7
            } else if vram_gb >= 4.0 {
                vram_gb
            } else {
                0.0
            };
            (v * 10.0).round() / 10.0
        },
    }
}

async fn detect_gpu() -> (String, f64) {
    if let Some(x) = gpu_nvidia().await {
        return x;
    }
    match std::env::consts::OS {
        "macos" => gpu_macos().await,
        "windows" => gpu_windows().await,
        "linux" => gpu_linux().await,
        _ => ("none detected (CPU)".into(), 0.0),
    }
}

async fn gpu_nvidia() -> Option<(String, f64)> {
    let out = quiet(&mut Command::new("nvidia-smi"))
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next()?;
    let parts: Vec<&str> = line.split(',').map(|x| x.trim()).collect();
    if parts.len() >= 2 {
        let gb = parts[1].parse::<f64>().unwrap_or(0.0) / 1024.0;
        return Some((format!("{} ({:.0} GB)", parts[0], gb), gb));
    }
    None
}

async fn gpu_macos() -> (String, f64) {
    if let Ok(out) = quiet(&mut Command::new("system_profiler"))
        .arg("SPDisplaysDataType")
        .output()
        .await
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let name = s
            .lines()
            .find_map(|l| l.trim().strip_prefix("Chipset Model:"))
            .map(|x| x.trim().to_string());
        let gb = s
            .lines()
            .find_map(|l| {
                let t = l.trim();
                t.strip_prefix("VRAM (Total):")
                    .or_else(|| t.strip_prefix("VRAM (Dynamic, Max):"))
            })
            .map(|x| parse_mem_gb(x.trim()))
            .unwrap_or(0.0);
        if let Some(n) = name {
            let label = if std::env::consts::ARCH == "aarch64" {
                format!("{n} (Metal)")
            } else if gb > 0.0 {
                format!("{n} ({gb:.0} GB)")
            } else {
                n
            };
            return (label, gb);
        }
    }
    if std::env::consts::ARCH == "aarch64" {
        return ("Apple Silicon (Metal)".into(), 0.0);
    }
    ("none detected (CPU)".into(), 0.0)
}

async fn gpu_windows() -> (String, f64) {
    // Preferred: the display-adapter class in the registry carries the real
    // 64-bit VRAM size (Win32_VideoController.AdapterRAM is capped at 4 GB).
    const REG: &str = r#"Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0*' -ErrorAction SilentlyContinue | ForEach-Object { '{0}|{1}' -f $_.DriverDesc, $_.'HardwareInformation.qwMemorySize' }"#;
    if let Ok(out) = quiet(&mut Command::new("powershell"))
        .args(["-NoProfile", "-Command", REG])
        .output()
        .await
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let mut labels: Vec<String> = Vec::new();
        let mut best = 0.0_f64;
        for line in s.lines() {
            let mut it = line.trim().splitn(2, '|');
            let name = it.next().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            let gb = it
                .next()
                .and_then(|x| x.trim().parse::<f64>().ok())
                .map(|b| b / 1_073_741_824.0)
                .unwrap_or(0.0);
            let label = if gb >= 1.0 {
                format!("{name} ({gb:.0} GB)")
            } else {
                name.to_string()
            };
            if !labels.contains(&label) {
                labels.push(label);
            }
            if gb > best {
                best = gb;
            }
        }
        if !labels.is_empty() {
            return (labels.join(", "), best);
        }
    }
    // Fallback: names only.
    if let Ok(out) = quiet(&mut Command::new("powershell"))
        .args([
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_VideoController).Name",
        ])
        .output()
        .await
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let names: Vec<String> = s
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if !names.is_empty() {
            return (names.join(", "), 0.0);
        }
    }
    ("none detected (CPU)".into(), 0.0)
}

async fn gpu_linux() -> (String, f64) {
    let mut name = String::new();
    if let Ok(out) = quiet(&mut Command::new("sh"))
        .args(["-c", "lspci 2>/dev/null | grep -Ei 'vga|3d|display'"])
        .output()
        .await
    {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(line) = s.lines().next() {
            if let Some(idx) = line.find(": ") {
                name = line[idx + 2..].trim().to_string();
            }
        }
    }
    // AMD VRAM from sysfs, if present
    let mut gb = 0.0;
    for n in 0..4 {
        let p = format!("/sys/class/drm/card{n}/device/mem_info_vram_total");
        if let Ok(txt) = std::fs::read_to_string(&p) {
            if let Ok(bytes) = txt.trim().parse::<f64>() {
                gb = bytes / 1_073_741_824.0;
                break;
            }
        }
    }
    if name.is_empty() {
        return ("none detected (CPU)".into(), 0.0);
    }
    let label = if gb > 0.0 {
        format!("{name} ({gb:.0} GB)")
    } else {
        name
    };
    (label, gb)
}

fn parse_mem_gb(s: &str) -> f64 {
    let num: f64 = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .parse()
        .unwrap_or(0.0);
    if s.to_lowercase().contains("mb") {
        num / 1024.0
    } else {
        num
    }
}

/* ------------------------- worker process ------------------------- */

#[derive(Deserialize)]
pub struct WorkerCfg {
    api_key: String,
    model: String,
    worker_name: String,
}

/// An OS-assigned free TCP port on loopback (used for the worker's dashboard,
/// so it never collides with another worker already holding 7861).
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

#[tauri::command]
pub async fn worker_start(
    app: AppHandle,
    state: State<'_, Arc<Procs>>,
    cfg: WorkerCfg,
    reason: Option<String>,
) -> Result<(), String> {
    // Reap/kill any previous worker so a restart is always clean.
    {
        let mut g = state.worker.lock().await;
        if let Some(mut c) = g.take() {
            let _ = c.start_kill();
            let _ = c.wait().await;
        }
    }
    let bin = worker_bin(&app);
    if !bin.exists() {
        return Err("Worker binary not found — download it first.".into());
    }
    if cfg.api_key.trim().is_empty() || cfg.model.trim().is_empty() {
        return Err("API key and model are required.".into());
    }
    let name = if cfg.worker_name.trim().is_empty() {
        "AIPG-Desktop-Worker".to_string()
    } else {
        cfg.worker_name.trim().to_string()
    };

    let mut cmd = Command::new(&bin);
    // One backend, declared explicitly: this also overrides a GRID_BACKENDS
    // entry in a .env left by a stand-alone worker install. The advertised
    // context is capped at the configured context length.
    let mut backend = serde_json::json!({
        "type": "ollama",
        "url": ollama_url(),
        "model": cfg.model.trim(),
        "grid_model": format!("grid/{}", cfg.model.trim()),
        "name": name,
        "concurrency": 1,
    });
    // Also with the user's own Ollama: the worker takes the smaller of this
    // cap and what the loaded model reports, so it can only lower the claim.
    backend["max_context"] = serde_json::json!(ollama_ctx(&app));
    if let Ok(mut k) = REDACT_KEY.lock() {
        *k = cfg.api_key.trim().to_string();
    }
    // The key goes in through the environment, not the command line, where
    // every local user could read it in the process list.
    cmd.env("GRID_API_KEY", cfg.api_key.trim())
        .env("GRID_BACKENDS", serde_json::json!([backend]).to_string())
        .args(["--no-gui", "--no-browser"])
        .arg("--model")
        .arg(cfg.model.trim())
        .arg("--backend-url")
        .arg(ollama_url())
        .arg("--worker-name")
        .arg(&name);
    // Give the worker's local dashboard a free port to avoid "port 7861 in use".
    let dash_port = free_port();
    if let Some(port) = dash_port {
        cmd.arg("--port").arg(port.to_string());
    }
    // The dashboard also serves the worker's log. The Windows build of the
    // worker has no console, so nothing arrives on its output pipes; the log
    // is then read from there, with a token only this app knows.
    let dash_token = {
        let mut b = [0u8; 16];
        let _ = getrandom::getrandom(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    };
    cmd.env("DASHBOARD_TOKEN", &dash_token);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    quiet(&mut cmd);

    let my_gen = state
        .worker_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    let mut child = cmd
        .spawn()
        .map_err(|x| format!("Failed to start the worker: {x}"))?;
    crate::job::adopt(&child);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *state.worker.lock().await = Some(child);

    // When stdout closes (process ended), clear the stored child so a restart
    // isn't blocked and worker_running() reflects reality.
    let piped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(out) = stdout {
        let procs = state.inner().clone();
        let app2 = app.clone();
        let piped = piped.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                piped.store(true, std::sync::atomic::Ordering::SeqCst);
                let line = redact(&line);
                log_append(&app2, &line);
                let _ = app2.emit("worker-log", line);
            }
            if procs.worker_gen.load(std::sync::atomic::Ordering::SeqCst) == my_gen {
                // still the stored child: it ended on its own, so say how
                let child = procs.worker.lock().await.take();
                let how = match child {
                    Some(mut c) => match tokio::time::timeout(std::time::Duration::from_secs(3), c.wait()).await {
                        Ok(Ok(st)) => format!("worker process ended on its own ({st})."),
                        _ => "worker process ended on its own.".to_string(),
                    },
                    None => "worker process stopped by the app.".to_string(),
                };
                log_append(&app2, &desk_line(&how));
                let _ = app2.emit("worker-exit", true);
            } else {
                log_append(&app2, &desk_line("previous worker process replaced by a new start."));
            }
        });
    }
    if let Some(err) = stderr {
        pump(app.clone(), err, true, piped.clone());
    }
    let why = reason.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("no reason given");
    let line = desk_line(&format!("worker started ({}) · model {} · reason: {}", WORKER_TAG, cfg.model.trim(), redact(why)));
    log_append(&app, &line);
    let _ = app.emit("worker-log", line);
    poll_dashboard_log(app.clone(), state.inner().clone(), my_gen, dash_port.unwrap_or(7861), dash_token, piped);
    preload_model(app.clone(), state.inner().clone(), my_gen, cfg.model.trim().to_string());
    Ok(())
}

/// `[desktop] 2026-11-11 20:31:05Z text`: lines the app itself adds to the log.
fn desk_line(msg: &str) -> String {
    format!("[desktop] {} {msg}", utc_stamp())
}

/// UTC date and time without a date crate (days-to-civil after H. Hinnant).
fn utc_stamp() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = ((s / 86_400) as i64, s % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// A note from the user interface for the log file (why a restart happens).
#[tauri::command]
pub fn worker_note(app: AppHandle, text: String) {
    let text: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
    log_append(&app, &desk_line(&redact(&text)));
}

/// How many lines at the end of `now` are new compared with `old`. The
/// dashboard keeps the last 500 lines, so `now` is `old` minus some lines at
/// the front plus the new ones at the end.
fn new_tail(old: &[String], now: &[String]) -> usize {
    for d in 0..=old.len() {
        let keep = &old[d..];
        if keep.len() <= now.len() && now[..keep.len()] == *keep {
            return now.len() - keep.len();
        }
    }
    now.len()
}

/// Read the worker's log from its local dashboard for as long as nothing
/// arrives on its output pipes (see `worker_start`).
fn poll_dashboard_log(
    app: AppHandle,
    procs: Arc<Procs>,
    my_gen: u64,
    port: u16,
    token: String,
    piped: Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering::SeqCst;
    tokio::spawn(async move {
        let Ok(http) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build() else { return };
        let mut seen: Vec<String> = Vec::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if procs.worker_gen.load(SeqCst) != my_gen || piped.load(SeqCst) || procs.worker.lock().await.is_none() {
                return;
            }
            let Ok(r) = http.get(format!("http://127.0.0.1:{port}/api/logs")).bearer_auth(&token).send().await else { continue };
            if !r.status().is_success() {
                continue;
            }
            let Ok(v) = r.json::<serde_json::Value>().await else { continue };
            let now: Vec<String> = v["lines"]
                .as_array()
                .map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let fresh = new_tail(&seen, &now);
            for line in &now[now.len() - fresh..] {
                let line = redact(line);
                log_append(&app, &line);
                let _ = app.emit("worker-log", line);
            }
            seen = now;
        }
    });
}

/// Load the model into memory right after the start, so the first grid job
/// does not have to wait for it (a job that waits too long times out).
fn preload_model(app: AppHandle, procs: Arc<Procs>, my_gen: u64, model: String) {
    tokio::spawn(async move {
        let note = |msg: String| {
            let line = desk_line(&msg);
            log_append(&app, &line);
            let _ = app.emit("worker-log", line);
        };
        let Ok(http) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(600)).build() else { return };
        note(format!("loading {model} into memory so the first job does not wait for it…"));
        let t0 = std::time::Instant::now();
        let r = http
            .post(format!("{}/api/generate", ollama_url()))
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await;
        if procs.worker_gen.load(std::sync::atomic::Ordering::SeqCst) != my_gen {
            return;
        }
        match r {
            Ok(r) if r.status().is_success() => note(format!("{model} is loaded ({} s).", t0.elapsed().as_secs())),
            Ok(r) => note(format!("{model} could not be loaded ahead of time (HTTP {}); it loads with the first job.", r.status().as_u16())),
            Err(e) => note(format!("{model} could not be loaded ahead of time ({e}); it loads with the first job.")),
        }
    });
}

/// Keep the worker's output in `worker.log` in the app data folder, so the
/// connector's status tool can report recent lines and errors. The worker
/// logs job ids and token counts, never prompts.
fn log_append(app: &AppHandle, line: &str) {
    use std::io::Write;
    let path = data_dir(app).join("worker.log");
    // start over when it gets large
    if std::fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

fn pump<R>(app: AppHandle, reader: R, is_err: bool, piped: Arc<std::sync::atomic::AtomicBool>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            piped.store(true, std::sync::atomic::Ordering::SeqCst);
            let line = redact(&line);
            log_append(&app, &line);
            let _ = app.emit("worker-log", line);
        }
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

/* ------------------------- models directory ------------------------- */

#[derive(Serialize)]
pub struct ModelsDir {
    pub path: String,
    pub custom: bool,
    pub free_bytes: u64,
    /// true when the app-managed Ollama had to be stopped to switch folders
    pub ollama_stopped: bool,
}

/// Bytes available on the volume that holds `path` (0 if it can't be determined).
fn free_space_for(path: &std::path::Path) -> u64 {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let p = path.to_string_lossy();
    let mut best: Option<(usize, u64)> = None;
    for d in disks.list() {
        let mp = d.mount_point().to_string_lossy();
        if p.starts_with(mp.as_ref()) {
            let len = mp.len();
            if best.map_or(true, |(b, _)| len > b) {
                best = Some((len, d.available_space()));
            }
        }
    }
    best.map(|(_, s)| s).unwrap_or(0)
}

#[tauri::command]
pub fn models_dir_get(app: AppHandle) -> Result<ModelsDir, String> {
    let path = ollama_models_dir(&app);
    // free space of the nearest existing ancestor (the dir itself may not exist yet)
    let mut probe = path.clone();
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => break,
        }
    }
    Ok(ModelsDir {
        free_bytes: free_space_for(&probe),
        ollama_stopped: false,
        path: path.display().to_string(),
        custom: models_override_file(&app).exists(),
    })
}

/// Move everything under `src` into `dst` (rename when possible, copy across
/// volumes otherwise). Existing files in `dst` are kept.
fn move_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            move_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        let _ = std::fs::remove_dir(src);
        Ok(())
    } else {
        if dst.exists() {
            return std::fs::remove_file(src);
        }
        if std::fs::rename(src, dst).is_err() {
            std::fs::copy(src, dst)?;
            std::fs::remove_file(src)?;
        }
        Ok(())
    }
}

/// Set a custom models directory (empty string resets to the managed default).
/// Stops the app-managed Ollama so the new folder takes effect on its next
/// start, and optionally moves the already downloaded models along.
#[tauri::command]
pub async fn models_dir_set(
    app: AppHandle,
    state: State<'_, Arc<Procs>>,
    path: String,
    move_existing: bool,
) -> Result<ModelsDir, String> {
    if state.worker.lock().await.is_some() {
        return Err("Stop the worker before changing the models folder.".into());
    }
    let old = ollama_models_dir(&app);
    let file = models_override_file(&app);
    let p = path.trim();
    let target = if p.is_empty() {
        ollama_root(&app).join("models")
    } else {
        PathBuf::from(p)
    };
    std::fs::create_dir_all(&target).map_err(|x| format!("Folder cannot be used: {x}"))?;
    // verify it's writable before committing the choice
    let probe = target.join(".aipg-write-test");
    std::fs::write(&probe, b"ok").map_err(|x| format!("No write access to the folder: {x}"))?;
    let _ = std::fs::remove_file(&probe);

    // Ollama reads OLLAMA_MODELS only at start — stop ours so it restarts clean.
    let mut stopped = false;
    if let Some(mut c) = state.ollama.lock().await.take() {
        let _ = c.start_kill();
        let _ = c.wait().await;
        stopped = true;
    }

    if move_existing && old != target && old.exists() {
        let (o, t) = (old.clone(), target.clone());
        tokio::task::spawn_blocking(move || move_tree(&o, &t))
            .await
            .map_err(e)?
            .map_err(|x| format!("Moving models failed: {x}"))?;
    }

    if p.is_empty() {
        let _ = std::fs::remove_file(&file);
    } else {
        std::fs::create_dir_all(data_dir(&app)).map_err(e)?;
        std::fs::write(&file, target.display().to_string()).map_err(e)?;
    }
    let mut d = models_dir_get(app)?;
    d.ollama_stopped = stopped;
    Ok(d)
}

/* ------------------------- self-update ------------------------- */

/// Returns the new version string when an update is available.
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let update = app.updater().map_err(e)?.check().await.map_err(e)?;
    Ok(update.map(|u| u.version.clone()))
}

/// Download the signed update, stop child processes, install and restart.
#[tauri::command]
pub async fn update_install(app: AppHandle, state: State<'_, Arc<Procs>>) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let update = app
        .updater()
        .map_err(e)?
        .check()
        .await
        .map_err(|x| format!("could not check for the update ({x})"))?
        .ok_or("Already up to date.")?;
    let mut received: u64 = 0;
    let app2 = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                received += chunk as u64;
                let _ = app2.emit(
                    "update-progress",
                    serde_json::json!({ "received": received, "total": total }),
                );
            },
            || {},
        )
        .await
        .map_err(|x| format!("download or signature check failed ({x})"))?;
    let _ = app.emit("update-progress", serde_json::json!({ "stage": "installing" }));
    kill_all(state.inner()).await;
    // The relaunch inherits this run's arguments; make sure a login start
    // (`--minimized`) does not leave the updated app hidden in the tray.
    let _ = std::fs::create_dir_all(data_dir(&app));
    let _ = std::fs::write(update_marker(&app), b"1");
    if let Err(x) = update.install(bytes) {
        let _ = std::fs::remove_file(update_marker(&app));
        return Err(format!("the installer could not be started ({x})"));
    }
    app.restart();
    #[allow(unreachable_code)]
    Ok(())
}

fn update_marker(app: &AppHandle) -> PathBuf {
    data_dir(app).join("just_updated")
}
/// True once after an in-app update (and clears the marker).
pub fn take_update_marker(app: &AppHandle) -> bool {
    std::fs::remove_file(update_marker(app)).is_ok()
}

/* ------------------------- app config (small JSON file) ------------------------- */

fn config_path(app: &AppHandle) -> PathBuf {
    data_dir(app).join("config.json")
}

#[tauri::command]
pub fn config_get(app: AppHandle) -> Result<String, String> {
    match std::fs::read_to_string(config_path(&app)) {
        Ok(s) => Ok(s),
        Err(ref err) if err.kind() == std::io::ErrorKind::NotFound => Ok("{}".into()),
        Err(err) => Err(err.to_string()),
    }
}

#[tauri::command]
pub fn config_set(app: AppHandle, data: String) -> Result<(), String> {
    std::fs::create_dir_all(data_dir(&app)).map_err(e)?;
    std::fs::write(config_path(&app), data.as_bytes()).map_err(e)
}

/// Read a boolean flag from config.json without failing (used by the tray/close
/// handler, which runs outside the async command layer).
pub fn config_bool(app: &AppHandle, key: &str, default: bool) -> bool {
    std::fs::read_to_string(config_path(app))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get(key).and_then(|b| b.as_bool()))
        .unwrap_or(default)
}

fn config_patch(app: &AppHandle, key: &str, val: serde_json::Value) -> Result<(), String> {
    let mut cfg = std::fs::read_to_string(config_path(app))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    cfg[key] = val;
    std::fs::create_dir_all(data_dir(app)).map_err(e)?;
    std::fs::write(config_path(app), cfg.to_string()).map_err(e)
}

/// Context window the app's Ollama loads models with. The same number is
/// given to the worker as the limit it advertises to the grid, so prompts
/// are never accepted that the model would silently cut off.
fn ollama_ctx(app: &AppHandle) -> u64 {
    std::fs::read_to_string(config_path(app))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["ollama_ctx"].as_u64())
        .unwrap_or(8192)
        .clamp(2048, 131_072)
}

/// Minutes the app's Ollama keeps a model loaded after its last request;
/// -1 = until Ollama stops. Ollama's own default is 5 minutes.
fn ollama_keep_alive(app: &AppHandle) -> i64 {
    let v = std::fs::read_to_string(config_path(app))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["ollama_keep_alive"].as_i64())
        .unwrap_or(30);
    if v < 0 { -1 } else { v.clamp(1, 1440) }
}
fn keep_alive_env(minutes: i64) -> String {
    if minutes < 0 { "-1".into() } else { format!("{minutes}m") }
}

/// Read the saved choice once at startup.
pub fn init_runtime(app: &AppHandle) {
    USE_SYSTEM_OLLAMA.store(
        config_bool(app, "use_system_ollama", false),
        std::sync::atomic::Ordering::SeqCst,
    );
}

#[derive(Serialize)]
pub struct OllamaSettings {
    base: String,
    ctx: u64,
    use_system: bool,
    managed_port: u16,
    keep_alive: i64,
}

#[tauri::command]
pub fn ollama_settings(app: AppHandle) -> OllamaSettings {
    OllamaSettings {
        base: ollama_url(),
        ctx: ollama_ctx(&app),
        use_system: use_system_ollama(),
        managed_port: OLLAMA_MANAGED_PORT,
        keep_alive: ollama_keep_alive(&app),
    }
}

/// Change the context length and/or which Ollama is used. Stops the app's
/// Ollama so the next start picks the new values up.
#[tauri::command]
pub async fn ollama_settings_set(
    app: AppHandle,
    state: State<'_, Arc<Procs>>,
    ctx: u64,
    use_system: bool,
    keep_alive: i64,
) -> Result<OllamaSettings, String> {
    if state.worker.lock().await.is_some() {
        return Err("Stop the worker before changing these settings.".into());
    }
    if !(2048..=131_072).contains(&ctx) {
        return Err("Context length must be between 2048 and 131072.".into());
    }
    if let Some(mut c) = state.ollama.lock().await.take() {
        let _ = c.start_kill();
        let _ = c.wait().await;
    }
    config_patch(&app, "ollama_ctx", serde_json::json!(ctx))?;
    config_patch(&app, "ollama_keep_alive", serde_json::json!(if keep_alive < 0 { -1 } else { keep_alive.clamp(1, 1440) }))?;
    config_patch(&app, "use_system_ollama", serde_json::json!(use_system))?;
    USE_SYSTEM_OLLAMA.store(use_system, std::sync::atomic::Ordering::SeqCst);
    Ok(ollama_settings(app))
}

/// Kill worker and Ollama processes that an earlier run of the app left
/// behind (crash, forced close). They live under the app's data folder, so
/// nothing the user installed themselves is touched.
pub fn sweep_orphans(app: &AppHandle) -> usize {
    let dirs = [worker_dir(app), ollama_dist(app)];
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let me = sysinfo::get_current_pid().ok();
    let mut n = 0;
    for (pid, p) in sys.processes() {
        if Some(*pid) == me {
            continue;
        }
        let Some(exe) = p.exe() else { continue };
        if dirs.iter().any(|d| exe.starts_with(d)) && p.kill() {
            n += 1;
        }
    }
    n
}

/// Blank API keys in a line before it is shown, stored or handed to an
/// assistant: the running worker's exact key, and anything shaped like one.
pub(crate) fn redact(line: &str) -> String {
    let mut out = line.to_string();
    if let Ok(k) = REDACT_KEY.lock() {
        if k.len() >= 8 && out.contains(k.as_str()) {
            out = out.replace(k.as_str(), "grid_…");
        }
    }
    crate::mcp::redact_keys(&out)
}

/* ------------------------- image gallery (local files) ------------------------- */

fn images_dir(app: &AppHandle) -> PathBuf {
    data_dir(app).join("images")
}
/// File holding an optional user-chosen folder for the image files (one line,
/// a path). The gallery index always stays in the app data folder.
fn images_override_file(app: &AppHandle) -> PathBuf {
    data_dir(app).join("images_dir.txt")
}
fn image_files_dir(app: &AppHandle) -> PathBuf {
    if let Ok(s) = std::fs::read_to_string(images_override_file(app)) {
        let s = s.trim();
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    images_dir(app)
}
fn images_index_path(app: &AppHandle) -> PathBuf {
    images_dir(app).join("index.json")
}
fn images_load(app: &AppHandle) -> Vec<serde_json::Value> {
    std::fs::read_to_string(images_index_path(app))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
        .unwrap_or_default()
}
fn images_save(app: &AppHandle, list: &[serde_json::Value]) -> Result<(), String> {
    std::fs::create_dir_all(images_dir(app)).map_err(e)?;
    let p = images_index_path(app);
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(list).map_err(e)?).map_err(e)?;
    std::fs::rename(&tmp, &p).map_err(e)
}
/// File extension + MIME type from the image's magic bytes.
fn image_kind(b: &[u8]) -> (&'static str, &'static str) {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ("jpg", "image/jpeg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        ("webp", "image/webp")
    } else {
        ("png", "image/png")
    }
}
/// Resolve a gallery id to its file, via the index only (no path input).
fn image_file(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    let list = images_load(app);
    let entry = list
        .iter()
        .find(|x| x["id"].as_str() == Some(id))
        .ok_or("Image not found.")?;
    let name = entry["file"].as_str().ok_or("Image not found.")?;
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("Invalid image entry.".into());
    }
    let p = image_files_dir(app).join(name);
    if !p.exists() {
        // not moved along with a folder change — still in the default place
        let d = images_dir(app).join(name);
        if d.exists() {
            return Ok(d);
        }
    }
    Ok(p)
}

/// Persist a generated image (an https URL or a data: URL) into the local
/// gallery together with its metadata. Returns the stored entry.
#[tauri::command]
pub async fn image_store(
    app: AppHandle,
    src: String,
    meta: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let bytes: Vec<u8> = if let Some(rest) = src.strip_prefix("data:") {
        let b64 = rest.split_once(',').map(|x| x.1).ok_or("Malformed data URL.")?;
        base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(e)?
    } else if src.starts_with("https://") {
        // only the grid's own hosts — never an arbitrary URL handed in by the page
        let host = src["https://".len()..].split(['/', '?', '#']).next().unwrap_or("");
        let ok = !host.contains('@')
            && !host.contains(':')
            && (host == "aipg.art"
                || host.ends_with(".aipg.art")
                || host == "aipowergrid.io"
                || host.ends_with(".aipowergrid.io"));
        if !ok {
            return Err("Images are only fetched from AI Power Grid hosts.".into());
        }
        let r = reqwest::Client::builder()
            .user_agent("aipg-desktop")
            .build()
            .map_err(e)?
            .get(&src)
            .send()
            .await
            .map_err(e)?;
        if !r.status().is_success() {
            return Err(format!("Could not download the image: HTTP {}", r.status()));
        }
        r.bytes().await.map_err(e)?.to_vec()
    } else {
        return Err("Unsupported image source.".into());
    };
    if bytes.is_empty() {
        return Err("Empty image.".into());
    }
    let (ext, _) = image_kind(&bytes);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut list = images_load(&app);
    let id = format!("{ts}{}", list.len() % 1000);
    let file = format!("{id}.{ext}");
    let dir = image_files_dir(&app);
    std::fs::create_dir_all(&dir).map_err(|x| format!("Images folder cannot be used: {x}"))?;
    std::fs::write(dir.join(&file), &bytes)
        .map_err(|x| format!("Could not save into {}: {x}", dir.display()))?;
    let mut entry = if meta.is_object() { meta } else { serde_json::json!({}) };
    entry["id"] = serde_json::json!(id);
    entry["file"] = serde_json::json!(file);
    entry["ts"] = serde_json::json!(ts as u64);
    entry["bytes"] = serde_json::json!(bytes.len());
    list.insert(0, entry.clone());
    images_save(&app, &list)?;
    Ok(entry)
}

#[tauri::command]
pub fn images_index(app: AppHandle) -> Vec<serde_json::Value> {
    images_load(&app)
}

/// The image as a data: URL (the webview's CSP allows data: images).
#[tauri::command]
pub async fn image_data(app: AppHandle, id: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = std::fs::read(image_file(&app, &id)?).map_err(e)?;
    let (_, mime) = image_kind(&bytes);
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

#[tauri::command]
pub fn image_remove(app: AppHandle, id: String) -> Result<(), String> {
    if let Ok(f) = image_file(&app, &id) {
        let _ = std::fs::remove_file(f);
    }
    let list: Vec<serde_json::Value> = images_load(&app)
        .into_iter()
        .filter(|x| x["id"].as_str() != Some(id.as_str()))
        .collect();
    images_save(&app, &list)
}

/// Copy a gallery image to a location the user picks. The save dialog is
/// opened here, so the destination never comes from the page. Returns false
/// when the dialog was cancelled.
#[tauri::command]
pub async fn image_export(app: AppHandle, id: String) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;
    let src = image_file(&app, &id)?;
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "aipg-image.png".into());
    let app2 = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        app2.dialog().file().set_title("Save image").set_file_name(name).blocking_save_file()
    })
    .await
    .map_err(e)?;
    let Some(dest) = picked else { return Ok(false) };
    let dest = dest.into_path().map_err(e)?;
    std::fs::copy(src, dest).map(|_| true).map_err(e)
}

#[derive(Serialize)]
pub struct ImagesDir {
    path: String,
    custom: bool,
    moved: usize,
}

#[tauri::command]
pub fn images_dir_get(app: AppHandle) -> ImagesDir {
    ImagesDir {
        path: image_files_dir(&app).display().to_string(),
        custom: images_override_file(&app).exists(),
        moved: 0,
    }
}

/// Choose where generated images are saved (empty string = default folder).
/// The images already in the gallery are moved along; the index stays put.
#[tauri::command]
pub async fn images_dir_set(app: AppHandle, path: String) -> Result<ImagesDir, String> {
    let p = path.trim().to_string();
    let target = if p.is_empty() {
        images_dir(&app)
    } else {
        PathBuf::from(&p)
    };
    std::fs::create_dir_all(&target).map_err(|x| format!("Folder cannot be used: {x}"))?;
    let probe = target.join(".aipg-write-test");
    std::fs::write(&probe, b"ok").map_err(|x| format!("No write access to the folder: {x}"))?;
    let _ = std::fs::remove_file(&probe);

    // move the gallery's own files only — never anything else in those folders
    let sources: Vec<PathBuf> = images_load(&app)
        .iter()
        .filter_map(|x| x["id"].as_str().map(str::to_string))
        .filter_map(|id| image_file(&app, &id).ok())
        .filter(|f| f.exists() && f.parent() != Some(target.as_path()))
        .collect();
    let t = target.clone();
    let moved = tokio::task::spawn_blocking(move || -> std::io::Result<usize> {
        let mut n = 0;
        for src in sources {
            let Some(name) = src.file_name() else { continue };
            let dst = t.join(name);
            if dst.exists() {
                continue; // keep both; the gallery falls back to the old file
            }
            if std::fs::rename(&src, &dst).is_err() {
                std::fs::copy(&src, &dst)?;
                std::fs::remove_file(&src)?;
            }
            n += 1;
        }
        Ok(n)
    })
    .await
    .map_err(e)?
    .map_err(|x| format!("Moving images failed: {x}"))?;

    let file = images_override_file(&app);
    if p.is_empty() {
        let _ = std::fs::remove_file(&file);
    } else {
        std::fs::create_dir_all(data_dir(&app)).map_err(e)?;
        std::fs::write(&file, target.display().to_string()).map_err(e)?;
    }
    let mut d = images_dir_get(app);
    d.moved = moved;
    Ok(d)
}

/// Show the images folder in the system file manager.
#[tauri::command]
pub fn images_dir_open(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = image_files_dir(&app);
    std::fs::create_dir_all(&dir).map_err(e)?;
    app.opener()
        .open_path(dir.display().to_string(), None::<&str>)
        .map_err(e)
}

/* ------------------------- chat attachments ------------------------- */

/// Plain text of a PDF (base64 in) so it can be attached to a chat message.
#[tauri::command]
pub async fn pdf_text(data: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(e)?;
    tokio::task::spawn_blocking(move || {
        // the extractor can panic on unusual files — report that as an error
        std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes))
            .map_err(|_| "This PDF could not be read.".to_string())?
            .map_err(|x| format!("This PDF could not be read: {x}"))
    })
    .await
    .map_err(e)?
}

/* ------------------------- chat store (local JSON file) ------------------------- */

fn chats_path(app: &AppHandle) -> PathBuf {
    data_dir(app).join("chats.json")
}

#[tauri::command]
pub async fn chats_read(app: AppHandle) -> Result<String, String> {
    match std::fs::read_to_string(chats_path(&app)) {
        Ok(s) => Ok(s),
        Err(ref err) if err.kind() == std::io::ErrorKind::NotFound => Ok("{}".into()),
        Err(err) => Err(err.to_string()),
    }
}

#[tauri::command]
pub async fn chats_write(app: AppHandle, data: String) -> Result<(), String> {
    std::fs::create_dir_all(data_dir(&app)).map_err(e)?;
    let p = chats_path(&app);
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, data.as_bytes()).map_err(e)?;
    std::fs::rename(&tmp, &p).map_err(e)?;
    Ok(())
}

pub async fn kill_all(procs: &Procs) {
    if let Some(mut c) = procs.worker.lock().await.take() {
        let _ = c.start_kill();
    }
    if let Some(mut c) = procs.ollama.lock().await.take() {
        let _ = c.start_kill();
    }
}
