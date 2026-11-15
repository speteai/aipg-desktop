//! Web search for the chat and for the connector.
//!
//! The search runs here, on the user's computer, not on a grid worker: the
//! worker only ever sees the results that are put into the prompt.
//!
//! Sources that need no key: Google News' public feed (recent headlines) and
//! Wikipedia's open API (background). With the user's own key, a general web
//! search is added: Tavily (keys start with `tvly-`) or Brave Search.
//! A single page can be read as text as well.
//!
//! Everything returned here is third-party text. The callers put it into a
//! frame that says so.

use std::net::{IpAddr, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};

const UA: &str = "Mozilla/5.0 (compatible; aipg-desktop)";
const PAGE_BYTES: usize = 2_000_000;
const PAGE_CHARS: usize = 12_000;

#[derive(Serialize, Clone)]
pub struct Hit {
    /// "web", "news", "wiki" or "page"
    pub kind: String,
    pub title: String,
    pub url: String,
    pub source: String,
    /// YYYY-MM-DD when known
    pub date: String,
    pub text: String,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

/// The user's own search key, kept in the app's config file.
pub fn stored_key(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(data_dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["web_key"].as_str().map(|k| k.trim().to_string()))
        .filter(|k| !k.is_empty())
}

fn lang_of(lang: &str) -> String {
    let l: String = lang.chars().take(2).collect::<String>().to_ascii_lowercase();
    if l.len() == 2 && l.chars().all(|c| c.is_ascii_lowercase()) {
        l
    } else {
        "en".into()
    }
}

fn clip(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

fn unescape(s: &str) -> String {
    let named = s
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    // numeric references, decimal and hexadecimal
    let re = Regex::new(r"&#(x[0-9a-fA-F]{1,6}|[0-9]{1,7});").expect("static pattern");
    re.replace_all(&named, |c: &regex::Captures| {
        let n = &c[1];
        let code = if let Some(hex) = n.strip_prefix('x') { u32::from_str_radix(hex, 16).ok() } else { n.parse::<u32>().ok() };
        code.and_then(char::from_u32).map(|ch| ch.to_string()).unwrap_or_default()
    })
    .into_owned()
}

fn strip_tags(s: &str) -> String {
    let re = Regex::new(r"(?s)<[^>]*>").expect("static pattern");
    unescape(&re.replace_all(s, " "))
}

/// "Fri, 13 Nov 2026 08:00:00 GMT" -> "2026-11-13"
fn rfc2822_day(s: &str) -> String {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 4 {
        return String::new();
    }
    let month = match parts[2].to_ascii_lowercase().as_str() {
        "jan" => 1, "feb" => 2, "mar" => 3, "apr" => 4, "may" => 5, "jun" => 6,
        "jul" => 7, "aug" => 8, "sep" => 9, "oct" => 10, "nov" => 11, "dec" => 12,
        _ => return String::new(),
    };
    match (parts[1].parse::<u32>(), parts[3].parse::<u32>()) {
        (Ok(d), Ok(y)) if (1..=31).contains(&d) && y > 1990 => format!("{y:04}-{month:02}-{d:02}"),
        _ => String::new(),
    }
}

fn tag(item: &str, name: &str) -> String {
    let re = Regex::new(&format!(r"(?s)<{name}[^>]*>(.*?)</{name}>")).expect("static pattern");
    re.captures(item)
        .map(|c| unescape(&c[1].replace("<![CDATA[", "").replace("]]>", "")).trim().to_string())
        .unwrap_or_default()
}

async fn news(http: &reqwest::Client, query: &str, lang: &str) -> Vec<Hit> {
    let (hl, gl) = match lang {
        "de" => ("de", "DE"),
        "fr" => ("fr", "FR"),
        "es" => ("es", "ES"),
        "it" => ("it", "IT"),
        "nl" => ("nl", "NL"),
        "pt" => ("pt-BR", "BR"),
        _ => ("en-US", "US"),
    };
    let ceid = format!("{gl}:{}", hl.split('-').next().unwrap_or("en"));
    let q = format!("{query} when:14d");
    let Ok(url) = reqwest::Url::parse_with_params("https://news.google.com/rss/search", &[("q", q.as_str()), ("hl", hl), ("gl", gl), ("ceid", ceid.as_str())]) else {
        return Vec::new();
    };
    let Ok(r) = http.get(url).send().await else { return Vec::new() };
    if !r.status().is_success() {
        return Vec::new();
    }
    let Ok(xml) = r.text().await else { return Vec::new() };
    let mut out: Vec<Hit> = Vec::new();
    for item in xml.split("<item>").skip(1) {
        let source = tag(item, "source");
        let mut title = tag(item, "title");
        if !source.is_empty() {
            if let Some(t) = title.strip_suffix(&format!(" - {source}")) {
                title = t.to_string();
            }
        }
        let link = tag(item, "link");
        if title.is_empty() || !link.starts_with("https://") {
            continue;
        }
        if out.iter().any(|h| h.title.eq_ignore_ascii_case(&title)) {
            continue;
        }
        out.push(Hit { kind: "news".into(), title: clip(&title, 160), url: link, source: clip(&source, 40), date: rfc2822_day(&tag(item, "pubDate")), text: String::new() });
    }
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out.truncate(6);
    out
}

async fn wiki(http: &reqwest::Client, query: &str, lang: &str) -> Vec<Hit> {
    let host = format!("https://{lang}.wikipedia.org/w/api.php");
    let params = [
        ("action", "query"), ("generator", "search"), ("gsrsearch", query), ("gsrlimit", "2"),
        ("prop", "extracts|info"), ("exintro", "1"), ("explaintext", "1"), ("exchars", "600"),
        ("inprop", "url"), ("format", "json"), ("utf8", "1"),
    ];
    let Ok(url) = reqwest::Url::parse_with_params(&host, &params) else { return Vec::new() };
    let Ok(r) = http.get(url).send().await else { return Vec::new() };
    let Ok(v) = r.json::<Value>().await else { return Vec::new() };
    let mut pages: Vec<&Value> = v["query"]["pages"].as_object().map(|m| m.values().collect()).unwrap_or_default();
    pages.sort_by_key(|p| p["index"].as_u64().unwrap_or(99));
    pages
        .into_iter()
        .filter_map(|p| {
            let text = clip(p["extract"].as_str().unwrap_or(""), 600);
            let url = p["fullurl"].as_str().unwrap_or("");
            if text.is_empty() || !url.starts_with("https://") {
                return None;
            }
            Some(Hit {
                kind: "wiki".into(),
                title: clip(p["title"].as_str().unwrap_or(""), 120),
                url: url.to_string(),
                source: "Wikipedia".into(),
                date: p["touched"].as_str().map(|t| t.chars().take(10).collect()).unwrap_or_default(),
                text,
            })
        })
        .collect()
}

/// General web search with the user's own key. Errors are returned, so a wrong
/// key is named instead of silently giving fewer results.
async fn keyed(http: &reqwest::Client, query: &str, key: &str) -> Result<Vec<Hit>, String> {
    let host = |u: &str| reqwest::Url::parse(u).ok().and_then(|x| x.host_str().map(|h| h.trim_start_matches("www.").to_string())).unwrap_or_default();
    if key.starts_with("tvly-") {
        let r = http
            .post("https://api.tavily.com/search")
            .bearer_auth(key)
            .json(&json!({ "query": query, "max_results": 5, "search_depth": "basic" }))
            .send()
            .await
            .map_err(|e| format!("Tavily could not be reached: {e}"))?;
        if !r.status().is_success() {
            return Err(format!("Tavily answered HTTP {} (check the search key in Settings).", r.status().as_u16()));
        }
        let v: Value = r.json().await.map_err(|e| e.to_string())?;
        return Ok(v["results"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        let url = x["url"].as_str().filter(|u| u.starts_with("http"))?;
                        Some(Hit {
                            kind: "web".into(),
                            title: clip(x["title"].as_str().unwrap_or(url), 160),
                            url: url.to_string(),
                            source: host(url),
                            date: x["published_date"].as_str().map(|d| if d.contains(',') { rfc2822_day(d) } else { d.chars().take(10).collect() }).unwrap_or_default(),
                            text: clip(x["content"].as_str().unwrap_or(""), 500),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default());
    }
    let url = reqwest::Url::parse_with_params("https://api.search.brave.com/res/v1/web/search", &[("q", query), ("count", "5")]).map_err(|e| e.to_string())?;
    let r = http
        .get(url)
        .header("X-Subscription-Token", key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("Brave Search could not be reached: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("Brave Search answered HTTP {} (check the search key in Settings).", r.status().as_u16()));
    }
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    Ok(v["web"]["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let url = x["url"].as_str().filter(|u| u.starts_with("http"))?;
                    Some(Hit {
                        kind: "web".into(),
                        title: clip(&strip_tags(x["title"].as_str().unwrap_or(url)), 160),
                        url: url.to_string(),
                        source: host(url),
                        date: x["page_age"].as_str().map(|d| d.chars().take(10).collect()).unwrap_or_default(),
                        text: clip(&strip_tags(x["description"].as_str().unwrap_or("")), 500),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

pub struct Found {
    pub hits: Vec<Hit>,
    /// set when the keyed search failed; the other sources are still returned
    pub warning: String,
}

/// Search the web for `query`. `lang` is a two-letter language code for the
/// news edition and the Wikipedia language.
pub async fn search(data_dir: &Path, query: &str, lang: &str) -> Result<Found, String> {
    let query = clip(query, 300);
    if query.chars().count() < 2 {
        return Err("The search needs a query.".into());
    }
    let lang = lang_of(lang);
    let http = client()?;
    let key = stored_key(data_dir);
    let (n, w, k) = tokio::join!(news(&http, &query, &lang), wiki(&http, &query, &lang), async {
        match &key {
            Some(key) => keyed(&http, &query, key).await,
            None => Ok(Vec::new()),
        }
    });
    let mut warning = String::new();
    let mut hits = match k {
        Ok(h) => h,
        Err(e) => {
            warning = e;
            Vec::new()
        }
    };
    hits.extend(n.into_iter().take(if hits.is_empty() { 6 } else { 4 }));
    hits.extend(w);
    Ok(Found { hits, warning })
}

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified() || a.is_broadcast() || a.is_multicast()
                || a.octets()[0] == 0
                || (a.octets()[0] == 100 && (64..128).contains(&a.octets()[1])) // carrier-grade NAT
        }
        IpAddr::V6(a) => {
            a.is_loopback() || a.is_unspecified() || a.is_multicast()
                || (a.segments()[0] & 0xfe00) == 0xfc00 // unique local
                || (a.segments()[0] & 0xffc0) == 0xfe80 // link local
                || a.to_ipv4_mapped().map(|v4| private_ip(IpAddr::V4(v4))).unwrap_or(false)
        }
    }
}

/// Only public web addresses: a page URL can come from a model or from a web
/// page, and must not reach the user's router, other machines at home, or
/// services on this computer.
async fn public_target(url: &reqwest::Url) -> Result<(), String> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Only http and https addresses can be read.".into());
    }
    let host = url.host_str().ok_or("The address has no host.")?.to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = tokio::task::spawn_blocking(move || (host.as_str(), port).to_socket_addrs().map(|a| a.collect::<Vec<_>>()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "The address could not be resolved.".to_string())?;
    if addrs.is_empty() {
        return Err("The address could not be resolved.".into());
    }
    if addrs.iter().any(|a| private_ip(a.ip())) {
        return Err("That address points into a private network or to this computer; only public web pages can be read.".into());
    }
    Ok(())
}

fn html_text(html: &str) -> (String, String) {
    let title = Regex::new(r"(?is)<title[^>]*>(.*?)</title>")
        .expect("static pattern")
        .captures(html)
        .map(|c| clip(&unescape(&c[1]), 160))
        .unwrap_or_default();
    let mut body = html.to_string();
    for name in ["script", "style", "noscript", "svg", "nav", "footer", "header", "form", "aside", "template", "iframe"] {
        let re = Regex::new(&format!(r"(?is)<{name}\b.*?</{name}\s*>")).expect("static pattern");
        body = re.replace_all(&body, " ").into_owned();
    }
    body = Regex::new(r"(?s)<!--.*?-->").expect("static pattern").replace_all(&body, " ").into_owned();
    // keep paragraph breaks readable
    body = Regex::new(r"(?i)</(p|div|li|h[1-6]|tr|section|article)\s*>|<br\s*/?>").expect("static pattern").replace_all(&body, "\n").into_owned();
    let text = strip_tags(&body);
    let lines: Vec<String> = text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
    (title, lines.join("\n"))
}

/// Read one public page as text.
pub async fn page(url: &str) -> Result<Hit, String> {
    let mut url = reqwest::Url::parse(url.trim()).map_err(|_| "That is not a web address.".to_string())?;
    // redirects are followed by hand, so every hop is checked like the first address
    let http = reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let mut hops = 0;
    let resp = loop {
        public_target(&url).await?;
        let r = http.get(url.clone()).header("Accept", "text/html,text/plain;q=0.9").send().await.map_err(|e| format!("The page could not be loaded: {e}"))?;
        if r.status().is_redirection() {
            hops += 1;
            let next = r.headers().get("location").and_then(|l| l.to_str().ok()).and_then(|l| url.join(l).ok());
            match next {
                Some(n) if hops <= 5 => {
                    url = n;
                    continue;
                }
                _ => return Err("The page redirects too often or to nowhere.".into()),
            }
        }
        break r;
    };
    if !resp.status().is_success() {
        return Err(format!("The page answered HTTP {}.", resp.status().as_u16()));
    }
    let kind = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if !(kind.contains("text/") || kind.contains("xhtml") || kind.contains("json") || kind.is_empty()) {
        return Err(format!("The address does not hold a text page ({kind})."));
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("The page could not be loaded: {e}"))?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() >= PAGE_BYTES {
            break; // the beginning is enough
        }
    }
    let raw = String::from_utf8_lossy(&bytes);
    let (title, text) = if kind.contains("html") || raw.trim_start().starts_with('<') { html_text(&raw) } else { (String::new(), raw.trim().to_string()) };
    if text.chars().count() < 40 {
        return Err("The page holds no readable text (it may need a browser to show its content).".into());
    }
    let cut = text.chars().count() > PAGE_CHARS;
    let mut text: String = text.chars().take(PAGE_CHARS).collect();
    if cut {
        text.push_str("\n[…the page continues]");
    }
    let host = url.host_str().unwrap_or("").trim_start_matches("www.").to_string();
    Ok(Hit { kind: "page".into(), title: if title.is_empty() { host.clone() } else { title }, url: url.to_string(), source: host, date: String::new(), text })
}

#[derive(Serialize)]
pub struct SearchReply {
    hits: Vec<Hit>,
    warning: String,
    /// whether a key for the general web search is stored
    keyed: bool,
}

#[tauri::command]
pub async fn web_search(app: tauri::AppHandle, query: String, lang: String) -> Result<SearchReply, String> {
    let dir = crate::worker::data_dir(&app);
    let found = search(&dir, &query, &lang).await?;
    Ok(SearchReply { hits: found.hits, warning: found.warning, keyed: stored_key(&dir).is_some() })
}

#[tauri::command]
pub async fn web_page(url: String) -> Result<Hit, String> {
    page(&url).await
}
