//! Desktop side of the local MCP connector: stores the API key where the
//! `--mcp` process can read it and registers that process with Claude Desktop.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

const SERVER: &str = "aipg-grid";

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

fn key_file(app: &AppHandle) -> PathBuf {
    crate::worker::data_dir(app).join("connector.json")
}

/// The program an MCP client has to launch. Inside an AppImage `current_exe`
/// points into a temporary mount, so the AppImage itself is used instead.
fn launcher() -> Result<String, String> {
    if let Ok(p) = std::env::var("APPIMAGE") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    Ok(std::env::current_exe().map_err(e)?.to_string_lossy().into_owned())
}

fn claude_config(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .config_dir()
        .map_err(e)?
        .join("Claude")
        .join("claude_desktop_config.json"))
}

fn read_claude(path: &PathBuf) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => {
            let v: Value = serde_json::from_str(&s).map_err(|_| {
                "Claude's config file is not valid JSON; fix or remove it first.".to_string()
            })?;
            if v.is_object() {
                Ok(v)
            } else {
                Err("Claude's config file has an unexpected format.".into())
            }
        }
        Err(_) => Ok(json!({})),
    }
}

fn write_claude(path: &PathBuf, cfg: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(e)?;
    }
    if path.exists() {
        // keep the user's previous config next to it
        let _ = std::fs::copy(path, path.with_extension("json.aipg-backup"));
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(cfg).map_err(e)?).map_err(e)?;
    std::fs::rename(&tmp, path).map_err(e)
}

#[derive(Serialize)]
pub struct ConnectorStatus {
    /// The connector has a key file, so the `--mcp` process can work.
    enabled: bool,
    launcher: String,
    key_file: String,
    /// Claude Desktop looks installed (its config folder exists).
    claude_found: bool,
    claude_registered: bool,
    claude_config: String,
    /// Private values in assistant requests: "mask", "block" or "off".
    protect: String,
    /// Generated images are handed back to the assistant (not only their paths).
    inline_images: bool,
    /// Daily spending limit for assistants in USD; 0 = none.
    daily_limit_usd: f64,
    /// Estimated spend and number of paid requests through the connector today (UTC).
    spent_today_usd: f64,
    requests_today: u64,
    /// "key", "signin" or "" when off.
    via: String,
    /// One-line command for Claude Code.
    command: String,
    /// One-line command for OpenClaw.
    openclaw_command: String,
    /// Generic `mcpServers` snippet for other MCP clients.
    snippet: String,
}

#[tauri::command]
pub fn connector_status(app: AppHandle) -> Result<ConnectorStatus, String> {
    let launcher = launcher()?;
    let key = key_file(&app).to_string_lossy().into_owned();
    let cfg_path = claude_config(&app)?;
    let registered = read_claude(&cfg_path)
        .map(|c| c["mcpServers"][SERVER].is_object())
        .unwrap_or(false);
    let entry = json!({ "command": launcher, "args": ["--mcp", key] });
    let (spent, requests) = crate::mcp::spend_today(&crate::worker::data_dir(&app));
    Ok(ConnectorStatus {
        protect: match read_key_file(&app)["protect"].as_str() {
            Some("block") => "block".into(),
            Some("off") => "off".into(),
            _ => "mask".into(),
        },
        inline_images: read_key_file(&app)["inline_images"] != false,
        daily_limit_usd: read_key_file(&app)["daily_limit_usd"].as_f64().unwrap_or(0.0),
        spent_today_usd: spent,
        requests_today: requests,
        enabled: via(&read_key_file(&app)) != "",
        via: via(&read_key_file(&app)).into(),
        claude_found: cfg_path.parent().map(|d| d.exists()).unwrap_or(false),
        claude_registered: registered,
        claude_config: cfg_path.to_string_lossy().into_owned(),
        command: format!("claude mcp add {SERVER} -- \"{launcher}\" --mcp \"{key}\""),
        // `--arg=--mcp`: with a space OpenClaw would read --mcp as its own option
        openclaw_command: format!("openclaw mcp add {SERVER} --command \"{launcher}\" --arg=--mcp --arg \"{key}\" --parallel"),
        snippet: serde_json::to_string_pretty(&json!({ "mcpServers": { SERVER: entry } }))
            .map_err(e)?,
        launcher,
        key_file: key,
    })
}

/// How the connector reaches the grid: "key", "signin" or "" (off).
fn via(cfg: &Value) -> &'static str {
    if cfg["api_key"].as_str().map(|k| !k.trim().is_empty()).unwrap_or(false) {
        "key"
    } else if cfg["use_signin"].as_bool() == Some(true) {
        "signin"
    } else {
        ""
    }
}

fn read_key_file(app: &AppHandle) -> Value {
    std::fs::read_to_string(key_file(app))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Set the daily spending limit for requests made through the connector
/// (0 removes it). It is kept when the connector is switched off and on.
#[tauri::command]
pub fn connector_limit_set(app: AppHandle, usd: f64) -> Result<(), String> {
    if !usd.is_finite() || usd < 0.0 {
        return Err("Enter an amount of 0 or more.".into());
    }
    let mut cfg = read_key_file(&app);
    cfg["daily_limit_usd"] = json!(usd);
    write_key_file(&app, &cfg)
}

/// Whether generated images go back to the assistant or only their paths.
#[tauri::command]
pub fn connector_inline_images_set(app: AppHandle, on: bool) -> Result<(), String> {
    let mut cfg = read_key_file(&app);
    cfg["inline_images"] = json!(on);
    write_key_file(&app, &cfg)
}

/// How the connector treats private values in assistant requests.
#[tauri::command]
pub fn connector_protect_set(app: AppHandle, mode: String) -> Result<(), String> {
    if !["mask", "block", "off"].contains(&mode.as_str()) {
        return Err("Unknown setting.".into());
    }
    let mut cfg = read_key_file(&app);
    cfg["protect"] = json!(mode);
    write_key_file(&app, &cfg)
}

/// Turn the connector on. With an API key, the key is stored for the connector
/// process (owner-readable only on Unix); without one, the connector uses the
/// browser sign-in.
#[tauri::command]
pub fn connector_enable(app: AppHandle, api_key: String) -> Result<(), String> {
    let api_key = api_key.trim();
    // keep other settings (the daily limit) when the key is refreshed
    let mut cfg = read_key_file(&app);
    if api_key.is_empty() {
        if !crate::auth::signed_in(&crate::worker::data_dir(&app)) {
            return Err("Sign in or save an API key in Settings first.".into());
        }
        if let Some(o) = cfg.as_object_mut() {
            o.remove("api_key");
        }
        cfg["use_signin"] = json!(true);
    } else {
        if let Some(o) = cfg.as_object_mut() {
            o.remove("use_signin");
        }
        cfg["api_key"] = json!(api_key);
    }
    write_key_file(&app, &cfg)
}

fn write_key_file(app: &AppHandle, cfg: &Value) -> Result<(), String> {
    let path = key_file(app);
    std::fs::create_dir_all(crate::worker::data_dir(app)).map_err(e)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(e)?;
        f.write_all(cfg.to_string().as_bytes()).map_err(e)?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, cfg.to_string()).map_err(e)?;
    Ok(())
}

/// Remove the stored key and the Claude Desktop registration.
#[tauri::command]
pub fn connector_disable(app: AppHandle) -> Result<(), String> {
    // drop the key but keep the other settings (the daily limit)
    let path = key_file(&app);
    if path.exists() {
        let mut cfg = read_key_file(&app);
        if let Some(o) = cfg.as_object_mut() {
            o.remove("api_key");
            o.remove("use_signin");
        }
        if cfg.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            std::fs::remove_file(&path).map_err(e)?;
        } else {
            write_key_file(&app, &cfg)?;
        }
    }
    connector_claude_remove(app)
}

/// Add the connector to Claude Desktop's config, keeping everything else.
#[tauri::command]
pub fn connector_claude_add(app: AppHandle) -> Result<(), String> {
    let path = claude_config(&app)?;
    let mut cfg = read_claude(&path)?;
    if !cfg["mcpServers"].is_object() {
        cfg["mcpServers"] = json!({});
    }
    cfg["mcpServers"][SERVER] = json!({
        "command": launcher()?,
        "args": ["--mcp", key_file(&app).to_string_lossy()],
    });
    write_claude(&path, &cfg)
}

#[tauri::command]
pub fn connector_claude_remove(app: AppHandle) -> Result<(), String> {
    let path = claude_config(&app)?;
    if !path.exists() {
        return Ok(());
    }
    let mut cfg = read_claude(&path)?;
    let removed = cfg["mcpServers"]
        .as_object_mut()
        .map(|m| m.remove(SERVER).is_some())
        .unwrap_or(false);
    if removed {
        write_claude(&path, &cfg)?;
    }
    Ok(())
}
