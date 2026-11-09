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
    : p.ollama_installed ? '<span class="chip warn">installed, stopped</span>' : '<span class="chip off">not installed</span>';
  $("#btn-ollama").disabled = p.ollama_running || !p.ollama_installed;

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

async function loadWorker() {
  if (!TAURI.core) {
    $("#wunsupported").style.display = "";
    $("#wunsupported").textContent = "Dieser Bereich funktioniert nur in der installierten Desktop-App.";
    return;
  }
  $("#wname").value = get("aipg_wname") || "";
  $("#wmodel").value = get("aipg_wmodel") || "qwen3:8b";
  if (!WORKER_WIRED) wireWorker();
  try { renderPrereqs(await invoke("prereqs")); } catch (e) { toast("prereqs: " + emsg(e)); }
  try { WORKER_RUNNING = await invoke("worker_running"); } catch (e) {}
  syncRunButtons();
  refreshOllamaModels();
}

function syncRunButtons() {
  $("#btn-stop").disabled = !WORKER_RUNNING;
  $("#btn-start").disabled = WORKER_RUNNING || $("#worker-dl-state").textContent.indexOf("downloaded") < 0;
  wsetStatus(WORKER_RUNNING ? "running" : "idle", WORKER_RUNNING ? "run" : "");
}

function wireWorker() {
  WORKER_WIRED = true;

  listen("worker-dl", (e) => {
    const { received, total } = e.payload || {};
    if (total) bar("#dlbar", received / total);
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

  $("#btn-stop").onclick = async () => {
    $("#btn-stop").disabled = true;
    try { await invoke("worker_stop"); } catch (e) {}
    WORKER_RUNNING = false; syncRunButtons();
  };
}

/* ---------- misc ---------- */
function esc(s) { return String(s == null ? "" : s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c])); }

/* ---------- views ---------- */
function show(view) {
  $$(".nav").forEach((b) => b.classList.toggle("active", b.dataset.view === view));
  $$(".view").forEach((v) => v.classList.toggle("active", v.id === "view-" + view));
  if (view === "overview") loadOverview();
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
