"use strict";

const $ = (id) => document.getElementById(id);
const els = {
  list: $("sessions"),
  count: $("count"),
  main: $("main"),
  record: $("btn-record"),
  example: $("btn-example"),
  scrim: $("scrim"),
  dialog: $("dialog"),
  toast: $("toast"),
};

const SURE = { high: "very likely", medium: "likely", low: "possible" };
const INCIDENT_PAGE = 60;
const LIVE_EVERY_MS = 500;
const HOTKEY = "Ctrl+Alt+L";

let invoke = null;
let caps = { canRecord: false };
let sessions = [];
let selectedId = null;
let view = null; // { kind: "report" | "empty" | "live", ... }
let timeline = null;
const cache = new Map(); // id -> parsed session, most recent last

if (/Mac/.test(navigator.userAgent)) document.documentElement.classList.add("mac");

/* ---------- backend ---------- */

async function connect() {
  if (window.__TAURI__) {
    invoke = window.__TAURI__.core.invoke;
    const win = window.__TAURI__.window.getCurrentWindow();
    $("btn-min").onclick = () => win.minimize();
    $("btn-max").onclick = () => win.toggleMaximize();
    $("btn-close").onclick = () => win.close();
    return;
  }
  // Opened in a plain browser: use the preview backend that serves the example sessions.
  await new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = "../ui-dev/mock.js";
    s.onload = resolve;
    s.onerror = () => reject(new Error("preview backend missing"));
    document.head.append(s);
  });
  invoke = window.culpritDev.invoke;
}

// Sessions arrive as raw JSON bytes so the backend never re-encodes them.
function decode(value) {
  if (value instanceof ArrayBuffer) return JSON.parse(new TextDecoder().decode(value));
  if (ArrayBuffer.isView(value)) return JSON.parse(new TextDecoder().decode(value));
  if (typeof value === "string") return JSON.parse(value);
  return value;
}

async function loadSession(id) {
  if (cache.has(id)) {
    const s = cache.get(id);
    cache.delete(id);
    cache.set(id, s);
    return s;
  }
  const s = decode(await invoke(id === "example" ? "example_session" : "get_session", id === "example" ? {} : { id }));
  cache.set(id, s);
  while (cache.size > 6) cache.delete(cache.keys().next().value);
  return s;
}

/* ---------- formatting ---------- */

const esc = window.escapeHtml;

// Escapes text and keeps a number on the same line as its unit ("28 ms", "40%").
function prose(s) {
  return esc(s).replace(/(\d) (ms|s|fps|MB\/s|Mbit\/s|MB|GB|GHz|°C|pings?|seconds|minutes)\b/g, "$1\u00a0$2");
}

function when(ms) {
  const d = new Date(ms);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  const time = d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  if (sameDay) return { day: "Today", time };
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return { day: "Yesterday", time };
  return { day: d.toLocaleDateString(undefined, { month: "short", day: "numeric" }), time };
}

function length(ms) {
  const m = Math.max(1, Math.round(ms / 60000));
  if (m < 60) return `${m} min`;
  return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, "0")} min`;
}

function fixed(v, digits = 0) {
  return v == null || Number.isNaN(v) ? null : Number(v).toFixed(digits);
}

let toastTimer = 0;
function toast(text) {
  els.toast.textContent = text;
  els.toast.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => els.toast.classList.remove("show"), 2600);
}

/* ---------- session list ---------- */

function renderList() {
  els.count.textContent = sessions.length ? sessions.length : "";
  if (!sessions.length) {
    els.list.innerHTML = "";
    return;
  }
  els.list.innerHTML = sessions
    .map((s) => {
      const w = when(s.startedAt);
      return `<button class="item${s.id === selectedId ? " selected" : ""}" role="option" data-id="${esc(s.id)}" aria-selected="${s.id === selectedId}">
        <div class="item-top"><span class="item-game">${esc(s.game || "Unknown game")}</span><span class="item-when">${esc(w.time)}</span></div>
        <div class="item-line">${esc(s.headline)}</div>
        <div class="item-sub"><span class="dot ${esc(s.mood)}"></span>${esc(w.day)}<span>${esc(length(s.durationMs))}</span></div>
      </button>`;
    })
    .join("");
}

function markSelected() {
  for (const el of els.list.querySelectorAll(".item")) {
    const on = el.dataset.id === selectedId;
    el.classList.toggle("selected", on);
    el.setAttribute("aria-selected", on);
    if (on) el.scrollIntoView({ block: "nearest" });
  }
}

async function refreshSessions() {
  sessions = await invoke("list_sessions");
  renderList();
}

/* ---------- views ---------- */

function setView(next, html) {
  timeline?.destroy();
  timeline = null;
  view = next;
  els.main.innerHTML = html;
  els.main.scrollTop = 0;
}

function renderEmpty() {
  stopLive();
  selectedId = null;
  markSelected();
  const text = caps.canRecord
    ? `Start a recording and play. When something feels off, press <kbd>${HOTKEY}</kbd>. Culprit works out what caused each hitch and lag spike, and what to do about it.`
    : "This build shows reports but can't record yet. Open the example to see what Culprit tells you after a session.";
  setView(
    { kind: "empty" },
    `<div class="empty"><div class="empty-body">
      <svg class="empty-mark" viewBox="0 0 16 16"><path d="M1 11h3l2-6 3 9 2-5h4"/></svg>
      <h1>No sessions yet</h1>
      <p>${text}</p>
      <div class="btns">
        ${caps.canRecord ? '<button class="btn primary" data-act="record">Start recording</button>' : ""}
        <button class="btn" data-act="example">Example report</button>
      </div>
    </div></div>`,
  );
}

async function openSession(id) {
  stopLive();
  selectedId = id === "example" ? null : id;
  markSelected();
  let session;
  try {
    session = await loadSession(id);
  } catch (e) {
    toast("Couldn't open that session.");
    console.error(e);
    return;
  }
  // The user may have clicked another session while this one loaded.
  if ((id === "example" ? null : id) !== selectedId) return;
  renderReport(session, id === "example");
}

function stat(label, value, unit = "", tone = "") {
  if (value == null) return "";
  return `<div class="stat"><div class="stat-label">${label}</div><div class="stat-value ${tone}">${esc(value)}${unit ? `<small>${unit}</small>` : ""}</div></div>`;
}

// Lays tiles out in even rows: one row up to six, otherwise rows of four, padded so the
// grid never shows a hole.
function statGrid(tiles, id = "") {
  const list = tiles.filter(Boolean);
  if (!list.length) return "";
  const cols = list.length <= 6 ? list.length : 4;
  while (list.length % cols) list.push('<div class="stat"></div>');
  return `<div class="stats"${id ? ` id="${id}"` : ""} style="--cols:${cols}">${list.join("")}</div>`;
}

function hasChart(session) {
  const c = session.charts;
  return c.frameMax.some((v) => v != null) || c.pings.some((p) => (p.role === "server" || p.role === "serverPath") && p.rtt.some((v) => v != null));
}

function renderReport(session, example) {
  const r = session.report;
  const w = when(session.startedAt);
  const perf = r.perf;
  const net = r.net;
  const stutterTone = !perf ? "" : perf.stuttersPerMin >= 1 ? "bad" : perf.stutters > 0 ? "warn" : "";
  const lossTone = !net || net.lossPct == null ? "" : net.lossPct >= 1 ? "bad" : net.lossPct > 0 ? "warn" : "";
  const lagTone = !net ? "" : net.lagSpikes > 0 ? "warn" : "";
  const stats = [
    perf && stat("average fps", fixed(perf.avgFps)),
    perf && stat("1% low", fixed(perf.low1Fps)),
    perf && stat("stutters", String(perf.stutters), "", stutterTone),
    perf && stat("worst frame", fixed(perf.worstMs), "ms"),
    net && stat(net.pingMs != null ? "ping" : "router ping", fixed(net.pingMs ?? net.routerMs), "ms"),
    net && net.pingMs != null && stat("jitter", fixed(net.jitterMs, 1), "ms"),
    net && net.lossPct != null && stat("packet loss", fixed(net.lossPct, 1), "%", lossTone),
    net && (net.server || net.lagSpikes > 0) && stat("lag spikes", String(net.lagSpikes), "", lagTone),
  ].filter(Boolean);

  const roles = new Set(session.charts.pings.filter((p) => p.rtt.some((v) => v != null)).map((p) => p.role));
  if (!roles.has("server") && !roles.has("serverPath")) roles.clear();
  const legend = [
    '<span><i class="swatch"></i>frame time</span>',
    roles.has("server") || roles.has("serverPath") ? '<span><i class="swatch"></i>server</span>' : "",
    roles.has("isp") ? '<span><i class="swatch isp"></i>provider</span>' : "",
    roles.has("router") ? '<span><i class="swatch router"></i>router</span>' : "",
    r.incidents.some((i) => i.kind === "stutter") ? '<span><i class="swatch tick stutter"></i>stutter</span>' : "",
    r.incidents.some((i) => i.kind === "lag") ? '<span><i class="swatch tick lag"></i>lag spike</span>' : "",
  ].join("");

  const culprits = r.culprits
    .map((c, i) => {
      const noun = c.kind === "lag" ? (c.of === 1 ? "lag spike" : "lag spikes") : c.of === 1 ? "stutter" : "stutters";
      return `<article class="culprit${i === 0 ? " lead" : ""}">
        <div class="culprit-top">
          <span class="culprit-title">${esc(c.title)}</span>
          <span class="culprit-count">${c.count} of ${c.of} ${noun}</span>
          <span class="sure ${c.confidence}">${SURE[c.confidence]}</span>
        </div>
        <div class="bar ${c.kind}"><span style="width:${Math.max(2, (c.count / Math.max(1, c.of)) * 100).toFixed(1)}%"></span></div>
        <p class="why">${prose(c.why)}</p>
        <p class="fix"><b>Fix</b>${prose(c.fix)}</p>
      </article>`;
    })
    .join("");

  const mc = session.machine ?? {};
  const link = { ethernet: "Ethernet", wifi: session.connection?.wifiBandGhz ? `Wi-Fi ${session.connection.wifiBandGhz} GHz` : "Wi-Fi" }[session.connection?.kind];
  const machine = [mc.cpu, mc.gpu, mc.memTotalMb ? `${Math.round(mc.memTotalMb / 1024)} GB RAM` : null, link].filter(Boolean);
  const notes = r.notes.map((n) => `<p>${prose(n)}</p>`).join("");

  setView(
    { kind: "report", session, example, filter: "all", showAll: false, open: new Set(), picked: -1 },
    `<div class="page">
      <div class="meta">
        <span class="game">${esc(r.game || "Unknown game")}</span>
        <span>${esc(w.day)}, ${esc(w.time)}</span>
        <span>${esc(length(r.durationMs))}</span>
        ${example ? '<span class="tag">example</span>' : ""}
      </div>
      <h1 class="headline"><span class="dot ${r.mood}"></span><span>${prose(r.headline)}</span></h1>
      ${r.detail ? `<p class="detail">${prose(r.detail)}</p>` : ""}
      ${statGrid(stats)}
      ${hasChart(session) ? `<section class="timeline"><div class="tl-head"><span>timeline</span><div class="legend">${legend}</div></div><div id="tl"></div></section>` : ""}
      ${culprits ? `<h2>what caused it</h2><div class="culprits">${culprits}</div>` : ""}
      ${r.incidents.length ? `<h2>moments<div class="chips" id="chips"></div></h2><div class="incidents" id="incidents"></div>` : ""}
      ${notes ? `<div class="notes">${notes}</div>` : ""}
      ${machine.length ? `<div class="machine">${machine.map(esc).join(", ")}</div>` : ""}
    </div>`,
  );

  const host = $("tl");
  if (host) timeline = new Timeline(host, session, (i) => pickIncident(i, true));
  renderIncidents();
}

function filtered() {
  const { session, filter } = view;
  return session.report.incidents.map((inc, i) => ({ inc, i })).filter(({ inc }) => filter === "all" || inc.kind === filter);
}

function renderIncidents() {
  const list = $("incidents");
  if (!list || view?.kind !== "report") return;
  const all = view.session.report.incidents;
  const count = (k) => all.filter((i) => i.kind === k).length;
  const chips = [["all", "All", all.length], ["stutter", "Stutters", count("stutter")], ["lag", "Lag", count("lag")], ["marked", "Marked", count("marked")]]
    .filter(([k, , n]) => k === "all" || n > 0);
  $("chips").innerHTML =
    chips.length > 2
      ? chips.map(([k, label, n]) => `<button class="chip${view.filter === k ? " on" : ""}" data-filter="${k}">${label}<span class="n">${n}</span></button>`).join("")
      : "";

  const rows = filtered();
  const shown = view.showAll ? rows : rows.slice(0, INCIDENT_PAGE);
  list.innerHTML =
    shown
      .map(({ inc, i }) => {
        const open = view.open.has(i);
        const evidence = inc.evidence.map((e) => `<p>${prose(e)}</p>`).join("");
        const marked = inc.marked && inc.kind !== "marked" ? '<p class="marked-note">You pressed the hotkey right after this.</p>' : "";
        return `<div class="inc${open ? " open" : ""}${view.picked === i ? " picked" : ""}" data-i="${i}">
          <button class="inc-row" data-i="${i}" aria-expanded="${open}">
            <span class="inc-time">${clock(inc.t)}</span>
            <span class="kind ${inc.kind}"></span>
            <span class="inc-what">${esc(inc.what)}</span>
            <span class="inc-cause">${esc(inc.causeTitle)}</span>
            <svg class="chev" viewBox="0 0 16 16"><path d="M6 4l4 4-4 4"/></svg>
          </button>
          <div class="inc-body">${marked}${evidence}<span class="sure ${inc.confidence}">${SURE[inc.confidence]}</span></div>
        </div>`;
      })
      .join("") +
    (rows.length > shown.length ? `<button class="more" data-act="more">Show all ${rows.length}</button>` : "");
}

function pickIncident(i, fromChart) {
  if (view?.kind !== "report") return;
  const inc = view.session.report.incidents[i];
  if (!inc) return;
  if (view.filter !== "all" && view.filter !== inc.kind) view.filter = "all";
  const position = filtered().findIndex((r) => r.i === i);
  if (position >= INCIDENT_PAGE) view.showAll = true;
  if (fromChart) view.open.add(i);
  else if (view.open.has(i)) view.open.delete(i);
  else view.open.add(i);
  view.picked = i;
  timeline?.pick(i);
  renderIncidents();
  if (fromChart) document.querySelector(`.inc[data-i="${i}"]`)?.scrollIntoView({ block: "center", behavior: "smooth" });
}

/* ---------- live view ---------- */

let liveTimer = 0;
let liveBusy = false;

function stopLive() {
  clearTimeout(liveTimer);
  liveTimer = 0;
}

function renderLiveShell() {
  selectedId = null;
  markSelected();
  setView(
    { kind: "live" },
    `<div class="page">
      <div class="live-head">
        <span class="live-badge"><span class="rec-dot"></span>Recording</span>
        <span class="live-clock" id="lv-clock">0:00</span>
        <button class="btn" data-act="stop">Stop and see the report</button>
      </div>
      <h1 class="live-game waiting" id="lv-game">Waiting for a game</h1>
      <p class="live-sub" id="lv-sub">Switch to your game. Culprit locks on to the game in front of you once it starts drawing frames.</p>
      <div id="lv-stats"></div>
      <h2>frame time, last few seconds</h2>
      <div class="spark"><canvas id="lv-frames"></canvas></div>
      <h2>ping</h2>
      <div class="pings" id="lv-pings"><div class="ping-row"><span class="ping-name">Looking for your router</span></div></div>
      <h2>busy right now</h2>
      <div class="busy" id="lv-busy"></div>
      <p class="hint">Press <kbd>${HOTKEY}</kbd> the moment something feels off. Culprit looks hardest at those moments. <span id="lv-marks"></span></p>
    </div>`,
  );
}

const ROLE_NAME = { router: "Router", isp: "Provider", server: "Game server", serverPath: "Near the server" };

function updateLive(l) {
  $("lv-clock").textContent = clock(l.elapsedMs);
  const game = $("lv-game");
  if (l.game) {
    game.textContent = l.game.name;
    game.classList.remove("waiting");
    $("lv-sub").textContent = l.server ? `Connected to ${l.server}.` : "Not talking to a game server yet.";
  }
  $("lv-stats").innerHTML = statGrid([
    stat("fps", fixed(l.fps) ?? "-"),
    stat("1% low", fixed(l.low1Fps) ?? "-"),
    stat("stutters", String(l.stutters), "", l.stutters ? "warn" : ""),
    stat("lag spikes", String(l.lagSpikes), "", l.lagSpikes ? "warn" : ""),
    stat("cpu", fixed(l.cpu) ?? "-", l.cpu != null ? "%" : ""),
    stat("gpu", fixed(l.gpu) ?? "-", l.gpu != null ? (l.gpuTemp != null ? `% ${Math.round(l.gpuTemp)}°C` : "%") : ""),
  ]);
  const typical = l.frames.length ? l.frames.slice().sort((a, b) => a - b)[l.frames.length >> 1] : null;
  drawSpark($("lv-frames"), l.frames, { fill: true, typical, cap: typical ? Math.max(typical * 4, 25) : undefined });

  const pings = $("lv-pings");
  if (l.pings.length) {
    const ids = l.pings.map((p) => p.id).join(",");
    if (pings.dataset.ids !== ids) {
      pings.dataset.ids = ids;
      pings.innerHTML = l.pings
        .map((p) => `<div class="ping-row" data-id="${p.id}">
          <span class="ping-name">${ROLE_NAME[p.role] ?? p.role}<small>${esc(p.ip)}</small></span>
          <canvas></canvas><span class="ping-ms"></span><span class="ping-loss"></span></div>`)
        .join("");
    }
    for (const p of l.pings) {
      const row = pings.querySelector(`[data-id="${p.id}"]`);
      row.querySelector(".ping-ms").textContent = p.last == null ? "no reply" : `${Math.round(p.last)} ms`;
      const loss = row.querySelector(".ping-loss");
      loss.textContent = `${p.lossPct.toFixed(1)}% lost`;
      loss.classList.toggle("bad", p.lossPct >= 1);
      drawSpark(row.querySelector("canvas"), p.history, { typical: p.typical });
    }
  }
  $("lv-busy").innerHTML = l.top.length
    ? l.top.map((t) => `<span>${esc(t.label)}<b>${Math.round(t.cpu)}%</b></span>`).join("")
    : '<span>Nothing else is using much CPU.</span>';
  $("lv-marks").textContent = l.marks ? `Marked ${l.marks} ${l.marks === 1 ? "moment" : "moments"} so far.` : "";
}

async function pollLive() {
  if (view?.kind !== "live" || liveBusy) return;
  liveBusy = true;
  try {
    updateLive(decode(await invoke("live")));
  } catch (e) {
    console.error(e);
  } finally {
    liveBusy = false;
    if (view?.kind === "live") liveTimer = setTimeout(pollLive, LIVE_EVERY_MS);
  }
}

async function startRecording() {
  try {
    await invoke("start_recording");
  } catch (e) {
    toast(String(e));
    return;
  }
  els.record.hidden = true;
  renderLiveShell();
  pollLive();
}

async function stopRecording() {
  stopLive();
  let id;
  try {
    id = await invoke("stop_recording");
  } catch (e) {
    toast(String(e));
    return;
  }
  els.record.hidden = !caps.canRecord;
  await refreshSessions();
  if (id) await openSession(id);
  else if (sessions.length) await openSession(sessions[0].id);
  else renderEmpty();
}

/* ---------- dialogs ---------- */

function confirmDelete(id) {
  const s = sessions.find((x) => x.id === id);
  if (!s) return;
  const w = when(s.startedAt);
  const day = w.day === "Today" || w.day === "Yesterday" ? w.day.toLowerCase() : w.day;
  els.dialog.innerHTML = `<h3>Delete this session?</h3>
    <p>This removes the ${esc(s.game || "game")} report from ${esc(day)} at ${esc(w.time)}.</p>
    <div class="btns"><button class="btn" data-act="cancel">Cancel</button><button class="btn danger" data-act="delete">Delete</button></div>`;
  els.scrim.hidden = false;
  els.dialog.querySelector('[data-act="delete"]').focus();
  els.dialog.dataset.id = id;
}

function closeDialog() {
  els.scrim.hidden = true;
  els.dialog.innerHTML = "";
}

async function deleteSession(id) {
  closeDialog();
  const index = sessions.findIndex((s) => s.id === id);
  try {
    await invoke("delete_session", { id });
  } catch (e) {
    toast("Couldn't delete that session.");
    return;
  }
  cache.delete(id);
  await refreshSessions();
  if (selectedId === id || !selectedId) {
    const next = sessions[Math.min(index, sessions.length - 1)];
    if (next) openSession(next.id);
    else renderEmpty();
  }
  toast("Session deleted");
}

/* ---------- events ---------- */

els.list.addEventListener("click", (e) => {
  const item = e.target.closest(".item");
  if (item && item.dataset.id !== selectedId) openSession(item.dataset.id);
});

els.list.addEventListener("contextmenu", (e) => {
  const item = e.target.closest(".item");
  if (!item) return;
  e.preventDefault();
  confirmDelete(item.dataset.id);
});

els.main.addEventListener("click", (e) => {
  const act = e.target.closest("[data-act]")?.dataset.act;
  if (act === "example") return openSession("example");
  if (act === "record") return startRecording();
  if (act === "stop") return stopRecording();
  if (act === "more") {
    view.showAll = true;
    return renderIncidents();
  }
  const chip = e.target.closest("[data-filter]");
  if (chip && view?.kind === "report") {
    view.filter = chip.dataset.filter;
    view.showAll = false;
    return renderIncidents();
  }
  const row = e.target.closest(".inc-row");
  if (row) pickIncident(Number(row.dataset.i), false);
});

els.example.addEventListener("click", () => openSession("example"));
els.record.addEventListener("click", startRecording);

els.scrim.addEventListener("click", (e) => {
  if (e.target === els.scrim) return closeDialog();
  const act = e.target.closest("[data-act]")?.dataset.act;
  if (act === "cancel") closeDialog();
  if (act === "delete") deleteSession(els.dialog.dataset.id);
});

document.addEventListener("keydown", (e) => {
  if (!els.scrim.hidden) {
    if (e.key === "Escape") closeDialog();
    return;
  }
  if (e.target.closest?.("input, textarea")) return;
  if ((e.key === "ArrowDown" || e.key === "ArrowUp") && sessions.length && view?.kind !== "live") {
    e.preventDefault();
    const i = sessions.findIndex((s) => s.id === selectedId);
    const next = e.key === "ArrowDown" ? Math.min(sessions.length - 1, i + 1) : Math.max(0, i < 0 ? 0 : i - 1);
    if (sessions[next].id !== selectedId) openSession(sessions[next].id);
  } else if (e.key === "Delete" && selectedId) {
    confirmDelete(selectedId);
  }
});

// The context menu only makes sense on sessions.
document.addEventListener("contextmenu", (e) => {
  if (!e.target.closest(".item, input, textarea, .detail, .why, .fix, .inc-body")) e.preventDefault();
});

/* ---------- start ---------- */

(async function start() {
  await connect();
  caps = await invoke("capabilities");
  els.record.hidden = !caps.canRecord;
  await refreshSessions();
  if (caps.recording) {
    els.record.hidden = true;
    renderLiveShell();
    pollLive();
  } else if (sessions.length) {
    openSession(sessions[0].id);
  } else {
    renderEmpty();
  }
})().catch((e) => {
  console.error(e);
  els.main.innerHTML = `<div class="empty"><div class="empty-body"><h1>Culprit couldn't start</h1><p>${esc(e.message || e)}</p></div></div>`;
});
