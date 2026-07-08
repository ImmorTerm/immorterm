/**
 * ImmorTerm shortcut keymap — single source of truth.
 *
 * Loaded by every webview (tab-shell.html + gpu-terminal.html). When any
 * webview has keyboard focus and the user presses a combo listed here,
 * the handler invokes `cmd_handle_shortcut` in Rust, which routes to the
 * actual behaviour. One dispatcher, one keymap — DRY.
 *
 * Adding a shortcut: append one entry + add a branch in
 * dispatch_shortcut_action() in lib.rs. No per-webview JS changes.
 *
 * `action` values map 1:1 to Rust match arms (see dispatch_shortcut_action).
 */
(function(){
  const KEYMAP = [
    // ── App-level tab / window ops ──
    { key: 'r', meta: true,                       action: 'reload'             },
    { key: 't', meta: true,                       action: 'open-picker'        },
    { key: 't', meta: true, shift: true,          action: 'plain-new-tab'      },
    { key: 'T', meta: true, shift: true,          action: 'plain-new-tab'      },
    { key: 'w', meta: true,                       action: 'close-tab'          },
    { key: '/', meta: true,                       action: 'open-shortcuts'     },
    { key: '?', meta: true, shift: true,          action: 'open-shortcuts'     },
    // Ctrl+Shift+←/→ NOT Cmd+Shift — Cmd+Shift+←/→ is macOS select-to-line-start/end
    { key: 'ArrowLeft',  ctrl: true, shift: true, action: 'prev-tab'           },
    { key: 'ArrowRight', ctrl: true, shift: true, action: 'next-tab'           },
    { key: 'n', meta: true,                       action: 'new-window'         },
    // ── Zoom ──
    { key: '=', meta: true,                       action: 'zoom-in'            },
    { key: '+', meta: true,                       action: 'zoom-in'            },
    { key: '-', meta: true,                       action: 'zoom-out'           },
    { key: '_', meta: true,                       action: 'zoom-out'           },
    { key: '0', meta: true,                       action: 'zoom-reset'         },
    // ── Project-scoped (synthetic key forwarded to active project webview) ──
    { key: 'ArrowUp',   shift: true,              action: 'project-prev-session' },
    { key: 'ArrowDown', shift: true,              action: 'project-next-session' },
    { key: 'a', ctrl: true, shift: true,          action: 'project-new-session'  },
    { key: 'A', ctrl: true, shift: true,          action: 'project-new-session'  },
  ];

  function matches(ev, spec) {
    if (ev.key !== spec.key) return false;
    // Modifier match is strict: every spec flag must align 1:1 with the
    // event, AND unspecified flags must be absent. This keeps Ctrl+X
    // distinct from Cmd+X (important on macOS where terminals rely on
    // literal Ctrl for things like Ctrl+A/E line-editing).
    //
    // `meta` → macOS Cmd key (ev.metaKey). `ctrl` → literal Ctrl
    // (ev.ctrlKey). Unspecified = must be released.
    const wantMeta  = !!spec.meta;
    const wantCtrl  = !!spec.ctrl;
    const wantShift = !!spec.shift;
    const wantAlt   = !!spec.alt;
    if (wantMeta  !== !!ev.metaKey)  return false;
    if (wantCtrl  !== !!ev.ctrlKey)  return false;
    if (wantShift !== !!ev.shiftKey) return false;
    if (wantAlt   !== !!ev.altKey)   return false;
    return true;
  }

  // Detect which webview we're running in. The project webview loads
  // gpu-terminal.html; the shell loads tab-shell.html. Project-scoped
  // actions (session switch, new session) must NOT be dispatched from
  // the project webview — gpu-terminal.html's own keydown listeners
  // handle them natively there. If we dispatched anyway, we'd call
  // Rust → Rust synthesises a keydown in this same webview → our
  // listener re-fires → infinite loop + frozen terminal.
  const IS_PROJECT_WEBVIEW =
    typeof location !== 'undefined' && /gpu-terminal\.html/.test(location.pathname);

  /**
   * Dispatcher — returns true if the event matched a shortcut and was
   * invoked. Callers preventDefault() on true so browser default doesn't
   * double-fire.
   */
  window.__immortermHandleShortcut = function(ev) {
    // Skip synthetic events (they're how Rust forwards shortcuts to us
    // on the project side; re-dispatching would loop).
    if (ev.isTrusted === false) return false;
    const tauri = window.__TAURI__ && window.__TAURI__.core;
    if (!tauri) return false;
    for (const spec of KEYMAP) {
      if (!matches(ev, spec)) continue;
      // Let project-scoped shortcuts fall through to gpu-terminal.html's
      // native listeners when we're IN the project webview.
      if (IS_PROJECT_WEBVIEW && spec.action.startsWith('project-')) return false;
      ev.preventDefault();
      tauri.invoke('cmd_handle_shortcut', { action: spec.action }).catch(() => {});
      return true;
    }
    return false;
  };

  // Auto-wire a document-level keydown listener. Both shell and project
  // webviews just include this script — no per-webview plumbing needed.
  document.addEventListener('keydown', window.__immortermHandleShortcut, true);
})();
