"use strict";
/* ---------- video (text or picture to clip via the grid's video jobs, local gallery) ----------
   The grid takes a video as a job: POST /videos/jobs answers at once with a job
   id, and the clip is fetched from GET /videos/jobs/{id} when it is done. A job
   cannot be cancelled and is billed when it completes, so every started job is
   written down first (localStorage) and collected even after a restart. */
const VID_JOBS_KEY = "aipg_video_jobs";
const VID_FPS = 24;
let VID_WIRED = false, VID_SOURCE = "", VID_TIMER = null, VID_BUSY = false, VID_RATES = {}, VID_PLAY_URL = "";

function vidJobs() { try { const l = JSON.parse(get(VID_JOBS_KEY) || "[]"); return Array.isArray(l) ? l : []; } catch (e) { return []; } }
function vidJobsSave(list) { set(VID_JOBS_KEY, JSON.stringify(list)); }
function vidJobPatch(key, patch) {
  const list = vidJobs(), j = list.find((x) => x.key === key);
  if (j) { Object.assign(j, patch); vidJobsSave(list); }
}
function vidJobDrop(key) { vidJobsSave(vidJobs().filter((x) => x.key !== key)); }
function vidSeconds(frames) { return frames / VID_FPS; }
function vidUuid() {
  if (window.crypto && crypto.randomUUID) return crypto.randomUUID();
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6] & 0x0f) | 0x40; b[8] = (b[8] & 0x3f) | 0x80;
  const h = [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}
function vidDetail(d, status) {
  const x = d && d.detail;
  return (typeof x === "string" && x) || (x ? JSON.stringify(x) : "HTTP " + status);
}

/* price per second of video, from the grid's public price book */
async function vidLoadRates() {
  try {
    const r = await fetch(API + "/pricing", { headers: { "Accept": "application/json" } });
    const d = await r.json();
    const out = {};
    for (const m of ((d.price_book || {}).models || [])) {
      const v = m.rates && m.rates.per_video_second_usd;
      if (typeof v === "number" && v > 0) out[String(m.model).toLowerCase()] = v;
    }
    VID_RATES = out;
  } catch (e) {}
  vidPaintPrice();
}
function vidCost() {
  const rate = VID_RATES[($("#vidmodel").value || "").toLowerCase()];
  const frames = parseInt($("#vidlen").value, 10) || 73;
  return rate ? rate * vidSeconds(frames) : null;
}
function vidPaintPrice() {
  const c = vidCost();
  $("#vidprice").textContent = c == null ? "" : `about $${c.toFixed(2)} per clip`;
}

async function fillVidModels() {
  const sel = $("#vidmodel"), keep = sel.value || get("aipg_vidmodel") || "";
  try {
    const r = await fetch(API + "/status/models", { headers: { "Accept": "application/json" } });
    const list = await r.json();
    const vids = (Array.isArray(list) ? list : []).filter((m) => m.type === "video");
    sel.innerHTML = vids.length
      ? vids.map((m) => `<option value="${esc(m.name)}">${esc(m.name)}</option>`).join("")
      : '<option value="">(no video models online)</option>';
    if (keep && [...sel.options].some((o) => o.value === keep)) sel.value = keep;
  } catch (e) { sel.innerHTML = '<option value="">(could not load)</option>'; }
  vidPaintPrice();
}

function setVidSource(dataUrl) {
  VID_SOURCE = dataUrl || "";
  const box = $("#vidsrcprev");
  if (!VID_SOURCE) { box.innerHTML = ""; $("#vidfile").value = ""; return; }
  box.innerHTML = `<img alt="start picture"><button class="btn ghost" id="vidsrcclear">Remove</button>`;
  box.querySelector("img").src = VID_SOURCE;
  $("#vidsrcclear").onclick = () => setVidSource("");
}

async function vidGenerate() {
  if (VID_BUSY) return;
  VID_BUSY = true;
  $("#vidgen").disabled = true;
  try { await vidGenerateRun(); } finally { VID_BUSY = false; $("#vidgen").disabled = false; }
}
async function vidGenerateRun() {
  const msg = $("#vidmsg");
  await authFresh();
  const key = apiKey(); if (!key) return needConnection();
  const model = $("#vidmodel").value; if (!model) { toast("No video model available"); return; }
  const prompt = $("#vidprompt").value.trim(); if (!prompt) { toast("Describe the clip first"); return; }
  const useSrc = $("#vidusesrc").checked;
  if (useSrc && !VID_SOURCE) { toast("Choose a start picture, or untick the box"); return; }
  if (protectMode() !== "off") {
    const hits = Privacy.find(prompt);
    if (hits.length && !(await askConfirm(
      `The video prompt contains: ${Privacy.summary(hits)}.\n\nVideo prompts go to someone else's computer in plain text and cannot be masked. Send it anyway?`,
      "Private data in the prompt"))) return;
  }
  const frames = parseInt($("#vidlen").value, 10) || 73, size = $("#vidsize").value;
  const cost = vidCost();
  if (cost == null) { msg.textContent = "⚠ No price is published for this model; nothing was sent."; return; }
  // the most this clip may cost: the published price, plus a little room for rounding
  const ceiling = Math.ceil(cost * 1e6) + 1000;
  const body = { model, prompt, frames, size, max_cost_micro_usd: ceiling };
  if (useSrc) body.image = VID_SOURCE;
  set("aipg_vidmodel", model); set("aipg_vidsize", size); set("aipg_vidlen", String(frames));

  // written down before sending: if the answer is lost, the job is found again by this key
  const idem = vidUuid();
  const list = vidJobs();
  list.push({ key: idem, job: "", t: Date.now(), prompt, model, size, frames, src: useSrc, tries: 0 });
  vidJobsSave(list);
  renderVidJobs();
  msg.textContent = "Sending…";
  try {
    const r = await fetch(API + "/videos/jobs", {
      method: "POST",
      headers: { "Authorization": "Bearer " + key, "Content-Type": "application/json", "Idempotency-Key": idem },
      body: JSON.stringify(body),
    });
    let d = null;
    try { d = await r.json(); } catch (e) {}
    if (r.status !== 200 && r.status !== 202) {
      // refused before a job existed: nothing runs and nothing is billed
      vidJobDrop(idem);
      const why = r.status === 402 ? "Not enough credit for this clip."
        : r.status === 429 ? "The grid is busy with your other clips or you sent too many requests; try again in a minute."
        : vidDetail(d, r.status);
      msg.textContent = "⚠ " + why + " Nothing was billed.";
      renderVidJobs();
      return;
    }
    vidJobPatch(idem, { job: (d && d.job_id) || "", state: (d && d.state) || "" });
    msg.textContent = "Started. The clip appears below when it is ready.";
  } catch (e) {
    // no answer: the job may or may not exist. Keep the entry; the next check asks by key.
    msg.textContent = "No answer from the grid yet; checking whether the clip was started…";
  }
  renderVidJobs();
  vidWatch(true);
}

/* one check of every open job */
async function vidCheck() {
  const list = vidJobs();
  if (!list.length) return;
  await authFresh();
  const key = apiKey();
  if (!key) return; // not connected right now; the jobs stay written down
  let changed = false, stored = false;
  for (const j of list) {
    if (j.hold) continue; // waiting for the user (download failed several times)
    let r, d = null;
    try {
      r = j.job
        ? await fetch(API + "/videos/jobs/" + encodeURIComponent(j.job), { headers: { "Authorization": "Bearer " + key } })
        : await fetch(API + "/videos/jobs", { headers: { "Authorization": "Bearer " + key, "Idempotency-Key": j.key } });
      try { d = await r.json(); } catch (e) {}
    } catch (e) { continue; } // offline: try again next time
    const age = Date.now() - (j.t || 0);
    if (r.status === 404) {
      // never accepted (asked by key), or no longer kept by the grid
      if (!j.job ? age > 120000 : age > 86400000) {
        vidJobDrop(j.key); changed = true;
        $("#vidmsg").textContent = j.job ? "A clip is no longer kept by the grid and was removed from the list."
          : "⚠ The grid never received the last request. Nothing was billed.";
      }
      continue;
    }
    if (!r.ok || !d) continue;
    if (!j.job && d.job_id) { vidJobPatch(j.key, { job: d.job_id }); }
    if (d.state !== j.state) { vidJobPatch(j.key, { state: d.state }); changed = true; }
    if (d.state === "completed") {
      const media = d.result && Array.isArray(d.result.media) ? d.result.media[0] : null;
      const url = media && media.url;
      const meta = {
        kind: "video", prompt: j.prompt, model: d.model || j.model, size: j.size, frames: j.frames,
        seconds: Math.round(vidSeconds(j.frames) * 100) / 100, from_picture: !!j.src,
        cost_usd: typeof d.actual_micro === "number" ? d.actual_micro / 1e6 : null,
        seed: media && media.seed != null ? String(media.seed) : "", job: d.job_id || j.job,
        worker: (d.result && d.result.worker) || "", gen_time: (d.result && d.result.gen_time) || null,
      };
      if (!url) { vidJobPatch(j.key, { hold: true, err: "The grid reports the clip as done but sent no file." }); changed = true; continue; }
      try {
        const entry = await invoke("image_store", { src: url, meta });
        vidJobDrop(j.key); changed = true; stored = true;
        toast("Video ready");
        vidMakePoster(entry.id);
      } catch (e) {
        const tries = (j.tries || 0) + 1;
        vidJobPatch(j.key, { tries, err: emsg(e), hold: tries >= 5 });
        changed = true;
      }
    } else if (d.state === "closed_without_result") {
      vidJobDrop(j.key); changed = true;
      const paid = typeof d.actual_micro === "number" && d.actual_micro > 0;
      $("#vidmsg").textContent = "⚠ The grid ended a clip without a result" +
        (paid ? ` and billed $${(d.actual_micro / 1e6).toFixed(3)}.` : "; nothing was billed.");
    }
  }
  if (changed) renderVidJobs();
  if (stored) renderVidGallery();
}

/* keep checking while jobs are open; stops by itself when the list is empty */
function vidWatch(soon) {
  if (VID_TIMER) return;
  const step = async () => {
    VID_TIMER = null;
    try { await vidCheck(); } catch (e) {}
    if (vidJobs().some((j) => !j.hold)) VID_TIMER = setTimeout(step, 4000);
  };
  VID_TIMER = setTimeout(step, soon ? 1500 : 4000);
}

function vidStateText(j) {
  if (j.err && j.hold) return "download failed";
  if (j.err) return "fetching again";
  const s = j.state || "";
  if (s === "execution_uncertain") return "generating";
  if (s === "queued" || s === "pending_dispatch") return "waiting for a worker";
  if (s === "completed") return "fetching";
  return j.job ? "started" : "sending";
}
function renderVidJobs() {
  const box = $("#vidjobs"); if (!box) return;
  box.innerHTML = "";
  for (const j of vidJobs()) {
    const row = document.createElement("div"); row.className = "vidjob";
    const st = document.createElement("span"); st.className = "st";
    const mins = Math.max(0, Math.round((Date.now() - (j.t || 0)) / 60000));
    st.textContent = `${vidStateText(j)} · ${vidSeconds(j.frames).toFixed(0)} s clip` + (mins >= 2 ? ` · ${mins} min` : "");
    const pr = document.createElement("span"); pr.className = "pr"; pr.textContent = j.prompt || ""; pr.title = j.err || j.prompt || "";
    row.appendChild(st); row.appendChild(pr);
    if (j.hold) {
      const again = document.createElement("button"); again.className = "btn ghost"; again.textContent = "Try again";
      again.onclick = () => { vidJobPatch(j.key, { hold: false, tries: 0, err: "" }); renderVidJobs(); vidWatch(true); };
      const drop = document.createElement("button"); drop.className = "btn ghost"; drop.textContent = "Remove";
      drop.onclick = async () => {
        if (!(await askConfirm("Remove this clip from the list? If it was generated, it has been billed and will not be fetched.", "Remove clip"))) return;
        vidJobDrop(j.key); renderVidJobs();
      };
      row.appendChild(again); row.appendChild(drop);
    }
    box.appendChild(row);
  }
}

/* the stored clip as an object URL the page may play */
async function vidObjectUrl(id) {
  const data = await invoke("image_data", { id });
  const comma = data.indexOf(",");
  const bin = atob(data.slice(comma + 1));
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return URL.createObjectURL(new Blob([bytes], { type: "video/mp4" }));
}

/* a small still frame for the gallery, taken once after the clip was stored */
async function vidMakePoster(id) {
  let url = "";
  try {
    url = await vidObjectUrl(id);
    const v = document.createElement("video");
    v.muted = true; v.playsInline = true; v.preload = "auto";
    const frame = await new Promise((resolve, reject) => {
      const giveUp = setTimeout(() => reject(new Error("timeout")), 15000);
      v.onerror = () => { clearTimeout(giveUp); reject(new Error("cannot decode")); };
      v.onloadeddata = () => { try { v.currentTime = Math.min(0.5, (v.duration || 1) / 2); } catch (e) { reject(e); } };
      v.onseeked = () => {
        clearTimeout(giveUp);
        const w = 480, h = Math.max(1, Math.round(w * (v.videoHeight || 9) / (v.videoWidth || 16)));
        const c = document.createElement("canvas"); c.width = w; c.height = h;
        c.getContext("2d").drawImage(v, 0, 0, w, h);
        resolve(c.toDataURL("image/jpeg", 0.72));
      };
      v.src = url;
    });
    await invoke("image_poster_set", { id, poster: frame });
    renderVidGallery();
  } catch (e) { /* the clip is stored; it just shows without a preview */ }
  finally { if (url) URL.revokeObjectURL(url); }
}

async function renderVidGallery() {
  const box = $("#vidresult");
  if (!box) return;
  if (!TAURI.core) { box.innerHTML = '<p class="muted">Clips are stored by the desktop app.</p>'; return; }
  let list = [];
  try { list = await invoke("images_index"); } catch (e) {}
  list = list.filter((x) => x.kind === "video");
  $("#vidcountlabel").textContent = list.length ? `${list.length} clip${list.length === 1 ? "" : "s"} · stored locally` : "";
  if (!list.length) { box.innerHTML = '<p class="muted">Nothing here yet. Your generated clips are kept on this machine.</p>'; return; }
  box.innerHTML = "";
  for (const it of list.slice(0, 120)) {
    const card = document.createElement("div"); card.className = "gcard vcard";
    const thumb = document.createElement("button"); thumb.className = "vthumb"; thumb.title = "Play";
    if (it.poster) { const img = document.createElement("img"); img.alt = it.prompt || ""; img.src = it.poster; thumb.appendChild(img); }
    thumb.onclick = () => vidPlay(it);
    const cap = document.createElement("p"); cap.className = "gcap"; cap.textContent = it.prompt || ""; cap.title = it.prompt || "";
    const meta = document.createElement("p"); meta.className = "gmeta";
    meta.textContent = [
      it.model, it.seconds ? `${Number(it.seconds).toFixed(0)} s` : "", it.size,
      typeof it.cost_usd === "number" ? `$${it.cost_usd.toFixed(3)}` : "",
      it.ts ? new Date(it.ts).toLocaleDateString() : "",
    ].filter(Boolean).join(" · ");
    const row = document.createElement("div"); row.className = "grow";
    const mk = (label, fn) => { const b = document.createElement("button"); b.className = "btn ghost"; b.textContent = label; b.onclick = fn; row.appendChild(b); return b; };
    mk("Play", () => vidPlay(it));
    mk("Save…", async () => { try { if (await invoke("image_export", { id: it.id })) toast("Video saved"); } catch (e) { toast(emsg(e)); } });
    mk("Reuse prompt", () => { $("#vidprompt").value = it.prompt || ""; $("#vidprompt").focus(); $("#vidprompt").scrollIntoView({ block: "center" }); });
    mk("Delete", async () => {
      if (!(await askConfirm("Delete this clip from this computer?", "Delete clip"))) return;
      try { await invoke("image_remove", { id: it.id }); } catch (e) { toast(emsg(e)); }
      renderVidGallery();
    });
    card.appendChild(thumb); card.appendChild(cap); card.appendChild(meta); card.appendChild(row);
    box.appendChild(card);
  }
}

async function vidPlay(it) {
  const lb = $("#vidbox"), v = lb.querySelector("video");
  try {
    vidClose();
    VID_PLAY_URL = await vidObjectUrl(it.id);
    v.src = VID_PLAY_URL;
    lb.querySelector("p").textContent = it.prompt || "";
    lb.style.display = "";
    v.play().catch(() => {});
  } catch (e) { toast("Could not open the clip: " + emsg(e)); }
}
function vidClose() {
  const lb = $("#vidbox"), v = lb.querySelector("video");
  try { v.pause(); } catch (e) {}
  v.removeAttribute("src"); try { v.load(); } catch (e) {}
  if (VID_PLAY_URL) { URL.revokeObjectURL(VID_PLAY_URL); VID_PLAY_URL = ""; }
  lb.style.display = "none";
}

function wireVideo() {
  VID_WIRED = true;
  $("#vidgen").onclick = vidGenerate;
  $("#vidprompt").addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); vidGenerate(); } });
  $("#vidusesrc").onchange = () => { $("#vidsrcrow").style.display = $("#vidusesrc").checked ? "" : "none"; };
  $("#vidfile").onchange = async () => {
    const f = $("#vidfile").files[0]; if (!f) return;
    if (f.size > 12 * 1048576) { toast("The picture is larger than 12 MB"); $("#vidfile").value = ""; return; }
    setVidSource(await fileToDataURL(f));
  };
  $("#vidmodel").onchange = vidPaintPrice;
  $("#vidlen").onchange = vidPaintPrice;
  const sz = get("aipg_vidsize"); if (sz && [...$("#vidsize").options].some((o) => o.value === sz)) $("#vidsize").value = sz;
  const ln = get("aipg_vidlen"); if (ln && [...$("#vidlen").options].some((o) => o.value === ln)) $("#vidlen").value = ln;
  const lb = $("#vidbox");
  lb.addEventListener("mousedown", (e) => { if (e.target === lb) vidClose(); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && lb.style.display !== "none") vidClose(); });
}
function loadVideo() {
  if (!VID_WIRED) wireVideo();
  fillVidModels().then(vidLoadRates);
  renderVidJobs();
  renderVidGallery();
  if (vidJobs().length) vidWatch(true);
}

// clips started before the app was closed are collected without opening the view
if (TAURI.core && vidJobs().length) setTimeout(() => vidWatch(false), 8000);
