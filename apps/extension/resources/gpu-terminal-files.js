// ── ImmorTerm File Browser ──────────────────────────────────────
// Left-side project file browser for the GPU terminal. Mirrors the
// sessions sidebar (right side): lazy tree, fuzzy filename search,
// content grep (`>` prefix), cmd-hover preview, drag-into-terminal.
//
// Pure factory module — no host globals. All host integration is
// injected, so the same file serves VS Code, Tauri, and bare web.
// Loaded via dynamic import from gpu-terminal.html (wasm-init).

'use strict';

import { iconForFile, iconForFolder, svgFor } from './gpu-terminal-file-icons.js';

// ── Small helpers ──────────────────────────────────────────────

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const IS_MAC = navigator.platform.toLowerCase().includes('mac');

// Indent geometry (px). Twisty column reserves space so file icons align
// with folder icons; guides are drawn one per ancestor level.
const FB_INDENT_STEP = 12;
const FB_INDENT_BASE = 6;
const DIR_RENDER_CAP = 200; // max rows rendered per folder before a "N more" affordance

// Parse each unique SVG string ONCE into a detached node, then cloneNode() per
// use. innerHTML re-invokes the HTML parser on every assignment — across the
// hundreds of rows a large folder expands to (each row builds an icon + a
// twisty), that parsing dominated render time (a 451-entry dir took ~3.2s).
// Cloning a cached node is an order of magnitude cheaper. The icon set is
// bounded (~30 file types + folder open/closed), so the cache stays tiny.
const _svgCache = new Map();
function svgNode(svg) {
  let tpl = _svgCache.get(svg);
  if (!tpl) {
    tpl = document.createElement('template');
    tpl.innerHTML = svg;
    _svgCache.set(svg, tpl);
  }
  return tpl.content.firstElementChild.cloneNode(true);
}

// Collapsed-chevron glyph — identical for every dir row, so it caches to one
// parse via svgNode(). The CSS rotates it when the row carries `.open`.
const FB_TWISTY_SVG = '<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path fill="currentColor" d="M5.7 13.7 5 13l5-5-5-5 .7-.7L11.4 8z"/></svg>';

// Build the icon element for an entry. Files keep Material's brand colors;
// folders are tinted to the theme accent (their SVGs use currentColor).
// Exported: it's a pure module-level helper, so other file-listing surfaces
// (e.g. the reveal-tree preview in gpu-terminal.html) reuse it via the module
// object — no factory instance required.
export function buildIconEl(name, kind, expanded) {
  const span = el('span', 'fb-icon');
  let iconName;
  if (kind === 'dir') {
    iconName = iconForFolder(name, !!expanded);
    span.classList.add('is-folder');
  } else {
    iconName = iconForFile(name);
  }
  span.appendChild(svgNode(svgFor(iconName)));
  return span;
}

/**
 * Make `rowEl` draggable into the terminal — the shared mouse-drag (no HTML5
 * dataTransfer, so it never engages tauri://drag-drop) used by BOTH the file
 * browser sidebar and the cmd-hover / file-link reveal-tree preview. Exported
 * so any file-listing surface reuses ONE implementation. Self-contained: it
 * lazily wires document mousemove/mouseup only for the duration of a drag, and
 * suppresses the click that would otherwise fire if a drag ends back on a row.
 *
 * @param {HTMLElement} rowEl
 * @param {{path:string, name:string, isDir:boolean}} item
 * @param {{onDragState:(over:boolean,label:string|null)=>void,
 *          onDropOnTerminal:(path:string,x:number,y:number)=>void}} cb
 */
export function dragItemToTerminal(rowEl, item, { onDragState, onDropOnTerminal }) {
  rowEl.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    // Suppress the browser's native text-selection-on-drag so dragging a row
    // MOVES the file instead of selecting its filename. This is how VS Code's
    // explorer behaves — tree rows aren't a text document; drag = move, click =
    // select/navigate, and copying a name is via the row context menu. The
    // click event still fires (preventDefault on mousedown doesn't cancel it),
    // so single-click select/navigate is unaffected.
    e.preventDefault();
    const startX = e.clientX;
    const startY = e.clientY;
    let dragging = false;
    let ghostEl = null;
    let prevBodyUserSelect = '';
    const onMove = (ev) => {
      if (!dragging && (Math.abs(ev.clientX - startX) > 4 || Math.abs(ev.clientY - startY) > 4)) {
        dragging = true;
        // Belt-and-suspenders: clear any selection that slipped through and
        // pin user-select off for the drag duration (the ghost sweeps over
        // selectable terminal/preview text on its way to the drop target).
        const sel = window.getSelection && window.getSelection();
        if (sel && sel.removeAllRanges) sel.removeAllRanges();
        prevBodyUserSelect = document.body.style.userSelect;
        document.body.style.userSelect = 'none';
        rowEl.classList.add('dragging');
        const ghost = el('div', 'fb-drag-ghost');
        ghost.appendChild(buildIconEl(item.name, item.isDir ? 'dir' : 'file', false));
        ghost.appendChild(el('span', null, item.name));
        ghost.style.pointerEvents = 'none';
        document.body.appendChild(ghost);
        ghostEl = ghost;
      }
      if (!dragging) return;
      ghostEl.style.left = (ev.clientX + 8) + 'px';
      ghostEl.style.top = (ev.clientY + 8) + 'px';
      const over = document.elementFromPoint(ev.clientX, ev.clientY);
      const overTerminal = !!(over && over.closest && over.closest('#terminal-area'));
      onDragState(overTerminal, overTerminal ? item.name : null);
    };
    const onUp = (ev) => {
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
      if (!dragging) return;
      document.body.style.userSelect = prevBodyUserSelect;
      rowEl.classList.remove('dragging');
      if (ghostEl) ghostEl.remove();
      onDragState(false, null);
      const over = document.elementFromPoint(ev.clientX, ev.clientY);
      if (over && over.closest && over.closest('#terminal-area')) {
        onDropOnTerminal(item.path, ev.clientX, ev.clientY);
      }
      // A drag that ends back on a row would otherwise fire a click and
      // navigate/preview — swallow it once (capture phase, before the row's
      // own click listener), and clean up if no click ever lands.
      const suppress = (ce) => { ce.stopPropagation(); ce.preventDefault(); };
      rowEl.addEventListener('click', suppress, { capture: true, once: true });
      setTimeout(() => rowEl.removeEventListener('click', suppress, { capture: true }), 120);
    };
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
  });
}

/**
 * Fuzzy subsequence scorer for filename search. Returns a score > 0
 * when every query char appears in order in `path`, else -1.
 * Bonuses: consecutive matches, word/segment boundaries, basename
 * matches; penalty for path depth so shallow files rank first.
 */
function fuzzyScore(query, path) {
  const q = query.toLowerCase();
  const p = path.toLowerCase();
  let qi = 0;
  let score = 0;
  let lastMatch = -2;
  const baseStart = p.lastIndexOf('/') + 1;
  for (let pi = 0; pi < p.length && qi < q.length; pi++) {
    if (p[pi] !== q[qi]) continue;
    score += 1;
    if (pi === lastMatch + 1) score += 4; // consecutive run
    const prev = pi > 0 ? p[pi - 1] : '/';
    if (prev === '/' || prev === '_' || prev === '-' || prev === '.') score += 6; // boundary
    if (pi >= baseStart) score += 3; // basename hit
    lastMatch = pi;
    qi++;
  }
  if (qi < q.length) return -1;
  // Shallow paths and short names win ties.
  score -= (path.split('/').length - 1) * 2;
  score -= Math.floor(path.length / 16);
  return score;
}

// ── Floating menu primitive ────────────────────────────────────
// One implementation behind both the row context menu and the
// drop-on-terminal chooser (gpu-terminal.html imports it too).

let _floatingMenu = null;

export function dismissFloatingMenu() {
  if (_floatingMenu) { _floatingMenu.remove(); _floatingMenu = null; }
  document.removeEventListener('mousedown', _onFloatingMenuOutside, true);
}
function _onFloatingMenuOutside(e) {
  if (_floatingMenu && !_floatingMenu.contains(e.target)) dismissFloatingMenu();
}

/**
 * Show a floating menu at viewport coords, clamped on-screen.
 * @param {number} x
 * @param {number} y
 * @param {object} opts
 * @param {string} [opts.title]      — optional header line (e.g. the file path)
 * @param {string} [opts.className]  — extra class on the menu root
 * @param {Array<{label: string, hint?: string, fn: () => void}>} opts.items
 */
export function showFloatingMenu(x, y, { title, className, items }) {
  dismissFloatingMenu();
  const menu = el('div', 'fb-context-menu' + (className ? ' ' + className : ''));
  if (title) menu.appendChild(el('div', 'fb-drop-chooser-title', title));
  for (const { label, hint, fn } of items) {
    const item = el('button', 'fb-context-item', label);
    item.type = 'button';
    if (hint) item.appendChild(el('span', 'fb-drop-hint', hint));
    item.addEventListener('click', () => { dismissFloatingMenu(); fn(); });
    menu.appendChild(item);
  }
  menu.style.left = x + 'px';
  menu.style.top = y + 'px';
  document.body.appendChild(menu);
  // Clamp to viewport after layout
  requestAnimationFrame(() => {
    const r = menu.getBoundingClientRect();
    if (r.right > window.innerWidth - 8) menu.style.left = (window.innerWidth - r.width - 8) + 'px';
    if (r.bottom > window.innerHeight - 8) menu.style.top = (window.innerHeight - r.height - 8) + 'px';
    menu.classList.add('visible');
  });
  _floatingMenu = menu;
  document.addEventListener('mousedown', _onFloatingMenuOutside, true);
}

// ── Public factory ─────────────────────────────────────────────

/**
 * Creates the file browser panel. Returns { refresh, focusSearch, dispose }.
 *
 * @param {object} deps
 * @param {HTMLElement} deps.treeEl       — scrollable tree/results container
 * @param {HTMLInputElement} deps.searchInput — header search input
 * @param {() => string} deps.getRoot     — absolute project root ('' if unknown)
 * @param {string} deps.remoteName        — remote host name ('' = local)
 * @param {string} deps.hubBaseUrl        — absolute hub base URL
 * @param {(dir: string) => Promise<{entries?: Array, error?: string}>} deps.listDir
 * @param {(path: string, x: number, y: number, opts?: {pin?: boolean, line?: number}) => void} deps.openPreview
 * @param {(dragging: boolean, label: string|null) => void} deps.onDragState
 * @param {(path: string, x: number, y: number) => void} deps.onDropOnTerminal
 * @param {(path: string) => void} deps.onPastePath
 * @param {((path: string) => void)|null} deps.onOpenInEditor   — null when host has no editor
 * @param {((path: string) => void)|null} deps.onRevealInFinder — null on remote tabs
 * @param {(path: string) => void} deps.onOpenTerminalHere
 * @param {(path: string) => void} deps.onCopyPath
 */
export function createFileBrowser({
  treeEl,
  searchInput,
  getRoot,
  remoteName,
  hubBaseUrl,
  listDir,
  openPreview,
  onDragState,
  onDropOnTerminal,
  onPastePath,
  onOpenInEditor,
  onRevealInFinder,
  onOpenTerminalHere,
  onCopyPath,
}) {
  // ── State ────────────────────────────────────────────────────
  const expandedDirs = new Set(); // absolute dir paths currently expanded
  const fullyShown = new Set();   // dirs the user chose to render past DIR_RENDER_CAP
  const dirCache = new Map();     // absolute dir path → entries array
  let selectedPath = null;
  let renderSeq = 0;              // guards stale async renders
  let disposed = false;

  // Git working-tree status for dirty indicators.
  //   dirtyFiles:   relpath → code ('M'|'A'|'D'|'U'|'R'|'C')
  //   dirtyDirs:    relpath (no trailing slash) → code, for folders that
  //                 ARE themselves dirty (untracked dir) or CONTAIN dirty
  //                 descendants (propagated, shown as a dot).
  let dirtyFiles = new Map();
  let dirtyDirs = new Map();
  let statusFetchedAt = 0;
  const STATUS_TTL_MS = 4000;

  // Search state
  let searchQuery = '';
  let searchDebounce = null;
  let grepSeq = 0;
  // Project file index for fuzzy search: { fetchedAt, files, truncated }
  let fileIndex = null;
  let indexPromise = null;
  const INDEX_TTL_MS = 30_000;
  const GREP_DEBOUNCE_MS = 250;
  const FUZZY_DEBOUNCE_MS = 120;
  const MAX_RENDERED_RESULTS = 200;

  // ── Hub URL builders (remote-aware) ──────────────────────────
  const apiBase = (endpoint) => hubBaseUrl + (remoteName
    ? '/api/v1/remotes/' + encodeURIComponent(remoteName) + '/files/' + endpoint
    : '/api/v1/files/' + endpoint);

  // ── Git status (dirty indicators) ────────────────────────────

  function relOf(absPath) {
    const root = getRoot().replace(/\/$/, '');
    if (absPath === root) return '';
    return absPath.startsWith(root + '/') ? absPath.slice(root.length + 1) : absPath;
  }

  async function fetchStatus(force) {
    const root = getRoot();
    if (!root) return;
    if (!force && Date.now() - statusFetchedAt < STATUS_TTL_MS && dirtyFiles.size + dirtyDirs.size >= 0 && statusFetchedAt) return;
    try {
      const r = await fetch(apiBase('status') + '?root=' + encodeURIComponent(root));
      const d = await r.json();
      const entries = (d && d.entries) || {};
      const files = new Map();
      const dirs = new Map();
      const rank = { C: 6, U: 5, D: 4, R: 3, A: 2, M: 1 };
      const bump = (map, key, code) => {
        const cur = map.get(key);
        if (!cur || (rank[code] || 0) > (rank[cur] || 0)) map.set(key, code);
      };
      for (const [path, code] of Object.entries(entries)) {
        if (path.endsWith('/')) {
          // Untracked directory — mark the dir itself + propagate to ancestors.
          const d0 = path.replace(/\/$/, '');
          bump(dirs, d0, code);
          let parent = d0;
          while (parent.includes('/')) { parent = parent.replace(/\/[^/]+$/, ''); bump(dirs, parent, code); }
        } else {
          bump(files, path, code);
          // Propagate to every ancestor folder as a dot.
          let parent = path;
          while (parent.includes('/')) { parent = parent.replace(/\/[^/]+$/, ''); bump(dirs, parent, code); }
        }
      }
      dirtyFiles = files;
      dirtyDirs = dirs;
      statusFetchedAt = Date.now();
    } catch (_) {
      // Non-fatal — just no dirty marks.
    }
  }

  // Map a porcelain code to a CSS state class + badge glyph.
  function dirtyMeta(code) {
    switch (code) {
      case 'U': return { cls: 'dirty-untracked', badge: 'U' };
      case 'A': return { cls: 'dirty-added', badge: 'A' };
      case 'D': return { cls: 'dirty-deleted', badge: 'D' };
      case 'R': return { cls: 'dirty-renamed', badge: 'R' };
      case 'C': return { cls: 'dirty-conflict', badge: '!' };
      default:  return { cls: 'dirty-modified', badge: 'M' };
    }
  }

  // ── Tree rendering ───────────────────────────────────────────

  async function fetchDir(dirAbs) {
    if (dirCache.has(dirAbs)) return dirCache.get(dirAbs);
    const d = await listDir(dirAbs);
    if (d && d.error) throw new Error(d.error);
    const entries = (d && d.entries) || [];
    // dirs first, then alpha — the hub pre-sorts but the VS Code
    // list-dir path may not; normalize here so both hosts match.
    entries.sort((a, b) => {
      const ad = a.kind === 'dir' ? 0 : 1;
      const bd = b.kind === 'dir' ? 0 : 1;
      return ad - bd || a.name.localeCompare(b.name);
    });
    dirCache.set(dirAbs, entries);
    return entries;
  }

  function makeRow(entryAbs, name, kind, depth) {
    const isDir = kind === 'dir';
    const expanded = isDir && expandedDirs.has(entryAbs);
    const row = el('div', 'fb-row' + (isDir ? ' is-dir' : ''));
    row.dataset.path = entryAbs;
    row.dataset.kind = kind;
    row.dataset.depth = String(depth);
    row.style.paddingLeft = (FB_INDENT_BASE + depth * FB_INDENT_STEP) + 'px';

    // Indent guides — one vertical line per ancestor level. They line up
    // row-to-row to form continuous guides (VS Code style).
    for (let i = 0; i < depth; i++) {
      const g = el('span', 'fb-guide');
      g.style.left = (FB_INDENT_BASE + i * FB_INDENT_STEP + 6) + 'px';
      row.appendChild(g);
    }

    // Twisty column — chevron for dirs, empty spacer for files so icons align.
    const tw = el('span', 'fb-twisty' + (expanded ? ' open' : ''));
    if (isDir) tw.appendChild(svgNode(FB_TWISTY_SVG));
    row.appendChild(tw);

    row.appendChild(buildIconEl(name, kind, expanded));
    row.appendChild(el('span', 'fb-name', name));

    // Dirty indicator: files get a letter badge + colored name; dirs get a
    // dot when they contain dirty descendants.
    const rel = relOf(entryAbs);
    const code = isDir ? dirtyDirs.get(rel) : dirtyFiles.get(rel);
    if (code) {
      const m = dirtyMeta(code);
      row.classList.add('dirty', m.cls);
      const badge = el('span', 'fb-badge', isDir ? '●' : m.badge);
      if (isDir) badge.classList.add('is-dot');
      row.appendChild(badge);
    }

    if (entryAbs === selectedPath) row.classList.add('selected');
    wireRowEvents(row, entryAbs, name, isDir);
    return row;
  }

  function wireRowEvents(row, entryAbs, name, isDir) {
    row.addEventListener('click', () => {
      if (isDir) {
        if (expandedDirs.has(entryAbs)) expandedDirs.delete(entryAbs);
        else expandedDirs.add(entryAbs);
        renderTree();
      } else {
        selectedPath = entryAbs;
        treeEl.querySelectorAll('.fb-row.selected').forEach((r) => r.classList.remove('selected'));
        row.classList.add('selected');
      }
    });
    if (!isDir) {
      row.addEventListener('dblclick', (e) => {
        e.preventDefault();
        openPreview(entryAbs, e.clientX, e.clientY, { pin: true });
      });
      // Cmd/Ctrl-hover → quick (unpinned) preview, mirroring terminal
      // file-link hover. Fires once per hover-with-modifier.
      let previewArmed = true;
      row.addEventListener('mousemove', (ev) => {
        const mod = IS_MAC ? ev.metaKey : ev.ctrlKey;
        if (mod && previewArmed) {
          previewArmed = false;
          openPreview(entryAbs, ev.clientX, ev.clientY, { pin: false });
        } else if (!mod) {
          previewArmed = true;
        }
      });
    }
    row.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      e.stopPropagation();
      showContextMenu(entryAbs, isDir, e.clientX, e.clientY);
    });
    // Mouse-based drag into the terminal — shared with the reveal-tree preview.
    dragItemToTerminal(row, { path: entryAbs, name, isDir }, { onDragState, onDropOnTerminal });
  }

  async function renderDirInto(container, dirAbs, depth, seq) {
    let entries;
    try {
      entries = await fetchDir(dirAbs);
    } catch (err) {
      if (seq !== renderSeq) return;
      container.appendChild(el('div', 'fb-error', 'Listing failed: ' + (err && err.message || err)));
      return;
    }
    if (seq !== renderSeq || disposed) return;
    // Cap how many entries a single folder renders at once. A build-artifacts
    // dir (e.g. target/debug/examples) can hold hundreds-to-thousands of files;
    // since every toggle rebuilds the visible tree, rendering them all blocks
    // the event loop for seconds (the folder appears never to open). Render the
    // first DIR_RENDER_CAP and offer a "N more" row to load the rest on demand.
    const overflow = entries.length - DIR_RENDER_CAP;
    const showAll = fullyShown.has(dirAbs) || overflow <= 0;
    const shown = showAll ? entries : entries.slice(0, DIR_RENDER_CAP);
    for (const entry of shown) {
      const entryAbs = dirAbs.replace(/\/$/, '') + '/' + entry.name;
      container.appendChild(makeRow(entryAbs, entry.name, entry.kind, depth));
      if (entry.kind === 'dir' && expandedDirs.has(entryAbs)) {
        await renderDirInto(container, entryAbs, depth + 1, seq);
        if (seq !== renderSeq || disposed) return;
      }
    }
    if (!showAll) {
      const more = el('div', 'fb-more', '⋯ ' + overflow + ' more');
      more.style.paddingLeft = (FB_INDENT_BASE + depth * FB_INDENT_STEP + 22) + 'px';
      more.addEventListener('click', () => { fullyShown.add(dirAbs); renderTree(); });
      container.appendChild(more);
    }
    if (entries.length === 0 && depth === 0) {
      container.appendChild(el('div', 'fb-empty', '(empty)'));
    }
  }

  async function renderTree() {
    const root = getRoot();
    const seq = ++renderSeq;
    if (!root) {
      treeEl.textContent = '';
      treeEl.appendChild(el('div', 'fb-empty', 'No project root'));
      return;
    }
    await fetchStatus(false);
    if (seq !== renderSeq || disposed) return;
    // Build the whole tree OFF-DOM in a fragment, then swap it in atomically.
    // Rendering directly into treeEl appends rows incrementally across the
    // per-folder fetch awaits, so a first-time (uncached) folder briefly shows
    // partial, unscrolled content before settling. A fragment is invisible
    // until attached: the old tree stays put until the new one is fully built,
    // then one swap — no flash. Preserving scrollTop keeps expand/collapse in
    // place (clearing treeEl would otherwise drop it to 0). revealPath()
    // re-scrolls to its target afterward, so reveal still works.
    const prevScrollTop = treeEl.scrollTop;
    const frag = document.createDocumentFragment();
    await renderDirInto(frag, root, 0, seq);
    if (seq !== renderSeq || disposed) return;
    treeEl.textContent = '';
    treeEl.appendChild(frag);
    treeEl.prepend(stickyHost); // first child → sticky anchors from the top
    treeEl.scrollTop = prevScrollTop;
    stickyRowH = 0; // re-measure (font/zoom may have changed)
    updateSticky();
  }

  // ── Sticky scroll ("freeze folders", VS Code style) ───────────
  // As you scroll into a nested folder, its ANCESTOR folders pin at the top so
  // the path context stays visible; the deepest one slides up smoothly as you
  // scroll past its subtree, and clicking a pinned folder jumps to it. The host
  // is sticky+height:0 so the absolutely-positioned clones overlay the tree
  // without pushing content. Only applies in tree mode (search rows aren't
  // .fb-row, so the query is empty → widget hides).
  const STICKY_MAX = 7;
  const stickyHost = el('div', 'fb-sticky-host');
  const stickyEl = el('div', 'fb-sticky');
  stickyHost.appendChild(stickyEl);
  let stickyRowH = 0;

  function makeStickyClone(row, idx, total) {
    const clone = row.cloneNode(true);
    clone.classList.add('fb-sticky-row');
    clone.classList.remove('selected', 'reveal-flash', 'dragging');
    clone.style.zIndex = String(total - idx); // shallower paints over deeper (slide-under)
    clone.addEventListener('click', () => {
      const path = row.dataset.path;
      const target = treeEl.querySelector('.fb-row[data-path="'
        + (window.CSS && CSS.escape ? CSS.escape(path) : path) + '"]');
      if (target) treeEl.scrollTop = Math.max(0, target.offsetTop - idx * (stickyRowH || 0));
    });
    return clone;
  }

  function updateSticky() {
    const rows = treeEl.querySelectorAll(':scope > .fb-row');
    if (rows.length < 2) { stickyEl.replaceChildren(); stickyEl.style.display = 'none'; return; }
    const H = stickyRowH || (stickyRowH = rows[0].offsetHeight);
    if (!H) { stickyEl.style.display = 'none'; return; }
    const scrollTop = treeEl.scrollTop;
    const topIndex = Math.min(rows.length - 1, Math.max(0, Math.floor(scrollTop / H)));
    const anchorDepth = parseInt(rows[topIndex].dataset.depth || '0', 10);
    // Walk back over the DFS-ordered rows to collect the ancestor folder spine.
    const anc = [];
    let want = anchorDepth - 1;
    for (let i = topIndex - 1; i >= 0 && want >= 0; i--) {
      if (parseInt(rows[i].dataset.depth || '0', 10) === want && rows[i].dataset.kind === 'dir') {
        anc.unshift(rows[i]); want--;
      }
    }
    const sticky = anc.length > STICKY_MAX ? anc.slice(0, STICKY_MAX) : anc;
    if (!sticky.length) { stickyEl.replaceChildren(); stickyEl.style.display = 'none'; return; }
    // Push-up: find where the deepest pinned folder's subtree ends (first row at
    // depth <= its depth), and slide that clone up as the boundary nears the
    // widget's bottom so it pops smoothly instead of snapping.
    const k = sticky.length - 1;
    let boundary = rows.length;
    for (let i = topIndex; i < rows.length; i++) {
      if (parseInt(rows[i].dataset.depth || '0', 10) <= k) { boundary = i; break; }
    }
    const stickyH = sticky.length * H;
    const overlap = Math.max(0, Math.min(H, stickyH - (boundary * H - scrollTop)));
    stickyEl.style.display = 'block';
    stickyEl.replaceChildren();
    sticky.forEach((row, idx) => {
      const clone = makeStickyClone(row, idx, sticky.length);
      if (idx === k && overlap > 0) clone.style.transform = 'translateY(' + (-overlap) + 'px)';
      stickyEl.appendChild(clone);
    });
  }

  let _stickyRaf = 0;
  function scheduleSticky() {
    if (_stickyRaf) return;
    _stickyRaf = requestAnimationFrame(() => { _stickyRaf = 0; updateSticky(); });
  }
  treeEl.addEventListener('scroll', scheduleSticky, { passive: true });

  // ── Search ───────────────────────────────────────────────────

  function ensureIndex() {
    const root = getRoot();
    if (!root) return Promise.resolve(null);
    if (fileIndex && Date.now() - fileIndex.fetchedAt < INDEX_TTL_MS) {
      return Promise.resolve(fileIndex);
    }
    if (indexPromise) return indexPromise;
    indexPromise = fetch(apiBase('index') + '?root=' + encodeURIComponent(root))
      .then((r) => r.json())
      .then((d) => {
        indexPromise = null;
        if (d && Array.isArray(d.files)) {
          fileIndex = { fetchedAt: Date.now(), files: d.files, truncated: !!d.truncated };
          return fileIndex;
        }
        throw new Error((d && d.error) || 'bad index response');
      })
      .catch((err) => {
        indexPromise = null;
        console.warn('[file-browser] index fetch failed:', err);
        return null;
      });
    return indexPromise;
  }

  function renderResultRow(relPath, line, snippet) {
    const root = getRoot();
    const abs = root.replace(/\/$/, '') + '/' + relPath;
    const row = el('div', 'fb-row fb-result');
    row.dataset.path = abs;
    row.dataset.kind = 'file';
    const base = relPath.split('/').pop();
    const dir = relPath.slice(0, relPath.length - base.length).replace(/\/$/, '');
    const nameEl = el('span', 'fb-name', base + (line ? ':' + line : ''));
    row.appendChild(buildIconEl(base, 'file', false));
    row.appendChild(nameEl);
    if (dir) row.appendChild(el('span', 'fb-result-dir', dir));
    if (snippet) row.appendChild(el('span', 'fb-result-snippet', snippet.trim()));
    row.addEventListener('click', (e) => {
      openPreview(abs, e.clientX, e.clientY, { pin: true, line: line || undefined });
    });
    row.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      e.stopPropagation();
      showContextMenu(abs, false, e.clientX, e.clientY);
    });
    dragItemToTerminal(row, { path: abs, name: base, isDir: false }, { onDragState, onDropOnTerminal });
    return row;
  }

  async function renderFuzzyResults(q, seq) {
    const idx = await ensureIndex();
    if (seq !== renderSeq || disposed || searchQuery !== q) return;
    treeEl.textContent = '';
    if (!idx) {
      treeEl.appendChild(el('div', 'fb-error', 'File index unavailable'));
      return;
    }
    const scored = [];
    for (const f of idx.files) {
      const s = fuzzyScore(q, f);
      if (s >= 0) scored.push([s, f]);
    }
    scored.sort((a, b) => b[0] - a[0]);
    const top = scored.slice(0, MAX_RENDERED_RESULTS);
    for (const [, f] of top) treeEl.appendChild(renderResultRow(f, 0, null));
    if (top.length === 0) treeEl.appendChild(el('div', 'fb-empty', 'No matches'));
    else if (scored.length > top.length || idx.truncated) {
      treeEl.appendChild(el('div', 'fb-empty', top.length + ' of ' + scored.length + ' matches'));
    }
  }

  async function renderGrepResults(q, seq) {
    const root = getRoot();
    const mySeq = ++grepSeq;
    let d;
    try {
      const r = await fetch(apiBase('grep') + '?root=' + encodeURIComponent(root)
        + '&q=' + encodeURIComponent(q) + '&limit=300');
      d = await r.json();
    } catch (err) {
      d = { error: String(err) };
    }
    if (seq !== renderSeq || mySeq !== grepSeq || disposed || !searchQuery.startsWith('>')) return;
    treeEl.textContent = '';
    if (d.error) {
      treeEl.appendChild(el('div', 'fb-error', 'Search failed: ' + d.error));
      return;
    }
    const matches = d.matches || [];
    for (const m of matches.slice(0, MAX_RENDERED_RESULTS)) {
      treeEl.appendChild(renderResultRow(m.file, m.line, m.text));
    }
    if (matches.length === 0) treeEl.appendChild(el('div', 'fb-empty', 'No matches'));
    else if (d.truncated) treeEl.appendChild(el('div', 'fb-empty', 'first ' + matches.length + ' matches'));
  }

  function onSearchChanged() {
    const q = searchInput.value;
    searchQuery = q;
    clearTimeout(searchDebounce);
    if (!q.trim()) {
      renderTree();
      return;
    }
    const isGrep = q.startsWith('>');
    const body = isGrep ? q.slice(1).trim() : q.trim();
    if (!body) {
      treeEl.textContent = '';
      treeEl.appendChild(el('div', 'fb-empty', 'Type to search file contents…'));
      return;
    }
    const seq = ++renderSeq;
    searchDebounce = setTimeout(() => {
      if (isGrep) renderGrepResults(body, seq);
      else renderFuzzyResults(body, seq);
    }, isGrep ? GREP_DEBOUNCE_MS : FUZZY_DEBOUNCE_MS);
  }

  searchInput.addEventListener('input', onSearchChanged);
  searchInput.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      searchInput.value = '';
      onSearchChanged();
      searchInput.blur();
    }
    e.stopPropagation(); // keep terminal shortcuts out of the input
  });

  // ── Context menu ─────────────────────────────────────────────

  function showContextMenu(absPath, isDir, x, y) {
    const items = [];
    if (!isDir) {
      items.push({ label: 'Show preview', fn: () => openPreview(absPath, x, y, { pin: true }) });
    }
    items.push({ label: 'Paste path to prompt', fn: () => onPastePath(absPath) });
    if (onOpenInEditor && !isDir) {
      items.push({ label: 'Open in VS Code', fn: () => onOpenInEditor(absPath) });
    }
    if (onRevealInFinder) {
      items.push({
        label: IS_MAC ? 'Reveal in Finder' : 'Reveal in file manager',
        fn: () => onRevealInFinder(absPath),
      });
    }
    items.push({ label: 'Open terminal here', fn: () => onOpenTerminalHere(absPath) });
    items.push({ label: 'Copy path', fn: () => onCopyPath(absPath) });
    showFloatingMenu(x, y, { items });
  }

  // Per-row drag into the terminal is wired via the shared dragItemToTerminal()
  // helper at row-build time (wireRowEvents / search rows) — no factory-level
  // drag state or document listeners to manage here.

  // ── Public API ───────────────────────────────────────────────

  function refresh() {
    dirCache.clear();
    fileIndex = null;
    statusFetchedAt = 0; // force a fresh git-status read
    if (searchQuery.trim()) onSearchChanged();
    else renderTree();
  }

  function focusSearch() {
    searchInput.focus();
    searchInput.select();
  }

  // Collapse the whole tree back to the project root.
  function collapseAll() {
    expandedDirs.clear();
    if (searchInput.value) { searchInput.value = ''; searchQuery = ''; }
    renderTree();
  }

  // Directory a new entry should be created in: the selected dir, the
  // selected file's parent, or the root.
  function createTargetDir() {
    const root = getRoot().replace(/\/$/, '');
    if (!selectedPath) return root;
    const sel = treeEl.querySelector('.fb-row.selected');
    if (sel && sel.dataset.kind === 'dir') { expandedDirs.add(selectedPath); return selectedPath; }
    return selectedPath.replace(/\/[^/]+$/, '') || root;
  }

  // New File / New Folder — inline input row (VS Code style). Commits on
  // Enter (POST /files/create → refresh + reveal); cancels on Escape/blur.
  // Local tabs only — remote create isn't wired.
  function startCreate(kind) {
    if (remoteName) return;
    if (searchInput.value) { searchInput.value = ''; searchQuery = ''; renderTree(); }
    const targetDir = createTargetDir();
    const root = getRoot().replace(/\/$/, '');
    const row = el('div', 'fb-row fb-create-row');
    row.appendChild(el('span', 'fb-twisty'));
    row.appendChild(buildIconEl(kind === 'dir' ? 'new' : 'new.txt', kind === 'dir' ? 'dir' : 'file', false));
    const input = el('input', 'fb-create-input');
    input.type = 'text';
    input.placeholder = kind === 'dir' ? 'Folder name…' : 'File name…';
    row.appendChild(input);
    treeEl.insertBefore(row, treeEl.firstChild);
    input.focus();

    let done = false;
    const cleanup = () => { if (row.parentNode) row.remove(); };
    const commit = async () => {
      if (done) return; done = true;
      const name = input.value.trim();
      cleanup();
      if (!name) return;
      const relDir = targetDir === root ? '' : targetDir.slice(root.length + 1) + '/';
      const rel = relDir + name;
      try {
        const r = await fetch(apiBase('create'), {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ root, path: rel, kind }),
        });
        const d = await r.json();
        if (d && d.error) { console.warn('[file-browser] create:', d.error); return; }
        dirCache.delete(targetDir);
        await refresh();
        revealPath(root + '/' + rel);
      } catch (err) { console.warn('[file-browser] create failed:', err); }
    };
    input.addEventListener('keydown', (e) => {
      e.stopPropagation();
      if (e.key === 'Enter') commit();
      else if (e.key === 'Escape') { done = true; cleanup(); }
    });
    input.addEventListener('blur', () => commit());
  }

  // Reveal a file/dir: clear search, expand every ancestor, render, then
  // select + scroll it into view. Powers "Browse in ImmorTerm".
  async function revealPath(absPath) {
    const root = getRoot().replace(/\/$/, '');
    if (!absPath || !absPath.startsWith(root)) return;
    if (searchInput.value) { searchInput.value = ''; searchQuery = ''; }
    // Expand each ancestor directory of the target.
    const relParts = relOf(absPath).split('/').filter(Boolean);
    let acc = root;
    for (let i = 0; i < relParts.length - 1; i++) {
      acc = acc + '/' + relParts[i];
      expandedDirs.add(acc);
    }
    selectedPath = absPath;
    await renderTree();
    const rowEl = treeEl.querySelector('.fb-row.selected')
      || treeEl.querySelector('[data-path="' + (window.CSS && CSS.escape ? CSS.escape(absPath) : absPath) + '"]');
    if (rowEl) {
      rowEl.classList.add('selected', 'reveal-flash');
      rowEl.scrollIntoView({ block: 'center' });
      setTimeout(() => rowEl.classList.remove('reveal-flash'), 1200);
    }
  }

  function dispose() {
    disposed = true;
    treeEl.removeEventListener('scroll', scheduleSticky);
    dismissFloatingMenu();
  }

  // Initial render
  renderTree();

  return { refresh, focusSearch, dispose, renderTree, collapseAll, revealPath, startCreate };
}
