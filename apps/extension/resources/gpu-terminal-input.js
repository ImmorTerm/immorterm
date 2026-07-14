// ── ImmorTerm shared terminal input handling ─────────────────────
// Every TERMINAL-INTRINSIC input behavior of the GPU terminal, extracted
// from gpu-terminal.html so the main terminal and the scratch panel share
// EXACTLY the same input pipeline: keyboard (incl. pseudo-cursor /
// multi-cursor, keyboard selection, readline shortcuts, type-to-replace),
// rich copy, paste, IME/dictation composition, mouse selection (drag /
// word / line / block), scroll-thumb drag, drag auto-scroll, wheel
// scrollback, click-to-cursor teleport, Cmd+hover link hit-testing and
// Cmd+click link opening, and context-menu suppression.
//
// HOST-SPECIFIC behavior (bullets/comments/pills wizards, paste-undo,
// AI buttons, session switching, link tooltips, remote-tab image paste,
// voice-burst cosmetics) is spliced in through the `hooks` interceptor
// chain — the module never touches host globals. The GENERIC local-daemon
// clipboard-image legs (Cmd+V image detection, Cmd+Opt+V paste-as-path,
// presence probe) live here in createClipboardImageRpc, parameterized
// over each consumer's ws + request_id resolver map.
//
// Selection anchoring rules preserved from the original handlers:
//   • KEYBOARD selection (Shift+Arrow) anchors from the terminal cursor —
//     WASM `selection_extend` falls back to the reverse-video cell when an
//     Ink/Claude-style TUI hides the real cursor (lib.rs).
//   • MOUSE selection anchors from CSS coordinates (`selection_start*`).
//     These two must stay strictly separate.
//   • Modifier keys alone must NEVER clear the selection; any other key
//     without Shift clears it (falls through, no preventDefault).

// Render-tick settle time (60fps = ~16.7ms). Follow-ups after a
// replaceSelection() must wait at least one tick so Ink fully drains
// the delete chunk before the next write lands.
export const INK_SETTLE_MS = 20;

// ── Shared clipboard-image machinery (daemon WS RPC) ─────────────
// Every daemon speaks the same clipboard RPCs (websocket.rs):
//   clipboard_check_image      → clipboard_image_presence {has_image, file_url}
//   clipboard_save_image       → clipboard_image_saved    {path}
//   clipboard_save_image_bytes → clipboard_image_saved    {path}
// All replies echo request_id. Consumers keep a request_id → resolver map
// and route the two reply types from their own WS onmessage dispatch
// through handleClipboardImageReply() (the main terminal keeps its inline
// equivalent — its dispatch can run before this module is imported).

/** Resolve a pending clipboard RPC from a WS JSON message. Returns true
 *  when the message type was a clipboard reply (even if no resolver was
 *  waiting — timed-out requests delete theirs). */
export function handleClipboardImageReply(resolvers, msg) {
  if (msg.type === 'clipboard_image_presence') {
    const resolver = resolvers.get(msg.request_id);
    if (resolver) {
      resolvers.delete(msg.request_id);
      resolver({ hasImage: !!msg.has_image, fileUrl: msg.file_url || null });
    }
    return true;
  }
  if (msg.type === 'clipboard_image_saved') {
    const resolver = resolvers.get(msg.request_id);
    if (resolver) {
      resolvers.delete(msg.request_id);
      resolver({ path: msg.path || null });
    }
    return true;
  }
  return false;
}

/**
 * Generic legs of the image-paste flow, parameterized over the consumer's
 * ws + resolver map: presence probe (askDaemon), Cmd+V empty-bracketed-paste
 * dispatch, and Cmd+Opt+V paste-image-as-path. The main terminal delegates
 * its local-daemon hooks here (keeping its remote-tab variants host-side);
 * the scratch panel gets it built-in via createTerminalInput's
 * `clipboardImage` dep.
 *
 * @param {Function} opts.getWs           () => ws-like | null
 * @param {Function} opts.getTerm         () => WasmTerminal | null
 * @param {Function} opts.replaceSelection (ws, afterFn) => void
 * @param {Map}      opts.resolvers       request_id → resolver (shared with
 *                                        the consumer's WS reply dispatch)
 * @param {Function} opts.makeRequestId   (prefix) => unique request id
 * @param {Function} [opts.isRpcStale]    () => bool — skip the daemon probe
 *                                        (pre-RPC daemons never reply)
 * @param {Function} [opts.armPasteUndo]  (kind) => void
 */
export function createClipboardImageRpc({
  getWs, getTerm, replaceSelection, resolvers, makeRequestId,
  isRpcStale = () => false, armPasteUndo = null,
}) {
  // Empty bracketed-paste markers — Claude Code's TUI sees the paste event
  // and reads the OS clipboard itself, producing its native [Image #N]
  // marker for image bytes (PNG, JPEG, TIFF, file copies).
  const sendEmptyPaste = () => {
    const ws = getWs();
    if (!ws) return;
    if (armPasteUndo) armPasteUndo('image');
    ws.send(JSON.stringify({ type: 'input_raw', data: btoa('\x1b[200~\x1b[201~') }));
  };

  // Cmd+V image leg (local daemon). With an active selection, first erase
  // it (editor-style replace) before dispatching the paste event.
  const dispatchImagePaste = () => {
    const term = getTerm();
    if (term && term.has_selection()) {
      const ws = getWs();
      if (ws) replaceSelection(ws, () => setTimeout(sendEmptyPaste, INK_SETTLE_MS));
      else sendEmptyPaste();
    } else {
      sendEmptyPaste();
    }
  };

  // Cmd+Opt+V — write the clipboard image to a temp PNG on the daemon
  // (arboard, zero-copy) and type the returned path so Claude Code reads
  // it lazily via Read instead of inlining it into the conversation.
  // Bypasses the many-image 2000px dimension cap that the default
  // [Image #N] flow can hit on big screenshots.
  const pasteImageAsPath = () => {
    const ws = getWs();
    if (!ws) return;
    const requestId = makeRequestId('cb-save-');
    resolvers.set(requestId, (reply) => {
      if (reply && reply.path) {
        const ws2 = getWs();
        if (ws2) {
          if (armPasteUndo) armPasteUndo('text');
          ws2.send(JSON.stringify({ type: 'input', data: reply.path }));
        }
      }
    });
    ws.send(JSON.stringify({ type: 'clipboard_save_image', request_id: requestId }));
    setTimeout(() => { resolvers.delete(requestId); }, 5000);
  };

  // Daemon clipboard probe for the Cmd+V resolver chain — fills the web
  // API's blind spots (JPEG/TIFF images and Finder file URLs). 200ms
  // covers the daemon's microsecond reply with margin.
  const askDaemon = () => new Promise((resolve) => {
    const ws = getWs();
    const empty = { hasImage: false, fileUrl: null };
    if (!ws) { resolve(empty); return; }
    if (isRpcStale()) { resolve(empty); return; }
    const requestId = makeRequestId('cb-');
    resolvers.set(requestId, resolve);
    ws.send(JSON.stringify({ type: 'clipboard_check_image', request_id: requestId }));
    setTimeout(() => {
      if (resolvers.delete(requestId)) resolve(empty);
    }, 200);
  });

  return { askDaemon, dispatchImagePaste, pasteImageAsPath };
}

/**
 * Wire the full input pipeline for one terminal instance.
 *
 * @param {object}   deps
 * @param {Function} deps.getTerm       () => WasmTerminal | null
 * @param {Element}  deps.canvas        canvas element (coordinate origin + cursor styling)
 * @param {Element}  deps.keyTarget     hidden textarea receiving keyboard/IME events
 * @param {Element}  deps.pointerTarget element receiving pointer/mouse/wheel events
 *                                      (main: the canvas; scratch: the panel body,
 *                                      because a capture textarea overlays its canvas)
 * @param {Function} deps.getWs         () => ws-like { send(str) } | null
 * @param {Function} deps.markDirty     () => void — request a re-render
 * @param {Function} deps.isReady       () => bool — GPU/WASM ready gate for pointerdown
 * @param {Function} deps.focus         () => void — keep keyboard focus on keyTarget
 * @param {Function} deps.getCellSize   () => {w, h} cell dims in CSS px
 * @param {Function} [deps.openLink]    (link) => void — Cmd+click open transport
 * @param {Element}  [deps.phantomEl]   click-to-cursor phantom cursor element
 * @param {Element}  [deps.maskEl]      click-to-cursor real-cursor mask element
 * @param {number}   [deps.padTop=2]    content padding CSS px (must match set_content_padding)
 * @param {number}   [deps.padLeft=10]
 * @param {boolean}  [deps.scrollProximity=false] drive set_scroll_indicator_proximity
 *                   from pointermove/mouseleave (main does this in its own
 *                   status-bar hover handler; scratch enables it here)
 * @param {object}   [deps.clipboardImage] enable the built-in clipboard-image
 *                   RPC legs (createClipboardImageRpc): {resolvers, makeRequestId}.
 *                   Hosts that wire hooks.dispatchImagePaste/askClipboardDaemon
 *                   (the main terminal — remote-aware variants) skip this.
 * @param {object}   [deps.hooks]       host interceptors — all optional:
 *   guard(e) -> bool            swallow keydown entirely (modals / task wizard)
 *   preKey(e) -> bool           after ws guard, before multi-cursor (paste-undo
 *                               eager clear, bullet preview, staged comments)
 *   chordKey(e) -> bool         after copy/multi-cursor, before Cmd+A
 *                               (Cmd+D/E families, Ctrl+Shift+F)
 *   clipboardKey(e) -> bool     after Cmd+A, before Cmd+V (Cmd+Opt+V, Cmd+Z)
 *   pointerDown(e, x, y) -> bool after link open, before scroll-thumb (AI buttons)
 *   onPasteText(text)           voice-burst flash
 *   armPasteUndo(kind)          paste-undo arm
 *   dispatchImagePaste()        image paste leg (presence enables image detection)
 *   askClipboardDaemon() -> Promise<{hasImage, fileUrl}> daemon clipboard RPC
 *   onPrintableKey()            voice-burst counter
 *   linkHover(link, e)          Cmd+hover feedback (tooltips); default: pointer cursor
 *   linkHoverEnd()              clear hover feedback; default: reset cursor
 *   updateScrollLock()          after any scroll mutation (wheel/thumb/auto-scroll)
 *   afterWheel() / afterScrollDrag() / afterAutoScroll()  host repositioning
 *   onScrollNeedsMore()         wheel scrolled past local scrollback
 *   compositionUI: {start(), update(text), end()}  dictation pulse / overlay
 *
 * @returns {{ replaceSelection, INK_SETTLE_MS, isComposing, isMouseDown, dispose }}
 */
export function createTerminalInput(deps) {
  const {
    getTerm, canvas, keyTarget, pointerTarget, getWs,
    markDirty, isReady, focus, getCellSize,
    openLink = null, phantomEl = null, maskEl = null,
    padTop = 2, padLeft = 10,
    scrollProximity = false,
    hooks = {},
  } = deps;

  // ── State (was: gpu-terminal.html module-scope globals) ────────
  let composing = false;
  let mouseDown = false;
  let mouseDownPos = null;   // {x, y[, altKey, dragged]} of initial pointerdown (CSS coords)
  let lastClickTime = 0;     // timestamp of last pointerdown for multi-click detection
  let clickCount = 0;        // 1=single, 2=double (word), 3=triple (line)
  let scrollDrag = null;     // { startY, startOffset } — dragging the scroll thumb
  let scrollAccumulator = 0; // fractional line accumulator for smooth trackpad scrolling
  let autoScrollTimer = null;
  let autoScrollSpeed = 0;   // lines per tick (negative = down, positive = up)
  let lastDragCssX = 0;      // last known mouse X during drag (for selection_update)
  // Alt double-tap detection for vertical multi-cursor
  let altTapCount = 0;
  let altTapTimer = null;
  let verticalMultiCursorActive = false;

  const DRAG_THRESHOLD = 3;  // px of movement before a drag starts a selection
  const DBLCLICK_MS = 400;   // max ms between clicks to count as multi-click

  // ── Selection replace (Ink-aware chunked delete) ───────────────
  // Erase the current terminal selection by replaying the WASM-computed
  // delete sequence (arrows + backspaces) to Ink's readline, one chunk per
  // 4ms macrotask (Ink processes one escape sequence per render tick).
  // `afterFn` is invoked once the last chunk has been sent — callers that
  // follow up with more input should setTimeout by INK_SETTLE_MS.
  function replaceSelection(ws, afterFn) {
    const term = getTerm();
    if (!term || !term.has_selection()) { if (afterFn) afterFn(); return; }
    const seq = term.delete_selection_sequence();
    const chunks = [];
    let i = 0;
    while (i < seq.length) {
      const b = seq[i];
      if (b === 0x1b) {
        // CSI: ESC [ params final(0x40-0x7e). Fallback: ESC-x 2-byte.
        if (i + 1 < seq.length && seq[i + 1] === 0x5b /* [ */) {
          let end = i + 2;
          while (end < seq.length && seq[end] < 0x40) end++;
          if (end < seq.length) end++;
          chunks.push(seq.slice(i, end));
          i = end;
        } else if (i + 1 < seq.length) {
          chunks.push(seq.slice(i, i + 2));
          i += 2;
        } else {
          chunks.push(seq.slice(i, i + 1));
          i += 1;
        }
      } else if (b === 0x7f) {
        // Coalesce backspace runs — Ink handles them atomically per tick.
        let j = i;
        while (j < seq.length && seq[j] === 0x7f) j++;
        chunks.push(seq.slice(i, j));
        i = j;
      } else {
        chunks.push(seq.slice(i, i + 1));
        i += 1;
      }
    }
    term.selection_clear();
    markDirty();
    if (chunks.length === 0) { if (afterFn) afterFn(); return; }
    const deliver = (idx) => {
      const b64 = btoa(String.fromCharCode(...chunks[idx]));
      ws.send(JSON.stringify({ type: 'input_raw', data: b64 }));
      if (idx + 1 < chunks.length) {
        setTimeout(() => deliver(idx + 1), 4);
      } else if (afterFn) {
        afterFn();
      }
    };
    deliver(0);
  }

  // ── Link hover feedback defaults (host overrides for tooltips) ──
  function linkHoverDefault() { canvas.style.cursor = 'pointer'; }
  function linkHoverEndDefault() { canvas.style.cursor = ''; }
  const linkHover = hooks.linkHover || linkHoverDefault;
  const linkHoverEnd = hooks.linkHoverEnd || linkHoverEndDefault;

  // ── Clipboard-image legs ────────────────────────────────────────
  // Hosts either wire their own hooks (main terminal: remote-aware variants
  // + paste-undo) or pass `clipboardImage` to get the built-in daemon-RPC
  // legs over this instance's ws + replaceSelection (scratch panel).
  const clipboardRpc = deps.clipboardImage
    ? createClipboardImageRpc({
        getWs, getTerm, replaceSelection,
        resolvers: deps.clipboardImage.resolvers,
        makeRequestId: deps.clipboardImage.makeRequestId,
        armPasteUndo: hooks.armPasteUndo,
      })
    : null;
  const dispatchImagePaste = hooks.dispatchImagePaste
    || (clipboardRpc ? clipboardRpc.dispatchImagePaste : null);
  const askClipboardDaemon = hooks.askClipboardDaemon
    || (clipboardRpc ? clipboardRpc.askDaemon : null);

  // ── IME / Dictation composition (hidden textarea) ───────────────
  // Keystrokes do NOT trigger composition events, so there is no
  // double-fire risk with the keydown handler below.
  const onCompositionStart = () => {
    composing = true;
    if (hooks.compositionUI) hooks.compositionUI.start();
  };
  const onCompositionUpdate = (e) => {
    // Live interim text from macOS Dictation / IME.
    if (hooks.compositionUI) hooks.compositionUI.update(e.data || keyTarget.value || '');
  };
  const onCompositionEnd = (e) => {
    if (hooks.compositionUI) hooks.compositionUI.end();
    composing = false;
    const text = e.data || '';
    keyTarget.value = '';
    if (!text) return;
    const ws = getWs();
    if (!ws) return;
    const term = getTerm();
    const send = () => ws.send(JSON.stringify({ type: 'input', data: text }));
    if (term && term.has_selection()) {
      replaceSelection(ws, () => setTimeout(send, INK_SETTLE_MS));
    } else {
      send();
    }
  };
  // beforeinput fallback for inputs that bypass composition entirely
  // (e.g. some dictation tools commit via insertReplacementText only).
  const onBeforeInput = (e) => {
    if (composing) return;
    if (e.inputType !== 'insertReplacementText' && e.inputType !== 'insertFromDictation') return;
    const text = e.data || '';
    e.preventDefault();
    keyTarget.value = '';
    if (!text) return;
    const ws = getWs();
    if (!ws) return;
    const term = getTerm();
    const send = () => ws.send(JSON.stringify({ type: 'input', data: text }));
    if (term && term.has_selection()) {
      replaceSelection(ws, () => setTimeout(send, INK_SETTLE_MS));
    } else {
      send();
    }
  };
  // Drop any chars the textarea accumulates from un-prevented keystrokes —
  // never mid-composition (clearing cancels the IME string).
  const onInput = () => {
    if (!composing) keyTarget.value = '';
  };

  // ── Keydown ─────────────────────────────────────────────────────
  const onKeyDown = (e) => {
    const term = getTerm();
    if (!term) return;
    if (hooks.guard && hooks.guard(e)) return;
    const ws = getWs();
    if (!ws) return;

    // Host pre-key interceptors (paste-undo eager clear, bullet preview,
    // staged comments Enter). May fall through without consuming.
    if (hooks.preKey && hooks.preKey(e)) return;

    // ── Alt double-tap + Arrow = vertical multi-cursor ──
    if (e.key === 'Alt') {
      altTapCount++;
      clearTimeout(altTapTimer);
      altTapTimer = setTimeout(() => { altTapCount = 0; }, 400);
      if (altTapCount >= 2) {
        verticalMultiCursorActive = true;
        // If no pseudo-cursors yet, seed one at the visual cursor position
        if (!term.has_pseudo_cursors()) {
          term.pseudo_cursor_add_at_visual_cursor();
          markDirty();
        }
      }
      return; // Don't clear selection on modifier key
    }
    // Alt held + Arrow in vertical multi-cursor mode
    if (verticalMultiCursorActive && e.altKey && ['ArrowUp', 'ArrowDown'].includes(e.key)) {
      term.pseudo_cursor_add_vertical(e.key === 'ArrowUp' ? 'up' : 'down');
      markDirty();
      e.preventDefault();
      return;
    }
    // Any non-Alt key exits vertical multi-cursor placement mode
    if (e.key !== 'Alt') {
      verticalMultiCursorActive = false;
    }

    // Cmd+C with selection = copy (rich text + plain text). Selection is
    // intentionally preserved after copy — many users press Cmd+C multiple
    // times to confirm the copy succeeded, and clearing would surprise them.
    // Matches macOS native behavior (Finder, text editors, browsers).
    if ((e.metaKey || e.ctrlKey) && e.key === 'c' && term.has_selection()) {
      const text = term.get_selected_text();
      const html = term.get_selected_html();
      if (text) {
        // Prefer rich text (text/plain + text/html) via ClipboardItem
        // so users can paste into editors with formatting. Fall back to
        // plain text via Tauri plugin in WKWebView where ClipboardItem
        // sometimes silently fails (same root cause as the Cmd+V flake).
        // Final fallback: navigator.clipboard.writeText.
        const tcb = window.__TAURI__ && window.__TAURI__.clipboardManager;
        const writeRich = () => {
          const item = new ClipboardItem({
            'text/plain': new Blob([text], { type: 'text/plain' }),
            'text/html': new Blob([html || text], { type: 'text/html' }),
          });
          return navigator.clipboard.write([item]);
        };
        const writePlain = () => {
          if (tcb && typeof tcb.writeText === 'function') return tcb.writeText(text);
          return navigator.clipboard.writeText(text);
        };
        writeRich().catch(() => writePlain()).catch((err) => {
          console.error('[copy] all clipboard writes failed:', err);
        });
      }
      e.preventDefault();
      return;
    }

    // Host chord interceptors (Cmd+D/E bullet + comment wizards,
    // Ctrl+Shift+F explore popup).
    if (hooks.chordKey && hooks.chordKey(e)) return;

    // Cmd+A = select all input text (Claude Code interactive mode)
    if ((e.metaKey || e.ctrlKey) && e.key === 'a') {
      if (term.select_all_input()) {
        markDirty();
        e.preventDefault();
        return;
      }
    }

    // Host clipboard-chord interceptors (Cmd+Opt+V paste-as-path,
    // Cmd+Z / Ctrl+Z paste undo).
    if (hooks.clipboardKey && hooks.clipboardKey(e)) return;

    // Cmd+Opt+V / Ctrl+Alt+V — paste clipboard image as a file path.
    // Built-in leg for hosts on the shared clipboard machinery (scratch);
    // the main terminal intercepts this chord in clipboardKey above with
    // its remote-aware version. `'√'` covers macOS layouts where Option
    // mangles e.key.
    if (clipboardRpc && (e.metaKey || e.ctrlKey) && e.altKey
        && (e.key === 'v' || e.key === '√' || e.code === 'KeyV')) {
      clipboardRpc.pasteImageAsPath();
      e.preventDefault();
      return;
    }

    // Cmd+V / Ctrl+V = paste (text or image). With an active selection,
    // first erase the selection (editor-style replace) before inserting.
    if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key === 'v') {
      // Send `text` as a paste, erasing any current selection first so
      // the paste lands into an empty input (editor semantics).
      const pasteText = (text) => {
        const aws = getWs();
        if (!aws || !text) return;
        if (hooks.onPasteText) hooks.onPasteText(text);
        // Arm inside send() (not earlier): after a selection-replace the
        // snapshot must capture the post-delete text or the undo diff
        // would see a deletion and refuse.
        const send = () => {
          if (hooks.armPasteUndo) hooks.armPasteUndo('text');
          aws.send(JSON.stringify({ type: 'input', data: text }));
        };
        if (term.has_selection()) {
          replaceSelection(aws, () => setTimeout(send, INK_SETTLE_MS));
        } else {
          send();
        }
      };
      (async () => {
        // Resolver chain for clipboard reads:
        //   1. Tauri clipboard plugin — reliable in WKWebView where
        //      navigator.clipboard often silently fails.
        //   2. navigator.clipboard.read() — VS Code webview + Chrome path.
        //   3. host daemon RPC for fileUrl + JPEG/TIFF detection.
        // Image legs come from the host's hooks (main terminal) or the
        // built-in clipboardImage machinery (scratch panel).
        const tcb = window.__TAURI__ && window.__TAURI__.clipboardManager;

        if (dispatchImagePaste && tcb && typeof tcb.readImage === 'function') {
          try {
            const img = await tcb.readImage();
            if (img) { dispatchImagePaste(); return; }
          } catch (_) { /* not an image, try text */ }
        }
        if (tcb && typeof tcb.readText === 'function') {
          try {
            const text = await tcb.readText();
            if (text && text.length > 0) { pasteText(text); return; }
          } catch (err) {
            console.warn('[paste] Tauri clipboardManager.readText failed:', err);
          }
        }

        // Fall back to browser clipboard API.
        if (dispatchImagePaste) {
          try {
            const items = await navigator.clipboard.read();
            for (const item of items) {
              if (item.types.find(t => t.startsWith('image/'))) {
                dispatchImagePaste();
                return;
              }
            }
          } catch (_) { /* fall through to daemon RPC */ }
        }
        if (askClipboardDaemon) {
          const result = await askClipboardDaemon();
          if (result.fileUrl) { pasteText(result.fileUrl); return; }
          if (result.hasImage) { dispatchImagePaste(); return; }
        }
        try {
          pasteText(await navigator.clipboard.readText());
        } catch (_) { /* clipboard unavailable */ }
      })();
      e.preventDefault();
      return;
    }

    // ── Keyboard selection (Shift+Arrow, Shift+Option+Arrow for word,
    // Cmd+Shift for line). Anchors from the TERMINAL CURSOR (WASM
    // selection_extend), falling back to the reverse-video cell when an
    // Ink-style TUI hides the real cursor — never from mouse coords. ──
    if (e.shiftKey && ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) {
      let dir;
      if (e.metaKey) {
        dir = e.key === 'ArrowLeft' ? 'home' : e.key === 'ArrowRight' ? 'end' : e.key === 'ArrowUp' ? 'up' : 'down';
      } else if (e.altKey) {
        dir = e.key === 'ArrowLeft' ? 'word_left' : e.key === 'ArrowRight' ? 'word_right' : e.key === 'ArrowUp' ? 'up' : 'down';
      } else {
        dir = e.key === 'ArrowLeft' ? 'left' : e.key === 'ArrowRight' ? 'right' : e.key === 'ArrowUp' ? 'up' : 'down';
      }
      // Multi-cursor mode: extend all pseudo-cursors
      if (term.has_pseudo_cursors()) {
        term.pseudo_cursor_extend_all(dir);
      } else {
        term.selection_extend(dir);
      }
      markDirty();
      e.preventDefault();
      return;
    }

    // Delete/Backspace with selection = erase via replaceSelection().
    // Why not Ctrl+U: Ink's readline kills only the CURRENT visual line,
    // not the whole multi-row input buffer.
    if (term.has_selection() && (e.key === 'Backspace' || e.key === 'Delete')) {
      replaceSelection(ws, null);
      e.preventDefault();
      return;
    }

    // Editor-style type-to-replace: any printable character (incl. Enter,
    // Tab) while a selection is active first erases the selection, then
    // inserts the typed character — matching VS Code / textarea behavior.
    // Excludes modified keys so shortcuts like Ctrl+U still pass through.
    if (term.has_selection() && !e.metaKey && !e.ctrlKey && !e.altKey
        && (e.key.length === 1 || e.key === 'Enter' || e.key === 'Tab')) {
      const bytes = term.handle_key(e.key, e.ctrlKey, e.shiftKey, e.altKey);
      replaceSelection(ws, () => {
        if (bytes.length > 0) {
          const b64 = btoa(String.fromCharCode(...bytes));
          setTimeout(() => {
            ws.send(JSON.stringify({ type: 'input_raw', data: b64 }));
          }, INK_SETTLE_MS);
        }
      });
      e.preventDefault();
      return;
    }

    // Clear selection on non-modifier keystrokes (standard input behavior)
    if (term.has_selection() && !['Shift', 'Meta', 'Alt', 'Control', 'CapsLock'].includes(e.key)) {
      term.selection_clear();
      markDirty();
    }

    // Cmd+Arrow = Home/End (macOS convention: Cmd+Left = beginning of line, Cmd+Right = end)
    if (e.metaKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      const seq = e.key === 'ArrowLeft' ? '\x01' : '\x05'; // Ctrl+A / Ctrl+E (readline)
      if (window.__keylog !== false) {
        const hex = e.key === 'ArrowLeft' ? '01' : '05';
        console.log(`[keylog] Cmd+${e.key} → [${hex}] (1B) = ${e.key === 'ArrowLeft' ? 'Home' : 'End'}`);
      }
      ws.send(JSON.stringify({ type: 'input_raw', data: btoa(seq) }));
      e.preventDefault();
      return;
    }

    // Ctrl+Shift+B — dump BiDi debug info for cursor row to console
    if (e.ctrlKey && e.shiftKey && e.key === 'B') {
      if (typeof term.debug_bidi_row === 'function') {
        console.log('[BiDi-debug]', term.debug_bidi_row());
      }
      e.preventDefault();
      return;
    }

    // Let all other Cmd-modified keys pass through to the host (zoom, find, etc.)
    // Only Ctrl+<letter> without Cmd should go to the terminal (Ctrl+C, Ctrl+D, etc.)
    if (e.metaKey) return;

    // Voice-burst detection: count printable, unmodified keystrokes only.
    // Excludes shortcuts (Ctrl/Alt) and navigation keys.
    if (!e.ctrlKey && !e.altKey && e.key.length === 1 && hooks.onPrintableKey) {
      hooks.onPrintableKey();
    }

    const bytes = term.handle_key(e.key, e.ctrlKey, e.shiftKey, e.altKey);
    if (window.__keylog !== false) {
      const hex = Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join(' ');
      const mods = (e.ctrlKey ? 'Ctrl+' : '') + (e.altKey ? 'Alt+' : '') + (e.shiftKey ? 'Shift+' : '');
      console.log(`[keylog] ${mods}${e.key} → [${hex}] (${bytes.length}B)`);
    }
    if (bytes.length > 0) {
      const b64 = btoa(String.fromCharCode(...bytes));
      ws.send(JSON.stringify({ type: 'input_raw', data: b64 }));
    }
    markDirty();
    e.preventDefault();
  };

  // ── Auto-scroll during drag selection ───────────────────────────
  function startAutoScroll(speed, cssX) {
    autoScrollSpeed = speed;
    lastDragCssX = cssX;
    if (autoScrollTimer) return; // already running
    autoScrollTimer = setInterval(() => {
      const term = getTerm();
      if (!term || !term.has_selection()) { stopAutoScroll(); return; }
      term.scroll(autoScrollSpeed); // scroll() delta: positive = up into scrollback
      if (hooks.updateScrollLock) hooks.updateScrollLock();
      // Update selection endpoint at the edge row
      const edgeY = autoScrollSpeed > 0
        ? padTop                                          // scrolling up → top edge
        : padTop + term.visible_rows() * getCellSize().h; // scrolling down → bottom edge
      term.selection_update(lastDragCssX, edgeY);
      markDirty();
      if (hooks.afterAutoScroll) hooks.afterAutoScroll();
    }, 50);
  }

  function stopAutoScroll() {
    if (autoScrollTimer) { clearInterval(autoScrollTimer); autoScrollTimer = null; }
    autoScrollSpeed = 0;
  }

  // ── Pointer down: link open / multi-click / Alt / drag-select ──
  const onPointerDown = (e) => {
    const term = getTerm();
    if (!term || !isReady() || e.button !== 0) return;
    // Keep hidden textarea focused so dictation/IME keep working after a click.
    focus();
    const rect = canvas.getBoundingClientRect();
    const cssX = e.clientX - rect.left;
    const cssY = e.clientY - rect.top;

    // ── Cmd/Ctrl+click: open link (URL or file path) ──
    const isMac = navigator.platform.toLowerCase().includes('mac');
    const linkModifier = isMac ? e.metaKey : e.ctrlKey;
    if (linkModifier) {
      let linkJson = '';
      try { linkJson = term.link_at(cssX, cssY); }
      catch (err) { console.warn('[link_at] click threw:', err); }
      console.log('[link] click result:', linkJson);
      if (linkJson) {
        try {
          const link = JSON.parse(linkJson);
          if (openLink) openLink(link);
          e.preventDefault();
          mouseDown = false;
          return;
        } catch (_) {}
      }
    }

    // Host pointerdown interceptor (AI button hit-test).
    if (hooks.pointerDown && hooks.pointerDown(e, cssX, cssY)) {
      mouseDown = false;
      return;
    }

    // ── Scroll indicator hit-test ──
    const canvasCSSWidth = canvas.getBoundingClientRect().width;
    const indicatorW = 20;
    const indicatorInset = 6;
    const indicatorLeft = canvasCSSWidth - indicatorInset - indicatorW;
    const sbLen = term.scrollback_len();
    if (cssX >= indicatorLeft && sbLen > 0 && term.scroll_offset() > 0) {
      scrollDrag = { startY: cssY, startOffset: term.scroll_offset() };
      mouseDown = true;
      pointerTarget.setPointerCapture(e.pointerId);
      e.preventDefault();
      return;
    }

    // Multi-click: double = select word, triple = select line
    const now = Date.now();
    if (now - lastClickTime < DBLCLICK_MS) {
      clickCount++;
    } else {
      clickCount = 1;
    }
    lastClickTime = now;
    if (clickCount === 2) {
      try { term.select_word_at(cssX, cssY); } catch (_) {}
      markDirty();
      mouseDown = false;
      e.preventDefault();
      return;
    }
    if (clickCount >= 3) {
      clickCount = 0;
      try { term.select_line_at(cssX, cssY); } catch (_) {}
      markDirty();
      mouseDown = false;
      e.preventDefault();
      return;
    }

    // Alt+Click = pseudo-cursor OR Alt+drag = block selection
    // Defer pseudo-cursor to pointerup (only if no drag happened)
    if (e.altKey) {
      mouseDownPos = { x: cssX, y: cssY, altKey: true, dragged: false };
      mouseDown = true;
      pointerTarget.setPointerCapture(e.pointerId);
      e.preventDefault();
      return;
    }

    mouseDownPos = { x: cssX, y: cssY };
    mouseDown = true;
    pointerTarget.setPointerCapture(e.pointerId);
    // Regular click clears all selections and pseudo-cursors
    if (term.has_selection()) {
      term.selection_clear();
      markDirty();
    }
    // Don't call selection_start yet — wait for drag threshold.
    // MOUSE selection anchors from CSS coordinates (selection_start*),
    // strictly separate from the keyboard anchor rule above.
  };

  // ── Pointer move: thumb drag / drag-select / edge auto-scroll ──
  const onPointerMove = (e) => {
    const term = getTerm();
    // ── Scroll indicator drag ──
    if (scrollDrag && term) {
      const rect = canvas.getBoundingClientRect();
      const cy = e.clientY - rect.top;
      const sbLen = term.scrollback_len();
      if (sbLen === 0) return;
      const dpr = window.devicePixelRatio || 1;
      const cellH = getCellSize().h;
      const visibleRows = Math.floor((rect.height - padTop) / cellH);
      const trackHeight = visibleRows * cellH;
      const totalLines = sbLen + visibleRows;
      const thumbHeight = Math.max(trackHeight * visibleRows / totalLines, 80 / dpr);
      const scrollableRange = trackHeight - thumbHeight;
      if (scrollableRange <= 0) return;
      const deltaY = cy - scrollDrag.startY;
      const deltaFraction = deltaY / scrollableRange;
      const deltaOffset = Math.round(deltaFraction * sbLen);
      const newOffset = Math.max(0, scrollDrag.startOffset - deltaOffset);
      term.set_scroll_offset(newOffset);
      if (hooks.updateScrollLock) hooks.updateScrollLock();
      markDirty();
      if (hooks.afterScrollDrag) hooks.afterScrollDrag();
      return;
    }
    if (!term || !mouseDown || !mouseDownPos) return;
    const rect = canvas.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;

    // For Alt+drag: use the dragged flag to know if we already started this drag.
    // We can't rely on has_selection() because pseudo-cursors also make it true.
    const needsStart = mouseDownPos.altKey ? !mouseDownPos.dragged : !term.has_selection();
    if (needsStart) {
      // Not yet dragging — check if mouse moved enough to start selection
      const dx = cx - mouseDownPos.x;
      const dy = cy - mouseDownPos.y;
      if (Math.abs(dx) < DRAG_THRESHOLD && Math.abs(dy) < DRAG_THRESHOLD) return;
      // Threshold exceeded — initiate selection from the original mousedown point
      // Alt+drag = block (rectangular) selection; regular drag = line selection
      try {
        if (mouseDownPos.altKey) {
          term.pseudo_cursor_clear();
          term.selection_start_block(mouseDownPos.x, mouseDownPos.y);
          mouseDownPos.dragged = true;
        } else {
          term.selection_start(mouseDownPos.x, mouseDownPos.y);
        }
      } catch (err) {
        console.warn('[GPU] selection_start skipped (re-entrant):', err.message);
        return;
      }
    }
    term.selection_update(cx, cy);
    markDirty();

    // ── Auto-scroll when dragging above/below viewport ──
    const cellH = getCellSize().h;
    const viewportTop = padTop;
    const viewportBottom = padTop + term.visible_rows() * cellH;
    if (cy < viewportTop && term.scroll_offset() < term.scrollback_len()) {
      const lines = (viewportTop - cy) / cellH;
      startAutoScroll(Math.ceil(Math.min(lines * (1 + lines * 0.1), 20)), cx);
    } else if (cy > viewportBottom && term.scroll_offset() > 0) {
      const lines = (cy - viewportBottom) / cellH;
      startAutoScroll(-Math.ceil(Math.min(lines * (1 + lines * 0.1), 20)), cx);
    } else {
      stopAutoScroll();
    }
  };

  // ── Pointer up: pseudo-cursor add / click-to-cursor teleport ──
  const onPointerUp = () => {
    const term = getTerm();
    // Alt+click with no drag = add pseudo-cursor
    if (mouseDownPos && mouseDownPos.altKey && !mouseDownPos.dragged && term) {
      term.pseudo_cursor_add(mouseDownPos.x, mouseDownPos.y);
      markDirty();
    }
    // Plain click with no drag and no selection = teleport cursor via arrow keys.
    // Strategy: show phantom at target + mask the real cursor during transit, then
    // poll until the real position matches the phantom target (1.5s timeout).
    if (mouseDownPos && !mouseDownPos.altKey && !mouseDownPos.dragged && term && !term.has_selection()) {
      try {
        // Get plan metadata first (returns JSON String, borrow released immediately),
        // then get the byte sequence. Separate calls avoid RefCell aliasing.
        const planJson = term.click_to_cursor_plan(mouseDownPos.x, mouseDownPos.y);
        const plan = JSON.parse(planJson || '{}');
        const seq = term.click_to_cursor_sequence(mouseDownPos.x, mouseDownPos.y);
        if (seq.length > 0) {
          const cell = getCellSize();
          if (typeof plan.target_col === 'number' && typeof plan.disp_row === 'number' && cell.w > 0 && phantomEl && maskEl) {
            // Position phantom at target cell.
            phantomEl.style.left = (padLeft + plan.target_col * cell.w) + 'px';
            phantomEl.style.top = (padTop + plan.disp_row * cell.h) + 'px';
            phantomEl.style.width = cell.w + 'px';
            phantomEl.style.height = cell.h + 'px';
            phantomEl.style.display = 'block';
            // Mask: covers the real cursor cell with terminal-bg + the cell's
            // character rendered in WHITE (to neutralize Ink's INVERSE cursor).
            maskEl.style.width = cell.w + 'px';
            maskEl.style.height = cell.h + 'px';
            maskEl.style.fontSize = (cell.h * 0.85) + 'px';
            const placeMask = (col, dispRow, gridRow) => {
              maskEl.style.left = (padLeft + col * cell.w) + 'px';
              maskEl.style.top = (padTop + dispRow * cell.h) + 'px';
              const ch = (typeof term.cell_grapheme_at === 'function')
                ? term.cell_grapheme_at(gridRow, col) : '';
              maskEl.textContent = ch || ' ';
              maskEl.style.display = 'block';
            };
            // Debug: log the computed path so we can diagnose mismatches.
            console.log('[click-to-cursor]', 'plan=', plan, 'seq_bytes=', seq.length,
              'seq=', Array.from(seq).map(b => '0x' + b.toString(16).padStart(2, '0')).join(' '));
            try { console.log('[click-trace]', JSON.parse(term.debug_click_trace(mouseDownPos.x, mouseDownPos.y))); } catch (e) {}
            const ws = getWs();
            if (ws) {
              const b64 = btoa(String.fromCharCode(...seq));
              ws.send(JSON.stringify({ type: 'input_raw', data: b64 }));
            }
            // Initial mask at current visual cursor.
            const vc0 = term.visual_cursor_display();
            if (vc0 && vc0[0] >= 0) {
              placeMask(vc0[0], vc0[1], vc0[1] + term.scroll_offset());
            }

            // rAF loop: poll visual cursor to track mask position and
            // hide phantom+mask once cursor reaches the target.
            // NO corrections — the initial Dijkstra plan is optimal and
            // corrections while Ink is still processing cause overshoot.
            const startedAt = Date.now();
            const targetCol = plan.target_col;
            const targetRow = plan.target_row;
            const tick = () => {
              const vc = term.visual_cursor_display();
              if (vc && vc[0] >= 0) {
                const curGridRow = vc[1] + term.scroll_offset();
                placeMask(vc[0], vc[1], curGridRow);
                if (vc[0] === targetCol && curGridRow === targetRow) {
                  phantomEl.style.display = 'none';
                  maskEl.style.display = 'none';
                  return;
                }
              }
              if (Date.now() - startedAt > 1500) {
                phantomEl.style.display = 'none';
                maskEl.style.display = 'none';
                return;
              }
              requestAnimationFrame(tick);
            };
            requestAnimationFrame(tick);
          }
        }
      } catch (_e) {
        if (phantomEl) phantomEl.style.display = 'none';
        if (maskEl) maskEl.style.display = 'none';
      }
    }
    mouseDown = false;
    mouseDownPos = null;
    scrollDrag = null;
    stopAutoScroll();
  };

  // Suppress the host webview context menu on the terminal surface.
  const onContextMenu = (e) => { e.preventDefault(); e.stopPropagation(); };

  // ── Wheel: local scrollback + on-demand daemon fetch ────────────
  const onWheel = (e) => {
    const term = getTerm();
    if (!term) { console.warn('[scroll] wheel ignored — terminal is null'); return; }
    // Accumulate fractional scroll for smooth trackpad + momentum support.
    // deltaMode 0 = pixels (trackpad), 1 = lines (mouse wheel).
    // Use actual cell height for 1:1 scroll like a native terminal.
    const pixelsPerLine = getCellSize().h || 16;
    const delta = e.deltaMode === 1 ? e.deltaY : e.deltaY / pixelsPerLine;
    scrollAccumulator += delta;

    // Consume whole lines from the accumulator
    const lines = Math.trunc(scrollAccumulator);
    if (lines !== 0) {
      // Negate: browser deltaY>0 = scroll down, but WASM scroll(+) = up into history
      const needsMore = term.scroll(-lines);
      scrollAccumulator -= lines;
      // On-demand scrollback: host fetches more rows from its daemon when
      // the user scrolls past the local buffer.
      if (needsMore && hooks.onScrollNeedsMore) hooks.onScrollNeedsMore();
    }

    if (hooks.updateScrollLock) hooks.updateScrollLock();
    markDirty();
    if (hooks.afterWheel) hooks.afterWheel();
    e.preventDefault();
  };

  // ── Cmd/Ctrl+hover: detect clickable links ──────────────────────
  const onHoverMove = (e) => {
    const term = getTerm();
    if (!term || mouseDown) return;
    const isMac = navigator.platform.toLowerCase().includes('mac');
    const linkModifier = isMac ? e.metaKey : e.ctrlKey;
    if (!linkModifier) {
      linkHoverEnd();
      return;
    }
    const rect = canvas.getBoundingClientRect();
    const rawX = e.clientX - rect.left;
    const rawY = e.clientY - rect.top;
    let linkJson = '';
    try { linkJson = term.link_at(rawX, rawY); }
    catch (err) { console.warn('[link_at] threw:', err); }
    if (window.__immortermDebugLinks) console.log('[link_at]', { rawX, rawY, linkJson });
    if (!linkJson) { linkHoverEnd(); return; }
    try {
      const link = JSON.parse(linkJson);
      linkHover(link, e);
    } catch (_) { linkHoverEnd(); }
  };

  // Clear link cursor when modifier released
  const onKeyUp = (e) => {
    if (e.key === 'Meta' || e.key === 'Control') linkHoverEnd();
  };

  // Scroll-indicator proximity (hover expansion). The main terminal drives
  // this from its status-bar hover handler; scratch enables it here.
  const onProximityMove = (e) => {
    const term = getTerm();
    if (!term || mouseDown) return;
    const rect = canvas.getBoundingClientRect();
    const distFromRight = rect.width - (e.clientX - rect.left);
    const proximity = Math.max(0, 1 - distFromRight / 50);
    term.set_scroll_indicator_proximity(proximity);
    if (proximity > 0) markDirty();
  };

  const onMouseLeave = () => {
    linkHoverEnd();
    if (scrollProximity) {
      const term = getTerm();
      if (term) { term.set_scroll_indicator_proximity(0); markDirty(); }
    }
  };

  // ── Registration (order mirrors the original gpu-terminal.html) ──
  keyTarget.addEventListener('compositionstart', onCompositionStart);
  keyTarget.addEventListener('compositionupdate', onCompositionUpdate);
  keyTarget.addEventListener('compositionend', onCompositionEnd);
  keyTarget.addEventListener('beforeinput', onBeforeInput);
  keyTarget.addEventListener('input', onInput);
  keyTarget.addEventListener('keydown', onKeyDown);
  pointerTarget.addEventListener('pointerdown', onPointerDown);
  pointerTarget.addEventListener('pointermove', onPointerMove);
  window.addEventListener('pointerup', onPointerUp);
  pointerTarget.addEventListener('contextmenu', onContextMenu);
  pointerTarget.addEventListener('wheel', onWheel, { passive: false });
  // Hover feedback rides POINTERMOVE, not mousemove: the scratch panel's
  // capture <textarea> overlays its canvas, and WKWebView doesn't reliably
  // deliver compat mousemove over a focused textarea — pointer events are,
  // and selection drag (onPointerMove above) already proves that stream
  // works. Coordinates stay canvas-rect-relative either way.
  pointerTarget.addEventListener('pointermove', onHoverMove);
  window.addEventListener('keyup', onKeyUp);
  if (scrollProximity) pointerTarget.addEventListener('pointermove', onProximityMove);
  pointerTarget.addEventListener('mouseleave', onMouseLeave);

  function dispose() {
    stopAutoScroll();
    clearTimeout(altTapTimer);
    window.removeEventListener('pointerup', onPointerUp);
    window.removeEventListener('keyup', onKeyUp);
    // Element-scoped listeners die with their elements (scratch removes
    // its panel); no need to unregister them individually.
  }

  return {
    replaceSelection,
    INK_SETTLE_MS,
    isComposing: () => composing,
    isMouseDown: () => mouseDown,
    dispose,
  };
}
