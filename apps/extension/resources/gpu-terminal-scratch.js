// ── ImmorTerm Scratch — floating disposable terminal panel ──────
// A SECOND WasmTerminal instance rendered into its own light-DOM floating
// panel (NOT a workshop card — shadow DOM breaks init_gpu's getElementById
// lookup; NOT the single-slot modal system). The panel is appended to
// document.body, centered, CSS-resizable, and owns its full stack: canvas,
// GPU terminal, keyboard capture textarea, ResizeObserver, render loop, and
// WebSocket to the scratch daemon.
//
// ── Message contract ────────────────────────────────────────────
// The host obtains the scratch daemon's ws_port via {type:"scratch_open"} on
// the main session's WS (reply: {type:"scratch_info", ws_port, alive}) and
// passes it here. This module then speaks the normal daemon WS protocol on
// its OWN socket: resize / subscribe_raw / input_raw / input, binary frames
// as raw PTY bytes, JSON "snapshot" for state restore.
//
// Lifecycle: × (hide) keeps the terminal + WS alive (display:none only);
// trash sends {type:"scratch_kill"} on the main session's WS (host-supplied
// sendKill) and destroys everything. Scratch is disposable — one WS retry,
// no backoff machinery.

'use strict';

// Shared input pipeline — the EXACT same keyboard/mouse/paste/IME handling
// as the main terminal (see gpu-terminal-input.js). Resolved relative to
// this module's own URL so it works from both the VS Code webview
// (asWebviewUri) and the standalone relative path.
import { createTerminalInput } from './gpu-terminal-input.js';

// ── Panel HTML (light DOM — canvas must be reachable by getElementById) ──
// canvasId is per-controller: with one scratch per session, several panels
// can coexist and init_gpu looks its canvas up by id.
function scratchPanelHtml(canvasId) {
  return `
  <div class="scratch-header">
    <span class="scratch-title">Scratch</span>
    <span class="scratch-spacer"></span>
    <button class="scratch-trash" type="button" title="Kill scratch terminal">🗑</button>
    <button class="scratch-close" type="button" title="Hide (keeps running)">✕</button>
  </div>
  <div class="scratch-body">
    <canvas id="${canvasId}"></canvas>
    <!-- Click-to-cursor teleport visuals (same elements as the main
         terminal's #phantom-cursor / #cursor-mask, scoped to this panel). -->
    <div class="scratch-cursor-mask" style="position:absolute;pointer-events:none;z-index:5;display:none;background:var(--vscode-terminal-background,#000);color:#fff;font-family:var(--vscode-editor-font-family,monospace);line-height:1;white-space:pre;overflow:hidden;transition:none;text-decoration:none;text-align:left;padding:0;margin:0;border:0;"></div>
    <div class="scratch-phantom-cursor" style="position:absolute;pointer-events:none;z-index:6;display:none;background:rgba(255,255,255,0.85);mix-blend-mode:difference;transition:none;"></div>
    <!-- Transparent capture surface (mirrors the terminal's hidden kbInput /
         the browser panel's bw-capture): a real <textarea> is the only element
         that reliably receives keydown/paste from real user input. -->
    <textarea class="scratch-capture" aria-hidden="true" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"></textarea>
  </div>`;
}

// Wait until the canvas has non-zero CSS dimensions — a 0×0 surface makes
// wgpu's request_adapter(compatible_surface) fail (same guard as initWasm).
function waitForDims(canvas, maxMs = 10000) {
  const rect = canvas.getBoundingClientRect();
  if (rect.width > 0 && rect.height > 0) return Promise.resolve(rect);
  return new Promise((resolve) => {
    const timeout = setTimeout(() => { observer.disconnect(); resolve(null); }, maxMs);
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        if (entry.contentRect.width > 0 && entry.contentRect.height > 0) {
          clearTimeout(timeout);
          observer.disconnect();
          resolve(canvas.getBoundingClientRect());
          return;
        }
      }
    });
    observer.observe(canvas);
  });
}

// ── Controller factory ──────────────────────────────────────────
// Brings up the full second-terminal stack. Async — resolves once the GPU
// terminal is initialized and the WS is connecting.
//
// @param wasmModule  the ALREADY-LOADED wasm module (WasmTerminal ctor)
// @param wsPort      scratch daemon WS port (from the scratch_info reply)
// @param scratchName "scratch-<main session>" — label for load_snapshot
// @param fontData/fontName/fontSize/lineHeight/fontWeight — same values the
//                    host fed the main terminal (wasm-init message)
// @param applyColors (term)=>void — host's applyVSCodeTerminalColors
// @param sendKill    ()=>void — sends {type:"scratch_kill"} on the main
//                    session's WS (the scratch daemon is owned by it)
// @param onDestroy   ()=>void — host clears its controller reference
// @param onHide      ()=>void — host returns focus to the main terminal
//                    whenever the panel minimizes (✕, blur, outside click)
// @param openLink    (link)=>void — Cmd/Ctrl+click link opener (host's
//                    postMessage transport, same one the main terminal uses)
export async function createScratchController({
  wasmModule, wsPort, scratchName,
  fontData, fontName, fontSize, lineHeight, fontWeight,
  applyColors, sendKill, onDestroy, onHide, openLink,
}) {
  // ── Panel DOM ─────────────────────────────────────────────────
  // Canvas id must be unique per controller — scratchName is
  // "scratch-<session>" and there is at most one scratch per session.
  const canvasId = 'scratch-canvas-' + (scratchName || 'scratch').replace(/[^A-Za-z0-9_-]/g, '_');
  const panel = document.createElement('div');
  panel.className = 'scratch-panel';
  panel.innerHTML = scratchPanelHtml(canvasId);
  document.body.appendChild(panel);

  const canvas = panel.querySelector('#' + canvasId);
  const body = panel.querySelector('.scratch-body');
  const capture = panel.querySelector('.scratch-capture');

  let term = null;
  let ws = null;
  let destroyed = false;
  let dirty = false;
  let rafId = 0;
  let wsRetried = false;
  // Own pending-bytes buffer — NEVER the host's global pendingPtyChunks.
  const pendingChunks = [];
  let pendingLen = 0;

  // ── WasmTerminal bring-up (same order as the host's initWasm) ─
  term = new wasmModule.WasmTerminal(80, 24); // placeholder dims
  try {
    if (fontData) {
      const raw = atob(fontData);
      const bytes = new Uint8Array(raw.length);
      for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
      term.set_custom_font(bytes);
      if (fontName) term.set_custom_font_name(fontName);
    }
    term.set_font_size(fontSize);
    term.set_line_height(lineHeight);
    // DOM-measured char height (matches xterm.js CharSizeService — see initWasm).
    {
      const span = document.createElement('span');
      span.style.fontFamily = fontName || 'monospace';
      span.style.fontSize = fontSize + 'px';
      span.style.lineHeight = 'normal';
      span.style.position = 'absolute';
      span.style.visibility = 'hidden';
      span.textContent = 'W';
      document.body.appendChild(span);
      const charHeight = span.getBoundingClientRect().height;
      document.body.removeChild(span);
      term.set_char_height_css(charHeight);
    }
    term.set_font_weight(fontWeight);
    const padDpr = window.devicePixelRatio || 1;
    term.set_content_padding(2 * padDpr, 0, 0, 10 * padDpr);
    if (applyColors) applyColors(term);

    const rect = await waitForDims(canvas);
    if (!rect) throw new Error('scratch canvas has 0×0 dimensions after 10s');
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(rect.width * dpr);
    canvas.height = Math.floor(rect.height * dpr);
    await term.init_gpu(canvasId, dpr);
    // Real first resize — the 80×24 construction dims were placeholders.
    term.resize(canvas.width, canvas.height);
  } catch (e) {
    panel.remove();
    try { if (term && term.free) term.free(); } catch (_) { /* best effort */ }
    throw e;
  }

  // ── Render loop (own rAF chain, own pending buffer) ───────────
  function renderLoop() {
    if (destroyed) return;
    if (pendingChunks.length > 0) {
      if (pendingChunks.length === 1) {
        term.process(pendingChunks[0]);
      } else {
        const merged = new Uint8Array(pendingLen);
        let off = 0;
        for (const chunk of pendingChunks) { merged.set(chunk, off); off += chunk.length; }
        term.process(merged);
      }
      pendingChunks.length = 0;
      pendingLen = 0;
      dirty = true;
    }
    if (dirty) {
      dirty = false;
      try { term.render(); } catch (_) { dirty = true; /* retry next frame */ }
    }
    rafId = requestAnimationFrame(renderLoop);
  }
  dirty = true;
  rafId = requestAnimationFrame(renderLoop);

  // Compositor workaround: after display:none→block (or first show) the
  // presentation layer needs 2 frames; a resize reconfigures the surface.
  // Same fix as initWasm's post-init double-rAF.
  function reconfigureSurface() {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        if (!destroyed && canvas.width > 0 && canvas.height > 0) {
          term.resize(canvas.width, canvas.height);
          dirty = true;
        }
      });
    });
  }
  reconfigureSurface();

  // ── Own WebSocket to the scratch daemon ───────────────────────
  function sendWs(msg) {
    try {
      if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
    } catch (_) { /* best effort */ }
  }
  function sendResize() {
    const dims = term.dimensions();
    sendWs({ type: 'resize', cols: dims[0], rows: dims[1] });
  }
  function connect() {
    ws = new WebSocket('ws://127.0.0.1:' + wsPort);
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
      // resize BEFORE subscribe so the snapshot arrives at our grid size.
      sendResize();
      ws.send(JSON.stringify({ type: 'subscribe_raw', full_snapshot: true }));
    };
    ws.onmessage = (event) => {
      if (destroyed) return;
      if (event.data instanceof ArrayBuffer) {
        const data = new Uint8Array(event.data);
        pendingChunks.push(data);
        pendingLen += data.length;
        dirty = true;
        return;
      }
      try {
        const msg = JSON.parse(event.data);
        if (msg.type === 'snapshot' && msg.snapshot_json) {
          term.load_snapshot(msg.snapshot_json, scratchName || 'scratch');
          if (canvas.width > 0 && canvas.height > 0) term.resize(canvas.width, canvas.height);
          dirty = true;
        } else if (msg.type === 'resize') {
          term.resize(canvas.width, canvas.height);
          dirty = true;
        }
        // ponytail: everything else (control events, ai layers) is main-terminal
        // machinery — a scratch shell doesn't need it.
      } catch (_) { /* non-JSON text frame — ignore */ }
    };
    ws.onclose = () => {
      // Disposable surface: exactly one reconnect attempt, then give up.
      if (!destroyed && !wsRetried) {
        wsRetried = true;
        setTimeout(() => { if (!destroyed) connect(); }, 1000);
      }
    };
    ws.onerror = () => { /* onclose handles retry */ };
  }
  connect();

  // ── Panel resize → canvas backing store + PTY grid ────────────
  let lastW = 0, lastH = 0, sizeTimer = null;
  function applyResize() {
    if (destroyed) return;
    const rect = canvas.getBoundingClientRect();
    if (rect.width < 20 || rect.height < 20) return;
    const dpr = window.devicePixelRatio || 1;
    const w = Math.floor(rect.width * dpr);
    const h = Math.floor(rect.height * dpr);
    if (w === lastW && h === lastH) return;
    lastW = w; lastH = h;
    canvas.width = w;
    canvas.height = h;
    term.resize(w, h);
    sendResize();
    dirty = true;
  }
  const ro = new ResizeObserver(() => {
    if (sizeTimer) clearTimeout(sizeTimer);
    sizeTimer = setTimeout(applyResize, 80); // same debounce as the main terminal
  });
  ro.observe(body);

  // ── Input pipeline (shared with the main terminal) ─────────────
  // EXACT input parity: keyboard (multi-cursor, keyboard selection,
  // readline shortcuts, type-to-replace), rich copy, paste, IME, mouse
  // selection (drag/word/line/block), scroll-thumb drag, drag auto-scroll,
  // wheel, click-to-cursor teleport, Cmd+hover/Cmd+click links, context
  // menu suppression. Pointer/mouse listeners land on `body` because the
  // transparent capture textarea overlays the canvas; coordinates are
  // computed against the canvas rect (same area).
  // No host hooks: bullets/comments/pills wizards, paste-undo, AI buttons,
  // image paste, and daemon scrollback fetch are main-terminal-only.
  const phantomEl = panel.querySelector('.scratch-phantom-cursor');
  const maskEl = panel.querySelector('.scratch-cursor-mask');
  const input = createTerminalInput({
    getTerm: () => (destroyed ? null : term),
    canvas,
    keyTarget: capture,
    pointerTarget: body,
    getWs: () => (ws && ws.readyState === WebSocket.OPEN) ? ws : null,
    markDirty: () => { dirty = true; },
    isReady: () => !destroyed,
    // Keep the capture textarea focused so keydown/paste keep working.
    focus: () => setTimeout(() => { if (!destroyed) capture.focus({ preventScroll: true }); }, 0),
    getCellSize: () => {
      const dpr = window.devicePixelRatio || 1;
      const cs = (term && term.cell_size_device) ? term.cell_size_device() : null;
      return cs ? { w: cs[0] / dpr, h: cs[1] / dpr } : { w: 8, h: 16 };
    },
    openLink, // host postMessage transport — same mechanism as main
    phantomEl, maskEl,
    scrollProximity: true,
    hooks: {
      // Cursor feedback on the capture textarea (it overlays the canvas,
      // so the canvas cursor style would never be visible).
      linkHover: () => { capture.style.cursor = 'pointer'; },
      linkHoverEnd: () => { capture.style.cursor = ''; },
    },
  });

  // ── Drag (header) + CSS `resize: both` handle ─────────────────
  // The panel is CSS-centered via translate(-50%,-50%); both dragging and
  // the corner resize need explicit left/top px (a center-translated box
  // grows around its middle, so the resize handle drifts away from the
  // cursor). First pointerdown anywhere on the panel freezes geometry once.
  let frozen = false;
  function freezeGeometry() {
    if (frozen) return;
    frozen = true;
    const r = panel.getBoundingClientRect();
    panel.style.left = r.left + 'px';
    panel.style.top = r.top + 'px';
    panel.style.width = r.width + 'px';
    panel.style.height = r.height + 'px';
    panel.style.transform = 'none';
  }
  panel.addEventListener('pointerdown', freezeGeometry, true);
  const header = panel.querySelector('.scratch-header');
  let drag = null; // pointer offset from the panel's top-left corner
  header.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || e.target.closest('button')) return; // buttons aren't a handle
    const r = panel.getBoundingClientRect();
    drag = { dx: e.clientX - r.left, dy: e.clientY - r.top };
    header.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  header.addEventListener('pointermove', (e) => {
    if (!drag) return;
    // Clamp so the header always stays reachable inside the viewport.
    const r = panel.getBoundingClientRect();
    const headerH = header.getBoundingClientRect().height;
    panel.style.left = Math.min(Math.max(e.clientX - drag.dx, 0),
      Math.max(0, window.innerWidth - r.width)) + 'px';
    panel.style.top = Math.min(Math.max(e.clientY - drag.dy, 0),
      Math.max(0, window.innerHeight - headerH)) + 'px';
  });
  header.addEventListener('pointerup', () => { drag = null; });
  header.addEventListener('lostpointercapture', () => { drag = null; });

  // ── Blur = minimize ───────────────────────────────────────────
  // Pointerdown anywhere OUTSIDE the panel hides it (PTY + WS stay alive).
  // Capture phase so it runs even if the target swallows the event. The
  // status-bar ">_" icon can't insta-undo its own show(): show fires on the
  // icon's `click`, which lands AFTER this pointerdown — a hidden panel is
  // skipped here, then shown by the click.
  function onDocPointerDown(e) {
    if (destroyed || panel.style.display === 'none') return;
    if (panel.contains(e.target)) return;
    hide();
  }
  document.addEventListener('pointerdown', onDocPointerDown, true);
  // Focus leaving the panel (e.g. Tab) also minimizes. relatedTarget is
  // null on canvas clicks inside the panel — require a real outside target.
  panel.addEventListener('focusout', (e) => {
    if (destroyed || !e.relatedTarget || panel.contains(e.relatedTarget)) return;
    hide();
  });

  // ── Show / hide / destroy ─────────────────────────────────────
  function show() {
    panel.style.display = 'flex';
    reconfigureSurface(); // display:none→flex needs the compositor kick
    capture.focus({ preventScroll: true });
  }
  function hide() {
    if (panel.style.display === 'none') return;
    panel.style.display = 'none';
    if (onHide) onHide(); // host returns focus to the main terminal
  }
  function destroy() {
    if (destroyed) return;
    destroyed = true;
    input.dispose(); // remove window-level pointerup/keyup listeners + timers
    if (rafId) cancelAnimationFrame(rafId);
    if (sizeTimer) clearTimeout(sizeTimer);
    ro.disconnect();
    document.removeEventListener('pointerdown', onDocPointerDown, true);
    try { if (ws) ws.close(); } catch (_) { /* best effort */ }
    ws = null;
    try { if (term && term.free) term.free(); } catch (_) { /* best effort */ }
    term = null;
    panel.remove();
    if (onDestroy) onDestroy();
  }

  panel.querySelector('.scratch-close').addEventListener('click', hide);
  panel.querySelector('.scratch-trash').addEventListener('click', () => {
    try { if (sendKill) sendKill(); } catch (_) { /* best effort */ }
    destroy();
  });

  capture.focus({ preventScroll: true });

  return {
    show, hide, destroy,
    isVisible: () => panel.style.display !== 'none',
  };
}
