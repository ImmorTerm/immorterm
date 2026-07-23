/**
 * GPU Terminal — Extracted utility functions.
 *
 * Pure functions + factory-pattern wrappers that allow dependency injection
 * for unit testing without a real VS Code webview or DOM.
 *
 * Imported by gpu-terminal.html via dynamic import (same pattern as WASM).
 */

// ── Pure Utilities ──────────────────────────────────────────────

/** Format kilobytes to human-readable (M/G). */
export function formatMemory(kb) {
  if (kb >= 1048576) return (kb / 1048576).toFixed(1) + 'G';
  return Math.round(kb / 1024) + 'M';
}

/** Format seconds to Xh Xm or Xm Xs. */
export function formatRuntime(secs) {
  if (secs >= 3600) {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return h + 'h' + (m > 0 ? m + 'm' : '');
  }
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return m + 'm' + (s > 0 ? s + 's' : '');
}

/** Build context progress bar: ▰▰▰▰▰▰▱▱▱▱ */
export function contextBar(pct) {
  const filled = Math.round((pct || 0) / 10);
  return '\u25B0'.repeat(filled) + '\u25B1'.repeat(10 - filled);
}

/** Parse "#RRGGBB" → {r, g, b} (0-255). */
export function hexToRgb(hex) {
  const n = parseInt(hex.slice(1), 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

/** Parse a CSS color value (#hex or rgb()) into [r, g, b] floats (0.0–1.0). */
export function parseCSSColor(value) {
  if (!value) return null;
  value = value.trim();
  if (!value) return null;
  if (value.startsWith('#')) {
    let hex = value.slice(1);
    if (hex.length === 3) hex = hex[0]+hex[0]+hex[1]+hex[1]+hex[2]+hex[2];
    if (hex.length === 8) hex = hex.slice(0, 6);
    if (hex.length !== 6) return null;
    return [
      parseInt(hex.slice(0,2), 16) / 255,
      parseInt(hex.slice(2,4), 16) / 255,
      parseInt(hex.slice(4,6), 16) / 255,
    ];
  }
  const m = value.match(/rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/);
  if (m) return [+m[1]/255, +m[2]/255, +m[3]/255];
  return null;
}

/** Parse a CSS color with alpha into [r, g, b, a] floats (0.0–1.0). */
export function parseCSSColorWithAlpha(value) {
  if (!value) return null;
  value = value.trim();
  if (!value) return null;
  if (value.startsWith('#')) {
    let hex = value.slice(1);
    if (hex.length === 3) hex = hex[0]+hex[0]+hex[1]+hex[1]+hex[2]+hex[2] + 'ff';
    if (hex.length === 6) hex = hex + 'ff';
    if (hex.length !== 8) return null;
    return [
      parseInt(hex.slice(0,2), 16) / 255,
      parseInt(hex.slice(2,4), 16) / 255,
      parseInt(hex.slice(4,6), 16) / 255,
      parseInt(hex.slice(6,8), 16) / 255,
    ];
  }
  const m = value.match(/rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)(?:\s*,\s*([\d.]+))?\s*\)/);
  if (m) return [+m[1]/255, +m[2]/255, +m[3]/255, m[4] != null ? +m[4] : 1.0];
  return null;
}

/**
 * Strip emojis from a string, replacing each with 2 spaces (double-width).
 * Returns { clean, emojis: [{ char, col }] }.
 */
export function stripEmojis(str) {
  const EMOJI_RE = /[\u{1F300}-\u{1FAFF}\u{2600}-\u{26FF}\u{2700}-\u{27BF}]/u;
  let clean = '';
  const emojis = [];
  let col = 0;
  for (const ch of str) {
    if (EMOJI_RE.test(ch)) {
      emojis.push({ char: ch, col });
      clean += '  ';
      col += 2;
    } else {
      clean += ch;
      col += 1;
    }
  }
  return { clean, emojis };
}

// ── DOM Utilities ───────────────────────────────────────────────

/** Create an element with optional class and text content. */
export function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** Clear all children of a parent element (safe — uses textContent). */
export function clearEl(parent) {
  parent.textContent = '';
}

// ── Factory: Error Toast ────────────────────────────────────────

/**
 * Create a reportError function with injected dependencies.
 *
 * @param {Object} opts
 * @param {Function} opts.postMessage - sends messages to extension (e.g. vscode.postMessage)
 * @param {Element} opts.appendTo - DOM element to append toast to (e.g. document.body)
 * @param {number} [opts.autoDismissMs=8000] - auto-dismiss timeout in ms
 * @returns {Function} reportError(source, err)
 */
export function createReportError({ postMessage, appendTo, autoDismissMs = 8000 }) {
  return function reportError(source, err) {
    const msg = `[${source}] ${err?.message || err}`;
    console.error(msg, err);
    try { postMessage({ type: 'error', message: msg, stack: err?.stack }); } catch(_) {}

    const toast = document.createElement('div');
    toast.style.cssText = 'position:fixed;bottom:8px;left:8px;right:8px;z-index:9999;'
      + 'background:#1e1e2e;color:#f38ba8;padding:10px 36px 10px 14px;border-radius:8px;'
      + 'font:12px/1.5 monospace;max-height:120px;overflow:auto;white-space:pre-wrap;'
      + 'border:1px solid #45475a;box-shadow:0 4px 12px rgba(0,0,0,0.4);'
      + 'animation:errorSlideUp .25s ease-out;';
    toast.textContent = msg;

    const closeBtn = document.createElement('button');
    closeBtn.textContent = '\u00d7';
    closeBtn.style.cssText = 'position:absolute;top:6px;right:8px;background:none;border:none;'
      + 'color:#6c7086;font-size:18px;cursor:pointer;padding:0 4px;line-height:1;';
    closeBtn.onmouseenter = () => closeBtn.style.color = '#cdd6f4';
    closeBtn.onmouseleave = () => closeBtn.style.color = '#6c7086';
    closeBtn.onclick = () => toast.remove();
    toast.appendChild(closeBtn);

    appendTo.appendChild(toast);
    setTimeout(() => { if (toast.parentNode) toast.remove(); }, autoDismissMs);
  };
}

// ── Factory: Sidebar Renderer ───────────────────────────────────

/**
 * Create a guarded renderSidebar function with injected dependencies.
 *
 * The reentrant guard prevents crashes when blur events fire during DOM
 * teardown (e.g. a rename input loses focus while renderSidebar clears
 * the session list).
 *
 * @param {Object} opts
 * @param {Element} opts.sessionListEl - the #session-list DOM element
 * @param {Map} opts.sessions - Map<name, session> of current sessions
 * @param {Function} opts.getActiveSessionName - returns current active session name
 * @param {Function} opts.onSwitch - callback(name) when user clicks a session
 * @param {Function} opts.onRename - callback(name, session, nameEl) for rename flow
 * @param {Function} opts.onRemove - callback(name) when user clicks close
 * @param {Function} opts.onContextMenu - callback(event, name) for right-click
 * @param {Function} [opts.isRenaming] - returns true when a rename input is active (blocks re-renders)
 * @returns {Function} renderSidebar()
 */
// ── SP2 Spaces: canonical persisted-record shape ──────────────────────
// Pure normalizer for one space record from index.json. Used on load AND
// covered by the toJSON→fromJSON binding+geometry+lock round-trip self-check
// (arch §8): `layout` is dockview's opaque geometry blob (split ratios), kept
// verbatim; `tiles` is our panelId→binding map; lock state is gridLocked +
// per-tile `locked`. Normalizing on the way in means a hand-edited or
// partial index can never crash the grid rebuild.
export function normalizeSpaceRecord(rec) {
  const r = rec || {};
  const tiles = {};
  const src = (r.tiles && typeof r.tiles === 'object') ? r.tiles : {};
  // sessionName from a persisted index is untrusted and reaches DOM/renderers —
  // bound it to session-id shape so a crafted index can't smuggle markup or
  // path tricks. Non-conforming → dropped (tile renders as a "gone" placeholder).
  const validName = (n) => (typeof n === 'string' && /^[a-zA-Z0-9._-]{1,128}$/.test(n)) ? n : undefined;
  for (const id in src) {
    const t = src[id] || {};
    tiles[id] = { kind: t.kind || 'session', sessionName: validName(t.sessionName), locked: !!t.locked };
  }
  return {
    name: r.name || 'Space',
    createdMs: r.createdMs || 0,
    layout: (r.layout !== undefined) ? r.layout : null,
    tiles,
    gridLocked: !!r.gridLocked,
  };
}

export function createRenderSidebar({
  sessionListEl,
  sessions,
  getActiveSessionName,
  onSwitch,
  onRename,
  onRemove,
  onContextMenu,
  isRenaming,
  getOrder,
  onReorder,
  onShareContext,
  onShareDragState,
  onShareModeSelect,
  getCharacterDefs,
}) {
  let _rendering = false;
  let _dragState = null; // { sessionName, startY, itemEl, dragging }
  const _seenTaskBadges = new Set(); // Track sessions whose badge arrival already played

  // Drop indicator — reused across drags
  const dropIndicator = document.createElement('div');
  dropIndicator.className = 'drop-indicator';
  dropIndicator.style.display = 'none';

  // Calculate drop index from cursor Y position
  function calcDropIndex(clientY) {
    const items = [...sessionListEl.querySelectorAll('.session-item')];
    for (let i = 0; i < items.length; i++) {
      const rect = items[i].getBoundingClientRect();
      if (clientY < rect.top + rect.height / 2) return i;
    }
    return items.length;
  }

  // Document-level drag handlers (attached once)
  let _listenersAttached = false;
  function attachDragListeners() {
    if (_listenersAttached) return;
    _listenersAttached = true;

    document.addEventListener('mousemove', (e) => {
      if (!_dragState) return;
      const dy = e.clientY - _dragState.startY;
      const dx = e.clientX - _dragState.startX;
      if (!_dragState.dragging && (Math.abs(dy) > 4 || Math.abs(dx) > 4)) {
        _dragState.dragging = true;
        _dragState.itemEl.classList.add('dragging');
      }
      if (!_dragState.dragging) return;

      // Check if cursor is outside the sidebar
      const sidebar = sessionListEl.closest('#sidebar');
      const sidebarRect = sidebar ? sidebar.getBoundingClientRect() : null;
      const outsideSidebar = sidebarRect && e.clientX < sidebarRect.left;

      // Always show drop zone in terminal area while dragging
      if (onShareDragState) onShareDragState(true, _dragState.sessionName);
      try { fetch('/api/dev-log', { method:'POST', headers:{'content-type':'application/json'}, body: JSON.stringify({level:'info', msg:'[dnd] dragging=' + _dragState.sessionName + ' cb=' + !!onShareDragState}) }); } catch(_) {}

      if (outsideSidebar) {
        // Outside sidebar → share mode: hide reorder indicator
        dropIndicator.style.display = 'none';
      } else {
        // Inside sidebar → reorder mode (drop zone still visible)
        dropIndicator.style.display = '';
        const items = [...sessionListEl.querySelectorAll('.session-item')];
        if (items.length === 0) return;
        const dropIndex = calcDropIndex(e.clientY);
        const listRect = sessionListEl.getBoundingClientRect();
        if (dropIndex < items.length) {
          const rect = items[dropIndex].getBoundingClientRect();
          dropIndicator.style.top = (rect.top - listRect.top - 1) + 'px';
        } else {
          const lastRect = items[items.length - 1].getBoundingClientRect();
          dropIndicator.style.top = (lastRect.bottom - listRect.top - 1) + 'px';
        }
      }
    });

    document.addEventListener('mouseup', (e) => {
      if (!_dragState) return;
      if (_dragState.dragging) {
        _dragState.itemEl.classList.remove('dragging');
        dropIndicator.style.display = 'none';
        if (onShareDragState) onShareDragState(false, null);

        // Check if dropped outside sidebar → share with active session
        const sidebar = sessionListEl.closest('#sidebar');
        const sidebarRect = sidebar ? sidebar.getBoundingClientRect() : null;
        const outsideSidebar = sidebarRect && e.clientX < sidebarRect.left;

        if (outsideSidebar) {
          const activeName = getActiveSessionName();
          if (activeName && activeName !== _dragState.sessionName) {
            // Always go directly to Static share — Interactive upgrade is on the badge
            if (onShareContext) {
              onShareContext(_dragState.sessionName, activeName);
            }
          }
        } else {
          // Reorder mode
          const order = getOrder ? getOrder() : [...sessions.keys()];
          const dropIndex = calcDropIndex(e.clientY);
          const dragIndex = order.indexOf(_dragState.sessionName);
          if (dragIndex !== -1 && dropIndex !== dragIndex && dropIndex !== dragIndex + 1) {
            const newOrder = [...order];
            newOrder.splice(dragIndex, 1);
            const insertAt = dropIndex > dragIndex ? dropIndex - 1 : dropIndex;
            newOrder.splice(insertAt, 0, _dragState.sessionName);
            if (onReorder) onReorder(newOrder);
          }
        }
      }
      _dragState = null;
    });
  }

  return function renderSidebar() {
    if (_rendering) return;
    if (isRenaming && isRenaming()) return; // Don't rebuild while user is typing a new name
    _rendering = true;

    // Clear — use textContent to avoid individual removeChild triggering blur events
    sessionListEl.textContent = '';

    // Ensure session-list has relative positioning for the drop indicator
    if (!sessionListEl.style.position) sessionListEl.style.position = 'relative';

    // Re-append drop indicator (cleared by textContent)
    sessionListEl.appendChild(dropIndicator);

    // Attach document-level drag listeners once
    attachDragListeners();

    const activeSessionName = getActiveSessionName();
    const order = getOrder ? getOrder() : [...sessions.keys()];

    for (const name of order) {
      const session = sessions.get(name);
      if (!session) continue;

      const item = document.createElement('div');
      item.className = 'session-item' + (name === activeSessionName ? ' active' : '');

      const dot = document.createElement('span');
      let dotClass = 'dot';
      if (!session.connected) dotClass += ' disconnected';
      else if (session.working) dotClass += ' working';
      dot.className = dotClass;
      // Preserve animation phase across DOM rebuilds: negative animation-delay
      // makes the new dot appear to have been running since workingSince. Without
      // this, the breathing restarts from frame 0 on every renderSidebar call.
      // Cycle = 2.8s (1.4s inhale + 1.4s exhale via animation-direction: alternate).
      if (session.working && session.workingSince) {
        const elapsedSec = (performance.now() - session.workingSince) / 1000;
        const delaySec = -(elapsedSec % 2.8);
        dot.style.animationDelay = delaySec.toFixed(3) + 's';
      }
      item.appendChild(dot);

      const nameEl = document.createElement('span');
      nameEl.className = 'name';
      nameEl.textContent = session.displayName || name;
      nameEl.title = name;
      item.appendChild(nameEl);

      // Title lock indicator \u2014 small right-aligned codicon (S5a L2)
      if (session.titleLocked) {
        const lock = document.createElement('span');
        lock.className = 'codicon codicon-lock lock-badge';
        lock.title = 'Title locked \u2014 Claude cannot rename this tab';
        item.appendChild(lock);
      }

      // Bell indicator (program needs attention)
      if (session.bell) {
        const bell = document.createElement('span');
        bell.className = 'codicon codicon-bell bell-badge';
        bell.title = 'Needs attention';
        item.appendChild(bell);
      }

      // Speak Mode badge — session-level character override
      // (shown only when session has an override AND it's non-default)
      if (session.speakMode && session.speakMode !== 'default') {
        const defs = typeof getCharacterDefs === 'function' ? getCharacterDefs() : null;
        const def = defs && defs[session.speakMode];
        if (def && def.emoji) {
          const speaker = document.createElement('span');
          speaker.className = 'speaker-badge';
          speaker.textContent = def.emoji;
          speaker.title = (def.label || session.speakMode) + ' — right-click to change';
          item.appendChild(speaker);
        }
      }

      // Drawing indicator
      if (session.aiPrimitiveCount > 0) {
        const badge = document.createElement('span');
        badge.className = 'drawing-badge';
        badge.textContent = '\u{1F58C}';
        badge.title = session.aiPrimitiveCount + ' drawing' + (session.aiPrimitiveCount === 1 ? '' : 's');
        item.appendChild(badge);
      }

      // Workshop indicator — persistent AI-authored panes attached to this
      // session. Shown alongside the drawing badge so devs see at a glance
      // which sessions have running workshop apps without having to switch.
      if (session.workshopCount > 0) {
        const badge = document.createElement('span');
        badge.className = 'workshop-badge';
        badge.textContent = '⚒';
        badge.title = session.workshopCount + ' workshop' + (session.workshopCount === 1 ? '' : 's') + ' open';
        item.appendChild(badge);
      }

      // Task badge — active tasks linked to this session
      if (session.taskCount > 0) {
        const badge = document.createElement('span');
        const badgeKey = session.name + ':' + session.taskCount;
        badge.className = _seenTaskBadges.has(badgeKey) ? 'task-badge task-badge-seen' : 'task-badge';
        _seenTaskBadges.add(badgeKey);
        badge.textContent = String(session.taskCount);
        badge.title = session.taskCount + ' active task' + (session.taskCount === 1 ? '' : 's');
        item.appendChild(badge);
      }

      const closeBtn = document.createElement('button');
      closeBtn.className = 'close';
      closeBtn.textContent = '\u00d7';
      closeBtn.title = 'Close session';
      closeBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        onRemove(name);
      });
      item.appendChild(closeBtn);

      item.dataset.name = name;
      item.addEventListener('click', () => {
        if (_dragState?.dragging) return; // Don't switch during drag
        onSwitch(name);
      });
      item.addEventListener('contextmenu', (e) => onContextMenu(e, name));

      nameEl.addEventListener('dblclick', (e) => {
        e.stopPropagation();
        onRename(name, session, nameEl);
      });

      // Drag-and-drop: initiate on mousedown
      item.addEventListener('mousedown', (e) => {
        if (e.button !== 0) return; // left button only
        if (e.target.closest('.close')) return;
        if (isRenaming && isRenaming()) return;
        _dragState = {
          sessionName: name,
          startX: e.clientX,
          startY: e.clientY,
          itemEl: item,
          dragging: false,
        };
      });

      sessionListEl.appendChild(item);
    }

    // Re-apply drag state to the new DOM element after rebuild
    if (_dragState && _dragState.dragging) {
      const draggedEl = sessionListEl.querySelector(`.session-item[data-name="${CSS.escape(_dragState.sessionName)}"]`);
      if (draggedEl) {
        draggedEl.classList.add('dragging');
        _dragState.itemEl = draggedEl;
      }
    }

    // Scroll active session into view (scrollIntoView may not exist in jsdom)
    const activeEl = sessionListEl.querySelector('.session-item.active');
    if (activeEl && activeEl.scrollIntoView) activeEl.scrollIntoView({ block: 'nearest' });

    _rendering = false;
  };
}
