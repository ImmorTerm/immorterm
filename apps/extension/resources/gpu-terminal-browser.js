// ── ImmorTerm Browser — Workshop surface ────────────────────────
// The self-driven browser (immorterm_browser_* CDP tools in the daemon)
// mirrored as a LIVE STREAM inside a REAL WORKSHOP card (not a floating
// modal). The workshop system (gpu-terminal.html renderWorkshop /
// syncWorkshopVisibility / requestCloseWorkshop) owns the chrome: the ⚒ tab,
// collapse (= minimize), × close, resize, and the sidebar entry — all for
// free, and all correctly scoped to the OWNING session so it never leaks onto
// other tabs. This module only supplies the surface HTML/CSS and a controller
// that drives that surface's shadow DOM per-frame.
//
// ── Message contract ────────────────────────────────────────────
// daemon → webview (RECEIVE): browser_frame {png_base64,title,url,seq},
//   browser_state {paused}, browser_human_request {reason,instructions},
//   browser_cursor {x,y,action}, browser_narration {text}.
// webview → daemon (SEND, best-effort): browser_input {kind:click|key|scroll|
//   resize, ...}, browser_control {action:pause|continue|close}.
//
// Coordinate mapping: the engine sets the page viewport to the workshop card's
// pixel size (via the resize message), so the page fills the card at its own
// aspect ratio — no letterbox (frame px == card px == page CSS px, ~1:1).
// mapToPageCss() still handles any transient AR mismatch before a resize lands.
//
// The frame data is JPEG (engine streams jpeg q75); showFrame sniffs the base64
// magic bytes for the MIME so JPEG/PNG both render.

'use strict';

export const BROWSER_WORKSHOP_NAME = '🦕 Browser';

const IDLE_MS = 3000; // "Mort is driving" pulse fades after this much frame silence

// ── Surface CSS (lives inside the workshop's Shadow DOM) ─────────
// No modal wrapper, no minimize/close/border chrome — the workshop card
// provides all of that. Themed via the terminal's --theme/--sidebar accent
// tokens (they pierce the shadow boundary as inherited custom properties).
export function browserWorkshopCss() {
  return `
:host, .bw-root { height: 100%; }
/* The renderWorkshop wrapper is content-sized with 16px padding by default;
   the browser must fill the card, so neutralize both here (this CSS is injected
   into the workshop's shadow root, so .ai-html-content is in scope). */
.ai-html-content { height: 100%; padding: 0; }
.bw-root {
  display: flex; flex-direction: column;
  height: 100%; min-height: 0;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
  color: var(--sidebar-text, #cdd6f4);
  background: #11111b;
}
/* Header — Mort (the driver) + state label + address pill + FPS HUD. */
.bw-header {
  display: flex; align-items: center; gap: 8px;
  padding: 6px 10px;
  flex-shrink: 0;
  font-size: 11px;
  border-bottom: 1px solid var(--sidebar-border, #313244);
  background: var(--sidebar-bg, #181825);
}
.bw-mort {
  /* Transparent sticker cutout, 256x168 (wider than tall — frills spread
     horizontally), so contain (not cover/circle) keeps him un-cropped. */
  flex-shrink: 0; width: 30px; height: 22px;
  object-fit: contain; display: block;
}
.bw-driver {
  flex-shrink: 0;
  font-size: 10px; font-weight: 600; letter-spacing: 0.01em;
  white-space: nowrap;
  color: var(--sidebar-accent, #b482ff);
  text-shadow: 0 0 8px var(--badge-glow-soft, rgba(180, 130, 255, 0.35));
}
.bw-root.paused .bw-driver {
  color: #f9e2af;
  text-shadow: 0 0 8px rgba(249, 226, 175, 0.4);
}
.bw-url {
  flex: 1; min-width: 40px;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  font-size: 10px; opacity: 0.85;
  padding: 3px 9px; border-radius: 999px;
  border: 1px solid var(--badge-glow-border, rgba(180, 130, 255, 0.3));
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 7%, transparent);
  box-shadow: inset 0 0 6px var(--badge-glow-soft, rgba(180, 130, 255, 0.08));
}
.bw-fps {
  flex-shrink: 0;
  font-size: 9px; font-variant-numeric: tabular-nums; font-weight: 600;
  letter-spacing: 0.02em; white-space: nowrap;
  color: var(--sidebar-accent, #b482ff); opacity: 0.75;
  padding: 2px 6px; border-radius: 999px;
  border: 1px solid var(--badge-glow-border, rgba(180, 130, 255, 0.28));
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 8%, transparent);
  box-shadow: 0 0 6px var(--badge-glow-soft, rgba(180, 130, 255, 0.12));
}
.bw-toggle {
  flex-shrink: 0;
  background: transparent;
  border: 1px solid var(--sidebar-border, #313244);
  color: var(--sidebar-text, #cdd6f4);
  font-size: 10px; font-weight: 600; line-height: 1;
  cursor: pointer; opacity: 0.85;
  padding: 3px 8px; border-radius: 5px; white-space: nowrap;
  transition: background 120ms ease, opacity 120ms ease, border-color 120ms ease;
}
.bw-toggle:hover { opacity: 1; }
.bw-root.paused .bw-toggle {
  border-color: #f9e2af; color: #f9e2af;
  background: color-mix(in srgb, #f9e2af 12%, transparent);
}
/* Handoff / pause banner. */
.bw-banner {
  display: none; flex-shrink: 0;
  padding: 8px 12px; font-size: 11px; line-height: 1.4;
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 16%, var(--sidebar-bg, #181825));
  border-bottom: 1px solid var(--sidebar-accent, #b482ff);
}
.bw-banner.on { display: block; }
.bw-banner-title { font-weight: 700; }
.bw-banner-reason { opacity: 0.85; margin-top: 2px; }
.bw-continue {
  margin-top: 7px;
  background: var(--sidebar-accent, #b482ff);
  border: 0; color: #11111b;
  font-size: 11px; font-weight: 700; line-height: 1;
  cursor: pointer; padding: 6px 12px; border-radius: 5px;
}
.bw-continue:hover { filter: brightness(1.08); }
/* Frame area — fills the card; the img fills the area with matched AR. */
.bw-body {
  flex: 1; min-height: 0;
  position: relative;
  background: #11111b;
  overflow: hidden;
}
.bw-body img.bw-frame {
  width: 100%; height: 100%;
  object-fit: fill; /* AR is matched via set_viewport → no distortion, no letterbox */
  display: block;
  outline: none;
}
.bw-root:not(.paused) .bw-frame { cursor: crosshair; }
.bw-root.paused .bw-frame { cursor: crosshair; }
/* Mort cursor overlay. */
.bw-mort-cursor {
  position: absolute; top: 0; left: 0;
  width: 34px; height: 34px;
  pointer-events: none; z-index: 6; opacity: 0;
  transform: translate(-100px, -100px);
  transition: transform 380ms cubic-bezier(0.22, 1, 0.36, 1), opacity 200ms ease;
  filter: drop-shadow(0 2px 3px rgba(0,0,0,0.45));
  will-change: transform, opacity;
}
.bw-mort-cursor.on { opacity: 1; }
.bw-mort-cursor.squish { animation: bw-squish 320ms ease; }
.bw-mort-cursor.pulse { animation: bw-pulse 300ms ease; }
@keyframes bw-squish { 0%{scale:1 1;} 40%{scale:1.18 0.82;} 70%{scale:0.92 1.08;} 100%{scale:1 1;} }
@keyframes bw-pulse { 0%,100%{scale:1;} 50%{scale:1.14;} }
.bw-ripple {
  position: absolute; pointer-events: none; z-index: 6;
  width: 14px; height: 14px; margin: -7px 0 0 -7px;
  border-radius: 50%;
  border: 2px solid var(--sidebar-accent, #b482ff);
  opacity: 0.9; animation: bw-ripple 480ms ease-out forwards;
}
@keyframes bw-ripple { to { transform: scale(4.5); opacity: 0; } }
.bw-balloons {
  position: absolute; right: 12px; bottom: 12px; z-index: 7;
  display: flex; flex-direction: column; align-items: flex-end; gap: 6px;
  pointer-events: none; max-width: 78%;
}
.bw-balloon {
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 22%, var(--sidebar-bg, #181825));
  border: 1px solid var(--sidebar-accent, #b482ff);
  color: var(--sidebar-text, #cdd6f4);
  font-size: 11px; line-height: 1.3;
  padding: 6px 11px; border-radius: 12px 12px 3px 12px;
  box-shadow: 0 2px 8px rgba(0,0,0,0.35);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 100%;
  animation: bw-balloon-in 260ms cubic-bezier(0.22, 1, 0.36, 1);
}
.bw-balloon.out { animation: bw-balloon-out 340ms ease forwards; }
@keyframes bw-balloon-in { from{opacity:0;transform:translateY(8px) scale(0.96);} to{opacity:1;transform:translateY(0) scale(1);} }
@keyframes bw-balloon-out { to { opacity: 0; transform: translateX(12px); } }
`;
}

// The Mort cursor mascot (kept as inline art — separate surface from the header
// avatar PNG). Solid axolotl, nose = cursor hotspot.
const MORT_CURSOR_SVG = `<svg viewBox="0 0 64 64" width="34" height="34" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <g stroke="#FF7E9D" stroke-width="3" stroke-linecap="round" fill="none">
    <path d="M20 20 C10 16 6 18 4 14"/><path d="M20 26 C9 25 5 27 3 24"/><path d="M21 32 C10 34 6 37 5 34"/>
    <path d="M44 20 C54 16 58 18 60 14"/><path d="M44 26 C55 25 59 27 61 24"/><path d="M43 32 C54 34 58 37 59 34"/>
  </g>
  <g fill="#FFB3AE"><circle cx="5" cy="13" r="3"/><circle cx="4" cy="24" r="3"/><circle cx="6" cy="35" r="3"/><circle cx="59" cy="13" r="3"/><circle cx="60" cy="24" r="3"/><circle cx="58" cy="35" r="3"/></g>
  <g fill="#FF9E8A"><ellipse cx="22" cy="52" rx="5" ry="7"/><ellipse cx="42" cy="52" rx="5" ry="7"/></g>
  <path d="M32 12 C18 12 14 26 14 36 C14 50 22 56 32 56 C42 56 50 50 50 36 C50 26 46 12 32 12 Z" fill="#FF9E8A"/>
  <circle cx="25" cy="32" r="3" fill="#2b2b3a"/><circle cx="39" cy="32" r="3" fill="#2b2b3a"/>
  <circle cx="26" cy="31" r="1" fill="#fff"/><circle cx="40" cy="31" r="1" fill="#fff"/>
  <path d="M27 42 Q32 47 37 42" stroke="#2b2b3a" stroke-width="2" stroke-linecap="round" fill="none"/>
</svg>`;

// ── Surface HTML ────────────────────────────────────────────────
// `mortAvatarUrl` = webview-resolvable URL for the sticker PNG (host passes it).
export function browserWorkshopHtml(mortAvatarUrl) {
  const avatar = mortAvatarUrl
    ? `<img class="bw-mort" src="${mortAvatarUrl}" alt="Mort">`
    : `<span class="bw-mort"></span>`;
  return `
<div class="bw-root">
  <div class="bw-header">
    ${avatar}
    <span class="bw-driver">mort's driving</span>
    <span class="bw-url"></span>
    <span class="bw-fps"></span>
    <button class="bw-toggle" type="button">⏸ Pause</button>
  </div>
  <div class="bw-banner">
    <div class="bw-banner-title"></div>
    <div class="bw-banner-reason"></div>
    <button class="bw-continue" type="button">✓ Done — continue</button>
  </div>
  <div class="bw-body">
    <img class="bw-frame" tabindex="0" draggable="false" alt="Self-driven browser">
    <div class="bw-mort-cursor">${MORT_CURSOR_SVG}</div>
    <div class="bw-balloons"></div>
  </div>
</div>`;
}

// ── Controller ──────────────────────────────────────────────────
// Wires input capture + resize reporting on the workshop card's shadow DOM and
// returns per-message appliers the host calls from the browser_* WS handlers.
//
// @param shadow  the workshop card's ShadowRoot (card.shadowRoot)
// @param send    (msg)=>void — routes browser_input/browser_control to the
//                OWNING session's daemon WS (host supplies; wrapped best-effort)
// Returns { showFrame, setState, humanRequest, cursorMove, narrate, destroy }.
export function createBrowserController({ shadow, send }) {
  const emit = (msg) => { try { if (send) send(msg); } catch { /* best-effort */ } };
  const q = (sel) => shadow.querySelector(sel);

  const root = q('.bw-root');
  const img = q('.bw-frame');
  const bodyEl = q('.bw-body');
  const driverEl = q('.bw-driver');
  const urlEl = q('.bw-url');
  const fpsEl = q('.bw-fps');
  const toggleBtn = q('.bw-toggle');
  const banner = q('.bw-banner');
  const bannerTitle = q('.bw-banner-title');
  const bannerReason = q('.bw-banner-reason');
  const continueBtn = q('.bw-continue');
  const mort = q('.bw-mort-cursor');
  const balloons = q('.bw-balloons');

  let lastSeq = -Infinity;
  let idleTimer = null;
  let paused = false;

  // ── Pause / continue ──────────────────────────────────────────
  function applyPaused(next) {
    paused = !!next;
    root.classList.toggle('paused', paused);
    toggleBtn.textContent = paused ? '▶ Continue' : '⏸ Pause';
    toggleBtn.title = paused ? 'Resume the AI' : 'Pause the AI and take over';
    driverEl.textContent = paused ? "you're driving" : "mort's driving";
    if (paused) {
      if (!banner.dataset.request) showPauseBanner();
      mort.classList.remove('on');
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

  // ── Input forwarding ──────────────────────────────────────────
  // Map a pointer event on the frame img back to page CSS px. CDP dispatches in
  // CSS px (the layout viewport), which the engine pins to this panel's CSS
  // size (via the resize message). With object-fit:fill the img box == the CSS
  // viewport, so the CSS-px offset inside the box IS the page CSS coordinate —
  // measured off the box rect, so it's independent of the frame's pixel density.
  function mapToPageCss(ev) {
    const rect = img.getBoundingClientRect();
    if (!rect.width || !rect.height) return null;
    const px = ev.clientX - rect.left;
    const py = ev.clientY - rect.top;
    if (px < 0 || py < 0 || px > rect.width || py > rect.height) return null;
    return { x: Math.round(px), y: Math.round(py) };
  }

  img.addEventListener('click', (ev) => {
    const p = mapToPageCss(ev);
    if (!p) return;
    if (paused) img.focus();
    emit({ type: 'browser_input', kind: 'click', x: p.x, y: p.y });
  });
  img.addEventListener('keydown', (ev) => {
    if (!paused) return;
    if (ev.key === 'Escape') { img.blur(); return; }
    // Let the browser's native paste/copy events fire (handled below) instead
    // of forwarding a bare 'v'/'c' keystroke — clipboardData only reaches us
    // via the paste/copy events under the user gesture.
    const accel = ev.metaKey || ev.ctrlKey;
    if (accel && (ev.key === 'v' || ev.key === 'c')) return;
    ev.preventDefault();
    emit({ type: 'browser_input', kind: 'key', key: ev.key });
  });
  // Paste: read text from the paste event's clipboardData (works under the user
  // gesture; navigator.clipboard.readText is blocked in webviews). preventDefault
  // + stopPropagation so the terminal's own Cmd+V handler doesn't also fire.
  img.addEventListener('paste', (ev) => {
    if (!paused) return;
    ev.preventDefault();
    ev.stopPropagation();
    const text = ev.clipboardData && ev.clipboardData.getData('text');
    if (text) emit({ type: 'browser_input', kind: 'paste', text });
  });
  // Copy: ask the daemon for the page selection; it replies over the control
  // channel and the host writes it to the clipboard (see applyCopy below).
  img.addEventListener('copy', (ev) => {
    if (!paused) return;
    ev.preventDefault();
    ev.stopPropagation();
    emit({ type: 'browser_input', kind: 'copy' });
  });
  bodyEl.addEventListener('wheel', (ev) => {
    if (!paused) return;
    ev.preventDefault();
    emit({ type: 'browser_input', kind: 'scroll', dy: ev.deltaY });
  }, { passive: false });

  // ── Report card size → daemon → engine viewport (fill, no letterbox) ──
  let lastW = 0, lastH = 0, sizeTimer = null, ro = null;
  function reportSize() {
    const r = bodyEl.getBoundingClientRect();
    const w = Math.round(r.width), h = Math.round(r.height);
    if (w < 50 || h < 50) return;
    if (w === lastW && h === lastH) return;
    lastW = w; lastH = h;
    emit({ type: 'browser_input', kind: 'resize', width: w, height: h });
  }
  function reportSizeDebounced() {
    if (sizeTimer) clearTimeout(sizeTimer);
    sizeTimer = setTimeout(reportSize, 200);
  }
  if (typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(reportSizeDebounced);
    ro.observe(bodyEl);
  }
  // Fire an initial measurement once layout settles.
  setTimeout(reportSize, 60);

  // ── FPS: rolling 1s window of delivered frames ────────────────
  const frameTimes = [];
  function tickFps() {
    const now = performance.now();
    frameTimes.push(now);
    while (frameTimes.length && now - frameTimes[0] > 1000) frameTimes.shift();
    fpsEl.textContent = frameTimes.length + ' fps';
  }
  function frameMime(b64) {
    return b64.startsWith('/9j/') ? 'image/jpeg' : 'image/png';
  }

  // ── Inbound appliers (host calls these from the WS handlers) ──
  function showFrame(msg) {
    if (!msg || typeof msg.png_base64 !== 'string' || !msg.png_base64) return;
    const seq = typeof msg.seq === 'number' ? msg.seq : lastSeq + 1;
    if (seq <= lastSeq) return;
    lastSeq = seq;
    img.src = `data:${frameMime(msg.png_base64)};base64,` + msg.png_base64;
    urlEl.textContent = msg.url || '';
    urlEl.title = (msg.title || '') + (msg.url ? '\n' + msg.url : '');
    tickFps();
    if (!paused) {
      if (idleTimer) clearTimeout(idleTimer);
      idleTimer = setTimeout(() => {}, IDLE_MS);
    }
  }
  function setState(msg) {
    if (!msg || typeof msg.paused !== 'boolean') return;
    if (msg.paused !== paused) applyPaused(msg.paused);
  }
  function humanRequest(msg) {
    if (!msg) return;
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
    paused = true;
    root.classList.add('paused');
    toggleBtn.textContent = '▶ Continue';
    driverEl.textContent = "you're driving";
    img.focus();
  }

  // ── Mort cursor ───────────────────────────────────────────────
  // Daemon cursor coords are page CSS px. The img box == the CSS viewport
  // (viewport pinned to the box CSS size), so page CSS px maps to box-relative
  // px 1:1 — measured off the box rect, independent of frame pixel density.
  function mapPageToPanel(x, y) {
    const ir = img.getBoundingClientRect();
    const br = bodyEl.getBoundingClientRect();
    if (!ir.width || !ir.height) return null;
    return { x: (ir.left - br.left) + x, y: (ir.top - br.top) + y };
  }
  const HOT_X = 8, HOT_Y = 6;
  function moveMortTo(px, py) {
    mort.classList.add('on');
    mort.style.transform = `translate(${px - HOT_X}px, ${py - HOT_Y}px)`;
  }
  function flashMort(cls) {
    mort.classList.remove(cls); void mort.offsetWidth; mort.classList.add(cls);
  }
  function ripple(px, py) {
    const r = document.createElement('div');
    r.className = 'bw-ripple';
    r.style.left = px + 'px'; r.style.top = py + 'px';
    bodyEl.appendChild(r);
    r.addEventListener('animationend', () => r.remove());
  }
  function cursorMove(msg) {
    if (!msg || typeof msg.x !== 'number' || typeof msg.y !== 'number') return;
    if (paused) { mort.classList.remove('on'); return; }
    const p = mapPageToPanel(msg.x, msg.y);
    if (!p) return;
    moveMortTo(p.x, p.y);
    const action = msg.action || 'move';
    if (action === 'click') {
      setTimeout(() => { flashMort('squish'); ripple(p.x, p.y); }, 360);
    } else if (action === 'type' || action === 'scroll') {
      setTimeout(() => flashMort('pulse'), 360);
    }
  }

  // ── Intent balloons ───────────────────────────────────────────
  const BALLOON_MS = 3500, BALLOON_MAX = 3;
  function narrate(msg) {
    const text = msg && typeof msg.text === 'string' ? msg.text.trim() : '';
    if (!text) return;
    const chip = document.createElement('div');
    chip.className = 'bw-balloon';
    chip.textContent = text; chip.title = text;
    balloons.appendChild(chip);
    while (balloons.children.length > BALLOON_MAX) balloons.firstChild.remove();
    setTimeout(() => {
      chip.classList.add('out');
      chip.addEventListener('animationend', () => chip.remove(), { once: true });
    }, BALLOON_MS);
  }

  // Daemon replied with the page's current selection → write to the OS
  // clipboard. Prefer the async Clipboard API; fall back to execCommand for
  // webviews where writeText is blocked outside a user gesture.
  function applyCopy(msg) {
    const text = msg && typeof msg.text === 'string' ? msg.text : '';
    if (!text) return;
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).catch(() => execCopyFallback(text));
    } else {
      execCopyFallback(text);
    }
  }
  function execCopyFallback(text) {
    try {
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.cssText = 'position:fixed;opacity:0;pointer-events:none';
      document.body.appendChild(ta);
      ta.select();
      document.execCommand('copy');
      ta.remove();
    } catch { /* best-effort */ }
  }

  function destroy() {
    if (ro) ro.disconnect();
    if (sizeTimer) clearTimeout(sizeTimer);
    if (idleTimer) clearTimeout(idleTimer);
  }

  return { showFrame, setState, humanRequest, cursorMove, narrate, applyCopy, destroy, isPaused: () => paused };
}
