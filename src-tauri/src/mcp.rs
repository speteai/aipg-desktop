//! Local MCP connector: `aipg-desktop --mcp <connector.json>` speaks the Model
//! Context Protocol over stdio (newline-delimited JSON-RPC 2.0), so assistants
//! like Claude Desktop or Claude Code can hand work to the grid.
//!
//! This module is deliberately free of Tauri: it runs without a window and is
//! compiled into a small test harness as well. The API key is read from the
//! connector file on every call and is sent to api.aipowergrid.io only.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

const API: &str = "https://api.aipowergrid.io/v1";
const PROTOCOLS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_PROTOCOL: &str = "2025-06-18";
/// Images above this size are saved to disk only (tool results are size-capped).
const INLINE_IMAGE_BYTES: usize = 700_000;
const SIZES: [&str; 5] = ["1024x1024", "1344x768", "768x1344", "1216x832", "832x1216"];

struct Ctx {
    key_file: PathBuf,
    http: reqwest::Client,
}

impl Ctx {
    fn api_key(&self) -> Result<String, String> {
        let from_file = std::fs::read_to_string(&self.key_file)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v["api_key"].as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty());
        from_file
            .or_else(|| std::env::var("AIPG_API_KEY").ok().filter(|s| !s.trim().is_empty()))
            .ok_or_else(|| {
                "No grid API key available. Open AIPG Desktop → Connect and enable the connector."
                    .to_string()
            })
    }
    fn data_dir(&self) -> PathBuf {
        self.key_file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir)
    }
    /// Holds the gallery index (and the image files unless the user chose
    /// another folder in the app).
    fn images_dir(&self) -> PathBuf {
        self.data_dir().join("images")
    }
    fn image_files_dir(&self) -> PathBuf {
        std::fs::read_to_string(self.data_dir().join("images_dir.txt"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.images_dir())
    }
}

/// Entry point for `--mcp`. Blocks until stdin closes.
pub fn run_stdio(key_file: PathBuf) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("aipg connector: cannot start runtime: {err}");
            return;
        }
    };
    let http = reqwest::Client::builder()
        .user_agent("aipg-desktop-connector")
        .connect_timeout(Duration::from_secs(20))
        .build()
        .unwrap_or_default();
    let ctx = Ctx { key_file, http };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(msg) => rt.block_on(handle(&ctx, &msg)),
            Err(_) => Some(rpc_error(Value::Null, -32700, "Parse error")),
        };
        if let Some(r) = reply {
            if writeln!(out, "{r}").is_err() || out.flush().is_err() {
                break;
            }
        }
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn handle(ctx: &Ctx, msg: &Value) -> Option<Value> {
    let method = match msg.get("method").and_then(Value::as_str) {
        Some(m) => m,
        None => {
            // A response to something we never asked, or not a request object.
            return if msg.is_object() {
                None
            } else {
                Some(rpc_error(Value::Null, -32600, "Invalid request"))
            };
        }
    };
    // Notifications carry no id and must not be answered.
    let id = match msg.get("id") {
        Some(id) if !id.is_null() => id.clone(),
        _ => return None,
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = if PROTOCOLS.contains(&asked) { asked } else { DEFAULT_PROTOCOL };
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "aipg-grid", "title": "AI Power Grid", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Tools that run work on AI Power Grid, a decentralised GPU network, using the user's own grid account. Every generation is billed to that account (a daily free allowance may apply), so use them when the user asks to offload work to the grid or wants an image."
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let outcome = match name {
                "grid_chat" => grid_chat(ctx, &args).await,
                "grid_generate_image" => grid_generate_image(ctx, &args).await,
                "grid_models" => grid_models(ctx).await,
                "grid_balance" => grid_balance(ctx).await,
                _ => return Some(rpc_error(id, -32602, "Unknown tool")),
            };
            match outcome {
                Ok(content) => json!({ "content": content, "isError": false }),
                Err(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
            }
        }
        "resources/list" => json!({ "resources": [] }),
        "prompts/list" => json!({ "prompts": [] }),
        _ => return Some(rpc_error(id, -32601, "Method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tools() -> Value {
    json!([
        {
            "name": "grid_chat",
            "title": "Run a text task on the grid",
            "description": "Send a prompt to a text model on AI Power Grid and return its answer. Use it to offload self-contained work (summaries, translations, drafts, classification, bulk rewriting) to the grid. Billed to the user's grid account. Include everything the model needs in the prompt: it does not see this conversation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "The complete task, including any text to work on." },
                    "system": { "type": "string", "description": "Optional system instruction." },
                    "model": { "type": "string", "description": "Grid text model id from grid_models. Default \"auto\" lets the grid route the request." },
                    "max_tokens": { "type": "integer", "minimum": 1, "maximum": 32000, "description": "Upper bound for the answer length. Default 2048." },
                    "temperature": { "type": "number", "minimum": 0, "maximum": 2 }
                },
                "required": ["prompt"]
            }
        },
        {
            "name": "grid_generate_image",
            "title": "Generate images on the grid",
            "description": "Generate 1-4 images from a text prompt on AI Power Grid. Each image is billed to the user's grid account. Images are saved to the AIPG Desktop gallery on this computer; the result lists the file paths and includes the image itself when it is small enough.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "What the image should show." },
                    "model": { "type": "string", "description": "Grid image model id from grid_models. Default: an online image model." },
                    "size": { "type": "string", "enum": SIZES, "description": "Width x height. Default 1024x1024." },
                    "n": { "type": "integer", "minimum": 1, "maximum": 4, "description": "Number of images. Default 1." }
                },
                "required": ["prompt"]
            }
        },
        {
            "name": "grid_models",
            "title": "List grid models",
            "description": "List the models that are online on AI Power Grid right now, with type (text or image), number of workers, typical speed and price. Free to call.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "grid_balance",
            "title": "Show grid credit",
            "description": "Show what the user's grid account can spend: today's free allowance, promotional credit and purchased balance. Free to call.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

fn text(s: impl Into<String>) -> Value {
    json!({ "type": "text", "text": s.into() })
}

/// Turn a non-2xx grid response into a message the assistant can act on.
fn api_failure(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            let d = v.get("detail").or_else(|| v.get("error")).cloned()?;
            Some(match d {
                Value::String(s) => s,
                other => other
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| other.to_string()),
            })
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    let hint = match status {
        401 | 403 => " The grid rejected the API key: check it in AIPG Desktop → Settings (it needs the inference.submit and account.read scopes).",
        402 => " The account has no spendable credit for this request. Do not retry; the user can check grid_balance or top up in the grid console.",
        429 => " The grid is rate-limiting this account; wait before trying again.",
        _ => "",
    };
    format!("Grid request failed (HTTP {status}): {detail}.{hint}")
}

async fn get_json(ctx: &Ctx, path: &str, auth: bool) -> Result<Value, String> {
    let mut req = ctx
        .http
        .get(format!("{API}{path}"))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30));
    if auth {
        req = req.bearer_auth(ctx.api_key()?);
    }
    let r = req.send().await.map_err(|e| format!("Could not reach the grid: {e}"))?;
    let status = r.status();
    let body = r.text().await.map_err(|e| format!("Could not read the grid response: {e}"))?;
    if !status.is_success() {
        return Err(api_failure(status.as_u16(), &body));
    }
    serde_json::from_str(&body).map_err(|e| format!("Unexpected grid response: {e}"))
}

async fn post_json(ctx: &Ctx, path: &str, body: &Value, secs: u64) -> Result<Value, String> {
    let r = ctx
        .http
        .post(format!("{API}{path}"))
        .bearer_auth(ctx.api_key()?)
        .timeout(Duration::from_secs(secs))
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Could not reach the grid: {e}"))?;
    let status = r.status();
    let text = r.text().await.map_err(|e| format!("Could not read the grid response: {e}"))?;
    if !status.is_success() {
        return Err(api_failure(status.as_u16(), &text));
    }
    serde_json::from_str(&text).map_err(|e| format!("Unexpected grid response: {e}"))
}

async fn grid_chat(ctx: &Ctx, args: &Value) -> Result<Vec<Value>, String> {
    let prompt = args["prompt"].as_str().map(str::trim).unwrap_or("");
    if prompt.is_empty() {
        return Err("`prompt` is required.".into());
    }
    let model = args["model"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("auto");
    let max_tokens = args["max_tokens"].as_u64().unwrap_or(2048).clamp(1, 32000);
    let mut messages = Vec::new();
    if let Some(sys) = args["system"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        messages.push(json!({ "role": "system", "content": sys }));
    }
    messages.push(json!({ "role": "user", "content": prompt }));
    let mut body = json!({ "model": model, "messages": messages, "max_tokens": max_tokens, "stream": false });
    if let Some(t) = args["temperature"].as_f64() {
        body["temperature"] = json!(t.clamp(0.0, 2.0));
    }
    let d = post_json(ctx, "/chat/completions", &body, 600).await?;
    let msg = &d["choices"][0]["message"];
    let answer = msg["content"].as_str().map(str::trim).unwrap_or("");
    if answer.is_empty() {
        return Err("The grid model returned an empty answer (the token limit may have been used up by its reasoning). Try a larger max_tokens or another model.".into());
    }
    let used = d["model"].as_str().unwrap_or(model);
    let usage = &d["usage"];
    let note = match (usage["prompt_tokens"].as_u64(), usage["completion_tokens"].as_u64()) {
        (Some(i), Some(o)) => format!("[AI Power Grid · model {used} · {i} input + {o} output tokens]"),
        _ => format!("[AI Power Grid · model {used}]"),
    };
    Ok(vec![text(answer), text(note)])
}

/// Extension + MIME type from the image's magic bytes.
fn image_kind(b: &[u8]) -> (&'static str, &'static str) {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ("jpg", "image/jpeg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        ("webp", "image/webp")
    } else {
        ("png", "image/png")
    }
}

/// Save into the desktop app's gallery (same index format as `image_store`).
fn store_image(dir: &Path, files: &Path, bytes: &[u8], mut entry: Value) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(files).map_err(|e| e.to_string())?;
    let index = dir.join("index.json");
    let mut list: Vec<Value> = std::fs::read_to_string(&index)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let id = format!("{ts}{}", list.len() % 1000);
    let file = format!("{id}.{}", image_kind(bytes).0);
    let path = files.join(&file);
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    entry["id"] = json!(id);
    entry["file"] = json!(file);
    entry["ts"] = json!(ts);
    entry["bytes"] = json!(bytes.len());
    list.insert(0, entry);
    let tmp = index.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(&list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &index).map_err(|e| e.to_string())?;
    Ok(path)
}

async fn grid_generate_image(ctx: &Ctx, args: &Value) -> Result<Vec<Value>, String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let prompt = args["prompt"].as_str().map(str::trim).unwrap_or("");
    if prompt.is_empty() {
        return Err("`prompt` is required.".into());
    }
    let size = args["size"].as_str().filter(|s| SIZES.contains(s)).unwrap_or(SIZES[0]);
    let n = args["n"].as_u64().unwrap_or(1).clamp(1, 4);
    ctx.api_key()?; // fail before looking up models
    let model = match args["model"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        Some(m) => m.to_string(),
        None => {
            let list = get_json(ctx, "/status/models", false).await?;
            list.as_array()
                .and_then(|a| a.iter().find(|m| m["type"] == "image"))
                .and_then(|m| m["name"].as_str())
                .ok_or("No image model is online on the grid right now.")?
                .to_string()
        }
    };
    let body = json!({ "model": model, "prompt": prompt, "n": n, "size": size });
    let d = post_json(ctx, "/images/generations", &body, 900).await?;
    let items = d["data"].as_array().cloned().unwrap_or_default();
    let dir = ctx.images_dir();
    let files = ctx.image_files_dir();
    let mut content = Vec::new();
    let mut lines = Vec::new();
    let mut inlined = 0usize;
    for it in &items {
        let bytes: Vec<u8> = if let Some(s) = it["b64_json"].as_str() {
            b64.decode(s.trim()).map_err(|e| format!("Bad image data from the grid: {e}"))?
        } else if let Some(url) = it["url"].as_str().filter(|u| u.starts_with("https://")) {
            let r = ctx
                .http
                .get(url)
                .timeout(Duration::from_secs(120))
                .send()
                .await
                .map_err(|e| format!("Could not download the image: {e}"))?;
            if !r.status().is_success() {
                return Err(format!("Could not download the image: HTTP {}", r.status()));
            }
            r.bytes().await.map_err(|e| e.to_string())?.to_vec()
        } else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        let meta = json!({ "prompt": prompt, "model": model, "size": size, "mode": "txt2img", "source": "connector" });
        match store_image(&dir, &files, &bytes, meta) {
            Ok(path) => lines.push(format!("Saved: {}", path.display())),
            Err(err) => lines.push(format!("Could not save the image locally: {err}")),
        }
        if let Some(url) = it["url"].as_str() {
            lines.push(format!("URL: {url}"));
        }
        if inlined + bytes.len() <= INLINE_IMAGE_BYTES {
            inlined += bytes.len();
            content.push(json!({ "type": "image", "data": b64.encode(&bytes), "mimeType": image_kind(&bytes).1 }));
        }
    }
    if lines.is_empty() {
        return Err("The grid returned no image data.".into());
    }
    let mut out = vec![text(format!(
        "Generated on AI Power Grid with {model} ({size}). The images are in the AIPG Desktop gallery.\n{}",
        lines.join("\n")
    ))];
    out.extend(content);
    Ok(out)
}

fn usd(v: &Value) -> String {
    match v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())) {
        Some(x) => format!("${x:.3}"),
        None => "n/a".into(),
    }
}

async fn grid_models(ctx: &Ctx) -> Result<Vec<Value>, String> {
    let list = get_json(ctx, "/status/models", false).await?;
    let mut lines = Vec::new();
    for m in list.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let mut parts = vec![
            m["name"].as_str().unwrap_or("?").to_string(),
            m["type"].as_str().unwrap_or("?").to_string(),
            format!("{} worker(s)", m["count"].as_u64().unwrap_or(0)),
        ];
        if let Some(l) = m["avg_latency_s"].as_f64() {
            parts.push(format!("~{l:.1} s"));
        }
        if let Some(t) = m["tokens_per_s"].as_f64() {
            parts.push(format!("{t:.0} tokens/s"));
        }
        if let Some(c) = m["max_context_length"].as_u64() {
            parts.push(format!("context {c}"));
        }
        if let Some(p) = m["pricing"].as_object() {
            for (k, v) in p {
                if let (Some(x), Some(label)) = (v.as_f64(), k.strip_suffix("_usd")) {
                    parts.push(format!("{} ${x}", label.replace('_', " ")));
                }
            }
        }
        lines.push(parts.join(" · "));
    }
    if lines.is_empty() {
        return Err("No models are online on the grid right now.".into());
    }
    Ok(vec![text(format!(
        "Models online on AI Power Grid (use the exact name as `model`; text also accepts \"auto\"):\n{}",
        lines.join("\n")
    ))])
}

async fn grid_balance(ctx: &Ctx) -> Result<Vec<Value>, String> {
    let c = get_json(ctx, "/account/credits", true).await?;
    let free = &c["free"];
    let mut lines = vec![format!("Spendable now: {}", usd(&c["total_spendable_usd"]))];
    if free.is_object() {
        lines.push(format!(
            "Free today: {} of {} (resets at UTC midnight){}{}",
            usd(&free["remaining_usd"]),
            usd(&free["daily_cap_usd"]),
            if free["active"] == false { " — display only, not spendable yet" } else { "" },
            if free["holder_bonus_active"] == true { " — holder bonus active" } else { "" },
        ));
    }
    if c["promotional"].is_object() {
        lines.push(format!("Promotional credit: {}", usd(&c["promotional"]["remaining_usd"])));
    }
    if c["paid"].is_object() {
        lines.push(format!("Purchased balance: {}", usd(&c["paid"]["balance_usd"])));
    }
    if c["charging_enabled"] == false {
        lines.push("Charging is currently off for this account (metering preview).".into());
    }
    Ok(vec![text(lines.join("\n"))])
}
