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
function fmtBytes(n) {
  n = Number(n) || 0;
  if (n >= 1e9) return (n / 1073741824).toFixed(1) + " GB";
  if (n >= 1e6) return (n / 1048576).toFixed(0) + " MB";
  if (n >= 1e3) return (n / 1024).toFixed(0) + " KB";
  return n + " B";
}

async function loadOverview() {
  const hasKey = !!apiKey();
  $("#needkey").style.display = hasKey ? "none" : "";
  if (!hasKey) { setConn("", "not connected"); $("#stats").innerHTML = ""; $("#workers").innerHTML = '<p class="muted">—</p>'; return; }
  setConn("", "loading…");
  try {
    const [w, c] = await Promise.all([api("/account/workers"), api("/account/credits")]);
    setConn("ok", "connected");
    $("#stats").innerHTML = `
      ${stat("Spendable", money(c.total_spendable_usd), "green", creditNote(c))}
      ${c.free ? stat("Free today", `${money(c.free.remaining_usd)} <small>/ ${money(c.free.daily_cap_usd)}</small>`, "", freeNote(c.free)) : ""}
      ${stat("Workers online", (() => {
        const hid = hiddenWorkers(), vis = (w.workers || []).filter((x) => !hid.includes(x.name));
        return w.workers ? `${vis.filter((x) => x.online).length} / ${vis.length}` : `${w.online || 0} / ${w.count || 0}`;
      })(), "blue")}
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
function stat(k, v, cls, note) {
  return `<div class="stat"><div class="k">${k}</div><div class="v ${cls || ""}">${v}</div>${note ? `<div class="n">${note}</div>` : ""}</div>`;
}
/* The grid keeps three pockets: daily free allowance, promotional credit and
   purchased balance. `free.active === false` means the allowance is shown but
   not spendable yet. */
function freeNote(f) {
  const h = (new Date().setUTCHours(24, 0, 0, 0) - Date.now()) / 3600000;
  const reset = h < 1 ? `resets in ${Math.max(1, Math.round(h * 60))} min` : `resets in ${Math.round(h)} h`;
  return [reset, f.active === false ? "not spendable yet" : "", f.holder_bonus_active ? "holder bonus" : ""].filter(Boolean).join(" · ");
}
function creditNote(c) {
  const parts = [];
  if (c.paid && c.paid.balance_usd != null) parts.push(`balance ${money(c.paid.balance_usd)}`);
  if (c.promotional && Number(c.promotional.remaining_usd) > 0) parts.push(`promo ${money(c.promotional.remaining_usd)}`);
  if (c.charging_enabled === false) parts.push("billing off");
  return parts.join(" · ");
}
/* Workers can't be deleted through the grid API, so retired ones are hidden
   locally (per device). Hidden workers are also excluded from offline alerts. */
let LAST_WORKERS = [], SHOW_HIDDEN = false;
function hiddenWorkers() { try { return JSON.parse(get("aipg_hidden_workers") || "[]"); } catch (e) { return []; } }
function setHiddenWorkers(list) { set("aipg_hidden_workers", JSON.stringify([...new Set(list)])); }
function renderWorkers(workers) {
  LAST_WORKERS = workers;
  const hidden = hiddenWorkers();
  const nHidden = workers.filter((w) => hidden.includes(w.name)).length;
  const shown = workers.filter((w) => SHOW_HIDDEN || !hidden.includes(w.name));
  const offline = workers.filter((w) => !w.online && !hidden.includes(w.name)).length;
  $("#wtools").innerHTML =
    (offline ? `<button class="btn ghost" id="hide-offline">Hide ${offline} offline</button>` : "") +
    (nHidden ? `<button class="btn ghost" id="toggle-hidden">${SHOW_HIDDEN ? "Hide hidden" : `Show hidden (${nHidden})`}</button>` : "");
  const ho = $("#hide-offline");
  if (ho) ho.onclick = () => { setHiddenWorkers(hidden.concat(workers.filter((w) => !w.online).map((w) => w.name))); renderWorkers(workers); toast("Offline workers hidden"); };
  const th = $("#toggle-hidden");
  if (th) th.onclick = () => { SHOW_HIDDEN = !SHOW_HIDDEN; renderWorkers(workers); };
  if (!workers.length) { $("#workers").innerHTML = '<p class="muted">No workers on this account yet.</p>'; return; }
  if (!shown.length) { $("#workers").innerHTML = '<p class="muted">All workers are hidden.</p>'; return; }
  $("#workers").innerHTML = shown.map((w, i) => {
    const on = !!w.online, maint = !!w.maintenance, isHidden = hidden.includes(w.name);
    const badge = maint ? '<span class="chip">maintenance</span>'
      : on ? '<span class="chip on">online</span>' : '<span class="chip off">offline</span>';
    const models = (w.models || []).join(", ") || "—";
    return `<div class="card${isHidden ? " dim" : ""}"><h3>${esc(w.name)}</h3>
      <div class="chips">${badge}<span class="chip">${esc(w.type || "")}</span>${isHidden ? '<span class="chip">hidden</span>' : ""}</div>
      <div class="wmeta">models: ${esc(models)}<br>den: ${w.den_earned ?? 0} · jobs: ${w.jobs_completed ?? 0}</div>
      <div class="row"><button class="btn ghost" data-wi="${i}">${isHidden ? "Unhide" : "Hide"}</button></div></div>`;
  }).join("");
  $$("#workers [data-wi]").forEach((b) => (b.onclick = () => {
    const name = shown[Number(b.dataset.wi)].name, cur = hiddenWorkers();
    setHiddenWorkers(cur.includes(name) ? cur.filter((x) => x !== name) : cur.concat(name));
    renderWorkers(LAST_WORKERS);
  }));
}

async function loadModels() {
  $("#models").innerHTML = '<p class="muted"><span class="spin"></span> Loading…</p>';
  $("#imgmodels").innerHTML = '<p class="muted"><span class="spin"></span> Loading…</p>';
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

/* ---------- earnings history (local, per-device) ---------- */
function loadEarnHist() { try { return JSON.parse(get("aipg_earnhist") || "[]"); } catch (e) { return []; } }
function recordEarn(totalDen) {
  if (totalDen == null) return loadEarnHist();
  let h = loadEarnHist();
  const day = new Date().toISOString().slice(0, 10);
  if (h.length && h[h.length - 1].d === day) h[h.length - 1].v = totalDen; // keep latest for today
  else h.push({ d: day, v: totalDen });
  if (h.length > 400) h = h.slice(-400);
  set("aipg_earnhist", JSON.stringify(h));
  return h;
}
function renderEarnHistory(h) {
  const box = $("#earnchart"); if (!box) return;
  if (!h || h.length < 2) {
    box.innerHTML = '<p class="muted">The curve builds up over the next days — one data point per day the app runs.</p>';
    return;
  }
  const W = 560, H = 120, pad = 10;
  const vals = h.map((p) => p.v);
  const min = Math.min(...vals), max = Math.max(...vals), span = (max - min) || 1, n = h.length;
  const x = (i) => pad + i * (W - 2 * pad) / (n - 1);
  const y = (v) => H - pad - (v - min) / span * (H - 2 * pad);
  const d = h.map((p, i) => `${i ? "L" : "M"}${x(i).toFixed(1)},${y(p.v).toFixed(1)}`).join(" ");
  const last = h[n - 1].v, delta = h[n - 1].v - h[n - 2].v;
  box.innerHTML =
    `<svg viewBox="0 0 ${W} ${H}" width="100%" height="${H}" preserveAspectRatio="none" role="img" aria-label="Den earned over time">` +
    `<path d="${d}" fill="none" stroke="var(--acc)" stroke-width="2" vector-effect="non-scaling-stroke" stroke-linejoin="round"/>` +
    `</svg>` +
    `<p class="hint">Total den: <b>${last}</b> · last change: <b>${delta >= 0 ? "+" : ""}${delta}</b> · ${n} data points (stored locally only)</p>`;
}

async function loadEarnings() {
  try {
    const d = await api("/account/jobs");
    const jobs = d.jobs || [];
    $("#estats").innerHTML = `
      ${stat("Total den", d.total_den ?? 0, "green")}
      ${stat("Jobs", jobs.length, "blue")}`;
    renderEarnHistory(recordEarn(d.total_den));
    const tb = $("#jobs tbody");
    tb.innerHTML = jobs.slice(0, 50).map((j) => `<tr>
      <td>${j.ts ? new Date(j.ts).toLocaleString() : (j.timestamp || "—")}</td>
      <td>${esc(j.model || "")}</td><td>${esc(j.type || "")}</td>
      <td>${j.den ?? "—"}</td><td>${j.output_units ?? "—"}</td>
      <td>${j.duration_s != null ? j.duration_s + "s" : "—"}</td></tr>`).join("")
      || '<tr><td colspan="6" class="muted">No jobs yet.</td></tr>';
  } catch (e) {
    $("#estats").innerHTML = "";
    renderEarnHistory(loadEarnHist());
    $("#jobs tbody").innerHTML = `<tr><td colspan="6" class="muted">${e.message === "no-key" ? "Add an API key in Settings." : esc(e.message)}</td></tr>`;
  }
}

/* ---------- monitoring transitions ---------- */
function loadWState() { try { return JSON.parse(get(LS.wstate) || "{}"); } catch (e) { return {}; } }
function saveWState(s) { set(LS.wstate, JSON.stringify(s)); }
function checkTransitions(workers) {
  if (get(LS.alerts) !== "1") { saveWState(workerMap(workers)); return; }
  const prev = loadWState(), hid = hiddenWorkers();
  for (const w of workers) {
    if (hid.includes(w.name)) continue; // no alerts for hidden (retired) workers
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
  POLL = null;
  if (!apiKey()) return;
  const tick = async () => {
    if (get(LS.alerts) === "1") {
      try { const w = await api("/account/workers"); checkTransitions(w.workers || []); } catch (e) {}
    }
    // keep the earnings history growing in the background (at most every 30 min)
    if (Date.now() - Number(get("aipg_earn_ts") || 0) > 30 * 60000) {
      try {
        const d = await api("/account/jobs");
        recordEarn(d.total_den); set("aipg_earn_ts", String(Date.now()));
      } catch (e) {}
    }
  };
  POLL = setInterval(tick, 120000);
  tick();
}

/* ---------- worker (all-in-one, Tauri commands) ---------- */
const TAURI = window.__TAURI__ || {};
const invoke = (cmd, args) =>
  TAURI.core && TAURI.core.invoke ? TAURI.core.invoke(cmd, args) : Promise.reject(new Error("not-in-app"));
const listen = (ev, cb) =>
  TAURI.event && TAURI.event.listen ? TAURI.event.listen(ev, cb) : Promise.resolve(() => {});
// Tauri command errors arrive as plain strings, not Error objects.
const emsg = (e) => (e && typeof e === "object" && e.message) ? e.message : (e == null ? "unknown error" : String(e));

let WORKER_WIRED = false, WORKER_RUNNING = false;

function logLine(s) {
  const el = $("#wlog");
  if (el.textContent === "—") el.textContent = "";
  el.textContent += (el.textContent ? "\n" : "") + s;
  el.scrollTop = el.scrollHeight;
}
/* ---------- live job indicator (parsed from the worker's own log) ----------
   start:  "Job 1a2b3c4d | model=… | …"  /  "Raw job 1a2b3c4d | format=…"
   finish: "✅ 1a2b3c4d | 153 tokens | 4.2s | 36.4 TPS | +1.3 den | total: 9.9"
   Matching ignores the emoji so a mangled console encoding does not break it. */
const JOBS = new Map(); // short job id -> { t0, model }
let JOB_TICK = null, JOB_SESSION = { n: 0, den: 0, last: "" }, TRAY_TIP = "";
function jobsReset(newSession) {
  JOBS.clear();
  if (newSession) JOB_SESSION = { n: 0, den: 0, last: "" };
  renderJobs();
}
function trackJobs(line) {
  let m;
  if ((m = line.match(/\b(?:Raw job|Job) ([\w-]{4,8}) \| (?:model=([^|]+?) \|)?/))) {
    JOBS.set(m[1], { t0: Date.now(), model: (m[2] || "").trim() });
  } else if ((m = line.match(/ ([\w-]{4,8}) \| (.*\+([\d.]+) den) \| total/))) {
    JOBS.delete(m[1]);
    JOB_SESSION.n++; JOB_SESSION.den += parseFloat(m[3]) || 0;
    JOB_SESSION.last = m[2].replace(/ \| /g, " · ");
  } else if ((m = line.match(/(?:No ack received for (?:raw )?job|ReadTimeout for job|produced 0 tokens for job) ([\w-]{4,8})/))) {
    JOBS.delete(m[1]);
  } else if (/Backend unreachable mid-|Connection lost|Backend offline|Connected as worker/.test(line)) {
    JOBS.clear();
  } else return;
  renderJobs();
}
function renderJobs() {
  const now = Date.now();
  for (const [id, j] of JOBS) if (now - j.t0 > 900000) JOBS.delete(id); // never show a stuck job forever
  const n = JOBS.size, el = $("#wjob"), nav = $("#navjob");
  if (nav) nav.className = "navjob" + (n ? " on" : "");
  let tip = "AIPG Desktop";
  if (el) {
    if (!WORKER_RUNNING) { el.textContent = ""; el.className = "jobline"; }
    else if (n) {
      const parts = [...JOBS].map(([id, j]) =>
        `${id}${j.model ? " · " + j.model : ""} · ${Math.round((now - j.t0) / 1000)} s`);
      el.textContent = (n === 1 ? "Serving a grid job: " : `Serving ${n} grid jobs: `) + parts.join("  |  ");
      el.className = "jobline busy";
    } else {
      el.textContent = "Waiting for grid jobs" +
        (JOB_SESSION.n ? ` · ${JOB_SESSION.n} served this session · +${JOB_SESSION.den.toFixed(1)} den · last: ${JOB_SESSION.last}` : "");
      el.className = "jobline";
    }
  }
  if (WORKER_RUNNING) tip = n ? `AIPG Desktop: serving ${n} grid job${n === 1 ? "" : "s"}`
    : `AIPG Desktop: worker waiting for jobs` + (JOB_SESSION.n ? ` (${JOB_SESSION.n} served)` : "");
  if (tip !== TRAY_TIP && TAURI.core) { TRAY_TIP = tip; invoke("tray_status", { text: tip }).catch(() => {}); }
  if (WORKER_RUNNING && !RESTART_T) wsetStatus(n ? `working · ${n} job${n === 1 ? "" : "s"}` : "running", n ? "run busy" : "run");
  if (n && !JOB_TICK) JOB_TICK = setInterval(renderJobs, 1000);
  if (!n && JOB_TICK) { clearInterval(JOB_TICK); JOB_TICK = null; }
}

function wsetStatus(txt, cls) { const c = $("#wstatus"); c.textContent = txt; c.className = "chip" + (cls ? " " + cls : ""); }
function bar(sel, frac) {
  const b = $(sel); b.style.display = ""; b.firstElementChild.style.width = Math.max(0, Math.min(1, frac)) * 100 + "%";
}
/* always-visible worker step strip + progress */
function setStep(step, state) {
  const el = document.querySelector(`#wsteps .step[data-step="${step}"]`);
  if (el) el.className = "step" + (state ? " " + state : "");
}
function resetSteps() { ["ollama", "worker", "model", "run"].forEach((s) => setStep(s, "")); }
function wprog(frac, text) {
  const b = $("#wpbar");
  if (frac == null) b.style.display = "none";
  else { b.style.display = ""; b.firstElementChild.style.width = Math.max(0, Math.min(1, frac)) * 100 + "%"; }
  if (text != null) $("#wstatusline").textContent = text;
}

function renderPrereqs(p) {
  if (!p.supported) {
    $("#wunsupported").style.display = "";
    $("#wunsupported").innerHTML = `There is no official worker binary (v0.3.12) for <b>${esc(p.os)}/${esc(p.arch)}</b>. ` +
      `Supported: Linux x64/arm64, macOS (Apple Silicon), Windows x64.`;
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
  let list = null;
  try { list = await invoke("ollama_list"); } catch (e) { list = null; }
  $("#wmodels-list").innerHTML = (list || []).map((m) => `<option value="${esc(m.name)}">`).join("");
  renderLocalModels(list);
  return list || [];
}

/* installed local models: size, use, delete */
function renderLocalModels(list) {
  const box = $("#local-models"); if (!box) return;
  if (list == null) { box.innerHTML = '<p class="muted">Ollama is not running — start it to see installed models.</p>'; return; }
  if (!list.length) { box.innerHTML = '<p class="muted">No models installed yet.</p>'; return; }
  const total = list.reduce((a, m) => a + (m.size || 0), 0);
  box.innerHTML = list.map((m) =>
    `<div class="lmrow"><code class="path">${esc(m.name)}</code><span class="muted">${fmtBytes(m.size)}</span>` +
    `<button class="btn ghost" data-use="${esc(m.name)}">Use</button>` +
    `<button class="btn ghost" data-del="${esc(m.name)}">Delete</button></div>`).join("") +
    `<p class="hint">${list.length} installed · ${fmtBytes(total)} total</p>`;
  box.querySelectorAll("[data-use]").forEach((b) => (b.onclick = () => applyModel(b.dataset.use)));
  box.querySelectorAll("[data-del]").forEach((b) => (b.onclick = async () => {
    const name = b.dataset.del;
    if (WORKER_RUNNING && name === get("aipg_wmodel")) { toast("Stop the worker before deleting its model"); return; }
    if (!(await askConfirm(`Delete ${name} from this machine?`, "Delete model", false))) return;
    try { await invoke("ollama_delete", { model: name }); toast("Deleted " + name); } catch (e) { toast(emsg(e)); }
    refreshOllamaModels(); loadModelsDir();
  }));
}

function applyModel(m) {
  if (!m) return;
  $("#wmodel2").value = m; $("#wmodel").value = m; set("aipg_wmodel", m);
  toast("Model set to " + m);
}

let PULL_FRAC = 0;
const LOW_SPACE = 5 * 1073741824;

// Confirm dialog via the dialog plugin. `fallback` is the answer when the
// dialog is unavailable (true = proceed, false = don't).
async function askConfirm(message, title, fallback = true) {
  try {
    return await invoke("plugin:dialog|confirm", { message, title: title || "AIPG Desktop" });
  } catch (e) { return fallback; }
}

// Models are multi-GB: warn before a download that is likely to run out of disk.
async function spaceGuard() {
  try {
    const d = await invoke("models_dir_get");
    if (d && d.free_bytes && d.free_bytes < LOW_SPACE) {
      return await askConfirm(
        `Only ${fmtBytes(d.free_bytes)} free in:\n${d.path}\n\nModels are often 2–10 GB. Download anyway?\n` +
        `(You can change the location under “Models folder → Change…”.)`,
        "Low disk space");
    }
  } catch (e) {}
  return true;
}

// Download/update one Ollama model tag into the chosen models folder.
async function pullModel(model) {
  model = (model || "").trim();
  if (!model) { toast("Enter a model tag"); return false; }
  if (!(await spaceGuard())) { toast("Cancelled — change the models folder"); return false; }
  set("aipg_wmodel", model);
  PULL_FRAC = 0;
  setStep("model", "active"); wprog(0, "Pulling model…");
  try {
    await invoke("ollama_ensure");
    await invoke("ollama_pull", { model });
    setStep("model", "done"); wprog(null, "Model ready ✓"); toast("Model ready");
    refreshOllamaModels(); loadModelsDir();
    return true;
  } catch (e) {
    setStep("model", "error"); wprog(null, "⚠ " + emsg(e)); toast(emsg(e));
    return false;
  }
}

function showModelsDir(d) {
  $("#models-dir").textContent = d.path;
  $("#models-dir").title = d.path;
  $("#btn-models-dir-reset").style.display = d.custom ? "" : "none";
  const f = $("#models-free");
  if (f) {
    const low = d.free_bytes && d.free_bytes < LOW_SPACE;
    f.textContent = d.free_bytes ? `${fmtBytes(d.free_bytes)} free` : "";
    f.className = "hint" + (low ? " warn" : "");
  }
}
async function loadModelsDir() {
  if (!TAURI.core) return;
  try { showModelsDir(await invoke("models_dir_get")); } catch (e) {}
}

// Switch the models folder: restarts the managed Ollama so it takes effect now,
// and optionally moves the models that are already downloaded.
async function switchModelsDir(path) {
  if (WORKER_RUNNING) { toast("Stop the worker before changing the models folder"); return; }
  const move = await askConfirm(
    "Also move the models you already downloaded to the new folder?\n\n" +
    "OK = move them (can take a while across drives).\nCancel = leave them where they are.",
    "Move existing models?", false);
  wprog(null, move ? "Moving models to the new folder…" : "Switching models folder…");
  try {
    const d = await invoke("models_dir_set", { path, moveExisting: move });
    showModelsDir(d);
    if (d.ollama_stopped) { try { await invoke("ollama_ensure"); } catch (e) {} }
    wprog(null, "Models folder updated ✓"); toast("Models folder updated");
  } catch (e) { wprog(null, "⚠ " + emsg(e)); toast(emsg(e)); }
  refreshOllamaModels();
  try { renderPrereqs(await invoke("prereqs")); } catch (e) {}
}

async function pickModelsDir() {
  if (!TAURI.core) return;
  let dir;
  try {
    dir = await invoke("plugin:dialog|open", {
      options: { directory: true, multiple: false, title: "Choose where models are stored" },
    });
  } catch (e) { toast("Folder picker unavailable: " + emsg(e)); return; }
  if (!dir) return; // cancelled
  if (Array.isArray(dir)) dir = dir[0];
  await switchModelsDir(dir);
}

function resetModelsDir() { return switchModelsDir(""); }

function renderHardware(h) {
  $("#hwpanel").innerHTML =
    `<span><span class="k">RAM</span> <b>${h.ram_gb} GB</b></span>` +
    `<span><span class="k">CPU</span> <b>${esc(h.cpu)}</b></span>` +
    `<span><span class="k">GPU</span> <b>${esc(h.gpu)}</b></span>` +
    (h.recommend
      ? `<span><span class="k">Suggested model</span> <b>${esc(h.recommend)}</b>` +
        ` <button class="btn ghost" id="use-suggested" data-m="${esc(h.recommend)}">Use</button>` +
        ` <button class="btn ghost" id="dl-suggested" data-m="${esc(h.recommend)}">Download</button></span>`
      : "");
  const u = $("#use-suggested");
  if (u) u.onclick = () => applyModel(u.dataset.m);
  const dls = $("#dl-suggested");
  if (dls) dls.onclick = async () => {
    dls.disabled = true; applyModel(dls.dataset.m);
    await pullModel(dls.dataset.m); dls.disabled = false;
  };
  if (!h.can_run_worker) {
    const b = $("#hwfallback"); b.style.display = "";
    b.innerHTML = esc(h.note) + ' <a href="#" id="gotochat"><b>Go to grid chat →</b></a>';
    const g = $("#gotochat"); if (g) g.onclick = (e) => { e.preventDefault(); show("chat"); };
  } else {
    $("#hwfallback").style.display = "none";
    // prefill recommended model only if the user hasn't chosen one yet
    if (h.recommend && !get("aipg_wmodel")) { $("#wmodel2").value = h.recommend; $("#wmodel").value = h.recommend; }
  }
}

async function loadWorker() {
  if (!TAURI.core) {
    $("#wunsupported").style.display = "";
    $("#wunsupported").textContent = "This section only works in the installed desktop app.";
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
  loadModelsDir();
}

function syncRunButtons() {
  const dl = $("#worker-dl-state").textContent.indexOf("not downloaded") < 0 &&
    $("#worker-dl-state").textContent.indexOf("downloaded") >= 0;
  $("#btn-stop").disabled = !WORKER_RUNNING && !RESTART_T;
  $("#btn-start").disabled = WORKER_RUNNING || !dl;
  $("#btn-stop2").disabled = !WORKER_RUNNING && !RESTART_T;
  $("#btn-auto").disabled = WORKER_RUNNING;
  if (!RESTART_T) wsetStatus(WORKER_RUNNING ? "running" : "idle", WORKER_RUNNING ? "run" : "");
  renderJobs();
}

/* crash recovery: restart an unexpectedly exited worker, at most 3× per 10 min */
let WANT_RUNNING = false, LAST_CFG = null, RESTARTS = [], RESTART_T = null;
function markStarted(cfg) { WORKER_RUNNING = true; WANT_RUNNING = true; LAST_CFG = cfg; jobsReset(true); }
function scheduleRestart() {
  const now = Date.now();
  RESTARTS = RESTARTS.filter((t) => now - t < 600000);
  if (RESTARTS.length >= 3) {
    WANT_RUNNING = false; RESTART_T = null;
    logLine("[desktop] worker crashed 3 times within 10 minutes — not restarting again.");
    nativeNotify("Worker stopped", "It crashed repeatedly and was not restarted.");
    syncRunButtons(); wsetStatus("crashed", "off");
    return;
  }
  RESTARTS.push(now);
  const delay = 5000 * RESTARTS.length;
  logLine(`[desktop] worker exited unexpectedly — restarting in ${delay / 1000}s (attempt ${RESTARTS.length}/3)…`);
  clearTimeout(RESTART_T);
  RESTART_T = setTimeout(async () => {
    RESTART_T = null;
    if (!WANT_RUNNING) { syncRunButtons(); return; }
    try {
      await invoke("ollama_ensure");
      await invoke("worker_start", { cfg: LAST_CFG });
      WORKER_RUNNING = true; syncRunButtons();
    } catch (e) { logLine("[restart] " + emsg(e)); scheduleRestart(); }
  }, delay);
  syncRunButtons(); wsetStatus("restarting…", "warn");
}

async function stopWorker() {
  WANT_RUNNING = false; clearTimeout(RESTART_T); RESTART_T = null;
  $("#btn-stop").disabled = $("#btn-stop2").disabled = true;
  try { await invoke("worker_stop"); } catch (e) {}
  WORKER_RUNNING = false; jobsReset(false); syncRunButtons();
}

async function autoStartWorker() {
  const api_key = apiKey();
  if (!api_key) { toast("Save your API key in Settings first"); return show("settings"); }
  const worker_name = $("#wname2").value.trim();
  const model = $("#wmodel2").value.trim() || "qwen3:8b";
  set("aipg_wname", worker_name); set("aipg_wmodel", model);
  $("#wname").value = worker_name; $("#wmodel").value = model;
  $("#btn-auto").disabled = true; wsetStatus("starting…", "warn"); resetSteps();
  let step = "ollama";
  try {
    let p = await invoke("prereqs");
    // 1 — Ollama
    step = "ollama"; setStep("ollama", "active");
    if (!p.ollama_running && !p.ollama_managed && !p.ollama_installed) {
      wprog(0, "Downloading Ollama (one time)…"); logLine("[auto] setting up Ollama…");
      await invoke("setup_ollama");
    }
    wprog(null, "Starting Ollama…"); await invoke("ollama_ensure");
    setStep("ollama", "done");
    // 2 — worker binary
    step = "worker"; setStep("worker", "active");
    p = await invoke("prereqs");
    if (!p.worker_downloaded) { wprog(0, "Downloading worker binary (SHA-256 verified)…"); logLine("[auto] downloading worker binary…"); await invoke("download_worker"); }
    setStep("worker", "done");
    // 3 — model (skip the download when it is already installed)
    step = "model"; setStep("model", "active");
    const installed = await refreshOllamaModels();
    const have = installed.some((m) => m.name === model || m.name === model + ":latest");
    if (!have) {
      if (!(await spaceGuard())) throw "Cancelled — not enough disk space. Change the models folder first.";
      PULL_FRAC = 0;
      wprog(0, "Pulling model " + model + "…"); logLine("[auto] pulling model " + model + "…");
      await invoke("ollama_pull", { model });
      refreshOllamaModels(); loadModelsDir();
    }
    setStep("model", "done");
    // 4 — run
    step = "run"; setStep("run", "active"); wprog(null, "Starting worker…");
    const cfg = { api_key, model, worker_name };
    await invoke("worker_start", { cfg });
    markStarted(cfg); RESTARTS = [];
    setStep("run", "done"); wprog(null, "Worker running ✓"); toast("Worker running");
  } catch (e) {
    setStep(step, "error"); wprog(null, "⚠ " + emsg(e)); logLine("[auto] error: " + emsg(e)); toast(emsg(e));
  } finally {
    try { renderPrereqs(await invoke("prereqs")); } catch (e) {}
    syncRunButtons();
  }
}

function wireWorker() {
  WORKER_WIRED = true;

  listen("worker-dl", (e) => {
    const { received, total } = e.payload || {};
    setStep("worker", "active");
    if (total) wprog(received / total, `Worker binary… ${fmtBytes(received)} / ${fmtBytes(total)}`);
  });
  listen("ollama-dl", (e) => {
    const { received, total, status } = e.payload || {};
    setStep("ollama", "active");
    if (status === "extracting") { wprog(1, "Extracting Ollama…"); return; }
    if (total) wprog(received / total, `Downloading Ollama… ${fmtBytes(received)} / ${fmtBytes(total)} · ${Math.round(received / total * 100)} %`);
    else if (received) wprog(0.05, `Downloading Ollama… ${fmtBytes(received)}`);
  });
  listen("ollama-pull", (e) => {
    const j = e.payload || {};
    setStep("model", "active");
    const st = j.status || "pulling…";
    if (j.total && j.completed != null && j.total > 0) {
      PULL_FRAC = j.completed / j.total;
      wprog(PULL_FRAC,
        `Downloading model · ${fmtBytes(j.completed)} / ${fmtBytes(j.total)} · ${Math.round(PULL_FRAC * 100)} %`);
    } else if (st === "success") {
      wprog(1, "Model ready ✓");
    } else {
      // manifest / verify / write phases carry no byte counts — keep the bar, update text
      wprog(PULL_FRAC || 0.03, "Model: " + st);
    }
  });
  listen("worker-log", (e) => { const l = String(e.payload); logLine(l); trackJobs(l); });
  listen("worker-exit", () => {
    WORKER_RUNNING = false; jobsReset(false);
    logLine("[desktop] worker exited.");
    if (WANT_RUNNING && LAST_CFG) { scheduleRestart(); return; }
    syncRunButtons();
    nativeNotify("Worker stopped", "The worker process exited.");
  });

  $("#btn-setup-ollama").onclick = async () => {
    $("#btn-setup-ollama").disabled = true; setStep("ollama", "active"); wprog(0, "Preparing Ollama…");
    try { await invoke("setup_ollama"); setStep("ollama", "done"); wprog(null, "Ollama set up ✓"); toast("Ollama ready"); }
    catch (e) { setStep("ollama", "error"); wprog(null, "⚠ " + emsg(e)); toast(emsg(e)); }
    renderPrereqs(await invoke("prereqs"));
  };

  $("#btn-ollama").onclick = async () => {
    $("#btn-ollama").disabled = true; setStep("ollama", "active"); wprog(null, "Starting Ollama…");
    try { await invoke("ollama_ensure"); setStep("ollama", "done"); wprog(null, "Ollama running"); toast("Ollama running"); }
    catch (e) { setStep("ollama", "error"); wprog(null, "⚠ " + emsg(e)); toast(emsg(e)); }
    renderPrereqs(await invoke("prereqs")); refreshOllamaModels(); syncRunButtons();
  };

  $("#btn-download").onclick = async () => {
    $("#btn-download").disabled = true; setStep("worker", "active"); wprog(0, "Downloading worker binary…");
    try { await invoke("download_worker"); setStep("worker", "done"); wprog(null, "Worker binary ready ✓"); toast("Worker downloaded & verified"); }
    catch (e) { setStep("worker", "error"); wprog(null, "⚠ " + emsg(e)); logLine("[download] " + emsg(e)); toast(emsg(e)); }
    renderPrereqs(await invoke("prereqs")); syncRunButtons();
  };

  $("#btn-pull").onclick = async () => {
    $("#btn-pull").disabled = true;
    await pullModel($("#wmodel").value);
    $("#btn-pull").disabled = false;
  };

  $("#btn-models-dir").onclick = pickModelsDir;
  $("#btn-models-dir-reset").onclick = resetModelsDir;

  $("#btn-start").onclick = async () => {
    const api_key = apiKey();
    if (!api_key) { toast("Save your API key in Settings first"); return show("settings"); }
    const model = $("#wmodel").value.trim();
    const worker_name = $("#wname").value.trim();
    set("aipg_wname", worker_name); set("aipg_wmodel", model);
    $("#btn-start").disabled = true;
    try {
      const cfg = { api_key, model, worker_name };
      await invoke("ollama_ensure");
      await invoke("worker_start", { cfg });
      markStarted(cfg); RESTARTS = []; syncRunButtons();
    } catch (e) { toast(emsg(e)); logLine("[start] " + emsg(e)); syncRunButtons(); }
  };

  $("#btn-copylog").onclick = async () => {
    const txt = $("#wlog").textContent || "";
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        await navigator.clipboard.writeText(txt);
        toast("Log copied");
      } else throw new Error("no-clipboard");
    } catch (e) {
      // fallback: select the log so the user can Ctrl/Cmd+C
      const r = document.createRange(); r.selectNodeContents($("#wlog"));
      const sel = window.getSelection(); sel.removeAllRanges(); sel.addRange(r);
      toast("Selected — press Ctrl/Cmd+C to copy");
    }
  };
  $("#btn-clearlog").onclick = () => { $("#wlog").textContent = "—"; };

  $("#btn-auto").onclick = autoStartWorker;
  $("#btn-stop").onclick = stopWorker;
  $("#btn-stop2").onclick = stopWorker;
}

/* ---------- chat (multi-chat, local persistence, context cap) ---------- */
let CHAT_WIRED = false, CHAT_ABORT = null, CHAT_PERSIST_T = null, STORE = null;
const OLLAMA_LOCAL = "http://127.0.0.1:11434/v1";

function newId() { return Date.now().toString(36) + Math.random().toString(36).slice(2, 6); }
function chatSource() { const r = document.querySelector('input[name=chatsrc]:checked'); return r ? r.value : "grid"; }
function curChat() { return STORE && STORE.chats.find((c) => c.id === STORE.current); }

async function storeLoad() {
  let raw = "{}";
  try { raw = await invoke("chats_read"); } catch (e) {}
  let s; try { s = JSON.parse(raw); } catch (e) { s = {}; }
  STORE = {
    v: 1,
    chats: Array.isArray(s.chats) ? s.chats : [],
    current: s.current || null,
    cap: typeof s.cap === "number" ? s.cap : 20,
    summarize: !!s.summarize,
  };
  if (!STORE.chats.length) addChat(false);
  if (!STORE.current || !curChat()) STORE.current = STORE.chats[0].id;
}
function storePersist() {
  clearTimeout(CHAT_PERSIST_T);
  CHAT_PERSIST_T = setTimeout(() => { invoke("chats_write", { data: JSON.stringify(STORE) }).catch(() => {}); }, 400);
}
function addChat(persist = true) {
  const c = { id: newId(), title: "New chat", src: "grid", model: "auto", msgs: [], summary: "", summaryCount: 0, updated: Date.now() };
  STORE.chats.unshift(c); STORE.current = c.id;
  if (persist) { renderChatList(); loadCurrentChat(); storePersist(); }
  return c;
}
function deleteChat(id) {
  STORE.chats = STORE.chats.filter((c) => c.id !== id);
  if (!STORE.chats.length) addChat(false);
  if (!curChat()) STORE.current = STORE.chats[0].id;
  renderChatList(); loadCurrentChat(); storePersist();
}

function renderChatList() {
  const el = $("#chatlist"); el.innerHTML = "";
  for (const c of STORE.chats) {
    const d = document.createElement("div");
    d.className = "chatitem" + (c.id === STORE.current ? " active" : "");
    const t = document.createElement("span"); t.className = "t"; t.textContent = c.title || "New chat";
    const x = document.createElement("span"); x.className = "x"; x.textContent = "×"; x.title = "Delete";
    d.appendChild(t); d.appendChild(x);
    d.onclick = (ev) => {
      if (ev.target === x) { ev.stopPropagation(); deleteChat(c.id); }
      else { STORE.current = c.id; renderChatList(); loadCurrentChat(); storePersist(); }
    };
    el.appendChild(d);
  }
}

async function fillChatModels() {
  const src = chatSource(), sel = $("#chatmodel");
  if (src === "grid") {
    let ids = ["auto"]; const price = {}, speed = {};
    try { const d = await api("/models", false); ids = (d.data || []).map((m) => m.id); } catch (e) {}
    try {
      const r = await fetch(API + "/status/models", { headers: { "Accept": "application/json" } });
      const list = await r.json();
      (Array.isArray(list) ? list : []).forEach((m) => {
        if (m.pricing) price[m.name] = m.pricing;
        if (m.type === "text" && m.tokens_per_s) speed[m.name] = m.tokens_per_s;
      });
    } catch (e) {}
    // most suitable concrete model = fastest text model with known throughput
    const best = Object.keys(speed).sort((a, b) => speed[b] - speed[a])[0];
    ids.sort((a, b) => (a === "auto" ? -1 : b === "auto" ? 1 : 0));
    sel.innerHTML = ids.map((id) => {
      const p = price[id];
      const tag = p ? ` · $${p.input_per_mtok_usd}/$${p.output_per_mtok_usd} /Mtok` : "";
      const star = id === best ? " ★" : "";
      const label = id === "auto" ? "Auto (grid picks · recommended)" : esc(id) + star;
      return `<option value="${esc(id)}">${label}${esc(tag)}</option>`;
    }).join("");
    $("#chathint").innerHTML = "Grid · <b>Auto</b> picks the best model automatically. ★ = fastest right now. Prices are per million tokens.";
  } else {
    let list = [];
    try { list = await invoke("ollama_models"); } catch (e) {}
    sel.innerHTML = list.length
      ? list.map((m) => `<option value="${esc(m)}">${esc(m)}</option>`).join("")
      : '<option value="">(no local models — pull one under “Run a worker”)</option>';
    $("#chathint").textContent = "Local · runs on your own Ollama, fully offline and free.";
  }
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

function renderMessages() {
  const c = curChat(); $("#chatlog").innerHTML = ""; if (!c) return;
  for (const m of c.msgs) addMsg(m.role === "user" ? "user" : "bot", m.content, m.role !== "user" ? m.model : null);
}

function loadCurrentChat() {
  const c = curChat(); if (!c) return;
  const r = document.querySelector(`input[name=chatsrc][value="${c.src}"]`); if (r) r.checked = true;
  fillChatModels().then(() => { if (c.model && [...$("#chatmodel").options].some((o) => o.value === c.model)) $("#chatmodel").value = c.model; });
  $("#chatcap").value = String(STORE.cap);
  $("#chatsum").checked = STORE.summarize;
  renderMessages();
}

/* message list actually sent, honoring the context cap + summary */
function buildSend(c) {
  const cap = STORE.cap | 0;
  const plain = c.msgs.map((m) => ({ role: m.role, content: m.content }));
  if (cap > 0 && plain.length > cap) {
    const recent = plain.slice(-cap);
    if (STORE.summarize && c.summary) return [{ role: "system", content: "Summary of earlier conversation: " + c.summary }, ...recent];
    return recent;
  }
  return plain;
}

async function simpleComplete(src, model, prompt) {
  const url = (src === "grid" ? API : OLLAMA_LOCAL) + "/chat/completions";
  const headers = { "Content-Type": "application/json" };
  if (src === "grid") headers["Authorization"] = "Bearer " + apiKey();
  const r = await fetch(url, {
    method: "POST", headers,
    body: JSON.stringify({ model, messages: [{ role: "user", content: prompt }], stream: false, max_tokens: 400 }),
  });
  const d = await r.json(); if (!r.ok) throw new Error((d && d.detail) || ("HTTP " + r.status));
  return (d.choices && d.choices[0] && d.choices[0].message && d.choices[0].message.content) || "";
}

async function ensureSummary(c, src, model) {
  const cap = STORE.cap | 0; if (!STORE.summarize || cap <= 0) return;
  const olderEnd = Math.max(0, c.msgs.length - cap);
  if (olderEnd <= (c.summaryCount || 0)) return;
  const older = c.msgs.slice(0, olderEnd).map((m) => (m.role === "user" ? "User: " : "Assistant: ") + m.content).join("\n");
  const prompt = (c.summary ? "Earlier summary:\n" + c.summary + "\n\n" : "") +
    "Summarize the conversation so far in a concise paragraph, keeping key facts and decisions:\n" + older;
  try { const s = await simpleComplete(src, model, prompt); if (s) { c.summary = s.trim(); c.summaryCount = olderEnd; } } catch (e) {}
}

async function chatSend() {
  const ta = $("#chattext"), text = ta.value.trim(); if (!text) return;
  const c = curChat(); if (!c) return;
  const src = chatSource(), model = $("#chatmodel").value;
  if (src === "grid" && !apiKey()) { toast("Save your API key in Settings first"); return show("settings"); }
  if (src === "local" && !model) { toast("No local model — pull one under “Run a worker”"); return; }
  c.src = src; c.model = model;

  ta.value = ""; ta.style.height = "auto";
  addMsg("user", text);
  c.msgs.push({ role: "user", content: text });
  if (!c.title || c.title === "New chat") { c.title = text.slice(0, 40); renderChatList(); }
  const label = src === "grid" ? model : "local · " + model;
  const body = addMsg("bot", "▍", label);
  storePersist();

  $("#chatsend").style.display = "none"; $("#chatstop").style.display = "";
  CHAT_ABORT = new AbortController();

  await ensureSummary(c, src, model);
  const sendMsgs = buildSend(c);
  const url = (src === "grid" ? API : OLLAMA_LOCAL) + "/chat/completions";
  const headers = { "Content-Type": "application/json" };
  if (src === "grid") headers["Authorization"] = "Bearer " + apiKey();
  const payload = { model, messages: sendMsgs, stream: true };
  if (/qwen3|gpt-oss|auto|deepseek|qwen38/i.test(model)) payload.max_tokens = 2048;

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
    c.msgs.push({ role: "assistant", content: acc, model: label });
    c.updated = Date.now();
  } catch (e) {
    if (e.name === "AbortError") { body.textContent = acc + " …[stopped]"; c.msgs.push({ role: "assistant", content: acc, model: label }); }
    else { body.textContent = "⚠ " + emsg(e); c.msgs.pop(); }
  } finally {
    $("#chatsend").style.display = ""; $("#chatstop").style.display = "none"; CHAT_ABORT = null;
    storePersist();
  }
}

function wireChat() {
  CHAT_WIRED = true;
  $$('input[name=chatsrc]').forEach((r) => (r.onchange = () => { const c = curChat(); if (c) c.src = chatSource(); fillChatModels(); storePersist(); }));
  $("#chatmodel").onchange = () => { const c = curChat(); if (c) { c.model = $("#chatmodel").value; storePersist(); } };
  $("#chatcap").onchange = () => { STORE.cap = parseInt($("#chatcap").value, 10) || 0; storePersist(); };
  $("#chatsum").onchange = () => { STORE.summarize = $("#chatsum").checked; storePersist(); };
  $("#chatsend").onclick = chatSend;
  $("#chatstop").onclick = () => { if (CHAT_ABORT) CHAT_ABORT.abort(); };
  $("#chatnew").onclick = () => addChat(true);
  const ta = $("#chattext");
  ta.addEventListener("input", () => { ta.style.height = "auto"; ta.style.height = Math.min(ta.scrollHeight, 180) + "px"; });
  ta.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); chatSend(); } });
}

async function loadChat() {
  if (!STORE) await storeLoad();
  if (!CHAT_WIRED) wireChat();
  renderChatList();
  loadCurrentChat();
}

/* ---------- images (txt2img / img2img via grid, local gallery) ---------- */
let IMG_WIRED = false, IMG_ABORT = null, IMG_SOURCE = "", IMG_TICK = null;
const IMG_CACHE = {}; // gallery id -> data URL
function imgMode() { const r = document.querySelector('input[name=imgmode]:checked'); return r ? r.value : "txt2img"; }

async function fillImgModels() {
  const sel = $("#imgmodel"), keep = sel.value || get("aipg_imgmodel") || "";
  try {
    const r = await fetch(API + "/status/models", { headers: { "Accept": "application/json" } });
    const list = await r.json();
    const imgs = (Array.isArray(list) ? list : []).filter((m) => m.type === "image");
    sel.innerHTML = imgs.length
      ? imgs.map((m) => {
          const lat = m.avg_latency_s ? ` · ~${Math.round(m.avg_latency_s)} s` : "";
          return `<option value="${esc(m.name)}">${esc(m.name)}${lat}</option>`;
        }).join("")
      : '<option value="">(no image models online)</option>';
    if (keep && [...sel.options].some((o) => o.value === keep)) sel.value = keep;
  } catch (e) { sel.innerHTML = '<option value="">(could not load)</option>'; }
}

function fileToDataURL(f) {
  return new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(r.result); r.onerror = rej; r.readAsDataURL(f); });
}

function setImgSource(dataUrl) {
  IMG_SOURCE = dataUrl || "";
  const box = $("#imgsrcprev");
  if (!IMG_SOURCE) { box.innerHTML = ""; $("#imgfile").value = ""; return; }
  box.innerHTML = `<img alt="source image"><button class="btn ghost" id="imgsrcclear">Remove</button>`;
  box.querySelector("img").src = IMG_SOURCE;
  $("#imgsrcclear").onclick = () => setImgSource("");
}
function setImgMode(mode) {
  const r = document.querySelector(`input[name=imgmode][value="${mode}"]`); if (r) r.checked = true;
  $("#imgsrcrow").style.display = mode === "img2img" ? "" : "none";
}

function imgBusy(on) {
  $("#imggen").style.display = on ? "none" : "";
  $("#imgcancel").style.display = on ? "" : "none";
  if (!on) { clearInterval(IMG_TICK); IMG_TICK = null; $("#imgbar").style.display = "none"; }
}

async function imgGenerate() {
  const key = apiKey(); if (!key) { toast("Save your API key in Settings first"); return show("settings"); }
  const model = $("#imgmodel").value; if (!model) { toast("No image model available"); return; }
  const prompt = $("#imgprompt").value.trim(); if (!prompt) { toast("Enter a prompt"); return; }
  const size = $("#imgsize").value, n = parseInt($("#imgcount").value, 10) || 1;
  const mode = imgMode();
  const token = "d" + newId() + newId();
  const payload = { model, prompt, n, size, progress_token: token };
  if (mode === "img2img") {
    if (!IMG_SOURCE) { toast("Choose a source image"); return; }
    payload.image = IMG_SOURCE;
  }
  set("aipg_imgmodel", model); set("aipg_imgsize", size);

  IMG_ABORT = new AbortController();
  imgBusy(true);
  const t0 = Date.now(); let pct = null;
  const paint = () => {
    const s = Math.round((Date.now() - t0) / 1000);
    $("#imgmsg").textContent = pct != null ? `Generating… ${pct} % · ${s} s` : `Waiting for a worker… ${s} s`;
    if (pct != null) bar("#imgbar", pct / 100);
  };
  paint();
  IMG_TICK = setInterval(async () => {
    try {
      const r = await fetch(API + "/progress/" + token, { headers: { "Accept": "application/json" } });
      const d = await r.json();
      if (typeof d.progress === "number") pct = Math.max(0, Math.min(100, Math.round(d.progress)));
    } catch (e) {}
    paint();
  }, 1500);

  try {
    const r = await fetch(API + "/images/generations", {
      method: "POST", headers: { "Authorization": "Bearer " + key, "Content-Type": "application/json" },
      body: JSON.stringify(payload), signal: IMG_ABORT.signal,
    });
    const d = await r.json();
    if (!r.ok) throw new Error((d && d.detail && (typeof d.detail === "string" ? d.detail : JSON.stringify(d.detail))) || ("HTTP " + r.status));
    const items = (d.data || []).map((it) => ({
      src: it.url ? it.url : (it.b64_json ? "data:image/png;base64," + it.b64_json : ""),
      revised: it.revised_prompt || "",
    })).filter((x) => x.src);
    if (!items.length) throw new Error("No image data in the response.");
    const secs = Math.round((Date.now() - t0) / 1000);
    let saved = 0;
    for (const it of items) {
      const meta = { prompt, model, size, mode, revised: it.revised };
      if (TAURI.core) {
        try { await invoke("image_store", { src: it.src, meta }); saved++; }
        catch (e) { addLooseImage(it.src, meta); toast("Could not save to gallery: " + emsg(e)); }
      } else addLooseImage(it.src, meta);
    }
    $("#imgmsg").textContent = `Done in ${secs} s` + (saved ? ` · ${saved} saved to the gallery` : "");
    if (saved) await renderGallery();
  } catch (e) {
    $("#imgmsg").textContent = e.name === "AbortError" ? "Cancelled." : "⚠ " + emsg(e);
  } finally { IMG_ABORT = null; imgBusy(false); }
}

/* an image that could not be stored locally (or outside the desktop app) */
function addLooseImage(src, meta) {
  const card = document.createElement("div"); card.className = "gcard";
  const img = document.createElement("img"); img.src = src; img.alt = meta.prompt || "";
  const cap = document.createElement("p"); cap.className = "gcap"; cap.textContent = meta.prompt || "";
  card.appendChild(img); card.appendChild(cap);
  $("#imgresult").prepend(card);
}

async function renderGallery() {
  const box = $("#imgresult");
  if (!TAURI.core) return;
  let list = [];
  try { list = await invoke("images_index"); } catch (e) {}
  $("#imgcountlabel").textContent = list.length ? `${list.length} image${list.length === 1 ? "" : "s"} · stored locally` : "";
  if (!list.length) { box.innerHTML = '<p class="muted">Nothing here yet — your generated images are kept on this machine.</p>'; return; }
  box.innerHTML = "";
  for (const it of list.slice(0, 60)) {
    const card = document.createElement("div"); card.className = "gcard";
    const img = document.createElement("img"); img.alt = it.prompt || ""; img.loading = "lazy";
    const cap = document.createElement("p"); cap.className = "gcap"; cap.textContent = it.prompt || ""; cap.title = it.prompt || "";
    const meta = document.createElement("p"); meta.className = "gmeta";
    meta.textContent = [it.model, it.size, it.ts ? new Date(it.ts).toLocaleDateString() : ""].filter(Boolean).join(" · ");
    const row = document.createElement("div"); row.className = "grow";
    const mk = (label, fn) => { const b = document.createElement("button"); b.className = "btn ghost"; b.textContent = label; b.onclick = fn; row.appendChild(b); return b; };
    mk("Save…", () => exportImage(it));
    mk("Reuse prompt", () => { $("#imgprompt").value = it.prompt || ""; $("#imgprompt").focus(); $("#imgprompt").scrollIntoView({ block: "center" }); });
    mk("As source", async () => { const u = await galleryData(it.id); if (u) { setImgMode("img2img"); setImgSource(u); $("#imgprompt").scrollIntoView({ block: "center" }); toast("Set as source image"); } });
    mk("Delete", async () => {
      if (!(await askConfirm("Delete this image from the gallery?", "Delete image", false))) return;
      try { await invoke("image_remove", { id: it.id }); delete IMG_CACHE[it.id]; } catch (e) { toast(emsg(e)); }
      renderGallery();
    });
    img.onclick = () => openLightbox(img.src, it.prompt || "");
    card.appendChild(img); card.appendChild(cap); card.appendChild(meta); card.appendChild(row);
    box.appendChild(card);
    galleryData(it.id).then((u) => { if (u) img.src = u; });
  }
}
async function galleryData(id) {
  if (IMG_CACHE[id]) return IMG_CACHE[id];
  try { IMG_CACHE[id] = await invoke("image_data", { id }); return IMG_CACHE[id]; } catch (e) { return ""; }
}
async function exportImage(it) {
  let dest;
  try {
    dest = await invoke("plugin:dialog|save", {
      options: { title: "Save image", defaultPath: it.file || "aipg-image.png" },
    });
  } catch (e) { toast("Save dialog unavailable: " + emsg(e)); return; }
  if (!dest) return;
  try { await invoke("image_export", { id: it.id, dest }); toast("Image saved"); } catch (e) { toast(emsg(e)); }
}
function openLightbox(src, caption) {
  if (!src) return;
  const lb = $("#lightbox");
  lb.querySelector("img").src = src;
  lb.querySelector("p").textContent = caption;
  lb.style.display = "";
}

function wireImages() {
  IMG_WIRED = true;
  $$('input[name=imgmode]').forEach((r) => (r.onchange = () => setImgMode(imgMode())));
  $("#imgfile").onchange = async () => { const f = $("#imgfile").files[0]; if (f) setImgSource(await fileToDataURL(f)); };
  $("#imggen").onclick = imgGenerate;
  $("#imgcancel").onclick = () => { if (IMG_ABORT) IMG_ABORT.abort(); };
  $("#imgprompt").addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); imgGenerate(); } });
  const sz = get("aipg_imgsize"); if (sz && [...$("#imgsize").options].some((o) => o.value === sz)) $("#imgsize").value = sz;
  $("#lightbox").onclick = () => { $("#lightbox").style.display = "none"; };
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") $("#lightbox").style.display = "none"; });
}
function loadImages() { if (!IMG_WIRED) wireImages(); fillImgModels(); renderGallery(); }

/* ---------- misc ---------- */
function esc(s) { return String(s == null ? "" : s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c])); }

/* notify when new grid models appear (compared to what we've seen before) */
async function checkNewModels() {
  let ids = [];
  try {
    const d = await api("/models", false);
    ids = (d.data || []).map((m) => m.id).filter((x) => x && x !== "auto").sort();
  } catch (e) { return; }
  if (!ids.length) return;
  let known = [];
  try { known = JSON.parse(get("aipg_known_models") || "[]"); } catch (e) {}
  const fresh = ids.filter((x) => !known.includes(x));
  if (known.length && fresh.length) {
    const msg = fresh.join(", ");
    toast("New grid model: " + msg);
    nativeNotify("New AIPG model available", msg + " — now on the grid.");
  }
  set("aipg_known_models", JSON.stringify(ids));
}

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
  if (view === "connect") loadConnect();
}

/* ---------- connect: local MCP connector for Claude & co. ---------- */
let CONNECT_WIRED = false;
async function copyText(txt, okMsg) {
  try { await navigator.clipboard.writeText(txt); toast(okMsg || "Copied"); }
  catch (e) { toast("Could not copy; select the text manually"); }
}
function renderConnect(st) {
  $("#cn-state").textContent = st.enabled ? "enabled" : "off";
  $("#cn-state").className = "chip" + (st.enabled ? " on" : "");
  $("#cn-enable").style.display = st.enabled ? "none" : "";
  $("#cn-disable").style.display = st.enabled ? "" : "none";
  $("#cn-clients").style.display = st.enabled ? "" : "none";
  $("#cn-claude-add").textContent = st.claude_registered ? "Update Claude Desktop entry" : "Add to Claude Desktop";
  $("#cn-claude-remove").style.display = st.claude_registered ? "" : "none";
  $("#cn-claude-msg").textContent = st.claude_registered
    ? "Registered. Quit and reopen Claude Desktop to load it."
    : st.claude_found ? "Claude Desktop found on this computer."
    : "Claude Desktop was not found here; the entry is written anyway and used once it is installed.";
  $("#cn-claude-path").textContent = st.claude_config;
  $("#cn-command").textContent = st.command;
  $("#cn-snippet").textContent = st.snippet;
}
async function loadConnect() {
  if (!TAURI.core) { $("#cn-unavailable").style.display = ""; $("#cn-body").style.display = "none"; return; }
  if (!CONNECT_WIRED) {
    CONNECT_WIRED = true;
    const run = async (cmd, args, msg) => {
      try { await invoke(cmd, args || {}); if (msg) toast(msg); } catch (e) { toast(emsg(e)); }
      loadConnect();
    };
    $("#cn-enable").onclick = () => {
      if (!apiKey()) { toast("Save your API key in Settings first"); return show("settings"); }
      run("connector_enable", { apiKey: apiKey() }, "Connector enabled");
    };
    $("#cn-disable").onclick = async () => {
      if (!(await askConfirm("Turn the connector off? Its stored key and the Claude Desktop entry are removed.", "Disable connector", false))) return;
      run("connector_disable", {}, "Connector disabled");
    };
    $("#cn-claude-add").onclick = () => run("connector_claude_add", {}, "Added. Restart Claude Desktop.");
    $("#cn-claude-remove").onclick = () => run("connector_claude_remove", {}, "Removed from Claude Desktop");
    $("#cn-copy-command").onclick = () => copyText($("#cn-command").textContent, "Command copied");
    $("#cn-copy-snippet").onclick = () => copyText($("#cn-snippet").textContent, "Config copied");
  }
  try { renderConnect(await invoke("connector_status")); } catch (e) { toast(emsg(e)); }
}
/* keep the connector's stored key in step with the key in Settings */
async function syncConnectorKey() {
  if (!TAURI.core) return;
  try {
    const st = await invoke("connector_status");
    if (!st.enabled) return;
    if (apiKey()) await invoke("connector_enable", { apiKey: apiKey() });
    else await invoke("connector_disable");
  } catch (e) {}
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
  $("#savekey").onclick = () => { set(LS.key, $("#key").value.trim()); toast("Key saved"); syncConnectorKey(); startPolling(); show("overview"); };
  $("#clearkey").onclick = () => { del(LS.key); $("#key").value = ""; toast("Key forgotten"); syncConnectorKey(); startPolling(); loadOverview(); };
  $("#saventfy").onclick = () => { set(LS.ntfy, $("#ntfy").value.trim()); toast("ntfy topic saved"); };
  $("#alerts").onchange = (e) => {
    set(LS.alerts, e.target.checked ? "1" : "0");
    if (e.target.checked) nativeNotify("Alerts on", "You'll be notified if a worker goes offline.");
    startPolling();
  };

  // in-app-only settings: auto-start worker + close-to-tray
  if (TAURI.core) {
    $("#autostart").checked = get("aipg_autostart") === "1";
    $("#autostart").onchange = (e) => set("aipg_autostart", e.target.checked ? "1" : "0");
    cfgGet().then((c) => { $("#closetray").checked = c.close_to_tray !== false; });
    $("#closetray").onchange = (e) => cfgPatch("close_to_tray", e.target.checked);
    invoke("plugin:autostart|is_enabled").then((v) => { $("#loginstart").checked = !!v; }).catch(() => {});
    $("#loginstart").onchange = async (e) => {
      try { await invoke(e.target.checked ? "plugin:autostart|enable" : "plugin:autostart|disable"); }
      catch (err) { toast(emsg(err)); e.target.checked = !e.target.checked; }
    };
    wireUpdates();
  } else {
    ["#field-autostart", "#field-closetray", "#field-loginstart", "#field-update"].forEach((id) => {
      const f = $(id); if (f) f.style.display = "none";
    });
  }

  // external links
  document.body.addEventListener("click", (e) => {
    const a = e.target.closest("[data-ext]");
    if (a) { e.preventDefault(); openExternal(a.dataset.ext); }
  });

  // first-run onboarding
  if (!apiKey() && get("aipg_onboarded") !== "1") {
    $("#welcome").style.display = "";
    setTimeout(() => $("#wc-key").focus(), 50);
    $("#wc-save").onclick = () => {
      const k = $("#wc-key").value.trim();
      if (k) { set(LS.key, k); $("#key").value = k; }
      set("aipg_onboarded", "1"); $("#welcome").style.display = "none"; startPolling();
      show(k ? "overview" : "chat");
    };
    $("#wc-key").addEventListener("keydown", (e) => { if (e.key === "Enter") $("#wc-save").click(); });
    $("#wc-skip").onclick = () => { set("aipg_onboarded", "1"); $("#welcome").style.display = "none"; };
  }

  show("overview");
  startPolling();
  checkNewModels();
  bootAutostart();
  checkUpdate(false);
});

/* ---------- self-update (signed releases from GitHub) ---------- */
function wireUpdates() {
  const app = TAURI.app;
  if (app && app.getVersion) app.getVersion().then((v) => { $("#appver").textContent = "v" + v; }).catch(() => {});
  $("#btn-update").onclick = () => checkUpdate(true);
  $("#btn-update-install").onclick = installUpdate;
  listen("update-progress", (e) => {
    const { received, total } = e.payload || {};
    $("#updmsg").textContent = total
      ? `Downloading update… ${fmtBytes(received)} / ${fmtBytes(total)} · ${Math.round(received / total * 100)} %`
      : `Downloading update… ${fmtBytes(received)}`;
  });
}
async function checkUpdate(manual) {
  if (!TAURI.core) return;
  if (manual) $("#updmsg").textContent = "Checking…";
  try {
    const v = await invoke("update_check");
    if (v) {
      $("#updmsg").textContent = `Version ${v} is available.`;
      $("#btn-update-install").style.display = "";
      if (!manual) toast(`Update ${v} available — see Settings`);
    } else if (manual) $("#updmsg").textContent = "You are on the latest version.";
  } catch (e) { if (manual) $("#updmsg").textContent = "Update check failed: " + emsg(e); }
}
async function installUpdate() {
  if (WORKER_RUNNING && !(await askConfirm(
    "Installing the update stops the running worker and restarts the app. Continue?", "Install update", false))) return;
  $("#btn-update-install").disabled = true; $("#btn-update").disabled = true;
  $("#updmsg").textContent = "Downloading update…";
  WANT_RUNNING = false;
  try { await invoke("update_install"); }
  catch (e) {
    $("#updmsg").textContent = "Update failed: " + emsg(e);
    $("#btn-update-install").disabled = false; $("#btn-update").disabled = false;
  }
}

/* ---------- app config (in-app persistence via Rust) ---------- */
async function cfgGet() { try { return JSON.parse((await invoke("config_get")) || "{}"); } catch (e) { return {}; } }
async function cfgSet(obj) { try { await invoke("config_set", { data: JSON.stringify(obj) }); } catch (e) {} }
async function cfgPatch(key, val) { const c = await cfgGet(); c[key] = val; await cfgSet(c); }

async function bootAutostart() {
  if (!TAURI.core || get("aipg_autostart") !== "1" || !apiKey()) return;
  try { if (await invoke("worker_running")) return; } catch (e) { return; }
  show("worker"); // populates fields + lets the user watch progress
  $("#wname2").value = get("aipg_wname") || "";
  $("#wmodel2").value = get("aipg_wmodel") || "qwen3:8b";
  autoStartWorker();
}
