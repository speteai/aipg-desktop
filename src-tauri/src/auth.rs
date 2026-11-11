//! Sign-in through the browser, without an API key.
//!
//! The grid is an OAuth 2.1 server for public clients: the app registers
//! itself, opens the Console's consent page in the browser and receives a
//! one-use code on a loopback address (PKCE S256). The code is exchanged for
//! an access token that lasts 15 minutes and a refresh token that renews it.
//!
//! The refresh token rotates: every renewal returns a new one, and presenting
//! an old one a second time ends the whole session. The app and the connector
//! process therefore share one token file and renew it under one lock.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Issuer, token audience and API origin are the same address.
pub const BASE: &str = "https://api.aipowergrid.io";
const FILE: &str = "oauth.json";
const LOCK: &str = "oauth.lock";
const SCOPE: &str = "account.read inference.submit offline_access";
/// Renew once less than this many seconds are left on the access token.
const RENEW_BEFORE: u64 = 300;
/// How long the browser step may take.
const SIGNIN_WAIT: Duration = Duration::from_secs(300);
pub const ENDED: &str = "Your sign-in has ended. Sign in again in AIPG Desktop → Settings.";

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).map_err(|e| format!("No random numbers available: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

/// PKCE S256 challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The stored session, if there is one.
pub fn load(dir: &Path) -> Option<Value> {
    std::fs::read_to_string(file(dir))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v["refresh_token"].as_str().map(|s| !s.is_empty()).unwrap_or(false))
}

pub fn signed_in(dir: &Path) -> bool {
    load(dir).is_some()
}

/// Written to a temporary file first, so a crash never leaves half a session.
fn save(dir: &Path, v: &Value) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{FILE}.{}.tmp", std::process::id()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(v.to_string().as_bytes()).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, v.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, file(dir)).map_err(|e| e.to_string())
}

pub fn clear(dir: &Path) {
    let _ = std::fs::remove_file(file(dir));
}

/// Lock file shared by the app and the connector process. Removed on drop.
struct Lock(Option<PathBuf>);

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Waits for the lock. A lock older than 40 seconds belongs to a process that
/// died (a renewal takes at most 20) and is taken over.
async fn lock(dir: &Path) -> Lock {
    let path = dir.join(LOCK);
    let _ = std::fs::create_dir_all(dir);
    for _ in 0..600 {
        if std::fs::OpenOptions::new().write(true).create_new(true).open(&path).is_ok() {
            return Lock(Some(path));
        }
        let stale = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs() >= 40)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(&path);
        } else {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Lock(None)
}

fn usable(v: &Value, margin: u64) -> Option<String> {
    let token = v["access_token"].as_str().filter(|s| !s.is_empty())?;
    (v["expires_at"].as_u64().unwrap_or(0) > now() + margin).then(|| token.to_string())
}

/// A valid access token, renewed if it is about to expire. `Ok(None)` means
/// nobody is signed in.
pub async fn bearer(dir: &Path, http: &reqwest::Client, base: &str) -> Result<Option<String>, String> {
    let Some(v) = load(dir) else { return Ok(None) };
    if let Some(t) = usable(&v, RENEW_BEFORE) {
        return Ok(Some(t));
    }
    let _guard = lock(dir).await;
    // another process may have renewed it while this one waited
    let Some(v) = load(dir) else { return Ok(None) };
    if let Some(t) = usable(&v, RENEW_BEFORE) {
        return Ok(Some(t));
    }
    let client = v["client_id"].as_str().unwrap_or("");
    let refresh = v["refresh_token"].as_str().unwrap_or("");
    let sent = http
        .post(format!("{base}/v1/oauth/token"))
        .timeout(Duration::from_secs(20))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", client),
            ("resource", base),
        ])
        .send()
        .await;
    // Not renewed, but the grid did not say the session is over: keep it and
    // use the old token for as long as it lasts.
    let keep = |why: String| match usable(&v, 5) {
        Some(t) => Ok(Some(t)),
        None => Err(format!("The sign-in could not be renewed: {why}")),
    };
    let r = match sent {
        Ok(r) => r,
        Err(e) => return keep(format!("the grid could not be reached ({e}).")),
    };
    let status = r.status().as_u16();
    let body: Value = r.json().await.unwrap_or(Value::Null);
    if status == 200 {
        let (Some(access), Some(next)) = (body["access_token"].as_str(), body["refresh_token"].as_str()) else {
            return keep("the grid sent an incomplete answer.".into());
        };
        let mut v = v.clone();
        v["access_token"] = json!(access);
        v["refresh_token"] = json!(next);
        v["expires_at"] = json!(now() + body["expires_in"].as_u64().unwrap_or(900));
        save(dir, &v)?;
        return Ok(Some(access.to_string()));
    }
    if body["error"].as_str() == Some("invalid_grant") {
        clear(dir);
        return Err(ENDED.into());
    }
    keep(format!("the grid answered HTTP {status}."))
}

/// A sign-in that waits for the browser.
pub struct Pending {
    listener: TcpListener,
    /// The page to open in the browser.
    pub url: String,
    client_id: String,
    verifier: String,
    state: String,
    redirect: String,
}

/// Register this installation as a client and prepare the consent URL.
pub async fn begin(http: &reqwest::Client, base: &str) -> Result<Pending, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("Could not open a local port for the sign-in: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let r = http
        .post(format!("{base}/v1/oauth/register"))
        .timeout(Duration::from_secs(20))
        .json(&json!({
            "client_name": "AIPG Desktop",
            "redirect_uris": [redirect],
            "application_type": "native",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .map_err(|e| format!("Could not reach the grid: {e}"))?;
    let status = r.status().as_u16();
    let body: Value = r.json().await.unwrap_or(Value::Null);
    let Some(client_id) = body["client_id"].as_str().filter(|s| !s.is_empty()) else {
        let detail = body["detail"].as_str().unwrap_or("no details");
        return Err(format!("The grid did not accept the sign-in request (HTTP {status}: {detail}). You can use an API key instead."));
    };
    let verifier = random(48)?;
    let state = random(24)?;
    let url = format!(
        "{base}/v1/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256&resource={}",
        enc(client_id),
        enc(&redirect),
        enc(SCOPE),
        enc(&state),
        enc(&challenge(&verifier)),
        enc(base),
    );
    Ok(Pending { listener, url, client_id: client_id.to_string(), verifier, state, redirect })
}

fn page(title: &str, text: &str) -> String {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>AIPG Desktop</title><body style=\"font-family:system-ui,sans-serif;background:#0f1216;color:#e8eaed;display:grid;place-items:center;height:100vh;margin:0\"><div style=\"text-align:center;max-width:420px;padding:24px\"><h2>{title}</h2><p>{text}</p></div>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Wait for the browser to come back with the code. Only a request that
/// carries this sign-in's `state` is accepted; anything else is ignored.
fn wait_for_code(listener: TcpListener, state: String, issuer: String, cancel: Arc<AtomicBool>) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("Sign-in cancelled.".into());
        }
        if started.elapsed() > SIGNIN_WAIT {
            return Err("The sign-in was not completed in the browser within five minutes.".into());
        }
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let mut buf = [0u8; 8192];
        let mut len = 0;
        while len < buf.len() {
            match stream.read(&mut buf[len..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    len += n;
                    if buf[..len].windows(2).any(|w| w == b"\r\n") {
                        break;
                    }
                }
            }
        }
        let head = String::from_utf8_lossy(&buf[..len]);
        let target = head.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("");
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let get = |name: &str| {
            query.split('&').find_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                (k == name).then(|| dec(v))
            })
        };
        if path != "/callback" || get("state").as_deref() != Some(state.as_str()) {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        if get("iss").map(|i| i.trim_end_matches('/') != issuer).unwrap_or(false) {
            let _ = stream.write_all(page("Sign-in failed", "The answer did not come from AI Power Grid. Nothing was saved.").as_bytes());
            return Err("The sign-in answer did not come from AI Power Grid.".into());
        }
        if let Some(code) = get("code").filter(|c| !c.is_empty()) {
            let _ = stream.write_all(page("Signed in", "You can close this tab and return to AIPG Desktop.").as_bytes());
            return Ok(code);
        }
        let _ = stream.write_all(page("Not signed in", "The request was not approved. You can close this tab.").as_bytes());
        return Err(match get("error").as_deref() {
            Some("access_denied") | None => "The sign-in was not approved in the browser.".to_string(),
            Some(e) => format!("The sign-in failed in the browser ({e})."),
        });
    }
}

/// Wait for the browser, exchange the code and store the session.
pub async fn finish(p: Pending, dir: &Path, http: &reqwest::Client, base: &str, cancel: Arc<AtomicBool>) -> Result<(), String> {
    let Pending { listener, client_id, verifier, state, redirect, .. } = p;
    let issuer = base.to_string();
    let code = tokio::task::spawn_blocking(move || wait_for_code(listener, state, issuer, cancel))
        .await
        .map_err(|e| e.to_string())??;
    let r = http
        .post(format!("{base}/v1/oauth/token"))
        .timeout(Duration::from_secs(20))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier.as_str()),
            ("resource", base),
        ])
        .send()
        .await
        .map_err(|e| format!("Could not reach the grid: {e}"))?;
    let status = r.status().as_u16();
    let body: Value = r.json().await.unwrap_or(Value::Null);
    let Some(access) = body["access_token"].as_str().filter(|s| !s.is_empty()) else {
        let detail = body["error_description"].as_str().or(body["detail"].as_str()).unwrap_or("no details");
        return Err(format!("The grid did not complete the sign-in (HTTP {status}: {detail})."));
    };
    let Some(refresh) = body["refresh_token"].as_str().filter(|s| !s.is_empty()) else {
        return Err("The grid did not grant a lasting sign-in, so it would end after 15 minutes. Use an API key instead.".into());
    };
    let _guard = lock(dir).await;
    save(
        dir,
        &json!({
            "client_id": client_id,
            "access_token": access,
            "refresh_token": refresh,
            "expires_at": now() + body["expires_in"].as_u64().unwrap_or(900),
            "since": now(),
        }),
    )
}

/// End the session at the grid and forget it here.
pub async fn sign_out(dir: &Path, http: &reqwest::Client, base: &str) {
    let _guard = lock(dir).await;
    if let Some(v) = load(dir) {
        let _ = http
            .post(format!("{base}/v1/oauth/revoke"))
            .timeout(Duration::from_secs(15))
            .form(&[
                ("token", v["refresh_token"].as_str().unwrap_or("")),
                ("client_id", v["client_id"].as_str().unwrap_or("")),
            ])
            .send()
            .await;
    }
    clear(dir);
}
