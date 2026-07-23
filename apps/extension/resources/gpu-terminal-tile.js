// ── ImmorTerm TerminalTile — a live GPU terminal in a layout cell ──────
// SP1 (Spaces slice 1): the terminal-generic core of createScratchController
// (gpu-terminal-scratch.js), lifted into a durable, flex-laid-out tile. It
// owns its full stack — canvas, WasmTerminal, keyboard capture textarea,
// ResizeObserver, render loop, and its OWN second WebSocket to a session's
// daemon — and coexists with the primary terminal in one webview.
//
// What was LEFT BEHIND from scratch (chrome, not engine): the floating panel,
// header pointer-drag + freeze-geometry, close-confirmation overlay, minimize
// (show/hide) semantics, sendKill/onHide. A tile is laid out by flex, so all
// the translate(-50%,-50%) geometry math is dead weight here.
//
// KNOWN DEBT (deliberate): the ~engine core here is a near-verbatim copy of
// createScratchController's, NOT a shared import — the two duplicate. This is
// intentional for SP1: SP2 reworks this module for the grid + N-tile
// suspend/wake, so extracting a shared gpu-terminal-core.js NOW would freeze
// an interface that is about to change and risk regressing the stable, shipped
// scratch panel. Extract the shared core in SP2, once the tile API has settled,
// parameterized by the ~5 real differences (DOM/class prefix + button set,
// sendKill vs registerClaim, one-shot vs unbounded reconnect, onHide vs none).
//
// What CHANGED vs scratch: (1) unbounded reconnect with per-attempt port
// re-resolution (scratch does one fixed-port retry — fine for a disposable
// surface, wrong for a durable tile); (2) registerClaim/releaseClaim so the
// host's sendResize broadcast skips this session's PTY (the tile owns its
// dims — see §2 of the SP1 spec / claimedTileSessions in gpu-terminal.html);
// (3) onStatus header chip.

'use strict';

// Shared input pipeline — the EXACT same keyboard/mouse/paste/IME handling as
// the main terminal and the scratch panel (gpu-terminal-input.js).
import { createTerminalInput, handleClipboardImageReply } from './gpu-terminal-input.js';

// Wait until the canvas has non-zero CSS dimensions — a 0×0 surface makes
// wgpu's request_adapter(compatible_surface) fail. (Verbatim from scratch.js:56.)
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

// ── Tile DOM (light DOM — canvas must be reachable by getElementById,
// init_gpu does a document.getElementById lookup) ──
function tileHtml(canvasId, title) {
  return `
  <div class="tile-header">
    <span class="tile-title"></span>
    <span class="tile-status" data-state="connecting">connecting</span>
    <span class="tile-spacer"></span>
    <button class="tile-close" type="button" title="Close tile">✕</button>
  </div>
  <div class="tile-body">
    <canvas id="${canvasId}"></canvas>
    <div class="tile-cursor-mask" style="position:absolute;pointer-events:none;z-index:5;display:none;background:var(--vscode-terminal-background,#000);color:#fff;font-family:var(--vscode-editor-font-family,monospace);line-height:1;white-space:pre;overflow:hidden;transition:none;text-decoration:none;text-align:left;padding:0;margin:0;border:0;"></div>
    <div class="tile-phantom-cursor" style="position:absolute;pointer-events:none;z-index:6;display:none;background:rgba(255,255,255,0.85);mix-blend-mode:difference;transition:none;"></div>
    <textarea class="tile-capture" aria-hidden="true" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"></textarea>
  </div>`;
}

// ── Factory ──
// Brings up the full second-terminal stack in `mountEl`. Async — resolves once
// the GPU terminal is initialized and the WS is connecting.
export async function createTerminalTile({
  wasmModule,                 // already-loaded wasm module (shared linear memory)
  sessionName,                // key into the host `sessions` map
  wsPort,                     // initial port (sessions.get(sessionName).wsPort)
  resolveWsPort,              // () => Promise<number|null> — host resolveCurrentWsPort(sessionName)
  mountEl,                    // flex cell to append the tile into
  registerClaim, releaseClaim,// () => void — add/remove sessionName in host claimedTileSessions
  displayName,                // header label (falls back to sessionName)
  fontData, fontName, fontSize, lineHeight, fontWeight,
  applyColors, themeName,     // same host params the scratch call site assembles
  openLink, linkHover, linkHoverEnd, fallbackCwd,
  onStatus,                   // (state:'connecting'|'live'|'suspended'|'disconnected') => void
  onDestroy,                  // host clears its tile reference
}) {
  // Canvas id slug — mirrors scratch.js:113; unique per tile.
  const slug = (sessionName || 'tile').replace(/[^A-Za-z0-9_-]/g, '_');
  const canvasId = 'tile-canvas-' + slug;

  const el = document.createElement('div');
  el.className = 'tile';
  el.innerHTML = tileHtml(canvasId, displayName || sessionName);
  mountEl.appendChild(el);

  const canvas = el.querySelector('#' + canvasId);
  const body = el.querySelector('.tile-body');
  const capture = el.querySelector('.tile-capture');
  const statusEl = el.querySelector('.tile-status');
  el.querySelector('.tile-title').textContent = displayName || sessionName;

  let term = null;
  let ws = null;
  let destroyed = false;
  let dirty = false;
  let rafId = 0;
  let reconnectTimer = null;
  let currentState = 'connecting';
  // Own pending-bytes buffer — NEVER the host's global pendingPtyChunks.
  const pendingChunks = [];
  let pendingLen = 0;

  function setStatus(state) {
    currentState = state;
    if (statusEl) { statusEl.textContent = state; statusEl.dataset.state = state; }
    if (onStatus) { try { onStatus(state); } catch (_) { /* best effort */ } }
  }

  // ── WasmTerminal bring-up (same order as the host's initWasm / scratch) ──
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
    if (themeName) { try { term.set_theme(themeName); } catch (_) { /* unknown preset */ } }

    const rect = await waitForDims(canvas);
    if (!rect) throw new Error('tile canvas has 0×0 dimensions after 10s');
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(rect.width * dpr);
    canvas.height = Math.floor(rect.height * dpr);
    await term.init_gpu(canvasId, dpr);
    term.resize(canvas.width, canvas.height); // real first resize
  } catch (e) {
    el.remove();
    try { if (term && term.free) term.free(); } catch (_) { /* best effort */ }
    throw e;
  }

  // ── Render loop (own rAF chain, own pending buffer) ──
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

  // Compositor workaround: after a show/first-paint the presentation layer
  // needs 2 frames; a resize reconfigures the surface. (scratch.js:209.)
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

  // ── Clipboard-image RPC state (own resolver map + seq) ──
  const clipboardResolvers = new Map();
  let clipboardSeq = 0;

  // ── Own WebSocket to the session daemon ──
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
    reconnectTimer = null;
    setStatus('connecting');
    ws = new WebSocket('ws://127.0.0.1:' + wsPort);
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
      // resize BEFORE subscribe so the snapshot arrives at our grid size.
      sendResize();
      ws.send(JSON.stringify({ type: 'subscribe_raw', full_snapshot: true }));
      setStatus('live');
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
          term.load_snapshot(msg.snapshot_json, sessionName);
          if (canvas.width > 0 && canvas.height > 0) term.resize(canvas.width, canvas.height);
          dirty = true;
        } else if (msg.type === 'resize') {
          term.resize(canvas.width, canvas.height);
          dirty = true;
        } else if (msg.type === 'clipboard_image_presence' || msg.type === 'clipboard_image_saved') {
          handleClipboardImageReply(clipboardResolvers, msg);
        }
        // Everything else (control events, ai layers) is main-terminal
        // machinery a tiled shell doesn't need — ignore.
      } catch (_) { /* non-JSON text frame — ignore */ }
    };
    ws.onclose = () => {
      if (destroyed) return;
      setStatus('disconnected');
      // Durable tile: unbounded reconnect with a fresh port each attempt.
      // A daemon that died and respawned lives on a NEW port; re-resolve it.
      // Do NOT reuse the host's reconnectSession — it mutates the shared
      // sessions-map entry's session.ws (the PRIMARY's socket), not ours.
      // ponytail: the host DOES re-resolve the same background session (its
      // onclose reconnects ALL sessions), so the single-resolver
      // wsPortResolvePending CAN collide with ours on a daemon respawn. It's
      // tolerable because BOTH sides retry unbounded and re-resolve every
      // attempt — a later non-colliding attempt gets the fresh port (transient
      // extra delay, never a hang). Array-of-resolvers is the SP2 cleanup.
      Promise.resolve(resolveWsPort ? resolveWsPort() : null)
        .then((p) => { if (p) wsPort = p; })
        .catch(() => { /* keep last port */ })
        .finally(() => {
          if (destroyed) return;
          reconnectTimer = setTimeout(() => { if (!destroyed) connect(); }, 1000);
        });
    };
    ws.onerror = () => { /* onclose handles retry */ };
  }
  connect();
  if (registerClaim) registerClaim(); // primary's resize broadcast now skips us

  // ── Cell resize → canvas backing store + PTY grid (debounced RO) ──
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

  // ── Input pipeline (shared with the main terminal) ──
  const phantomEl = el.querySelector('.tile-phantom-cursor');
  const maskEl = el.querySelector('.tile-cursor-mask');
  function tileCwd() {
    try {
      const c = (term && typeof term.cwd === 'function') ? term.cwd() : '';
      if (c) return c;
    } catch (_) { /* best effort */ }
    return fallbackCwd ? fallbackCwd() : undefined;
  }
  function endHover() {
    capture.style.cursor = '';
    if (linkHoverEnd) linkHoverEnd();
  }
  const input = createTerminalInput({
    getTerm: () => (destroyed ? null : term),
    canvas,
    keyTarget: capture,
    pointerTarget: body,
    getWs: () => (ws && ws.readyState === WebSocket.OPEN) ? ws : null,
    markDirty: () => { dirty = true; },
    isReady: () => !destroyed,
    focus: () => setTimeout(() => { if (!destroyed) capture.focus({ preventScroll: true }); }, 0),
    getCellSize: () => {
      const dpr = window.devicePixelRatio || 1;
      const cs = (term && term.cell_size_device) ? term.cell_size_device() : null;
      return cs ? { w: cs[0] / dpr, h: cs[1] / dpr } : { w: 8, h: 16 };
    },
    openLink,
    phantomEl, maskEl,
    scrollProximity: true,
    clipboardImage: {
      resolvers: clipboardResolvers,
      makeRequestId: (prefix) => prefix + (++clipboardSeq),
    },
    hooks: {
      linkHover: (link, e) => {
        capture.style.cursor = 'pointer';
        if (linkHover) linkHover(link, e, tileCwd());
      },
      linkHoverEnd: endHover,
    },
  });

  // ── Lifecycle (suspend/wake minimal — full lifecycle is SP5) ──
  function suspend() {
    if (destroyed || currentState === 'suspended') return;
    if (rafId) cancelAnimationFrame(rafId);
    rafId = 0;
    sendWs({ type: 'subscribe_control' }); // host's background downgrade
    pendingChunks.length = 0;
    pendingLen = 0;
    setStatus('suspended');
  }
  function wake() {
    if (destroyed || currentState !== 'suspended') return;
    sendWs({ type: 'subscribe_raw', full_snapshot: true }); // snapshot handler resizes + repaints
    reconfigureSurface();
    if (!rafId) { dirty = true; rafId = requestAnimationFrame(renderLoop); }
    setStatus('live');
  }

  function destroy() {
    if (destroyed) return;
    destroyed = true;
    if (linkHoverEnd) linkHoverEnd();          // dismiss any live link preview
    input.dispose();                            // window pointerup/keyup + timers
    if (rafId) cancelAnimationFrame(rafId);
    if (sizeTimer) clearTimeout(sizeTimer);
    if (reconnectTimer) clearTimeout(reconnectTimer); // SP1 adds the retry loop scratch lacked
    ro.disconnect();
    if (releaseClaim) releaseClaim();           // undo the sendResize claim exemption
    try { if (ws) ws.close(); } catch (_) { /* best effort */ }
    ws = null;
    try { if (term && term.free) term.free(); } catch (_) { /* best effort */ } // frees the wgpu stack
    term = null;
    el.remove();                                // element-scoped input listeners die with it
    if (onDestroy) onDestroy();
  }

  // Keep the ✕ from stealing the first click off the capture textarea.
  for (const btn of el.querySelectorAll('.tile-header button')) {
    btn.addEventListener('mousedown', (e) => e.preventDefault());
  }
  el.querySelector('.tile-close').addEventListener('click', destroy);

  capture.focus({ preventScroll: true });

  return {
    el,
    suspend, wake, destroy,
    focus: () => { if (!destroyed) capture.focus({ preventScroll: true }); },
    setTheme: (name) => {
      if (destroyed || !name) return;
      try { term.set_theme(name); dirty = true; } catch (_) { /* best effort */ }
    },
    status: () => currentState,
  };
}
