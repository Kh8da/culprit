"use strict";

// Canvas drawing for the report timeline and the live view. The timeline has two layers:
// a base canvas that is only redrawn on resize, and an overlay for the hover line and the
// picked moment, so moving the mouse never redraws the data.

const COLORS = {
  frameFill: "rgba(255,255,255,0.16)",
  frameLine: "rgba(237,237,237,0.85)",
  grid: "#1c1c1c",
  label: "#4a4a4a",
  server: "#ededed",
  isp: "#8a8a8a",
  router: "#4f4f4f",
  stutter: "#f0605d",
  lag: "#e3b341",
  loss: "#f0605d",
  cursor: "rgba(255,255,255,0.35)",
};
const FONT = '10.5px "Geist Mono", ui-monospace, monospace';
const LABEL_W = 44;

function niceCeil(v) {
  const steps = [10, 16, 20, 25, 33, 40, 50, 60, 80, 100, 120, 150, 200, 250, 300, 400, 500, 750, 1000, 1500, 2000];
  return steps.find((s) => s >= v) ?? Math.ceil(v / 1000) * 1000;
}

function median(values) {
  const v = values.filter((x) => x != null && x >= 0).sort((a, b) => a - b);
  if (!v.length) return null;
  const m = v.length >> 1;
  return v.length % 2 ? v[m] : (v[m - 1] + v[m]) / 2;
}

function quantile(values, q) {
  const v = values.filter((x) => x != null && x >= 0).sort((a, b) => a - b);
  if (!v.length) return null;
  return v[Math.min(v.length - 1, Math.floor(q * (v.length - 1)))];
}

function clock(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

function fitCanvas(canvas) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { ctx, w, h };
}

class Timeline {
  constructor(host, session, onPick) {
    this.host = host;
    this.onPick = onPick;
    this.picked = -1;
    this.hoverX = null;
    const charts = session.charts;
    this.step = charts.stepMs;
    this.frameMax = charts.frameMax;
    this.frameAvg = charts.frameAvg;
    this.duration = Math.max(session.report.durationMs, this.frameMax.length * this.step, 1);
    this.incidents = session.report.incidents;

    const role = (r) => charts.pings.filter((p) => p.role === r && p.rtt.some((v) => v != null));
    const server = role("server");
    const toServer = server.length ? server : role("serverPath");
    // Without a game server there is no lag to show, so the ping panel is left out.
    this.lines = toServer.length
      ? [
          ...role("router").map((p) => ({ ...p, color: COLORS.router })),
          ...role("isp").map((p) => ({ ...p, color: COLORS.isp })),
          ...toServer.map((p) => ({ ...p, color: COLORS.server })),
        ]
      : [];
    this.hasPing = this.lines.length > 0;
    this.hasFrames = this.frameMax.some((v) => v != null);

    const typical = median(this.frameAvg) ?? 16;
    this.frameTypical = typical;
    this.frameCap = niceCeil(Math.max(typical * 4, 20));
    const main = this.lines.filter((l) => l.color === COLORS.server);
    const pingValues = (main.length ? main : this.lines).flatMap((l) => l.rtt);
    this.pingTypical = median(pingValues);
    this.pingCap = niceCeil(Math.max((quantile(pingValues, 0.95) ?? 20) * 2.2, 50));

    host.innerHTML = "";
    this.body = document.createElement("div");
    this.body.className = "tl-body" + (this.hasPing ? "" : " short");
    this.base = document.createElement("canvas");
    this.over = document.createElement("canvas");
    this.over.className = "over";
    this.tip = document.createElement("div");
    this.tip.className = "tl-tip";
    this.body.append(this.base, this.over, this.tip);
    host.append(this.body);

    this.over.addEventListener("mousemove", (e) => this.hover(e.offsetX));
    this.over.addEventListener("mouseleave", () => this.hover(null));
    this.over.addEventListener("click", (e) => {
      const i = this.nearest(e.offsetX);
      if (i >= 0) this.onPick?.(i);
    });

    this.pending = false;
    this.resize = new ResizeObserver(() => this.schedule(true));
    this.resize.observe(this.body);
  }

  destroy() {
    this.resize.disconnect();
  }

  layout(w) {
    const plotW = Math.max(10, w - LABEL_W);
    const frame = { top: 4, h: this.hasPing ? 112 : 108 };
    const ping = this.hasPing ? { top: frame.top + frame.h + 14, h: 56 } : null;
    const axisY = (ping ? ping.top + ping.h : frame.top + frame.h) + 14;
    return { plotW, frame, ping, axisY };
  }

  x(t, plotW) {
    return LABEL_W + (t / this.duration) * plotW;
  }

  schedule(full) {
    this.dirtyBase ||= full;
    if (this.pending) return;
    this.pending = true;
    requestAnimationFrame(() => {
      this.pending = false;
      if (this.dirtyBase) this.drawBase();
      this.dirtyBase = false;
      this.drawOver();
    });
  }

  drawBase() {
    const { ctx, w, h } = fitCanvas(this.base);
    ctx.clearRect(0, 0, w, h);
    const L = this.layout(w);
    this.L = L;
    ctx.font = FONT;
    ctx.textBaseline = "middle";

    // Frame times: worst frame per pixel as a soft area, average as a line.
    const n = this.frameMax.length;
    const { frame } = L;
    const fy = (ms) => frame.top + frame.h - (Math.min(ms, this.frameCap) / this.frameCap) * frame.h;
    this.gridLine(ctx, L, fy(this.frameTypical), `${Math.round(this.frameTypical)} ms`);
    this.gridLine(ctx, L, fy(this.frameCap), `${this.frameCap} ms`);
    if (this.hasFrames) {
      const cols = Math.floor(L.plotW);
      const maxCol = new Float32Array(cols).fill(NaN);
      const avgCol = new Float32Array(cols).fill(NaN);
      for (let c = 0; c < cols; c++) {
        const i0 = Math.floor((c * n) / cols);
        const i1 = Math.max(i0 + 1, Math.floor(((c + 1) * n) / cols));
        let mx = -1, sum = 0, cnt = 0;
        for (let i = i0; i < i1 && i < n; i++) {
          const m = this.frameMax[i];
          if (m != null && m > mx) mx = m;
          const a = this.frameAvg[i];
          if (a != null) { sum += a; cnt++; }
        }
        if (mx >= 0) maxCol[c] = mx;
        if (cnt) avgCol[c] = sum / cnt;
      }
      // The band from average to worst shows how uneven the frames were. A column whose
      // worst frame is a hitch (same rule as the analyzer) is drawn in the stutter colour,
      // clipped at the top of the scale.
      const t = this.frameTypical;
      const hitch = Math.max(t * 2.5, t + 12, 25);
      for (let c = 0; c < cols; c++) {
        const mx = maxCol[c], av = avgCol[c];
        if (Number.isNaN(mx)) continue;
        const top = fy(mx), base = Number.isNaN(av) ? top + 1 : fy(av);
        ctx.fillStyle = mx >= hitch ? COLORS.stutter : COLORS.frameFill;
        ctx.fillRect(LABEL_W + c, top, 1, Math.max(1, base - top));
      }
      ctx.strokeStyle = COLORS.frameLine;
      ctx.lineWidth = 1;
      ctx.beginPath();
      let open = false;
      for (let c = 0; c < cols; c++) {
        const v = avgCol[c];
        if (Number.isNaN(v)) { open = false; continue; }
        const x = LABEL_W + c + 0.5, y = fy(v);
        if (open) ctx.lineTo(x, y); else ctx.moveTo(x, y);
        open = true;
      }
      ctx.stroke();
      this.cols = { maxCol, avgCol };
    }

    // Pings: one line per target, losses as dots along the bottom.
    if (L.ping) {
      const { ping } = L;
      const py = (ms) => ping.top + ping.h - (Math.min(ms, this.pingCap) / this.pingCap) * ping.h;
      if (this.pingTypical != null) this.gridLine(ctx, L, py(this.pingTypical), `${Math.round(this.pingTypical)} ms`);
      this.gridLine(ctx, L, py(this.pingCap), `${this.pingCap} ms`);
      const cols = Math.floor(L.plotW);
      this.pingCols = [];
      for (const line of this.lines) {
        const m = line.rtt.length;
        const col = new Float32Array(cols).fill(NaN);
        const lost = new Uint8Array(cols);
        for (let c = 0; c < cols; c++) {
          const i0 = Math.floor((c * m) / cols);
          const i1 = Math.max(i0 + 1, Math.floor(((c + 1) * m) / cols));
          let mx = -1;
          for (let i = i0; i < i1 && i < m; i++) {
            const v = line.rtt[i];
            if (v == null) continue;
            if (v < 0) lost[c] = 1; else if (v > mx) mx = v;
          }
          if (mx >= 0) col[c] = mx;
        }
        ctx.strokeStyle = line.color;
        ctx.lineWidth = line.color === COLORS.server ? 1.2 : 1;
        ctx.beginPath();
        let open = false;
        for (let c = 0; c < cols; c++) {
          if (Number.isNaN(col[c])) { open = false; continue; }
          const x = LABEL_W + c + 0.5, y = py(col[c]);
          if (open) ctx.lineTo(x, y); else ctx.moveTo(x, y);
          open = true;
        }
        ctx.stroke();
        ctx.fillStyle = COLORS.loss;
        for (let c = 0; c < cols; c++) if (lost[c]) ctx.fillRect(LABEL_W + c - 1, ping.top + ping.h - 2, 2, 2);
        this.pingCols.push({ role: line.role, col });
      }
    }

    // Incident ticks above each panel.
    for (const inc of this.incidents) {
      const x = Math.round(this.x(inc.t, L.plotW)) + 0.5;
      if (inc.kind === "stutter") {
        ctx.fillStyle = COLORS.stutter;
        ctx.fillRect(x - 1, frame.top, 2, 7);
      } else if (inc.kind === "lag" && L.ping) {
        ctx.fillStyle = COLORS.lag;
        ctx.fillRect(x - 1, L.ping.top, 2, 7);
      } else if (inc.kind === "lag") {
        ctx.fillStyle = COLORS.lag;
        ctx.fillRect(x - 1, frame.top, 2, 7);
      } else {
        ctx.strokeStyle = "#ededed";
        ctx.lineWidth = 1.2;
        ctx.beginPath();
        ctx.arc(x, frame.top + 4, 3, 0, Math.PI * 2);
        ctx.stroke();
      }
    }

    // Time axis.
    const minutes = this.duration / 60000;
    const stepMin = [1, 2, 5, 10, 15, 30, 60, 120].find((s) => minutes / s <= 7) ?? 240;
    ctx.fillStyle = COLORS.label;
    ctx.textAlign = "center";
    for (let m = 0; m <= minutes; m += stepMin) {
      const x = this.x(m * 60000, L.plotW);
      if (x > w - 16) break;
      ctx.fillText(clock(m * 60000), Math.max(LABEL_W + 12, x), L.axisY);
    }
    ctx.textAlign = "left";
  }

  gridLine(ctx, L, y, label) {
    ctx.strokeStyle = COLORS.grid;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(LABEL_W, Math.round(y) + 0.5);
    ctx.lineTo(LABEL_W + L.plotW, Math.round(y) + 0.5);
    ctx.stroke();
    ctx.fillStyle = COLORS.label;
    ctx.fillText(label, 0, y);
  }

  nearest(px) {
    if (!this.L) return -1;
    let best = -1, bestD = 6;
    this.incidents.forEach((inc, i) => {
      const d = Math.abs(this.x(inc.t, this.L.plotW) - px);
      if (d < bestD) { bestD = d; best = i; }
    });
    return best;
  }

  hover(px) {
    this.hoverX = px;
    this.schedule(false);
  }

  pick(i) {
    this.picked = i;
    this.schedule(false);
  }

  drawOver() {
    const { ctx, w, h } = fitCanvas(this.over);
    ctx.clearRect(0, 0, w, h);
    const L = this.L;
    if (!L) return;
    const bottom = (L.ping ? L.ping.top + L.ping.h : L.frame.top + L.frame.h);
    if (this.picked >= 0 && this.incidents[this.picked]) {
      const x = Math.round(this.x(this.incidents[this.picked].t, L.plotW)) + 0.5;
      ctx.strokeStyle = "rgba(255,255,255,0.8)";
      ctx.lineWidth = 1;
      ctx.setLineDash([3, 3]);
      ctx.beginPath();
      ctx.moveTo(x, L.frame.top);
      ctx.lineTo(x, bottom);
      ctx.stroke();
      ctx.setLineDash([]);
    }
    const px = this.hoverX;
    if (px == null || px < LABEL_W || px > LABEL_W + L.plotW) {
      this.tip.classList.remove("show");
      this.over.style.cursor = "crosshair";
      return;
    }
    const near = this.nearest(px);
    this.over.style.cursor = near >= 0 ? "pointer" : "crosshair";
    ctx.strokeStyle = COLORS.cursor;
    ctx.beginPath();
    ctx.moveTo(Math.round(px) + 0.5, L.frame.top);
    ctx.lineTo(Math.round(px) + 0.5, bottom);
    ctx.stroke();

    const c = Math.min(Math.floor(L.plotW) - 1, Math.max(0, Math.floor(px - LABEL_W)));
    const t = ((px - LABEL_W) / L.plotW) * this.duration;
    const parts = [`<span class="t">${clock(t)}</span>`];
    if (this.cols) {
      const a = this.cols.avgCol[c], m = this.cols.maxCol[c];
      if (!Number.isNaN(a)) parts.push(`frame <b>${a.toFixed(1)} ms</b> worst <b>${m.toFixed(m < 20 ? 1 : 0)} ms</b>`);
    }
    for (const p of this.pingCols ?? []) {
      const v = p.col[c];
      if (Number.isNaN(v)) continue;
      const name = p.role === "router" ? "router" : p.role === "isp" ? "provider" : "server";
      parts.push(`${name} <b>${Math.round(v)} ms</b>`);
    }
    if (near >= 0) parts.push(escapeHtml(this.incidents[near].what));
    this.tip.innerHTML = parts.join("<br>");
    const tw = this.tip.offsetWidth;
    const left = px + 14 + tw > w ? px - 14 - tw : px + 14;
    this.tip.style.transform = `translate(${Math.round(left)}px, ${L.frame.top + 6}px)`;
    this.tip.classList.add("show");
  }
}

function escapeHtml(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

// A small line chart for the live view. `values` may contain null (no data) and -1 (lost).
function drawSpark(canvas, values, { cap, color = COLORS.server, fill = false, typical = null } = {}) {
  const { ctx, w, h } = fitCanvas(canvas);
  ctx.clearRect(0, 0, w, h);
  if (!values.length) return;
  const top = cap ?? niceCeil(Math.max(...values.filter((v) => v != null && v >= 0), 1) * 1.3);
  const y = (v) => h - 1 - (Math.min(v, top) / top) * (h - 3);
  const dx = w / Math.max(1, values.length - 1);
  if (typical != null) {
    ctx.strokeStyle = COLORS.grid;
    ctx.beginPath();
    ctx.moveTo(0, Math.round(y(typical)) + 0.5);
    ctx.lineTo(w, Math.round(y(typical)) + 0.5);
    ctx.stroke();
  }
  if (fill) {
    ctx.fillStyle = COLORS.frameFill;
    values.forEach((v, i) => {
      if (v == null || v < 0) return;
      const yy = y(v);
      ctx.fillRect(i * dx, yy, Math.max(1, dx), h - yy);
    });
  }
  ctx.strokeStyle = color;
  ctx.lineWidth = 1.2;
  ctx.beginPath();
  let open = false;
  values.forEach((v, i) => {
    if (v == null || v < 0) { open = false; return; }
    if (open) ctx.lineTo(i * dx, y(v)); else ctx.moveTo(i * dx, y(v));
    open = true;
  });
  ctx.stroke();
  ctx.fillStyle = COLORS.loss;
  values.forEach((v, i) => { if (v != null && v < 0) ctx.fillRect(i * dx - 1, h - 3, 2, 3); });
}

window.Timeline = Timeline;
window.drawSpark = drawSpark;
window.clock = clock;
window.escapeHtml = escapeHtml;
