// ── ImmorTerm Browser Panel ─────────────────────────────────────
// Docked right-side workshop tab that mirrors the self-driven browser
// (immorterm_browser_* CDP tools in the daemon) as a LIVE STREAM, and
// lets the human take over. Dedicated, nicer presentation than the
// show_image canvas fallback — the daemon targets it ADDITIVELY over
// the existing per-session daemon→webview WebSocket.
//
// ── Message contract ────────────────────────────────────────────
// daemon → webview (RECEIVE):
//   {type:"browser_frame", png_base64, title, url, seq}
//     Live stream: each frame REPLACES the previous image (never stacks),
//     newest seq wins, older/out-of-order seq dropped. Panel auto-opens on
//     the first frame. A "Claude is driving" pulse border shows while frames
//     stream and fades after ~IDLE_MS of silence.
//   {type:"browser_state", paused:bool}
//     Daemon-initiated pause/resume. Mirrors the ⏸/▶ toggle state.
//   {type:"browser_human_request", reason, instructions}
//     Handoff: Claude needs the human. Renders a prominent banner; the
//     "✓ Done — continue" button resumes the AI.
//
// webview → daemon (SEND — best-effort; the daemon may not handle these on
// older builds. Sends must never throw if unacknowledged):
//   {type:"browser_input", kind:"click", x, y}   // x,y = page CSS px
//   {type:"browser_input", kind:"key", key}      // while pane focused
//   {type:"browser_input", kind:"scroll", dy}
//   {type:"browser_control", action:"pause"}
//   {type:"browser_control", action:"continue"}
//
// Coordinate mapping: the frame image is scaled to fit (object-fit:contain),
// so the rendered <img> is letterboxed. We map a click on the <img> back to
// page CSS pixels using the image's naturalWidth/naturalHeight (the daemon
// captures at CSS px per the hardening spec, so natural size == page CSS
// size). See mapToPageCss().
//
// Pure factory module — no host globals. Loaded via dynamic import from
// gpu-terminal.html (wasm-init), same pattern as gpu-terminal-files.js.
// The Tauri app shares this file via the apps/immorterm-app/dist symlink
// to apps/extension/resources.

'use strict';

const IDLE_MS = 3000; // "Claude is driving" pulse fades after this much frame silence

const PANEL_CSS = `
#browser-panel {
  width: 45%;
  min-width: 240px;
  max-width: 70%;
  background: var(--sidebar-bg, #181825);
  border-left: 1px solid var(--sidebar-border, #313244);
  display: flex; flex-direction: column;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
  color: var(--sidebar-text, #cdd6f4);
  position: relative;
  overflow: hidden;
  flex-shrink: 0;
}
#browser-panel-resize {
  position: absolute;
  top: 0; left: 0; bottom: 0;
  width: 6px;
  cursor: ew-resize;
  background: transparent;
  z-index: 5;
  transition: background 120ms ease;
}
#browser-panel-resize:hover,
#browser-panel-resize.dragging {
  background: var(--sidebar-accent, #b482ff);
  opacity: 0.4;
}
#browser-panel-header {
  display: flex; align-items: center; gap: 6px;
  padding: 5px 8px 5px 12px;
  border-bottom: 1px solid var(--sidebar-border, #313244);
  flex-shrink: 0;
  font-size: 11px;
  min-width: 0;
}
#browser-panel-header .bp-globe { flex-shrink: 0; }
#browser-panel-header .bp-title {
  font-weight: 600;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  flex-shrink: 1; min-width: 0;
}
#browser-panel-header .bp-url {
  opacity: 0.55;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  flex: 1; min-width: 0;
}
#browser-panel-header .bp-toggle {
  flex-shrink: 0;
  background: transparent;
  border: 1px solid var(--sidebar-border, #313244);
  color: var(--sidebar-text, #cdd6f4);
  font-size: 10px; font-weight: 600; line-height: 1;
  cursor: pointer; opacity: 0.85;
  padding: 3px 8px; border-radius: 5px;
  white-space: nowrap;
  transition: background 120ms ease, opacity 120ms ease, border-color 120ms ease;
}
#browser-panel-header .bp-toggle:hover { opacity: 1; }
#browser-panel-header .bp-toggle.paused {
  border-color: var(--sidebar-accent, #b482ff);
  color: var(--sidebar-accent, #b482ff);
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 12%, transparent);
}
#browser-panel-header .bp-close {
  flex-shrink: 0;
  background: transparent; border: 0;
  color: var(--sidebar-text, #cdd6f4);
  font-size: 15px; line-height: 1;
  cursor: pointer; opacity: 0.6;
  padding: 2px 6px; border-radius: 4px;
}
#browser-panel-header .bp-close:hover {
  opacity: 1;
  background: var(--sidebar-border, #313244);
}
/* Banner (pause / human-handoff) — sits above the frame. */
#browser-panel-banner {
  display: none;
  flex-shrink: 0;
  padding: 8px 12px;
  font-size: 11px; line-height: 1.4;
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 16%, var(--sidebar-bg, #181825));
  border-bottom: 1px solid var(--sidebar-accent, #b482ff);
  color: var(--sidebar-text, #cdd6f4);
}
#browser-panel-banner.on { display: block; }
#browser-panel-banner .bp-banner-title { font-weight: 700; }
#browser-panel-banner .bp-banner-reason { opacity: 0.85; margin-top: 2px; }
#browser-panel-banner .bp-continue {
  margin-top: 7px;
  background: var(--sidebar-accent, #b482ff);
  border: 0; color: #11111b;
  font-size: 11px; font-weight: 700; line-height: 1;
  cursor: pointer;
  padding: 6px 12px; border-radius: 5px;
}
#browser-panel-banner .bp-continue:hover { filter: brightness(1.08); }
#browser-panel-body {
  flex: 1; min-height: 0;
  display: flex; align-items: center; justify-content: center;
  background: #11111b;
  padding: 6px;
  position: relative;
}
#browser-panel-body img {
  max-width: 100%; max-height: 100%;
  object-fit: contain;
  border-radius: 4px;
  border: 1px solid transparent;
  transition: border-color 600ms ease, box-shadow 600ms ease;
}
/* When NOT paused, clicks nudge the AI; when paused the human fully drives. */
#browser-panel.paused #browser-panel-body img,
#browser-panel-body img.interactive { cursor: crosshair; }
/* "Claude is driving" — subtle pulse while frames are arriving. */
#browser-panel-body img.driving {
  border-color: var(--sidebar-accent, #b482ff);
  animation: bp-drive-pulse 1.6s ease-in-out infinite;
}
@keyframes bp-drive-pulse {
  0%, 100% { box-shadow: 0 0 4px 0 var(--sidebar-accent, #b482ff); }
  50%      { box-shadow: 0 0 14px 2px var(--sidebar-accent, #b482ff); }
}
/* Paused: distinct border, no pulse — the human is in control. */
#browser-panel.paused #browser-panel-body img {
  border-color: #f9e2af;
  box-shadow: 0 0 0 1px #f9e2af, 0 0 12px 0 rgba(249,226,175,0.4);
  animation: none;
}
`;

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/**
 * Create the browser mirror panel. DOM + styles are built here and inserted
 * into `container` before `beforeEl` (the workshop panel), so the column
 * order stays: terminal | browser | workshops | sessions.
 *
 * @param {object} opts
 * @param {HTMLElement} opts.container
 * @param {HTMLElement} [opts.beforeEl]
 * @param {(msg:object)=>void} [opts.send] — routes a message to the active
 *   session's daemon WS. Best-effort: wrapped so a throw never breaks input.
 *
 * Returns { showFrame(msg), setState(msg), humanRequest(msg), hide(), el }.
 */
export function createBrowserPanel({ container, beforeEl, send }) {
  const style = document.createElement('style');
  style.textContent = PANEL_CSS;
  document.head.appendChild(style);

  // Best-effort send — the daemon may not handle browser_input/control on
  // older builds; never let an unacknowledged send throw into a UI handler.
  const emit = (msg) => { try { if (send) send(msg); } catch { /* best-effort */ } };

  const panel = el('div');
  panel.id = 'browser-panel';
  panel.style.display = 'none';

  const resize = el('div');
  resize.id = 'browser-panel-resize';
  resize.title = 'Drag to resize';

  const header = el('div');
  header.id = 'browser-panel-header';
  const globe = el('span', 'bp-globe', '\u{1F310}');
  const titleEl = el('span', 'bp-title', 'Browser');
  const urlEl = el('span', 'bp-url', '');
  const toggleBtn = el('button', 'bp-toggle', '⏸ Pause'); // ⏸
  toggleBtn.title = 'Pause the AI and take over';
  const closeBtn = el('button', 'bp-close', '×');
  closeBtn.title = 'Hide panel (browser keeps running)';
  header.append(globe, titleEl, urlEl, toggleBtn, closeBtn);

  const banner = el('div');
  banner.id = 'browser-panel-banner';
  const bannerTitle = el('div', 'bp-banner-title', '');
  const bannerReason = el('div', 'bp-banner-reason', '');
  const continueBtn = el('button', 'bp-continue', '✓ Done — continue');
  banner.append(bannerTitle, bannerReason, continueBtn);

  const bodyEl = el('div');
  bodyEl.id = 'browser-panel-body';
  const img = document.createElement('img');
  img.alt = 'Self-driven browser';
  img.draggable = false;
  img.tabIndex = 0; // focusable so the pane can capture keystrokes when paused
  bodyEl.appendChild(img);

  panel.append(resize, header, banner, bodyEl);
  container.insertBefore(panel, beforeEl || null);

  // Session-scoped hidden state: once the user closes the panel it stays
  // hidden for this webview lifetime; frames keep updating silently.
  // ponytail: in-memory only — persist to vscode.getState() if users ask.
  let userClosed = false;
  let lastSeq = -Infinity;
  let idleTimer = null;
  let paused = false;

  // ── Pause / continue ──────────────────────────────────────────
  function applyPaused(next) {
    paused = !!next;
    panel.classList.toggle('paused', paused);
    toggleBtn.classList.toggle('paused', paused);
    toggleBtn.textContent = paused ? '▶ Continue' : '⏸ Pause'; // ▶ / ⏸
    toggleBtn.title = paused ? 'Resume the AI' : 'Pause the AI and take over';
    if (paused) {
      // Only show the generic "you're driving" banner if a richer
      // human-request banner isn't already up.
      if (!banner.dataset.request) showPauseBanner();
    } else {
      hideBanner();
      img.blur();
    }
  }

  function showPauseBanner() {
    delete banner.dataset.request;
    bannerTitle.textContent = "You're driving — AI paused & not watching";
    bannerReason.textContent = '';
    bannerReason.style.display = 'none';
    continueBtn.textContent = '▶ Continue';
    banner.classList.add('on');
  }

  function hideBanner() {
    banner.classList.remove('on');
    delete banner.dataset.request;
  }

  toggleBtn.addEventListener('click', () => {
    const next = !paused;
    applyPaused(next);
    emit({ type: 'browser_control', action: next ? 'pause' : 'continue' });
  });

  continueBtn.addEventListener('click', () => {
    applyPaused(false);
    emit({ type: 'browser_control', action: 'continue' });
  });

  closeBtn.addEventListener('click', () => {
    userClosed = true;
    panel.style.display = 'none';
    // Closing the panel never touches the browser — no control message.
  });

  // ── Input forwarding ──────────────────────────────────────────
  // Map a pointer event on the letterboxed <img> back to page CSS pixels.
  // Returns null if the click landed in the letterbox (outside the frame).
  function mapToPageCss(ev) {
    const nw = img.naturalWidth, nh = img.naturalHeight;
    if (!nw || !nh) return null;
    const rect = img.getBoundingClientRect();
    // object-fit:contain — the drawn image is scaled uniformly and centered.
    const scale = Math.min(rect.width / nw, rect.height / nh);
    const drawnW = nw * scale, drawnH = nh * scale;
    const offX = rect.left + (rect.width - drawnW) / 2;
    const offY = rect.top + (rect.height - drawnH) / 2;
    const px = (ev.clientX - offX) / scale;
    const py = (ev.clientY - offY) / scale;
    if (px < 0 || py < 0 || px > nw || py > nh) return null; // letterbox
    return { x: Math.round(px), y: Math.round(py) };
  }

  img.addEventListener('click', (ev) => {
    // Not paused: clicks NUDGE the AI. Paused: the human fully drives.
    const p = mapToPageCss(ev);
    if (!p) return;
    if (paused) img.focus(); // capture subsequent keystrokes
    emit({ type: 'browser_input', kind: 'click', x: p.x, y: p.y });
  });

  // Keystrokes forward only while the pane is focused (paused, human driving).
  img.addEventListener('keydown', (ev) => {
    if (!paused) return;
    // Let the human release focus without forwarding.
    if (ev.key === 'Escape') { img.blur(); return; }
    ev.preventDefault();
    emit({ type: 'browser_input', kind: 'key', key: ev.key });
  });

  // Scroll forwarding — only when paused so we don't fight the AI's own
  // scrolling. Passive:false so we can preventDefault the page from scrolling.
  bodyEl.addEventListener('wheel', (ev) => {
    if (!paused) return;
    ev.preventDefault();
    emit({ type: 'browser_input', kind: 'scroll', dy: ev.deltaY });
  }, { passive: false });

  // ── Drag-to-resize ────────────────────────────────────────────
  // Same geometry as the workshop panel: the handle sits on the LEFT edge,
  // dragging left grows the panel.
  let dragging = false, dragStartX = 0, dragStartW = 0;
  resize.addEventListener('mousedown', (ev) => {
    if (ev.button !== 0) return;
    ev.preventDefault();
    dragging = true;
    dragStartX = ev.clientX;
    dragStartW = panel.getBoundingClientRect().width;
    resize.classList.add('dragging');
    document.body.style.userSelect = 'none';
    document.body.style.cursor = 'ew-resize';
  });
  window.addEventListener('mousemove', (ev) => {
    if (!dragging) return;
    const newW = Math.max(240, dragStartW - (ev.clientX - dragStartX));
    panel.style.width = newW + 'px';
  });
  window.addEventListener('mouseup', () => {
    if (!dragging) return;
    dragging = false;
    resize.classList.remove('dragging');
    document.body.style.userSelect = '';
    document.body.style.cursor = '';
  });

  // ── Inbound messages ──────────────────────────────────────────
  function showFrame(msg) {
    if (!msg || typeof msg.png_base64 !== 'string' || !msg.png_base64) return;
    const seq = typeof msg.seq === 'number' ? msg.seq : lastSeq + 1;
    if (seq <= lastSeq) return; // stale/out-of-order frame — newest wins
    lastSeq = seq;

    img.src = 'data:image/png;base64,' + msg.png_base64;
    titleEl.textContent = msg.title || 'Browser';
    urlEl.textContent = msg.url ? '— ' + msg.url : '';
    header.title = (msg.title || '') + (msg.url ? '\n' + msg.url : '');

    if (!userClosed) panel.style.display = 'flex';

    // Driving pulse only while the AI is actually streaming (not paused).
    if (!paused) {
      img.classList.add('driving');
      if (idleTimer) clearTimeout(idleTimer);
      idleTimer = setTimeout(() => img.classList.remove('driving'), IDLE_MS);
    }
  }

  // Daemon-initiated pause/resume. Older daemons never send this — the
  // stream-only path keeps working, and local ⏸/▶ still drives paused state.
  function setState(msg) {
    if (!msg || typeof msg.paused !== 'boolean') return;
    if (msg.paused !== paused) applyPaused(msg.paused);
  }

  // Human handoff — Claude needs the user. Visually the same as pause plus
  // a reason. Marking banner.dataset.request keeps applyPaused from
  // overwriting it with the generic pause banner.
  function humanRequest(msg) {
    if (!msg) return;
    if (!userClosed) panel.style.display = 'flex';
    banner.dataset.request = '1';
    bannerTitle.textContent = '\u{1F64B} Claude needs you: ' + (msg.reason || 'take over');
    if (msg.instructions) {
      bannerReason.textContent = msg.instructions;
      bannerReason.style.display = '';
    } else {
      bannerReason.textContent = '';
      bannerReason.style.display = 'none';
    }
    continueBtn.textContent = '✓ Done — continue';
    banner.classList.add('on');
    // A handoff implies the AI has stopped and is waiting on the human.
    paused = true;
    panel.classList.add('paused');
    toggleBtn.classList.add('paused');
    toggleBtn.textContent = '▶ Continue';
    img.classList.remove('driving');
    img.focus();
  }

  return {
    el: panel,
    showFrame,
    setState,
    humanRequest,
    hide() { panel.style.display = 'none'; },
  };
}
