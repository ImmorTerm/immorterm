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
//   {type:"browser_cursor", x, y, action:"move"|"click"|"type"|"scroll"}
//     The AI's virtual cursor at page CSS px. Glides a little Mort (coral
//     axolotl) mascot to the mapped point; click → tap-ripple + squish,
//     type/scroll → subtle pulse. Fields may be absent on older daemons →
//     no-op. See cursorMove()/mapPageToPanel().
//   {type:"browser_narration", text}
//     Short intent string ("Clicking Sign in…"). Pushes a bottom-right
//     speech-bubble chip that fades after a few seconds. See narrate().
//
// webview → daemon (SEND — best-effort; the daemon may not handle these on
// older builds. Sends must never throw if unacknowledged):
//   {type:"browser_input", kind:"click", x, y}   // x,y = page CSS px
//   {type:"browser_input", kind:"key", key}      // while pane focused
//   {type:"browser_input", kind:"scroll", dy}
//   {type:"browser_input", kind:"resize", width, height}  // panel px → viewport
//   {type:"browser_control", action:"pause"}
//   {type:"browser_control", action:"continue"}
//   {type:"browser_control", action:"close"}     // stop live view, tab stays
//
// Coordinate mapping: the engine sets the page viewport to the panel's actual
// pixel size (via the resize message), so the page fills the panel at its own
// aspect ratio — no letterbox in the common case (frame px == panel px == page
// CSS px, mapping ~1:1). mapToPageCss() still handles any transient AR mismatch
// (object-fit:contain) before a resize round-trip lands.
//
// Pure factory module — no host globals. Loaded via dynamic import from
// gpu-terminal.html (wasm-init), same pattern as gpu-terminal-files.js.
// The Tauri app shares this file via the apps/immorterm-app/dist symlink
// to apps/extension/resources.

'use strict';

const IDLE_MS = 3000; // "Claude is driving" pulse fades after this much frame silence

const PANEL_CSS = `
/* Floating OVERLAY over the terminal — never a flex sibling, so the terminal
   keeps its full width behind it. Mirrors the fs-browser reveal overlay:
   position:absolute inside #terminal-area (position:relative), high z-index,
   drop shadow. Anchored to the right, ~55% wide, full height. */
#browser-panel {
  position: absolute;
  top: 0; right: 0; bottom: 0;
  width: 55%;
  min-width: 320px;
  max-width: 90%;
  background: var(--sidebar-bg, #181825);
  /* Themed edge + accent glow: reads as part of the terminal, not a foreign
     modal. Mirrors the status bar's border+glow language (--theme/--sidebar
     accent tokens). */
  border-left: 1px solid var(--theme-4, var(--sidebar-accent, #b482ff));
  display: flex; flex-direction: column;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
  color: var(--sidebar-text, #cdd6f4);
  overflow: hidden;
  z-index: 60;
  box-shadow: -6px 0 18px rgba(0, 0, 0, 0.4),
              -1px 0 14px var(--badge-glow-soft, rgba(180, 130, 255, 0.14));
}
/* Minimized: collapse to just the header pill; the terminal is fully visible.
   The browser + pump stay alive; the header click restores it. */
#browser-panel.minimized {
  width: auto; min-width: 0; max-width: none;
  left: auto; right: 12px; top: 12px; bottom: auto;
  border: 1px solid var(--sidebar-border, #313244);
  border-radius: 8px;
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.4);
  cursor: pointer;
}
#browser-panel.minimized #browser-panel-resize,
#browser-panel.minimized #browser-panel-banner,
#browser-panel.minimized #browser-panel-body { display: none; }
#browser-panel.minimized #browser-panel-header { border-bottom: 0; }
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
  display: flex; align-items: center; gap: 7px;
  padding: 6px 8px 6px 10px;
  border-bottom: 1px solid var(--sidebar-border, #313244);
  flex-shrink: 0;
  font-size: 11px;
  min-width: 0;
}
/* Mort = the driver. Solid axolotl avatar (no glow-aura, no float — brand
   NEVER-HAS), sits left of a lowercase state label. */
#browser-panel-header .bp-mort {
  flex-shrink: 0;
  width: 22px; height: 22px;
  display: flex; align-items: center; justify-content: center;
}
#browser-panel-header .bp-mort svg { width: 22px; height: 22px; display: block; }
/* State label: "mort's driving" (self-driven) / "you're driving" (handoff).
   Lowercase deadpan — brand voice. Accent-glows when Mort drives. */
#browser-panel-header .bp-driver {
  flex-shrink: 0;
  font-size: 10px; font-weight: 600; letter-spacing: 0.01em;
  white-space: nowrap;
  color: var(--sidebar-accent, #b482ff);
  text-shadow: 0 0 8px var(--badge-glow-soft, rgba(180, 130, 255, 0.35));
}
#browser-panel.paused #browser-panel-header .bp-driver {
  color: #f9e2af; /* handoff amber — matches the paused frame border */
  text-shadow: 0 0 8px rgba(249, 226, 175, 0.4);
}
#browser-panel-header .bp-title {
  font-weight: 600;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  flex-shrink: 1; min-width: 0; opacity: 0.85;
}
/* URL = a themed PILL, not a boxy input: rounded, accent border, subtle glow. */
#browser-panel-header .bp-url {
  flex: 1; min-width: 40px;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  font-size: 10px; opacity: 0.8;
  padding: 3px 9px; border-radius: 999px;
  border: 1px solid var(--badge-glow-border, rgba(180, 130, 255, 0.3));
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 7%, transparent);
  box-shadow: inset 0 0 6px var(--badge-glow-soft, rgba(180, 130, 255, 0.08));
  transition: border-color 140ms ease, box-shadow 140ms ease;
}
#browser-panel-header .bp-url:hover {
  border-color: var(--badge-glow-border-bright, rgba(224, 176, 255, 0.5));
  box-shadow: inset 0 0 6px var(--badge-glow-soft, rgba(180, 130, 255, 0.12)),
              0 0 10px var(--badge-glow-soft, rgba(180, 130, 255, 0.15));
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
  color: var(--sidebar-accent, #b482ff);
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 14%, transparent);
  box-shadow: 0 0 8px var(--badge-glow-soft, rgba(180, 130, 255, 0.25));
}
/* Minimize button — same chrome as the workshop tools' header buttons, themed. */
#browser-panel-header .bp-min {
  flex-shrink: 0;
  background: transparent; border: 0;
  color: var(--sidebar-text, #cdd6f4);
  font-size: 14px; line-height: 1;
  cursor: pointer; opacity: 0.6;
  padding: 2px 6px; border-radius: 4px;
  transition: background 120ms ease, box-shadow 120ms ease, opacity 120ms ease;
}
#browser-panel-header .bp-min:hover {
  opacity: 1;
  color: var(--sidebar-accent, #b482ff);
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 14%, transparent);
  box-shadow: 0 0 8px var(--badge-glow-soft, rgba(180, 130, 255, 0.25));
}
/* Live delivered-FPS: a tiny HUD stat — muted accent that glows, not debug text. */
#browser-panel-header .bp-fps {
  flex-shrink: 0;
  font-size: 9px; font-variant-numeric: tabular-nums; font-weight: 600;
  letter-spacing: 0.02em; white-space: nowrap;
  color: var(--sidebar-accent, #b482ff); opacity: 0.75;
  padding: 2px 6px; border-radius: 999px;
  border: 1px solid var(--badge-glow-border, rgba(180, 130, 255, 0.28));
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 8%, transparent);
  box-shadow: 0 0 6px var(--badge-glow-soft, rgba(180, 130, 255, 0.12));
}
/* When minimized, show a 'restore' affordance on the header. */
#browser-panel.minimized #browser-panel-header .bp-min { transform: rotate(180deg); }
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
/* ── Mort cursor: the AI's virtual pointer, a little coral axolotl. ──
   Hotspot is its nose (top-left area); we offset so the nose sits on the
   click point. Glides via a transform transition; hidden until first event. */
#browser-mort-cursor {
  position: absolute;
  top: 0; left: 0;
  width: 34px; height: 34px;
  pointer-events: none;
  z-index: 6;
  opacity: 0;
  transform: translate(-100px, -100px);
  transition: transform 380ms cubic-bezier(0.22, 1, 0.36, 1), opacity 200ms ease;
  filter: drop-shadow(0 2px 3px rgba(0,0,0,0.45));
  will-change: transform, opacity;
}
#browser-mort-cursor.on { opacity: 1; }
#browser-mort-cursor.squish { animation: bp-mort-squish 320ms ease; }
#browser-mort-cursor.pulse { animation: bp-mort-pulse 300ms ease; }
@keyframes bp-mort-squish {
  0%   { scale: 1 1; }
  40%  { scale: 1.18 0.82; }
  70%  { scale: 0.92 1.08; }
  100% { scale: 1 1; }
}
@keyframes bp-mort-pulse {
  0%, 100% { scale: 1; }
  50%      { scale: 1.14; }
}
/* Tap-ripple: an expanding fading ring at the click point. */
.bp-ripple {
  position: absolute;
  pointer-events: none;
  z-index: 6;
  width: 14px; height: 14px;
  margin: -7px 0 0 -7px; /* center on point */
  border-radius: 50%;
  border: 2px solid var(--sidebar-accent, #b482ff);
  opacity: 0.9;
  animation: bp-ripple 480ms ease-out forwards;
}
@keyframes bp-ripple {
  to { transform: scale(4.5); opacity: 0; }
}
/* ── Intent balloons: bottom-right narration chips. ── */
#browser-balloons {
  position: absolute;
  right: 12px; bottom: 12px;
  z-index: 7;
  display: flex; flex-direction: column;
  align-items: flex-end; gap: 6px;
  pointer-events: none;
  max-width: 78%;
}
#browser-balloons .bp-balloon {
  background: color-mix(in srgb, var(--sidebar-accent, #b482ff) 22%, var(--sidebar-bg, #181825));
  border: 1px solid var(--sidebar-accent, #b482ff);
  color: var(--sidebar-text, #cdd6f4);
  font-size: 11px; line-height: 1.3;
  padding: 6px 11px;
  border-radius: 12px 12px 3px 12px; /* speech-bubble: notched bottom-right */
  box-shadow: 0 2px 8px rgba(0,0,0,0.35);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  max-width: 100%;
  animation: bp-balloon-in 260ms cubic-bezier(0.22, 1, 0.36, 1);
}
#browser-balloons .bp-balloon.out {
  animation: bp-balloon-out 340ms ease forwards;
}
@keyframes bp-balloon-in {
  from { opacity: 0; transform: translateY(8px) scale(0.96); }
  to   { opacity: 1; transform: translateY(0) scale(1); }
}
@keyframes bp-balloon-out {
  to { opacity: 0; transform: translateX(12px); }
}
`;

// Mort — ImmorTerm's coral axolotl mascot, minimal on-model rendering:
// rounded coral body, three external gill-stalks per side of the head (the
// axolotl signature), two dot eyes, a gentle smile, little legs. The nose
// (upper-left) is the cursor hotspot. NOT a robot/arrow/hand.
const MORT_SVG = `<svg viewBox="0 0 64 64" width="34" height="34" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <!-- gills: three stalks each side, coral-pink frilly tips -->
  <g stroke="#FF7E9D" stroke-width="3" stroke-linecap="round" fill="none">
    <path d="M20 20 C10 16 6 18 4 14"/>
    <path d="M20 26 C9 25 5 27 3 24"/>
    <path d="M21 32 C10 34 6 37 5 34"/>
    <path d="M44 20 C54 16 58 18 60 14"/>
    <path d="M44 26 C55 25 59 27 61 24"/>
    <path d="M43 32 C54 34 58 37 59 34"/>
  </g>
  <g fill="#FFB3AE"><circle cx="5" cy="13" r="3"/><circle cx="4" cy="24" r="3"/><circle cx="6" cy="35" r="3"/><circle cx="59" cy="13" r="3"/><circle cx="60" cy="24" r="3"/><circle cx="58" cy="35" r="3"/></g>
  <!-- little legs -->
  <g fill="#FF9E8A"><ellipse cx="22" cy="52" rx="5" ry="7"/><ellipse cx="42" cy="52" rx="5" ry="7"/></g>
  <!-- body/head -->
  <path d="M32 12 C18 12 14 26 14 36 C14 50 22 56 32 56 C42 56 50 50 50 36 C50 26 46 12 32 12 Z" fill="#FF9E8A"/>
  <!-- cheek blush -->
  <circle cx="22" cy="40" r="3.5" fill="#FF7E9D" opacity="0.55"/>
  <circle cx="42" cy="40" r="3.5" fill="#FF7E9D" opacity="0.55"/>
  <!-- eyes -->
  <circle cx="25" cy="32" r="3" fill="#2b2b3a"/>
  <circle cx="39" cy="32" r="3" fill="#2b2b3a"/>
  <circle cx="26" cy="31" r="1" fill="#fff"/>
  <circle cx="40" cy="31" r="1" fill="#fff"/>
  <!-- gentle smile -->
  <path d="M27 42 Q32 47 37 42" stroke="#2b2b3a" stroke-width="2" stroke-linecap="round" fill="none"/>
</svg>`;

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/**
 * Create the browser mirror panel. DOM + styles are built here and appended
 * into `container` — pass #terminal-area (position:relative) so the panel
 * (position:absolute) floats as an OVERLAY on top of the terminal and never
 * shrinks it. `beforeEl` is optional (legacy flex-sibling insertion point).
 *
 * @param {object} opts
 * @param {HTMLElement} opts.container  positioned ancestor (e.g. #terminal-area)
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
  // Mort is the driver — solid axolotl avatar (same art as the cursor) + a
  // lowercase deadpan state label that swaps mort's-driving / you're-driving.
  const mortAvatar = el('span', 'bp-mort');
  mortAvatar.innerHTML = MORT_SVG;
  const driverEl = el('span', 'bp-driver', "mort's driving");
  const titleEl = el('span', 'bp-title', 'Browser');
  const urlEl = el('span', 'bp-url', '');
  const fpsEl = el('span', 'bp-fps', ''); // live delivered-FPS HUD readout
  fpsEl.title = 'Live delivered frame rate';
  const toggleBtn = el('button', 'bp-toggle', '⏸ Pause'); // ⏸
  toggleBtn.title = 'Pause the AI and take over';
  const minBtn = el('button', 'bp-min', '–'); // en-dash "minimize"
  minBtn.title = 'Minimize (browser keeps running)';
  const closeBtn = el('button', 'bp-close', '×');
  closeBtn.title = 'Close panel & stop the live view (tab stays alive)';
  header.append(mortAvatar, driverEl, titleEl, urlEl, fpsEl, toggleBtn, minBtn, closeBtn);

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

  // Mort cursor + intent balloons — overlays layered over the frame, in the
  // same stacking context as the (later) handoff banner.
  const mort = el('div');
  mort.id = 'browser-mort-cursor';
  mort.innerHTML = MORT_SVG;
  const balloons = el('div');
  balloons.id = 'browser-balloons';
  bodyEl.append(mort, balloons);

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
    driverEl.textContent = paused ? "you're driving" : "mort's driving";
    if (paused) {
      // Only show the generic "you're driving" banner if a richer
      // human-request banner isn't already up.
      if (!banner.dataset.request) showPauseBanner();
    } else {
      hideBanner();
      img.blur();
    }
    // Paused = human drives; Mort idles out of the way (defined later, but
    // `mort` is in scope via closure).
    if (paused) mort.classList.remove('on');
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

  // ── Minimize / close (workshop-style chrome) ──────────────────
  let minimized = false;
  function setMinimized(next) {
    minimized = !!next;
    panel.classList.toggle('minimized', minimized);
    minBtn.title = minimized ? 'Restore' : 'Minimize (browser keeps running)';
    // Restoring re-syncs the viewport to the newly-expanded panel size.
    if (!minimized) reportSize();
  }
  minBtn.addEventListener('click', (ev) => {
    ev.stopPropagation();
    setMinimized(!minimized);
  });
  // Clicking the minimized pill restores it.
  header.addEventListener('click', (ev) => {
    if (minimized && ev.target !== closeBtn && ev.target !== minBtn) setMinimized(false);
  });

  closeBtn.addEventListener('click', (ev) => {
    ev.stopPropagation();
    userClosed = true;
    panel.style.display = 'none';
    // Close stops the live view (screencast) but leaves the tab alive; the
    // daemon reopens on the AI's next tool call or the human's next input.
    emit({ type: 'browser_control', action: 'close' });
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

  // ── Report panel size → daemon → engine viewport (fill, no letterbox) ──
  // The engine sets the page viewport to match, so the page fills the panel
  // at its aspect ratio. Debounced; skipped while minimized or zero-sized.
  let lastReportedW = 0, lastReportedH = 0, sizeTimer = null;
  function reportSize() {
    if (minimized) return;
    const r = bodyEl.getBoundingClientRect();
    const w = Math.round(r.width), h = Math.round(r.height);
    if (w < 50 || h < 50) return;
    if (w === lastReportedW && h === lastReportedH) return;
    lastReportedW = w; lastReportedH = h;
    emit({ type: 'browser_input', kind: 'resize', width: w, height: h });
  }
  function reportSizeDebounced() {
    if (sizeTimer) clearTimeout(sizeTimer);
    sizeTimer = setTimeout(reportSize, 200);
  }
  // Observe the body (the frame area) so drag-resize + window resize re-sync.
  if (typeof ResizeObserver !== 'undefined') {
    new ResizeObserver(reportSizeDebounced).observe(bodyEl);
  }

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

  // ── FPS: rolling 1s window of delivered frames ───────────────
  const frameTimes = [];
  function tickFps() {
    const now = performance.now();
    frameTimes.push(now);
    while (frameTimes.length && now - frameTimes[0] > 1000) frameTimes.shift();
    fpsEl.textContent = frameTimes.length + ' fps';
  }

  // MIME sniff from the base64 magic bytes: JPEG starts "/9j/", PNG "iVBOR".
  // The engine streams JPEG now; older daemons may still send PNG.
  function frameMime(b64) {
    return b64.startsWith('/9j/') ? 'image/jpeg' : 'image/png';
  }

  // Reveal the panel (unless the human closed it) and sync the viewport on the
  // transition from hidden → shown so the first frame is already panel-sized.
  function revealPanel() {
    if (userClosed) return;
    const firstShow = panel.style.display === 'none';
    panel.style.display = 'flex';
    if (firstShow) reportSize();
  }

  // ── Inbound messages ──────────────────────────────────────────
  function showFrame(msg) {
    if (!msg || typeof msg.png_base64 !== 'string' || !msg.png_base64) return;
    const seq = typeof msg.seq === 'number' ? msg.seq : lastSeq + 1;
    if (seq <= lastSeq) return; // stale/out-of-order frame — newest wins
    lastSeq = seq;

    img.src = `data:${frameMime(msg.png_base64)};base64,` + msg.png_base64;
    titleEl.textContent = msg.title || 'Browser';
    urlEl.textContent = msg.url ? '— ' + msg.url : '';
    header.title = (msg.title || '') + (msg.url ? '\n' + msg.url : '');
    tickFps();

    revealPanel();

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
    revealPanel();
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
    driverEl.textContent = "you're driving";
    img.classList.remove('driving');
    img.focus();
  }

  // ── Mort cursor ───────────────────────────────────────────────
  // Forward transform: page CSS px → panel-relative px within the letterboxed
  // <img>. Inverse of mapToPageCss (which goes displayed→page). Coords are
  // relative to bodyEl, the overlay's positioned ancestor.
  function mapPageToPanel(x, y) {
    const nw = img.naturalWidth, nh = img.naturalHeight;
    if (!nw || !nh) return null;
    const imgRect = img.getBoundingClientRect();
    const bodyRect = bodyEl.getBoundingClientRect();
    const scale = Math.min(imgRect.width / nw, imgRect.height / nh);
    const drawnW = nw * scale, drawnH = nh * scale;
    // Letterbox offset within the img, then img offset within bodyEl.
    const offX = (imgRect.left - bodyRect.left) + (imgRect.width - drawnW) / 2;
    const offY = (imgRect.top - bodyRect.top) + (imgRect.height - drawnH) / 2;
    return { x: offX + x * scale, y: offY + y * scale };
  }

  // Mort's nose hotspot sits ~upper-left; nudge so it lands on the point.
  const MORT_HOTSPOT_X = 8, MORT_HOTSPOT_Y = 6;
  function moveMortTo(px, py) {
    mort.classList.add('on');
    mort.style.transform =
      `translate(${px - MORT_HOTSPOT_X}px, ${py - MORT_HOTSPOT_Y}px)`;
  }
  function flashMort(cls) {
    mort.classList.remove(cls);
    void mort.offsetWidth; // restart the animation
    mort.classList.add(cls);
  }

  function ripple(px, py) {
    const r = el('div', 'bp-ripple');
    r.style.left = px + 'px';
    r.style.top = py + 'px';
    bodyEl.appendChild(r);
    r.addEventListener('animationend', () => r.remove());
  }

  // {type:"browser_cursor", x, y, action}. No-op if coords are missing or the
  // frame isn't laid out yet. Idle/hidden while paused (human is driving).
  function cursorMove(msg) {
    if (!msg || typeof msg.x !== 'number' || typeof msg.y !== 'number') return;
    // Any AI browser activity reveals the panel — Mort moving is proof the
    // browser is live even before the first screencast frame lands.
    revealPanel();
    if (paused) { mort.classList.remove('on'); return; }
    const p = mapPageToPanel(msg.x, msg.y);
    if (!p) return;
    moveMortTo(p.x, p.y);
    const action = msg.action || 'move';
    if (action === 'click') {
      // Squish + ripple land ~when Mort arrives (after the glide).
      const delay = 360;
      setTimeout(() => { flashMort('squish'); ripple(p.x, p.y); }, delay);
    } else if (action === 'type' || action === 'scroll') {
      setTimeout(() => flashMort('pulse'), 360);
    }
  }

  // ── Intent balloons ───────────────────────────────────────────
  const BALLOON_MS = 3500, BALLOON_MAX = 3;
  function narrate(msg) {
    const text = msg && typeof msg.text === 'string' ? msg.text.trim() : '';
    if (!text) return;
    // Narration fires on browser_open ("Opening …") before any frame, so this
    // is the reliable "open the panel" trigger the daemon guarantees per action.
    revealPanel();
    const chip = el('div', 'bp-balloon', text);
    chip.title = text;
    balloons.appendChild(chip);
    while (balloons.children.length > BALLOON_MAX) {
      balloons.firstChild.remove();
    }
    setTimeout(() => {
      chip.classList.add('out');
      chip.addEventListener('animationend', () => chip.remove(), { once: true });
    }, BALLOON_MS);
  }

  return {
    el: panel,
    showFrame,
    setState,
    humanRequest,
    cursorMove,
    narrate,
    hide() { panel.style.display = 'none'; },
  };
}
