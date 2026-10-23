"use strict";

// A stand-in backend for opening ui/index.html in a browser. It serves the example
// sessions written by `cargo run -p culpritcore --example export_demo -- ui-dev/sessions`
// and fakes a live recording, so the whole UI can be checked without the desktop app.

(function () {
  const base = new URL("../ui-dev/sessions/", document.baseURI);
  let index = null;
  let recording = null;
  let seed = 42;
  const rand = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);

  async function sessions() {
    index ??= await (await fetch(new URL("index.json", base))).json();
    return index;
  }

  async function raw(id) {
    const res = await fetch(new URL(`${id}.json`, base));
    if (!res.ok) throw new Error(`no session ${id}`);
    return res.arrayBuffer();
  }

  function live() {
    const r = recording;
    const elapsed = performance.now() - r.started;
    const locked = elapsed > 2500;
    const frames = [];
    for (let i = 0; i < 240; i++) frames.push(locked ? 5.8 + rand() * 0.8 + (rand() < 0.004 ? 40 : 0) : 0);
    const tick = (h, v) => {
      h.push(v);
      if (h.length > 60) h.shift();
      return h;
    };
    if (elapsed - r.lastTick >= 1000) {
      r.lastTick = elapsed;
      tick(r.router, 0.8 + rand() * 0.6);
      tick(r.isp, 8 + rand() * 2);
      tick(r.server, rand() < 0.03 ? -1 : 27 + rand() * 3 + (rand() < 0.04 ? 160 : 0));
      if (r.server.at(-1) > 100 || r.server.at(-1) < 0) r.lag++;
      if (rand() < 0.05) r.stutters++;
    }
    const ping = (id, role, ip, h, typical) => ({
      id, role, ip, last: h.at(-1) < 0 ? null : h.at(-1) ?? null, typical,
      lossPct: (h.filter((v) => v < 0).length * 100) / Math.max(1, h.length) / 4, history: h.slice(),
    });
    return {
      elapsedMs: elapsed,
      game: locked ? { name: "Counter-Strike 2", exe: "cs2.exe", pid: 4120, since: 2500 } : null,
      fps: locked ? 165 + rand() * 6 : null,
      low1Fps: locked ? 150 + rand() * 5 : null,
      frames: locked ? frames : [],
      stutters: r.stutters,
      lagSpikes: r.lag,
      pings: [
        ping(1, "router", "192.168.1.1", r.router, 1),
        ...(locked ? [ping(2, "isp", "84.116.0.1", r.isp, 9), ping(3, "server", "155.133.252.38", r.server, 28)] : []),
      ],
      server: locked ? "155.133.252.38" : null,
      link: "ethernet",
      wifiSignal: null,
      cpu: 38 + rand() * 6,
      gpu: 80 + rand() * 8,
      gpuTemp: 69,
      top: [{ label: "Discord", cpu: 3.1 }, { label: "Google Chrome", cpu: 2.2 }],
      marks: r.marks,
      notes: [],
      presenting: [],
    };
  }

  window.culpritDev = {
    async invoke(cmd, args = {}) {
      switch (cmd) {
        case "capabilities":
          return { canRecord: new URLSearchParams(location.search).has("record"), recording: !!recording };
        case "list_sessions":
          return (await sessions()).slice();
        case "get_session":
          return raw(args.id);
        case "example_session":
          return raw((await sessions())[0].id);
        case "delete_session":
          index = (await sessions()).filter((s) => s.id !== args.id);
          return null;
        case "start_recording":
          recording = { started: performance.now(), lastTick: -1000, router: [], isp: [], server: [], stutters: 0, lag: 0, marks: 0 };
          return null;
        case "live":
          return live();
        case "mark":
          if (recording) recording.marks++;
          return null;
        case "stop_recording":
          recording = null;
          return (await sessions())[0]?.id ?? null;
        default:
          throw new Error(`unknown command ${cmd}`);
      }
    },
  };
})();
