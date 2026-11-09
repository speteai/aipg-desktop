"use strict";
const API = "https://api.aipowergrid.io/v1";
const LS = {
  key: "aipg_key", ntfy: "aipg_ntfy", alerts: "aipg_alerts",
  theme: "aipg_theme", wstate: "aipg_wstate",
};
const $ = (s) => document.querySelector(s);
const $$ = (s) => document.querySelectorAll(s);
const get = (k) => { try { return localStorage.getItem(k); } catch (e) { return null; } };
const set = (k, v) => { try { localStorage.setItem(k, v); } catch (e) {} };
const del = (k) => { try { localStorage.removeItem(k); } catch (e) {} };

let POLL = null;

/* ---------- theme ---------- */
function applyTheme(t) {
  if (t) document.documentElement.setAttribute("data-theme", t);
  else document.documentElement.removeAttribute("data-theme");
}
applyTheme(get(LS.theme));

/* ---------- toast ---------- */
let toastT;
function toast(msg) {
  const el = $("#toast"); el.textContent = msg; el.classList.add("show");
  clearTimeout(toastT); toastT = setTimeout(() => el.classList.remove("show"), 2200);
}

/* ---------- native helpers (Tauri plugins, feature-detected) ---------- */
async function nativeNotify(title, body) {
  const n = window.__TAURI__ && window.__TAURI__.notification;
  if (!n) return false;
  try {
    let granted = await n.isPermissionGranted();
    if (!granted) granted = (await n.requestPermission()) === "granted";
    if (granted) { n.sendNotification({ title, body }); return true; }
  } catch (e) {}
  return false;
}
function openExternal(url) {
  const o = window.__TAURI__ && window.__TAURI__.opener;
  if (o && o.openUrl) { o.openUrl(url); return; }
  try { window.open(url, "_blank"); } catch (e) {}
}

/* ---------- API ---------- */
function apiKey() { return (get(LS.key) || "").trim(); }
async function api(path, auth = true) {
  const headers = { "Accept": "application/json" };
  if (auth) {
    const k = apiKey();
    if (!k) throw new Error("no-key");
    headers["Authorization"] = "Bearer " + k;
  }
  const r = await fetch(API + path, { headers });
  if (!r.ok) {
    let detail = "";
    try { detail = (await r.json()).detail || ""; } catch (e) {}
    const err = new Error(`HTTP ${r.status}${detail ? ": " + detail : ""}`);
    err.status = r.status; throw err;
  }
  return r.json();
}
async function ntfyPush(title, message, tags) {
  const topic = (get(LS.ntfy) || "").trim();
  if (!topic) return;
  try {
    await fetch("https://ntfy.sh/" + topic, {
      method: "POST", body: message,
      headers: { "Title": title.replace(/[^\x00-\x7F]/g, ""), "Tags": tags || "" },
    });
  } catch (e) {}
}

/* ---------- connection badge ---------- */
function setConn(state, label) {
  const c = $("#conn"); c.className = "conn" + (state ? " " + state : "");
  $("#connlabel").textContent = label;
}

/* ---------- renderers ---------- */
function money(v) { return v == null ? "—" : "$" + Number(v).toFixed(3); }

async function loadOverview() {
  const hasKey = !!apiKey();
  $("#needkey").style.display = hasKey ? "none" : "";
  if (!hasKey) { setConn("", "not connected"); $("#stats").innerHTML = ""; $("#workers").innerHTML = '<p class="muted">—</p>'; return; }
  setConn("", "loading…");
  try {
    const [w, c] = await Promise.all([api("/account/workers"), api("/account/credits")]);
    setConn("ok", "connected");
    $("#stats").innerHTML = `
      ${stat("Spendable", money(c.total_spendable_usd), "green")}
      ${stat("Workers online", `${w.online || 0} / ${w.count || 0}`, "blue")}
      ${stat("Den earned", (w.den_earned ?? 0), "")}
      ${stat("Jobs completed", (w.jobs_completed ?? 0), "")}`;
    renderWorkers(w.workers || []);
    $("#stamp").textContent = "Updated " + new Date().toLocaleTimeString();
    checkTransitions(w.workers || []);
  } catch (e) {
    if (e.message === "no-key") return;
    setConn("err", e.status === 401 ? "bad API key" : "error");
    toast(e.message);
  }
}
function stat(k, v, cls) {
  return `<div class="stat"><div class="k">${k}</div><div class="v ${cls || ""}">${v}</div></div>`;
}
function renderWorkers(workers) {
  if (!workers.length) { $("#workers").innerHTML = '<p class="muted">No workers on this account yet.</p>'; return; }
  $("#workers").innerHTML = workers.map((w) => {
    const on = !!w.online, maint = !!w.maintenance;
    const badge = maint ? '<span class="chip">maintenance</span>'
      : on ? '<span class="chip on">online</span>' : '<span class="chip off">offline</span>';
    const models = (w.models || []).join(", ") || "—";
    return `<div class="card"><h3>${esc(w.name)}</h3>
      <div class="chips">${badge}<span class="chip">${esc(w.type || "")}</span></div>
      <div class="wmeta">models: ${esc(models)}<br>den: ${w.den_earned ?? 0} · jobs: ${w.jobs_completed ?? 0}</div></div>`;
  }).join("");
}

async function loadModels() {
  try {
    const d = await api("/models", false);
    $("#models").innerHTML = (d.data || []).map((m) => {
      const mods = m.input_modalities || [];
      let chips = "";
      if (mods.includes("text")) chips += '<span class="chip txt">text</span>';
      if (mods.includes("image")) chips += '<span class="chip img">vision</span>';
      const p = m.pricing;
      const price = p ? `<span class="price"><span class="v">$${p.input_per_mtok_usd}</span> in · <span class="v">$${p.output_per_mtok_usd}</span> out /Mtok</span>`
        : '<span class="muted">no price</span>';
      return `<div class="card"><h3>${esc(m.id)}</h3><div class="chips">${chips || '<span class="chip">—</span>'}</div>${price}</div>`;
    }).join("");
  } catch (e) { $("#models").innerHTML = `<p class="muted">Could not load models: ${esc(e.message)}</p>`; }
  // image models (need key; discovered via error)
  const k = apiKey();
  if (!k) { $("#imgmodels").innerHTML = '<p class="muted">Add an API key to list image models.</p>'; return; }
  try {
    const r = await fetch(API + "/images/generations", {
      method: "POST", headers: { "Authorization": "Bearer " + k, "Content-Type": "application/json" },
      body: JSON.stringify({ model: "__discover__", prompt: "x" }),
    });
    const d = await r.json();
    const msg = d.detail || "";
    const m = msg.match(/\[([^\]]*)\]/);
    const list = m ? m[1].split(",").map((s) => s.replace(/['"]/g, "").trim()).filter(Boolean) : [];
    $("#imgmodels").innerHTML = list.length
      ? list.map((n) => `<div class="card"><h3>${esc(n)}</h3><div class="chips"><span class="chip img">image out</span></div></div>`).join("")
      : '<p class="muted">—</p>';
  } catch (e) { $("#imgmodels").innerHTML = '<p class="muted">—</p>'; }
}

async function loadEarnings() {
  try {
    const d = await api("/account/jobs");
    const jobs = d.jobs || [];
    $("#estats").innerHTML = `
      ${stat("Total den", d.total_den ?? 0, "green")}
      ${stat("Jobs", jobs.length, "blue")}`;
    const tb = $("#jobs tbody");
    tb.innerHTML = jobs.slice(0, 50).map((j) => `<tr>
      <td>${j.ts ? new Date(j.ts).toLocaleString() : (j.timestamp || "—")}</td>
      <td>${esc(j.model || "")}</td><td>${esc(j.type || "")}</td>
      <td>${j.den ?? "—"}</td><td>${j.output_units ?? "—"}</td>
      <td>${j.duration_s != null ? j.duration_s + "s" : "—"}</td></tr>`).join("")
      || '<tr><td colspan="6" class="muted">No jobs yet.</td></tr>';
  } catch (e) {
    $("#estats").innerHTML = "";
    $("#jobs tbody").innerHTML = `<tr><td colspan="6" class="muted">${e.message === "no-key" ? "Add an API key in Settings." : esc(e.message)}</td></tr>`;
  }
}

/* ---------- monitoring transitions ---------- */
function loadWState() { try { return JSON.parse(get(LS.wstate) || "{}"); } catch (e) { return {}; } }
function saveWState(s) { set(LS.wstate, JSON.stringify(s)); }
function checkTransitions(workers) {
  if (get(LS.alerts) !== "1") { saveWState(workerMap(workers)); return; }
  const prev = loadWState();
  for (const w of workers) {
    const p = prev[w.name] || {};
    const on = !!w.online, maint = !!w.maintenance;
    if (p.online === true && !on) {
      nativeNotify("Worker offline", `${w.name} dropped off the grid.`);
      ntfyPush("Worker offline: " + w.name, `${w.name} dropped off the grid.`, "warning");
    } else if (p.online === false && on) {
      nativeNotify("Worker back online", `${w.name} is serving again.`);
      ntfyPush("Worker back online: " + w.name, `${w.name} is serving again.`, "white_check_mark");
    }
    if (p.maintenance === false && maint) {
      nativeNotify("Worker in maintenance", `${w.name} entered maintenance.`);
    }
  }
  saveWState(workerMap(workers));
}
function workerMap(workers) {
  const m = {};
  for (const w of workers) m[w.name] = { online: !!w.online, maintenance: !!w.maintenance };
  return m;
}
function startPolling() {
  if (POLL) clearInterval(POLL);
  if (get(LS.alerts) === "1" && apiKey()) {
    POLL = setInterval(async () => {
      try { const w = await api("/account/workers"); checkTransitions(w.workers || []); } catch (e) {}
    }, 120000);
  }
}

/* ---------- worker (all-in-one, Tauri commands) ---------- */
const TAURI = window.__TAURI__ || {};
const invoke = (cmd, args) =>
  TAURI.core && TAURI.core.invoke ? TAURI.core.invoke(cmd, args) : Promise.reject(new Error("not-in-app"));
const listen = (ev, cb) =>
  TAURI.event && TAURI.event.listen ? TAURI.event.listen(ev, cb) : Promise.resolve(() => {});
// Tauri command errors arrive as plain strings, not Error objects.
const emsg = (e) => (e && typeof e === "object" && e.message) ? e.message : (e == null ? "unbekannter Fehler" : String(e));

let WORKER_WIRED = false, WORKER_RUNNING = false;

function logLine(s) {
  const el = $("#wlog");
  if (el.textContent === "—") el.textContent = "";
  el.textContent += (el.textContent ? "\n" : "") + s;
  el.scrollTop = el.scrollHeight;
}
function wsetStatus(txt, cls) { const c = $("#wstatus"); c.textContent = txt; c.className = "chip" + (cls ? " " + cls : ""); }
function bar(sel, frac) {
  const b = $(sel); b.style.display = ""; b.firstElementChild.style.width = Math.max(0, Math.min(1, frac)) * 100 + "%";
}

function renderPrereqs(p) {
  if (!p.supported) {
    $("#wunsupported").style.display = "";
    $("#wunsupported").innerHTML = `Für <b>${esc(p.os)}/${esc(p.arch)}</b> gibt es keine offizielle Worker-Binary (v0.3.12). ` +
      `Unterstützt: Linux x64/arm64, macOS (Apple Silicon), Windows x64.`;
  } else $("#wunsupported").style.display = "none";

  $("#ollama-state").innerHTML = p.ollama_running
    ? '<span class="chip run">running</span>'
    : (p.ollama_managed || p.ollama_installed) ? '<span class="chip warn">ready, stopped</span>' : '<span class="chip off">not set up</span>';
  $("#btn-setup-ollama").disabled = p.ollama_managed || p.ollama_running;
  $("#btn-setup-ollama").textContent = p.ollama_managed ? "Ollama ready" : "Set up Ollama";
  $("#btn-ollama").disabled = p.ollama_running || !(p.ollama_managed || p.ollama_installed);

  $("#worker-dl-state").innerHTML = p.worker_downloaded
    ? '<span class="chip run">downloaded</span>' : '<span class="chip">not downloaded</span>';
  $("#btn-download").disabled = !p.supported || p.worker_downloaded;
  $("#btn-download").textContent = p.worker_downloaded ? "Re-download" : "Download worker";
  if (p.worker_downloaded) $("#btn-download").disabled = false;

  const ready = p.supported && p.worker_downloaded;
  $("#btn-start").disabled = !ready || WORKER_RUNNING;
}

async function refreshOllamaModels() {
  try {
    const list = await invoke("ollama_models");
    $("#wmodels-list").innerHTML = (list || []).map((m) => `<option value="${esc(m)}">`).join("");
  } catch (e) {}
}

function renderHardware(h) {
  $("#hwpanel").innerHTML =
    `<span><span class="k">RAM</span> <b>${h.ram_gb} GB</b></span>` +
    `<span><span class="k">CPU</span> <b>${esc(h.cpu)}</b></span>` +
    `<span><span class="k">GPU</span> <b>${esc(h.gpu)}</b></span>` +
    (h.recommend ? `<span><span class="k">Suggested model</span> <b>${esc(h.recommend)}</b></span>` : "");
  if (!h.can_run_worker) {
    const b = $("#hwfallback"); b.style.display = "";
    b.innerHTML = esc(h.note) + ' <a href="#" id="gotochat"><b>Zum Grid-Chat →</b></a>';
    const g = $("#gotochat"); if (g) g.onclick = (e) => { e.preventDefault(); show("chat"); };
  } else {
    $("#hwfallback").style.display = "none";
    // prefill recommended model if the user hasn't chosen one
    if (h.recommend && !get("aipg_wmodel")) { $("#wmodel2").value = h.recommend; $("#wmodel").value = h.recommend; }
  }
}

async function loadWorker() {
  if (!TAURI.core) {
    $("#wunsupported").style.display = "";
    $("#wunsupported").textContent = "Dieser Bereich funktioniert nur in der installierten Desktop-App.";
    return;
  }
  try { renderHardware(await invoke("hardware")); } catch (e) {}
  $("#wname").value = get("aipg_wname") || "";
  $("#wmodel").value = get("aipg_wmodel") || "qwen3:8b";
  $("#wname2").value = get("aipg_wname") || "";
  $("#wmodel2").value = get("aipg_wmodel") || "qwen3:8b";
  if (!WORKER_WIRED) wireWorker();
  try { renderPrereqs(await invoke("prereqs")); } catch (e) { toast("prereqs: " + emsg(e)); }
  try { WORKER_RUNNING = await invoke("worker_running"); } catch (e) {}
  syncRunButtons();
  refreshOllamaModels();
}

function syncRunButtons() {
  const dl = $("#worker-dl-state").textContent.indexOf("downloaded") >= 0;
  $("#btn-stop").disabled = !WORKER_RUNNING;
  $("#btn-start").disabled = WORKER_RUNNING || !dl;
  $("#btn-stop2").disabled = !WORKER_RUNNING;
  $("#btn-auto").disabled = WORKER_RUNNING;
  wsetStatus(WORKER_RUNNING ? "running" : "idle", WORKER_RUNNING ? "run" : "");
}

async function stopWorker() {
  $("#btn-stop").disabled = $("#btn-stop2").disabled = true;
  try { await invoke("worker_stop"); } catch (e) {}
  WORKER_RUNNING = false; syncRunButtons();
}

async function autoStartWorker() {
  const api_key = apiKey();
  if (!api_key) { toast("Erst API-Key in Settings speichern"); return show("settings"); }
  const worker_name = $("#wname2").value.trim();
  const model = $("#wmodel2").value.trim() || "qwen3:8b";
  set("aipg_wname", worker_name); set("aipg_wmodel", model);
  $("#wname").value = worker_name; $("#wmodel").value = model;
  $("#btn-auto").disabled = true; wsetStatus("starting…", "warn");
  try {
    let p = await invoke("prereqs");
    if (!p.ollama_running && !p.ollama_managed && !p.ollama_installed) {
      logLine("[auto] Ollama einrichten (einmaliger Download)…"); bar("#olbar", 0);
      await invoke("setup_ollama"); $("#olbar").style.display = "none";
    }
    logLine("[auto] Ollama starten…"); await invoke("ollama_ensure");
    p = await invoke("prereqs");
    if (!p.worker_downloaded) { logLine("[auto] Worker-Binary laden (SHA-256-geprüft)…"); bar("#dlbar", 0); await invoke("download_worker"); $("#dlbar").style.display = "none"; }
    logLine("[auto] Modell " + model + " bereitstellen…"); bar("#pullbar", 0); await invoke("ollama_pull", { model }); $("#pullbar").style.display = "none";
    logLine("[auto] Worker starten…");
    await invoke("worker_start", { cfg: { api_key, model, worker_name } });
    WORKER_RUNNING = true; toast("Worker läuft");
  } catch (e) { toast(emsg(e)); logLine("[auto] Fehler: " + emsg(e)); }
  finally {
    try { renderPrereqs(await invoke("prereqs")); } catch (e) {}
    syncRunButtons();
  }
}

function wireWorker() {
  WORKER_WIRED = true;

  listen("worker-dl", (e) => {
    const { received, total } = e.payload || {};
    if (total) bar("#dlbar", received / total);
  });
  listen("ollama-dl", (e) => {
    const { received, total, status } = e.payload || {};
    if (status === "extracting") { $("#olmsg").textContent = "entpacke…"; bar("#olbar", 1); return; }
    if (total) { bar("#olbar", received / total); $("#olmsg").textContent = `downloading Ollama… ${Math.round(received / 1e6)} / ${Math.round(total / 1e6)} MB`; }
    else if (received) { bar("#olbar", 0.05); $("#olmsg").textContent = `downloading Ollama… ${Math.round(received / 1e6)} MB`; }
  });
  listen("ollama-pull", (e) => {
    const j = e.payload || {};
    if (j.total && j.completed != null) bar("#pullbar", j.completed / j.total);
    if (j.status) $("#pullmsg").textContent = j.status;
  });
  listen("worker-log", (e) => logLine(String(e.payload)));
  listen("worker-exit", () => {
    WORKER_RUNNING = false; syncRunButtons();
    logLine("[desktop] worker beendet.");
    nativeNotify("Worker stopped", "The worker process exited.");
  });

  $("#btn-setup-ollama").onclick = async () => {
    $("#btn-setup-ollama").disabled = true; $("#olmsg").textContent = "vorbereiten…"; bar("#olbar", 0);
    try { await invoke("setup_ollama"); $("#olmsg").textContent = "Ollama eingerichtet ✓"; toast("Ollama bereit"); }
    catch (e) { $("#olmsg").textContent = "⚠ " + emsg(e); toast(emsg(e)); }
    $("#olbar").style.display = "none";
    renderPrereqs(await invoke("prereqs"));
  };

  $("#btn-ollama").onclick = async () => {
    $("#btn-ollama").disabled = true; wsetStatus("starting Ollama…", "warn");
    try { await invoke("ollama_ensure"); toast("Ollama läuft"); }
    catch (e) { toast(emsg(e)); }
    renderPrereqs(await invoke("prereqs")); refreshOllamaModels(); syncRunButtons();
  };

  $("#btn-download").onclick = async () => {
    $("#btn-download").disabled = true; bar("#dlbar", 0);
    try { await invoke("download_worker"); toast("Worker heruntergeladen & verifiziert"); }
    catch (e) { toast(emsg(e)); logLine("[download] " + emsg(e)); }
    $("#dlbar").style.display = "none";
    renderPrereqs(await invoke("prereqs")); syncRunButtons();
  };

  $("#btn-pull").onclick = async () => {
    const model = $("#wmodel").value.trim(); if (!model) return toast("Modell-Tag eingeben");
    set("aipg_wmodel", model);
    $("#btn-pull").disabled = true; bar("#pullbar", 0); $("#pullmsg").textContent = "contacting Ollama…";
    try { await invoke("ollama_ensure"); await invoke("ollama_pull", { model }); toast("Modell bereit"); }
    catch (e) { toast(emsg(e)); $("#pullmsg").textContent = "Fehler: " + emsg(e); }
    $("#btn-pull").disabled = false; refreshOllamaModels();
  };

  $("#btn-start").onclick = async () => {
    const api_key = apiKey();
    if (!api_key) { toast("Erst API-Key in Settings speichern"); return show("settings"); }
    const model = $("#wmodel").value.trim();
    const worker_name = $("#wname").value.trim();
    set("aipg_wname", worker_name); set("aipg_wmodel", model);
    $("#btn-start").disabled = true;
    try {
      await invoke("ollama_ensure");
      await invoke("worker_start", { cfg: { api_key, model, worker_name } });
      WORKER_RUNNING = true; syncRunButtons();
    } catch (e) { toast(emsg(e)); logLine("[start] " + emsg(e)); syncRunButtons(); }
  };

  $("#btn-copylog").onclick = async () => {
    const txt = $("#wlog").textContent || "";
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        await navigator.clipboard.writeText(txt);
        toast("Log kopiert");
      } else throw new Error("no-clipboard");
    } catch (e) {
      // fallback: select the log so the user can Ctrl/Cmd+C
      const r = document.createRange(); r.selectNodeContents($("#wlog"));
      const sel = window.getSelection(); sel.removeAllRanges(); sel.addRange(r);
      toast("Markiert — mit Strg/Cmd+C kopieren");
    }
  };
  $("#btn-clearlog").onclick = () => { $("#wlog").textContent = "—"; };

  $("#btn-auto").onclick = autoStartWorker;
  $("#btn-stop").onclick = stopWorker;
  $("#btn-stop2").onclick = stopWorker;
}

/* ---------- chat (grid auto/model or local ollama, streaming) ---------- */
let CHAT_WIRED = false, CHAT_MSGS = [], CHAT_ABORT = null;
const OLLAMA_LOCAL = "http://127.0.0.1:11434/v1";

function chatSource() { const r = document.querySelector('input[name=chatsrc]:checked'); return r ? r.value : "grid"; }

async function fillChatModels() {
  const src = chatSource(), sel = $("#chatmodel");
  if (src === "grid") {
    let ids = ["auto"];
    try { const d = await api("/models", false); ids = (d.data || []).map((m) => m.id); } catch (e) {}
    ids.sort((a, b) => (a === "auto" ? -1 : b === "auto" ? 1 : 0));
    sel.innerHTML = ids.map((id) => `<option value="${esc(id)}">${id === "auto" ? "Auto (grid picks)" : esc(id)}</option>`).join("");
    $("#chathint").innerHTML = "Grid · <b>Auto</b> lets the grid pick the best model. Runs in AIPG's environment, uses your API key.";
  } else {
    let list = [];
    try { list = await invoke("ollama_models"); } catch (e) {}
    sel.innerHTML = list.length
      ? list.map((m) => `<option value="${esc(m)}">${esc(m)}</option>`).join("")
      : '<option value="">(no local models — pull one under “Run a worker”)</option>';
    $("#chathint").textContent = "Local · runs on your own Ollama, fully offline and free.";
  }
  const last = get(src === "grid" ? "aipg_chatgrid" : "aipg_chatlocal");
  if (last && [...sel.options].some((o) => o.value === last)) sel.value = last;
}

function addMsg(role, text, model) {
  const el = document.createElement("div");
  el.className = "msg " + (role === "user" ? "user" : "bot");
  if (role !== "user" && model) {
    const n = document.createElement("span"); n.className = "mname"; n.textContent = model; el.appendChild(n);
  }
  const body = document.createElement("span"); body.textContent = text; el.appendChild(body);
  $("#chatlog").appendChild(el); $("#chatlog").scrollTop = 1e9;
  return body;
}

async function chatSend() {
  const ta = $("#chattext"), text = ta.value.trim(); if (!text) return;
  const src = chatSource(), model = $("#chatmodel").value;
  if (src === "grid" && !apiKey()) { toast("Erst API-Key in Settings speichern"); return show("settings"); }
  if (src === "local" && !model) { toast("Kein lokales Modell — unter „Run a worker“ eines ziehen"); return; }
  set(src === "grid" ? "aipg_chatgrid" : "aipg_chatlocal", model);

  ta.value = ""; ta.style.height = "auto";
  addMsg("user", text);
  CHAT_MSGS.push({ role: "user", content: text });
  const body = addMsg("bot", "▍", src === "grid" ? model : "local · " + model);

  $("#chatsend").style.display = "none"; $("#chatstop").style.display = "";
  CHAT_ABORT = new AbortController();

  const url = (src === "grid" ? API : OLLAMA_LOCAL) + "/chat/completions";
  const headers = { "Content-Type": "application/json" };
  if (src === "grid") headers["Authorization"] = "Bearer " + apiKey();
  const payload = { model, messages: CHAT_MSGS, stream: true };
  if (/qwen3|gpt-oss|auto|deepseek|qwen38/i.test(model)) payload.max_tokens = 2048; // reasoning headroom

  let acc = "", think = "";
  try {
    const r = await fetch(url, { method: "POST", headers, body: JSON.stringify(payload), signal: CHAT_ABORT.signal });
    if (!r.ok) { let d = ""; try { d = (await r.json()).detail || ""; } catch (e) {} throw new Error("HTTP " + r.status + (d ? ": " + d : "")); }
    const reader = r.body.getReader(), dec = new TextDecoder(); let buf = "";
    for (;;) {
      const { value, done } = await reader.read(); if (done) break;
      buf += dec.decode(value, { stream: true });
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        let line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
        if (!line.startsWith("data:")) continue;
        line = line.slice(5).trim();
        if (line === "[DONE]") { buf = ""; break; }
        try {
          const j = JSON.parse(line);
          const delta = (j.choices && j.choices[0] && j.choices[0].delta) || {};
          if (delta.content) { acc += delta.content; body.textContent = acc; }
          if (delta.reasoning_content) think += delta.reasoning_content;
          $("#chatlog").scrollTop = 1e9;
        } catch (e) {}
      }
    }
    if (!acc && think) acc = "(only reasoning returned) " + think.slice(0, 500);
    if (!acc) acc = "(empty response)";
    body.textContent = acc;
    CHAT_MSGS.push({ role: "assistant", content: acc });
  } catch (e) {
    if (e.name === "AbortError") { body.textContent = acc + " …[stopped]"; CHAT_MSGS.push({ role: "assistant", content: acc }); }
    else { body.textContent = "⚠ " + emsg(e); CHAT_MSGS.pop(); }
  } finally {
    $("#chatsend").style.display = ""; $("#chatstop").style.display = "none"; CHAT_ABORT = null;
  }
}

function wireChat() {
  CHAT_WIRED = true;
  $$('input[name=chatsrc]').forEach((r) => (r.onchange = fillChatModels));
  $("#chatmodel").onchange = () => set(chatSource() === "grid" ? "aipg_chatgrid" : "aipg_chatlocal", $("#chatmodel").value);
  $("#chatsend").onclick = chatSend;
  $("#chatstop").onclick = () => { if (CHAT_ABORT) CHAT_ABORT.abort(); };
  $("#chatclear").onclick = () => { CHAT_MSGS = []; $("#chatlog").innerHTML = ""; };
  const ta = $("#chattext");
  ta.addEventListener("input", () => { ta.style.height = "auto"; ta.style.height = Math.min(ta.scrollHeight, 180) + "px"; });
  ta.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); chatSend(); } });
}

function loadChat() { if (!CHAT_WIRED) wireChat(); fillChatModels(); }

/* ---------- images (txt2img / img2img via grid) ---------- */
let IMG_WIRED = false;
function imgMode() { const r = document.querySelector('input[name=imgmode]:checked'); return r ? r.value : "txt2img"; }

async function fillImgModels() {
  const sel = $("#imgmodel");
  try {
    const r = await fetch(API + "/status/models", { headers: { "Accept": "application/json" } });
    const list = await r.json();
    const imgs = (Array.isArray(list) ? list : []).filter((m) => m.type === "image").map((m) => m.name);
    sel.innerHTML = imgs.length
      ? imgs.map((n) => `<option value="${esc(n)}">${esc(n)}</option>`).join("")
      : '<option value="">(no image models online)</option>';
  } catch (e) { sel.innerHTML = '<option value="">(could not load)</option>'; }
}

function fileToDataURL(f) {
  return new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(r.result); r.onerror = rej; r.readAsDataURL(f); });
}

async function imgGenerate() {
  const key = apiKey(); if (!key) { toast("Erst API-Key in Settings"); return show("settings"); }
  const model = $("#imgmodel").value; if (!model) { toast("Kein Bildmodell verfügbar"); return; }
  const prompt = $("#imgprompt").value.trim(); if (!prompt) { toast("Prompt eingeben"); return; }
  const payload = { model, prompt, n: 1 };
  if (imgMode() === "img2img") {
    const f = $("#imgfile").files[0]; if (!f) { toast("Bild auswählen"); return; }
    payload.image = await fileToDataURL(f);
  }
  $("#imggen").disabled = true; $("#imgmsg").textContent = "generating… (kann etwas dauern)"; $("#imgresult").innerHTML = "";
  try {
    const r = await fetch(API + "/images/generations", {
      method: "POST", headers: { "Authorization": "Bearer " + key, "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    const d = await r.json();
    if (!r.ok) throw new Error((d && d.detail) || ("HTTP " + r.status));
    const item = (d.data && d.data[0]) || {};
    const src = item.url ? item.url : (item.b64_json ? "data:image/png;base64," + item.b64_json : "");
    if (!src) throw new Error("Keine Bilddaten in der Antwort.");
    const img = document.createElement("img"); img.src = src;
    $("#imgresult").appendChild(img);
    const a = document.createElement("a"); a.href = src; a.download = "aipg-image.png"; a.textContent = "Download"; a.className = "btn ghost";
    $("#imgresult").appendChild(a);
    if (item.revised_prompt) { const p = document.createElement("p"); p.className = "hint"; p.textContent = item.revised_prompt; $("#imgresult").appendChild(p); }
    $("#imgmsg").textContent = "done";
  } catch (e) { $("#imgmsg").textContent = "⚠ " + emsg(e); }
  finally { $("#imggen").disabled = false; }
}

function wireImages() {
  IMG_WIRED = true;
  $$('input[name=imgmode]').forEach((r) => (r.onchange = () => { $("#imgsrcrow").style.display = imgMode() === "img2img" ? "" : "none"; }));
  $("#imggen").onclick = imgGenerate;
}
function loadImages() { if (!IMG_WIRED) wireImages(); fillImgModels(); }

/* ---------- misc ---------- */
function esc(s) { return String(s == null ? "" : s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c])); }

/* ---------- views ---------- */
function show(view) {
  $$(".nav").forEach((b) => b.classList.toggle("active", b.dataset.view === view));
  $$(".view").forEach((v) => v.classList.toggle("active", v.id === "view-" + view));
  if (view === "overview") loadOverview();
  if (view === "chat") loadChat();
  if (view === "images") loadImages();
  if (view === "worker") loadWorker();
  if (view === "models") loadModels();
  if (view === "earnings") loadEarnings();
}

/* ---------- wire up ---------- */
document.addEventListener("DOMContentLoaded", () => {
  $$(".nav").forEach((b) => (b.onclick = () => show(b.dataset.view)));
  $("#refresh").onclick = loadOverview;
  $("#refresh-models").onclick = loadModels;
  $("#refresh-earnings").onclick = loadEarnings;
  $("#theme").onclick = () => {
    const cur = document.documentElement.getAttribute("data-theme");
    const next = cur === "light" ? "dark" : "light";
    applyTheme(next); set(LS.theme, next);
  };

  // settings fields
  $("#key").value = get(LS.key) || "";
  $("#ntfy").value = get(LS.ntfy) || "";
  $("#alerts").checked = get(LS.alerts) === "1";
  $("#savekey").onclick = () => { set(LS.key, $("#key").value.trim()); toast("Key saved"); startPolling(); show("overview"); };
  $("#clearkey").onclick = () => { del(LS.key); $("#key").value = ""; toast("Key forgotten"); startPolling(); loadOverview(); };
  $("#saventfy").onclick = () => { set(LS.ntfy, $("#ntfy").value.trim()); toast("ntfy topic saved"); };
  $("#alerts").onchange = (e) => {
    set(LS.alerts, e.target.checked ? "1" : "0");
    if (e.target.checked) nativeNotify("Alerts on", "You'll be notified if a worker goes offline.");
    startPolling();
  };

  // external links
  document.body.addEventListener("click", (e) => {
    const a = e.target.closest("[data-ext]");
    if (a) { e.preventDefault(); openExternal(a.dataset.ext); }
  });

  show("overview");
  startPolling();
});
