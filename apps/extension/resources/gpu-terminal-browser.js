// ── ImmorTerm Browser Panel ─────────────────────────────────────
// Docked right-side panel that mirrors the self-driven browser
// (immorterm_browser_* CDP tools in the daemon). Dedicated, nicer
// presentation than the show_image canvas fallback — the daemon can
// target it ADDITIVELY by emitting `browser_frame` over the existing
// per-session daemon→webview WebSocket.
//
// browser_frame message contract (daemon → webview WS):
//   {
//     type: "browser_frame",
//     png_base64: string,  // raw base64 PNG, no "data:" prefix
//     title: string,       // page title (may be empty)
//     url: string,         // current page URL (may be empty)
//     seq: number          // monotonically increasing per browser session
//   }
// Semantics:
//   - Each frame REPLACES the previous image (no stacking).
//   - Stale frames (seq <= last rendered seq) are dropped.
//   - Panel auto-opens on the first frame; the close button hides it for
//     the rest of the webview session (frames keep updating silently) and
//     NEVER touches the underlying browser.
//   - If browser_frame never arrives, the panel stays hidden — zero impact.
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
#browser-panel-body {
  flex: 1; min-height: 0;
  display: flex; align-items: center; justify-content: center;
  background: #11111b;
  padding: 6px;
}
#browser-panel-body img {
  max-width: 100%; max-height: 100%;
  object-fit: contain;
  border-radius: 4px;
  border: 1px solid transparent;
  transition: border-color 600ms ease, box-shadow 600ms ease;
}
/* "Claude is driving" — subtle pulse while frames are arriving. */
#browser-panel-body img.driving {
  border-color: var(--sidebar-accent, #b482ff);
  animation: bp-drive-pulse 1.6s ease-in-out infinite;
}
@keyframes bp-drive-pulse {
  0%, 100% { box-shadow: 0 0 4px 0 var(--sidebar-accent, #b482ff); }
  50%      { box-shadow: 0 0 14px 2px var(--sidebar-accent, #b482ff); }
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
 * Returns { showFrame(msg), hide(), el }.
 */
export function createBrowserPanel({ container, beforeEl }) {
  const style = document.createElement('style');
  style.textContent = PANEL_CSS;
  document.head.appendChild(style);

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
  const closeBtn = el('button', 'bp-close', '×');
  closeBtn.title = 'Hide panel (browser keeps running)';
  header.append(globe, titleEl, urlEl, closeBtn);

  const body = el('div');
  body.id = 'browser-panel-body';
  const img = document.createElement('img');
  img.alt = 'Self-driven browser';
  img.draggable = false;
  body.appendChild(img);

  panel.append(resize, header, body);
  container.insertBefore(panel, beforeEl || null);

  // Session-scoped hidden state: once the user closes the panel it stays
  // hidden for this webview lifetime; frames keep updating silently.
  // ponytail: in-memory only — persist to vscode.getState() if users ask.
  let userClosed = false;
  let lastSeq = -Infinity;
  let idleTimer = null;

  closeBtn.addEventListener('click', () => {
    userClosed = true;
    panel.style.display = 'none';
  });

  // Drag-to-resize — same geometry as the workshop panel: the handle sits
  // on the LEFT edge, dragging left grows the panel.
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

  function showFrame(msg) {
    if (!msg || typeof msg.png_base64 !== 'string' || !msg.png_base64) return;
    const seq = typeof msg.seq === 'number' ? msg.seq : lastSeq + 1;
    if (seq <= lastSeq) return; // stale/out-of-order frame
    lastSeq = seq;

    img.src = 'data:image/png;base64,' + msg.png_base64;
    titleEl.textContent = msg.title || 'Browser';
    urlEl.textContent = msg.url ? '— ' + msg.url : '';
    header.title = (msg.title || '') + (msg.url ? '\n' + msg.url : '');

    if (!userClosed) panel.style.display = 'flex';

    img.classList.add('driving');
    if (idleTimer) clearTimeout(idleTimer);
    idleTimer = setTimeout(() => img.classList.remove('driving'), IDLE_MS);
  }

  return {
    el: panel,
    showFrame,
    hide() { panel.style.display = 'none'; },
  };
}
