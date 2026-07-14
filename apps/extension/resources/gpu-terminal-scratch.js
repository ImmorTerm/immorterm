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
export async function createScratchController({
  wasmModule, wsPort, scratchName,
  fontData, fontName, fontSize, lineHeight, fontWeight,
  applyColors, sendKill, onDestroy, onHide,
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

  // ── Keyboard capture (mirrors kbInput's handle_key → input_raw) ──
  capture.addEventListener('keydown', (e) => {
    // Cmd/Ctrl+C with selection = copy (rich + plain). Selection is
    // intentionally preserved after copy — same rationale as the main
    // terminal's Cmd+C handler (repeat-press-to-confirm habit).
    if ((e.metaKey || e.ctrlKey) && e.key === 'c' && term.has_selection()) {
      const text = term.get_selected_text();
      const html = term.get_selected_html();
      if (text) {
        // Same fallback chain as main: rich ClipboardItem → Tauri plugin
        // (WKWebView flake) → navigator.clipboard.writeText.
        const tcb = window.__TAURI__ && window.__TAURI__.clipboardManager;
        const writeRich = () => navigator.clipboard.write([new ClipboardItem({
          'text/plain': new Blob([text], { type: 'text/plain' }),
          'text/html': new Blob([html || text], { type: 'text/html' }),
        })]);
        const writePlain = () => (tcb && typeof tcb.writeText === 'function')
          ? tcb.writeText(text)
          : navigator.clipboard.writeText(text);
        writeRich().catch(() => writePlain()).catch(() => { /* clipboard unavailable */ });
      }
      e.preventDefault();
      return;
    }
    // Keyboard selection: Shift+Arrow (+Alt = word, +Cmd = home/end) —
    // same dir mapping as main, minus its pseudo-cursor branch.
    if (e.shiftKey && ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) {
      let dir;
      if (e.metaKey) {
        dir = e.key === 'ArrowLeft' ? 'home' : e.key === 'ArrowRight' ? 'end' : e.key === 'ArrowUp' ? 'up' : 'down';
      } else if (e.altKey) {
        dir = e.key === 'ArrowLeft' ? 'word_left' : e.key === 'ArrowRight' ? 'word_right' : e.key === 'ArrowUp' ? 'up' : 'down';
      } else {
        dir = e.key === 'ArrowLeft' ? 'left' : e.key === 'ArrowRight' ? 'right' : e.key === 'ArrowUp' ? 'up' : 'down';
      }
      term.selection_extend(dir);
      dirty = true;
      e.preventDefault();
      return;
    }
    // Cmd+Arrow = Home/End (readline Ctrl+A / Ctrl+E — same as main).
    if (e.metaKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      sendWs({ type: 'input_raw', data: btoa(e.key === 'ArrowLeft' ? '\x01' : '\x05') });
      e.preventDefault();
      return;
    }
    // Remaining Cmd-modified keys pass through to the host app (zoom,
    // find, Cmd+V — which reaches us as a native `paste` event on this
    // textarea) — same rule as the main terminal's kbInput.
    if (e.metaKey) return;
    // Clear selection on non-modifier keystrokes (standard input behavior).
    if (term.has_selection() && !['Shift', 'Meta', 'Alt', 'Control', 'CapsLock'].includes(e.key)) {
      term.selection_clear();
      dirty = true;
    }
    const bytes = term.handle_key(e.key, e.ctrlKey, e.shiftKey, e.altKey);
    if (bytes.length > 0) {
      const b64 = btoa(String.fromCharCode(...bytes));
      sendWs({ type: 'input_raw', data: b64 });
    }
    dirty = true;
    e.preventDefault();
  });
  // ── IME / dictation (mirrors kbInput's composition handlers, minus the
  // composition-view overlay + selection-replace, which are main-only) ──
  let composing = false;
  capture.addEventListener('compositionstart', () => { composing = true; });
  capture.addEventListener('compositionend', (e) => {
    composing = false;
    const text = e.data || '';
    capture.value = '';
    if (text) sendWs({ type: 'input', data: text });
  });
  // beforeinput fallback for inputs that bypass composition entirely —
  // same two inputTypes the main terminal special-cases.
  capture.addEventListener('beforeinput', (e) => {
    if (composing) return;
    if (e.inputType !== 'insertReplacementText' && e.inputType !== 'insertFromDictation') return;
    e.preventDefault();
    const text = e.data || '';
    capture.value = '';
    if (text) sendWs({ type: 'input', data: text });
  });
  // Drop chars the textarea accumulates from un-prevented keystrokes —
  // but never mid-composition (clearing cancels the IME string).
  capture.addEventListener('input', () => { if (!composing) capture.value = ''; });
  capture.addEventListener('paste', (e) => {
    e.preventDefault();
    e.stopPropagation();
    // Same message shape as main's pasteText ({type:'input'}, no client-side
    // bracketing). Image paste ([Image #N] pills) is main-terminal-only for now.
    const text = e.clipboardData && e.clipboardData.getData('text');
    if (text) sendWs({ type: 'input', data: text });
  });
  // Wheel → local scrollback (positive scroll() = up into history).
  let scrollAccum = 0;
  body.addEventListener('wheel', (e) => {
    e.preventDefault();
    const cell = term.cell_size_device ? (term.cell_size_device()[1] / (window.devicePixelRatio || 1)) : 16;
    scrollAccum += e.deltaMode === 1 ? e.deltaY : e.deltaY / (cell || 16);
    const lines = Math.trunc(scrollAccum);
    if (lines !== 0) {
      term.scroll(-lines);
      scrollAccum -= lines;
      dirty = true;
    }
  }, { passive: false });
  // ── Mouse selection (mirrors the main canvas pointer handlers) ──
  // Listeners live on `body` because the transparent capture textarea
  // overlays the canvas and receives the raw pointer events; coordinates
  // are computed against the canvas rect (same area). Drag = line
  // selection, Alt+drag = block, dbl-click = word, triple-click = line.
  // The main terminal never forwards mouse events to the PTY (it has no
  // mouse-tracking path), so neither do we.
  // ponytail: skipped vs main — link-click, AI buttons, scroll-indicator
  // drag, click-to-cursor teleport, pseudo-cursors, drag auto-scroll.
  const DRAG_THRESHOLD = 3; // px of movement before a drag starts a selection
  const DBLCLICK_MS = 400;  // max ms between clicks to count as multi-click
  let mouseDownPos = null;  // {x, y, altKey, dragged} of initial pointerdown
  let lastClickTime = 0;
  let clickCount = 0;       // 1=single, 2=double (word), 3=triple (line)
  body.addEventListener('pointerdown', (e) => {
    if (destroyed || e.button !== 0) return;
    // Keep the capture textarea focused so keydown/paste keep working.
    setTimeout(() => { if (!destroyed) capture.focus({ preventScroll: true }); }, 0);
    const rect = canvas.getBoundingClientRect();
    const cssX = e.clientX - rect.left;
    const cssY = e.clientY - rect.top;
    const now = Date.now();
    clickCount = (now - lastClickTime < DBLCLICK_MS) ? clickCount + 1 : 1;
    lastClickTime = now;
    if (clickCount === 2) {
      try { term.select_word_at(cssX, cssY); } catch (_) { /* best effort */ }
      dirty = true;
      e.preventDefault();
      return;
    }
    if (clickCount >= 3) {
      clickCount = 0;
      try { term.select_line_at(cssX, cssY); } catch (_) { /* best effort */ }
      dirty = true;
      e.preventDefault();
      return;
    }
    mouseDownPos = { x: cssX, y: cssY, altKey: e.altKey, dragged: false };
    body.setPointerCapture(e.pointerId);
    // Regular click clears the previous selection (same as main).
    if (!e.altKey && term.has_selection()) {
      term.selection_clear();
      dirty = true;
    }
    // Don't call selection_start yet — wait for the drag threshold.
  });
  body.addEventListener('pointermove', (e) => {
    if (destroyed || !mouseDownPos) return;
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    const needsStart = mouseDownPos.altKey ? !mouseDownPos.dragged : !term.has_selection();
    if (needsStart) {
      if (Math.abs(cx - mouseDownPos.x) < DRAG_THRESHOLD
          && Math.abs(cy - mouseDownPos.y) < DRAG_THRESHOLD) return;
      // Threshold exceeded — start from the original pointerdown point.
      // Alt+drag = block (rectangular) selection; regular drag = line.
      try {
        if (mouseDownPos.altKey) term.selection_start_block(mouseDownPos.x, mouseDownPos.y);
        else term.selection_start(mouseDownPos.x, mouseDownPos.y);
        mouseDownPos.dragged = true;
      } catch (_) { return; } // re-entrant start — same guard as main
    }
    term.selection_update(cx, cy);
    dirty = true;
  });
  body.addEventListener('pointerup', () => { mouseDownPos = null; });
  // Suppress the webview context menu over the terminal (same as main).
  body.addEventListener('contextmenu', (e) => { e.preventDefault(); e.stopPropagation(); });

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
