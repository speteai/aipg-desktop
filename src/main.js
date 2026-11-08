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

/* ---------- misc ---------- */
function esc(s) { return String(s == null ? "" : s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c])); }

/* ---------- views ---------- */
function show(view) {
  $$(".nav").forEach((b) => b.classList.toggle("active", b.dataset.view === view));
  $$(".view").forEach((v) => v.classList.toggle("active", v.id === "view-" + view));
  if (view === "overview") loadOverview();
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
