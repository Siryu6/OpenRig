// OpenRig remote: the pedal's screen on a phone or tablet.
//
// One WebSocket to the pedal (/ws, signed in by the HttpOnly cookie):
// - text messages: {"type":"state",...} whenever the pedal's state
//   changes, {"type":"telemetry",...} for meters and tuner;
// - binary messages: pictures of the full editor (the pedal's own UI),
//   rectangles that changed, painted on the canvas;
// - this page sends commands, pointer and keys as JSON.
"use strict";

const $ = (id) => document.getElementById(id);
const MIN_W = 800, MIN_H = 480, MAX_W = 1920, MAX_H = 1200;

let ws = null;
let state = null;
let view = localStorage.getItem("openrig.view") || "editor";
let tunerOn = false;
let retry = 500;
let toastTimer = 0;

// ------------------------------------------------------------ helpers

function el(tag, props = {}, children = []) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v);
  }
  for (const c of children) e.append(c);
  return e;
}

function send(msg) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function toast(text) {
  const t = $("toast");
  t.textContent = text;
  t.hidden = !text;
  clearTimeout(toastTimer);
  if (text) toastTimer = setTimeout(() => (t.hidden = true), 4000);
}

async function api(path, body) {
  const options = body === undefined
    ? { credentials: "same-origin" }
    : {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      };
  const response = await fetch(path, options);
  let data = {};
  try { data = await response.json(); } catch (_) { /* not JSON */ }
  return { ok: response.ok, status: response.status, data };
}

// ------------------------------------------------------------ sign-in

async function start() {
  const { data } = await api("/api/session").catch(() => ({ data: {} }));
  if (data.signed_in) showApp();
  else showLogin();
}

function showLogin(message) {
  if (ws) { ws.onclose = null; ws.close(); ws = null; }
  $("app").hidden = true;
  $("login").hidden = false;
  $("login-error").textContent = message || "";
  $("password").focus();
}

$("login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const password = $("password").value;
  $("login-error").textContent = "";
  const r = await api("/api/login", { password }).catch(() => null);
  if (r && r.ok) {
    $("password").value = "";
    showApp();
  } else if (r && r.status === 429) {
    $("login-error").textContent = "Too many attempts: wait a moment and try again.";
  } else {
    $("login-error").textContent = r ? "Wrong password." : "The pedal cannot be reached.";
  }
});

$("logout").addEventListener("click", async () => {
  await api("/api/logout", {}).catch(() => null);
  showLogin();
});

$("logout-all").addEventListener("click", async () => {
  if (!confirm("Sign out every device, this one too?")) return;
  await api("/api/logout-all", {}).catch(() => null);
  showLogin();
});

function showApp() {
  $("login").hidden = true;
  $("app").hidden = false;
  connect();
  setView(view);
}

// ------------------------------------------------------------ connection

function connect() {
  if (ws) return;
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${scheme}://${location.host}/ws`);
  ws.binaryType = "arraybuffer";
  ws.onopen = () => {
    retry = 500;
    $("conn").classList.add("ok");
    // What this page wants, again after a reconnection
    if (tunerOn) send({ type: "tuner", on: true });
    if (view === "editor") startStream();
  };
  ws.onmessage = (e) => {
    if (typeof e.data === "string") {
      const msg = JSON.parse(e.data);
      if (msg.type === "state") onState(msg);
      else if (msg.type === "telemetry") onTelemetry(msg);
    } else {
      onFrame(e.data);
    }
  };
  ws.onclose = async () => {
    ws = null;
    $("conn").classList.remove("ok");
    // Signed out (password changed, devices signed out)?
    const { data } = await api("/api/session").catch(() => ({ data: null }));
    if (data && !data.signed_in) {
      showLogin("Signed out: sign in again.");
      return;
    }
    setTimeout(connect, retry);
    retry = Math.min(retry * 2, 5000);
  };
}

// ------------------------------------------------------------ views

for (const b of document.querySelectorAll("#tabs button")) {
  b.addEventListener("click", () => setView(b.dataset.view));
}

function setView(name) {
  if (name === "editor" && state && !state.editor) name = "live";
  view = name;
  localStorage.setItem("openrig.view", name);
  for (const b of document.querySelectorAll("#tabs button")) {
    b.classList.toggle("on", b.dataset.view === name);
  }
  $("view-editor").hidden = name !== "editor";
  $("view-live").hidden = name !== "live";
  $("view-status").hidden = name !== "status";
  if (name === "editor") startStream();
  else send({ type: "stream", on: false });
  if (name === "status") loadStatus();
}

// ------------------------------------------------------------ full editor

const canvas = $("screen");
const ctx = canvas.getContext("2d", { alpha: false });
let windowSize = [0, 0];
let wanted = [0, 0];
let paint = Promise.resolve();

/// Window size for the pedal's UI: the space on this screen, within the
/// sizes the UI is made for
function wantedSize() {
  const area = $("view-editor").getBoundingClientRect();
  const w = Math.round(Math.min(MAX_W, Math.max(MIN_W, area.width)));
  const h = Math.round(Math.min(MAX_H, Math.max(MIN_H, area.height)));
  return [w, h];
}

function startStream() {
  if (!state || !state.editor) return;
  wanted = wantedSize();
  send({ type: "stream", on: true, width: wanted[0], height: wanted[1] });
  layoutCanvas();
}

/// Fit the canvas in the view (scaled down when the window is larger)
function layoutCanvas() {
  const area = $("view-editor").getBoundingClientRect();
  const [w, h] = windowSize[0] ? windowSize : wanted;
  if (!w || !h) return;
  const scale = Math.min(area.width / w, area.height / h, 4);
  // Whole device pixels: sharper pictures
  const dpr = window.devicePixelRatio || 1;
  canvas.style.width = `${Math.floor(w * scale * dpr) / dpr}px`;
  canvas.style.height = `${Math.floor(h * scale * dpr) / dpr}px`;
  $("rotate").hidden = !(area.height > area.width && area.width < MIN_W);
}

let resizeTimer = 0;
window.addEventListener("resize", () => {
  layoutCanvas();
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    if (view !== "editor" || !state || !state.editor) return;
    const size = wantedSize();
    if (size[0] !== wanted[0] || size[1] !== wanted[1]) {
      wanted = size;
      send({ type: "resize", width: size[0], height: size[1] });
    }
  }, 300);
});

function onFrame(buffer) {
  const head = new DataView(buffer, 0, 14);
  if (head.getUint8(0) !== 1) return;
  const type = head.getUint8(1) === 2 ? "image/jpeg" : "image/png";
  const x = head.getUint16(2, true), y = head.getUint16(4, true);
  const ww = head.getUint16(10, true), wh = head.getUint16(12, true);
  const blob = new Blob([new Uint8Array(buffer, 14)], { type });
  // In order: a later rectangle may cover an earlier one
  paint = paint.then(async () => {
    if (canvas.width !== ww || canvas.height !== wh) {
      canvas.width = ww;
      canvas.height = wh;
      windowSize = [ww, wh];
      layoutCanvas();
    }
    try {
      const bitmap = await createImageBitmap(blob);
      ctx.drawImage(bitmap, x, y);
      bitmap.close();
    } catch (_) { /* a broken picture: the next one repairs it */ }
  });
}

// Pointer: window pixels of the pedal's UI
function position(e) {
  const r = canvas.getBoundingClientRect();
  return {
    x: ((e.clientX - r.left) * canvas.width) / r.width,
    y: ((e.clientY - r.top) * canvas.height) / r.height,
  };
}

let pointerId = null;
let pendingMove = null;
canvas.addEventListener("pointerdown", (e) => {
  if (pointerId !== null && e.pointerId !== pointerId) return;
  pointerId = e.pointerId;
  canvas.setPointerCapture(e.pointerId);
  canvas.focus({ preventScroll: true });
  send({ type: "pointer", kind: "down", ...position(e) });
  e.preventDefault();
});
canvas.addEventListener("pointermove", (e) => {
  if (pointerId !== null && e.pointerId !== pointerId) return;
  if (pointerId === null && e.pointerType !== "mouse") return;
  // One move per frame of the browser
  if (!pendingMove) {
    requestAnimationFrame(() => {
      send(pendingMove);
      pendingMove = null;
    });
  }
  pendingMove = { type: "pointer", kind: "move", ...position(e) };
});
function release(e, kind) {
  if (e.pointerId !== pointerId) return;
  pointerId = null;
  if (pendingMove) { send(pendingMove); pendingMove = null; }
  send({ type: "pointer", kind, ...position(e) });
}
canvas.addEventListener("pointerup", (e) => release(e, "up"));
canvas.addEventListener("pointercancel", (e) => release(e, "leave"));
canvas.addEventListener("pointerleave", (e) => {
  if (pointerId === null && e.pointerType === "mouse") {
    send({ type: "pointer", kind: "leave", ...position(e) });
  }
});
canvas.addEventListener("wheel", (e) => {
  e.preventDefault();
  const line = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 400 : 1;
  send({ type: "pointer", kind: "scroll", ...position(e), dx: -e.deltaX * line, dy: -e.deltaY * line });
}, { passive: false });
canvas.addEventListener("contextmenu", (e) => e.preventDefault());

// Keys, as Slint names them
const KEYS = {
  Backspace: "\u0008", Tab: "\t", Enter: "\n", Escape: "\u001b", Delete: "\u007f",
  Shift: "\u0010", Control: "\u0011", Alt: "\u0012", Meta: "\u0017",
  ArrowUp: "", ArrowDown: "", ArrowLeft: "", ArrowRight: "",
  Home: "", End: "", PageUp: "", PageDown: "",
};
function keyText(e) {
  if (KEYS[e.key]) return KEYS[e.key];
  return [...e.key].length === 1 ? e.key : null;
}
function key(e, down) {
  if (view !== "editor" || $("app").hidden) return;
  const text = keyText(e);
  if (!text) return;
  send({ type: "key", text, down });
  e.preventDefault();
}
// A hardware keyboard on the canvas
canvas.addEventListener("keydown", (e) => key(e, true));
canvas.addEventListener("keyup", (e) => key(e, false));

// The phone's keyboard: a hidden field; what is typed is sent as keys
const keys = $("keys");
$("kbd").addEventListener("click", () => {
  keys.value = "";
  keys.focus();
});
keys.addEventListener("keydown", (e) => {
  if (KEYS[e.key]) key(e, true);
});
keys.addEventListener("keyup", (e) => {
  if (KEYS[e.key]) key(e, false);
});
keys.addEventListener("input", (e) => {
  if (e.inputType === "deleteContentBackward" && !keys.value) {
    send({ type: "key", text: "\u0008", down: true });
    send({ type: "key", text: "\u0008", down: false });
  }
  for (const c of keys.value) {
    send({ type: "key", text: c, down: true });
    send({ type: "key", text: c, down: false });
  }
  keys.value = "";
});

// ------------------------------------------------------------ live view

$("prev").addEventListener("click", () => send({ type: "preset_step", next: false }));
$("next").addEventListener("click", () => send({ type: "preset_step", next: true }));
$("preset-select").addEventListener("change", (e) => send({ type: "load_preset", id: e.target.value }));
$("tap").addEventListener("click", () => send({ type: "tap" }));
$("tuner-toggle").addEventListener("click", () => {
  tunerOn = !tunerOn;
  $("tuner").hidden = !tunerOn;
  $("tuner-toggle").classList.toggle("on", tunerOn);
  send({ type: "tuner", on: tunerOn });
});

function onState(s) {
  const first = !state;
  state = s;
  $("tab-editor").hidden = !s.editor;
  $("no-editor").hidden = s.editor;
  if (first || (view === "editor" && !s.editor)) setView(view);
  renderLive();
  if (s.status) toast(s.status);
}

function renderLive() {
  const s = state;
  // Presets
  const select = $("preset-select");
  const options = s.presets.map((p) => p.id + "\u0000" + p.name).join("\u0001");
  if (select.dataset.options !== options) {
    select.dataset.options = options;
    select.replaceChildren(...s.presets.map((p) => el("option", { value: p.id, text: p.name })));
    if (!s.preset) select.prepend(el("option", { value: "", text: "Unsaved" }));
  }
  select.value = s.preset ? s.preset.id : "";
  $("scene-name").textContent = s.scene_name + (s.audio ? "" : " · audio off");

  // Setlist
  $("setlist").hidden = !s.setlist;
  if (s.setlist) {
    $("setlist-name").textContent = "Setlist: " + s.setlist.name;
    $("setlist-slots").replaceChildren(...s.setlist.slots.map((slot) =>
      el("button", {
        class: (slot.active ? "on" : "") + (slot.empty ? " empty" : ""),
        text: slot.empty ? "Empty" : slot.name,
        onclick: () => send({ type: "setlist_slot", slot: slot.slot }),
      })));
  }

  // Scenes
  $("scenes").replaceChildren(...s.scenes.map((sc) =>
    el("button", {
      class: (sc.active ? "on" : "") + (sc.used ? "" : " empty"),
      text: sc.name,
      onclick: () => send({ type: "select_scene", slot: sc.slot }),
    })));
  $("bpm").textContent = Math.round(s.bpm) + " BPM";
  renderBlocks();
}

// Blocks: rebuilt when the chain changes, values updated in place so a
// slider being dragged stays under the finger
const expanded = new Set();
let dragging = null;

function blocksKey(blocks) {
  return blocks.map((b) => [b.id, b.uri, b.name, b.color, b.params.length, expanded.has(b.id)].join("|")).join("/");
}

function format(p, v) {
  if (p.kind === "toggle") return v >= 0.5 ? "On" : "Off";
  if (p.kind === "enum") {
    const o = p.options.find((o) => o.value === v);
    return o ? o.label : String(v);
  }
  if (p.kind === "int") return String(Math.round(v)) + (p.unit ? " " + p.unit : "");
  const abs = Math.abs(v);
  const text = abs >= 100 ? v.toFixed(0) : abs >= 10 ? v.toFixed(1) : v.toFixed(2);
  return text + (p.unit ? " " + p.unit : "");
}

function paramRow(b, p) {
  const id = `p-${b.id}-${p.index}`;
  const out = el("output", { text: format(p, p.value) });
  let input;
  if (p.kind === "enum" && p.options.length) {
    input = el("select", { id }, p.options.map((o) => el("option", { value: String(o.value), text: o.label })));
    input.value = String(p.value);
    input.addEventListener("change", () => {
      send({ type: "set_parameter", block: b.id, uri: b.uri, index: p.index, value: Number(input.value) });
    });
  } else if (p.kind === "toggle") {
    // A switch: its value is the parameter's (a button's value is a string)
    const on = (v) => v >= (p.min + p.max) / 2;
    input = el("button", { id, class: "switch", type: "button", role: "switch" });
    input.value = String(p.value);
    input.setAttribute("aria-checked", String(on(p.value)));
    input.addEventListener("click", () => {
      const value = on(Number(input.value)) ? p.min : p.max;
      input.value = String(value);
      input.setAttribute("aria-checked", String(on(value)));
      out.textContent = format(p, value);
      send({ type: "set_parameter", block: b.id, uri: b.uri, index: p.index, value });
    });
  } else {
    const step = p.kind === "float" ? (p.max - p.min) / 200 : 1;
    input = el("input", { id, type: "range", min: String(p.min), max: String(p.max), step: String(step) });
    input.value = String(p.value);
    input.addEventListener("pointerdown", () => (dragging = id));
    input.addEventListener("pointerup", () => (dragging = null));
    input.addEventListener("pointercancel", () => (dragging = null));
    input.addEventListener("input", () => {
      const value = Number(input.value);
      out.textContent = format(p, value);
      send({ type: "set_parameter", block: b.id, uri: b.uri, index: p.index, value });
    });
  }
  return el("div", { class: "param" }, [el("label", { for: id, text: p.name }), input, out]);
}

function renderBlocks() {
  const box = $("blocks");
  const key = blocksKey(state.blocks);
  if (box.dataset.key !== key) {
    box.dataset.key = key;
    box.replaceChildren(...state.blocks.map((b) => {
      const strip = b.params.filter((p) => p.strip);
      const shown = expanded.has(b.id) || !strip.length ? b.params : strip;
      const card = el("div", { class: "block", id: `b-${b.id}` }, [
        el("div", { class: "block-head" }, [
          el("span", { class: "block-name", text: b.name }),
          el("button", {
            class: "bypass",
            text: "On",
            onclick: () => send({ type: "toggle_bypass", block: b.id, uri: b.uri }),
          }),
        ]),
        el("div", { class: "params" }, shown.map((p) => paramRow(b, p))),
      ]);
      card.style.borderLeftColor = b.color;
      if (b.params.length > strip.length && strip.length) {
        card.append(el("button", {
          class: "more",
          text: expanded.has(b.id) ? "Fewer controls" : `All ${b.params.length} controls`,
          onclick: () => {
            if (expanded.has(b.id)) expanded.delete(b.id); else expanded.add(b.id);
            renderBlocks();
          },
        }));
      }
      return card;
    }));
  }
  // Values in place
  for (const b of state.blocks) {
    const card = $(`b-${b.id}`);
    if (!card) continue;
    card.classList.toggle("off", b.bypassed);
    const bypass = card.querySelector(".bypass");
    bypass.classList.toggle("on", !b.bypassed);
    bypass.textContent = b.bypassed ? "Off" : "On";
    for (const p of b.params) {
      const id = `p-${b.id}-${p.index}`;
      const input = $(id);
      if (!input || id === dragging) continue;
      if (input.type === "range" && document.activeElement === input) continue;
      if (Number(input.value) !== p.value) {
        input.value = String(p.value);
        if (input.role === "switch") {
          input.setAttribute("aria-checked", String(p.value >= (p.min + p.max) / 2));
        }
        input.parentElement.querySelector("output").textContent = format(p, p.value);
      }
    }
  }
}

function onTelemetry(t) {
  t.meters.forEach((v, i) => ($("m" + i).style.width = `${Math.round(v * 100)}%`));
  $("clip-in").classList.toggle("clip", t.clip_in);
  $("clip-out").classList.toggle("clip", t.clip_out);
  if (!tunerOn) return;
  const r = t.tuner;
  $("tuner").classList.toggle("tuned", !!(r && r.tuned));
  if (!r) {
    $("tuner-note").textContent = "–";
    $("tuner-octave").textContent = "";
    $("tuner-info").textContent = "Play a string";
    $("needle").style.left = "50%";
    return;
  }
  $("tuner-note").textContent = r.note;
  $("tuner-octave").textContent = r.octave;
  const cents = Math.max(-50, Math.min(50, r.cents));
  $("needle").style.left = `${50 + cents}%`;
  $("tuner-info").textContent = `${r.frequency.toFixed(1)} Hz · ${r.cents > 0 ? "+" : ""}${Math.round(r.cents)} cents`;
}

// ------------------------------------------------------------ status

async function loadStatus() {
  const r = await api("/api/status").catch(() => null);
  if (!r) return;
  if (r.status === 401) { showLogin("Signed out: sign in again."); return; }
  const s = r.data;
  const rows = [
    ["Version", s.version],
    ["Screen", s.editor ? "This device (no touchscreen on the pedal)" : "The pedal's touchscreen"],
    ["Devices connected", String(s.clients)],
    ["Hotspot", s.hotspot || "–"],
    ...s.addresses.map((a) => [a.kind, `${a.address} (${a.device})`]),
  ];
  $("status-list").replaceChildren(...rows.flatMap(([k, v]) => [el("dt", { text: k }), el("dd", { text: v })]));
}

start();
