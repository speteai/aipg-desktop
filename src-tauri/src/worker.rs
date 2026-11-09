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

const WORKER_TAG: &str = "v0.3.12";
const WORKER_REPO: &str = "AIPowerGrid/grid-text-worker";
const OLLAMA: &str = "http://127.0.0.1:11434";
const OLLAMA_TAG: &str = "v0.40.15";

#[derive(Default)]
pub struct Procs {
    worker: Mutex<Option<Child>>,
    ollama: Mutex<Option<Child>>,
}

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

/* ------------------------- paths ------------------------- */

fn data_dir(app: &AppHandle) -> PathBuf {
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
fn ollama_root(app: &AppHandle) -> PathBuf {
    data_dir(app).join("ollama")
}
fn ollama_dist(app: &AppHandle) -> PathBuf {
    ollama_root(app).join("dist")
}
fn ollama_models_dir(app: &AppHandle) -> PathBuf {
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
            .get(format!("{OLLAMA}/api/version"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await,
        Ok(r) if r.status().is_success()
    )
}

async fn system_ollama() -> bool {
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
        worker_downloaded: worker_bin(&app).exists(),
        ollama_running: running,
        ollama_installed: running || managed || system_ollama().await,
        ollama_managed: managed,
    }
}

#[tauri::command]
pub async fn download_worker(app: AppHandle) -> Result<String, String> {
    let (asset, is_zip) = worker_asset()
        .ok_or("Dein Betriebssystem/Architektur wird vom offiziellen Worker (v0.3.12) nicht abgedeckt.")?;
    let base = format!("https://github.com/{WORKER_REPO}/releases/download/{WORKER_TAG}");
    let client = reqwest::Client::builder()
        .user_agent("aipg-desktop")
        .build()
        .map_err(e)?;

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
        let _ = app.emit("worker-dl", serde_json::json!({ "received": received, "total": total }));
    }
    let got = hex::encode(hasher.finalize());
    if got != want_sha {
        return Err(format!(
            "SHA256 stimmt nicht überein (erwartet {}…, erhalten {}…) — verworfen.",
            &want_sha[..12.min(want_sha.len())],
            &got[..12.min(got.len())]
        ));
    }

    let dir = worker_dir(&app);
    std::fs::create_dir_all(&dir).map_err(e)?;
    let bin = worker_bin(&app);
    if is_zip {
        extract_single_from_zip(&buf, &bin)?;
    } else {
        std::fs::write(&bin, &buf).map_err(e)?;
    }
    make_executable(&bin)?;
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
    let (name, _) = best.ok_or("Zip enthält keine Datei.")?;
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
    let (asset, kind) = ollama_asset().ok_or("Ollama wird für dieses OS/Arch nicht angeboten.")?;
    let base = format!("https://github.com/ollama/ollama/releases/download/{OLLAMA_TAG}");
    let client = reqwest::Client::builder()
        .user_agent("aipg-desktop")
        .build()
        .map_err(e)?;

    // expected sha from the release checksum file
    let sums = client
        .get(format!("{base}/sha256sum.txt"))
        .send()
        .await
        .map_err(e)?
        .text()
        .await
        .map_err(e)?;
    let want_sha = sums
        .lines()
        .find(|l| l.trim_end().ends_with(asset))
        .and_then(|l| l.split_whitespace().next())
        .map(|s| s.to_lowercase())
        .ok_or("Checksumme für Ollama-Asset nicht gefunden.")?;

    let root = ollama_root(&app);
    std::fs::create_dir_all(&root).map_err(e)?;
    let tmp = root.join("download.part");

    // stream to disk (archives are up to ~1.5 GB — never buffer in RAM)
    let resp = client.get(format!("{base}/{asset}")).send().await.map_err(e)?;
    if !resp.status().is_success() {
        return Err(format!("Ollama-Download fehlgeschlagen: HTTP {}", resp.status()));
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
            "Ollama-SHA256 stimmt nicht (erwartet {}…) — verworfen.",
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
        .ok_or("Entpackt, aber keine ollama-Binary gefunden.")?;
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
    let bin = if let Some(b) = managed_ollama_bin(&app) {
        b
    } else if system_ollama().await {
        PathBuf::from("ollama")
    } else {
        return Err("Ollama ist nicht eingerichtet. Klicke „Set up Ollama“ (lädt es automatisch).".into());
    };

    let models = ollama_models_dir(&app);
    std::fs::create_dir_all(&models).map_err(e)?;
    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .env("OLLAMA_HOST", "127.0.0.1:11434")
        .env("OLLAMA_MODELS", &models)
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
    let child = cmd
        .spawn()
        .map_err(|x| format!("Konnte Ollama nicht starten: {x}"))?;
    *state.ollama.lock().await = Some(child);

    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if ollama_up().await {
            let _ = app.emit("ollama-ready", true);
            return Ok(());
        }
    }
    Err("Ollama gestartet, antwortet aber nicht auf Port 11434.".into())
}

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
        .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect())
        .unwrap_or_default())
}

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

/* ------------------------- hardware ------------------------- */

#[derive(Serialize)]
pub struct Hardware {
    ram_gb: f64,
    cpu: String,
    gpu: String,
    can_run_worker: bool,
    recommend: String,
    note: String,
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
            "Zu wenig Speicher für einen lokalen Worker. Nutze stattdessen den Chat mit „Grid / Auto“ — die Berechnung läuft dann remote auf dem Grid, nicht auf diesem Gerät.".into(),
        )
    } else if budget < 6.0 {
        ("llama3.2:1b".into(), true, "Kleines Modell für knappen Speicher.".into())
    } else if budget < 10.0 {
        ("llama3.2:3b".into(), true, "Solides kleines Modell.".into())
    } else if budget < 20.0 {
        ("qwen3:8b".into(), true, "Guter Allrounder.".into())
    } else {
        ("qwen3:14b".into(), true, "Reichlich Speicher — größeres Modell möglich.".into())
    };

    Hardware {
        ram_gb: (ram_gb * 10.0).round() / 10.0,
        cpu,
        gpu,
        can_run_worker,
        recommend,
        note,
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
    let out = Command::new("nvidia-smi")
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
    if let Ok(out) = Command::new("system_profiler")
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
    if let Ok(out) = Command::new("powershell")
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
    if let Ok(out) = Command::new("sh")
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
        .arg(&name);
    // Give the worker's local dashboard a free port to avoid "port 7861 in use".
    if let Some(port) = free_port() {
        cmd.arg("--port").arg(port.to_string());
    }
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|x| format!("Worker-Start fehlgeschlagen: {x}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *state.worker.lock().await = Some(child);

    // When stdout closes (process ended), clear the stored child so a restart
    // isn't blocked and worker_running() reflects reality.
    if let Some(out) = stdout {
        let procs = state.inner().clone();
        let app2 = app.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = app2.emit("worker-log", line);
            }
            procs.worker.lock().await.take();
            let _ = app2.emit("worker-exit", true);
        });
    }
    if let Some(err) = stderr {
        pump(app.clone(), err, true);
    }
    let _ = app.emit("worker-log", "[desktop] worker gestartet.");
    Ok(())
}

fn pump<R>(app: AppHandle, reader: R, is_err: bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
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
