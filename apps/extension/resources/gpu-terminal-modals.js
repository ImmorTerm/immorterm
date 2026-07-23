/**
 * GPU Terminal — Modal System.
 *
 * Factory-pattern module for all modal dialogs (appearance, diagnostics,
 * services, license, logs, setup wizard). Dependency-injected for testability.
 *
 * Imported by gpu-terminal.html via dynamic import (same pattern as utils).
 */

// ── Pure helpers ────────────────────────────────────────────────

/** Create an element with optional class and text content. */
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** Clear all children of a parent element. */
function clearEl(parent) {
  parent.textContent = '';
}

/** Spinner element for loading states. */
function modalSpinner(text) {
  const s = el('div', 'modal-spinner');
  s.appendChild(document.createTextNode(text || 'Loading...'));
  return s;
}

/** Status row: dot + label + optional detail. */
function modalStatusRow(name, status, detail) {
  const row = el('div', 'modal-row');
  row.appendChild(el('span', 'modal-status-dot ' + status));
  row.appendChild(el('span', 'modal-row-label', name));
  if (detail) row.appendChild(el('span', 'modal-row-detail', detail));
  return row;
}

// ── Factory ─────────────────────────────────────────────────────

/**
 * Create the modal system with injected dependencies.
 *
 * @param {Object} deps
 * @param {Element} deps.modalBackdrop  - the backdrop overlay element
 * @param {Element} deps.modalContainer - the modal container element
 * @param {Element} deps.modalTitle     - the title element inside the modal
 * @param {Element} deps.modalBody      - the body/content element
 * @param {Element} deps.modalFooter    - the footer element
 * @param {Element} deps.modalCloseBtn  - the close button element
 * @param {Element} deps.canvas         - the terminal canvas (for re-focus after dismiss)
 * @param {Function} deps.postMessage   - vscode.postMessage or equivalent
 * @param {Function} deps.dismissPopup  - dismiss any open popup menu
 * @param {Function} deps.getPrefs      - () => { prefBorderEnabled, prefBorderOpacity, ... }
 * @param {Function} deps.setPrefs      - (partial) => void — updates pref state + calls WASM
 * @param {Function} deps.getTerminal   - () => terminal instance (or null)
 */
export function createModalSystem({
  modalBackdrop,
  modalContainer,
  modalTitle,
  modalBody,
  modalFooter,
  modalCloseBtn,
  canvas,
  postMessage,
  dismissPopup,
  getPrefs,
  setPrefs,
  getTerminal,
  getBackgroundControlMode,
  setBackgroundControlMode,
  getSessionCount,
  onShowThemePicker,
  getCharacters,
  getActiveSpeakMode,
  getProjectSpeakMode,
  hubBaseUrl,
  remoteName,
}) {
  // Fall back to reading from the URL — modals.js loads into the same
  // webview as gpu-terminal.html, so location.search is identical.
  // Lets us skip plumbing the value through the factory call site.
  if (!remoteName) {
    try {
      remoteName = (new URLSearchParams(location.search).get('remote') || '').trim() || null;
    } catch (_) { remoteName = null; }
  }
  // Absolute hub URL for cross-origin fetches (VS Code webview \u2192 hub
  // on 127.0.0.1:1440). Empty string means use relative paths (the
  // standalone Tauri webview is served by the hub, so relative is fine).
  // Either way, prefer hubBaseUrl when supplied so behavior is uniform.
  const HUB = (hubBaseUrl && typeof hubBaseUrl === 'string') ? hubBaseUrl.replace(/\/$/, '') : '';

  // Remote-aware config URL helpers. When `remoteName` is set, route
  // through the hub's remote proxy endpoints \u2014 the read goes to the
  // remote box's hub via SSH-cat; the write proxies a PUT via SSH-curl.
  // For local tabs (no remoteName), use the direct endpoints unchanged.
  function configReadUrl(qs) {
    const q = qs ? '?' + qs : '';
    return remoteName
      ? HUB + '/api/v1/remotes/' + encodeURIComponent(remoteName) + '/config' + q
      : HUB + '/api/v1/config' + q;
  }
  function configProjectWriteUrl() {
    return remoteName
      ? HUB + '/api/v1/remotes/' + encodeURIComponent(remoteName) + '/config/project'
      : HUB + '/api/v1/config/project';
  }
  function configPreferencesWriteUrl() {
    return remoteName
      ? HUB + '/api/v1/remotes/' + encodeURIComponent(remoteName) + '/config/preferences'
      : HUB + '/api/v1/config/preferences';
  }
  let activeModal = null;
  let modalStack = [];  // stack of parent modals to return to on dismiss
  let pomodoroModule = null; // set by setPomodoroModule() after lazy import

  // ── Core show/dismiss ──

  function showModal(kind, _fromStack) {
    if (dismissPopup) dismissPopup();
    if (!_fromStack) modalStack = [];  // fresh open — clear any stale stack
    activeModal = kind;
    modalBody.textContent = '';
    modalFooter.textContent = '';
    modalFooter.style.display = 'none';
    const titles = {
      diagnostics: 'Diagnostics',
      services: 'Services',
      insights: 'Insights',
      license: 'License',
      logs: 'Session Logs',
      wizard: 'Setup Wizard',
      appearance: 'Personalize',
      'session-summary': 'Session Summary',
      'shelved-sessions': 'Shelved Sessions',
      'pomodoro': 'Pomodoro Focus',
      'settings': 'Settings',
      'performance': 'Performance',
      'session-info': 'Session Info',
      'digest-llm': 'Digest LLM',
    };
    // If opened from stack (sub-page), add back button
    if (_fromStack && modalStack.length > 0) {
      modalTitle.textContent = '';
      const backBtn = el('span', 'modal-back-btn', '\u2190');
      backBtn.addEventListener('click', dismissModal);
      modalTitle.appendChild(backBtn);
      modalTitle.appendChild(document.createTextNode(' ' + (titles[kind] || kind)));
    } else {
      modalTitle.textContent = titles[kind] || kind;
    }
    modalBackdrop.classList.add('visible');
    modalContainer.classList.add('visible');
    if (kind === 'diagnostics') renderDiagnosticsModal();
    else if (kind === 'insights') renderInsightsModal();
    else if (kind === 'services') renderServicesModal();
    else if (kind === 'license') renderLicenseModal();
    else if (kind === 'logs') renderLogsModal();
    else if (kind === 'wizard') renderWizardModal();
    else if (kind === 'wizard-vendors') renderWizardVendorsOnly();
    else if (kind === 'appearance') renderAppearanceModal();
    else if (kind === 'session-summary') renderSessionSummaryModal();
    else if (kind === 'shelved-sessions') renderShelvedSessionsModal();
    else if (kind === 'settings') renderSettingsModal();
    else if (kind === 'performance') renderPerformanceModal();
    else if (kind === 'session-info') renderSessionInfoModal();
    else if (kind === 'pomodoro' && pomodoroModule) pomodoroModule.renderInModal(modalBody, modalFooter);
    else if (kind === 'digest-llm') renderDigestLlmModal();
    else if (kind === 'digest-llm-test') renderDigestLlmTestModal();
  }

  function dismissModal() {
    if (modalStack.length > 0) {
      // Return to parent modal instead of closing entirely
      const parent = modalStack.pop();
      showModal(parent, true);
      return;
    }
    // Detach pomodoro renderer so it stops updating modal DOM
    if (activeModal === 'pomodoro' && pomodoroModule) pomodoroModule.detachModal();
    modalBackdrop.classList.remove('visible');
    modalContainer.classList.remove('visible');
    activeModal = null;
    // Refocus the hidden keyboard input. canvas.focus() is a no-op (canvas
    // isn't focusable), and without this kbInput stays unfocused after a
    // modal closes — its keydown listener (Cmd+Option+V image-paste,
    // Cmd+E/D wizards, all terminal shortcuts) never fires until the user
    // manually clicks back into the canvas. window.focusInput is set by
    // gpu-terminal.html.
    if (typeof window !== 'undefined' && typeof window.focusInput === 'function') {
      try { window.focusInput(); } catch { /* best effort */ }
    } else if (canvas && canvas.focus) {
      canvas.focus();
    }
  }

  function isActive() {
    return activeModal !== null;
  }

  // Wire up event listeners
  modalCloseBtn.addEventListener('click', dismissModal);
  modalBackdrop.addEventListener('click', dismissModal);
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && activeModal) { e.preventDefault(); dismissModal(); }
  });

  // ── Appearance Modal ──

  function renderAppearanceModal() {
    postMessage({ type: 'get-preferences' });
    const prefs = getPrefs();
    const terminal = getTerminal();

    const section = el('div', 'modal-section');

    function addToggleRow(label, value, onChange) {
      const row = el('div', 'modal-row');
      row.appendChild(el('span', 'modal-row-label', label));
      const toggle = el('div', 'modal-toggle' + (value ? ' on' : ''));
      row.appendChild(toggle);
      row.style.cursor = 'pointer';
      row.addEventListener('click', () => {
        const newVal = !toggle.classList.contains('on');
        toggle.classList.toggle('on', newVal);
        onChange(newVal);
      });
      section.appendChild(row);
      return toggle;
    }

    // ── Speak Mode (AI character) ──
    // Project-default character. Per-session override is via sidebar
    // right-click; this control only sets the fallback for sessions
    // without an override, and the default for future terminals.
    if (typeof getCharacters === 'function') {
      const characters = getCharacters() || {};
      const charIds = Object.keys(characters);
      if (charIds.length > 1) {
        const smHeader = el('div', 'modal-section-header', 'Speak Mode');
        smHeader.style.marginBottom = '4px';
        smHeader.style.fontWeight = '600';
        smHeader.style.fontSize = '11px';
        smHeader.style.textTransform = 'uppercase';
        smHeader.style.opacity = '0.6';
        section.appendChild(smHeader);

        const smHint = el('div', 'modal-row-detail',
          'Project default voice for the AI. Right-click a session in the sidebar to override for just that terminal.');
        smHint.style.marginBottom = '8px';
        smHint.style.fontSize = '11px';
        smHint.style.opacity = '0.7';
        section.appendChild(smHint);

        const current = (typeof getProjectSpeakMode === 'function' && getProjectSpeakMode()) || 'default';
        const smRow = el('div', 'modal-row');
        smRow.appendChild(el('span', 'modal-row-label', 'Character'));
        const smSeg = el('div', 'modal-segmented');
        charIds.forEach(id => {
          const def = characters[id];
          const btn = document.createElement('button');
          btn.textContent = def.label || id;
          btn.title = def.description || '';
          if (id === current) btn.classList.add('active');
          btn.addEventListener('click', () => {
            smSeg.querySelectorAll('button').forEach(b => b.classList.remove('active'));
            btn.classList.add('active');
            postMessage({ type: 'set-speak-mode', mode: id, scope: 'project' });
          });
          smSeg.appendChild(btn);
        });
        smRow.appendChild(smSeg);
        section.appendChild(smRow);
      }
    }

    // ── Planning discipline (plans.enforce) — interim home until the S5b
    // gear. Tri-state: project override On/Off, or inherit the global
    // default. Persists via the hub's shallow-merge project-config PUT
    // (plans: {} clears the override); read at hook RUNTIME so it applies
    // at next session start with no hook reinstall. Values load async from
    // GET /api/v1/config (plansEnforce keys) — same flow as the digest
    // modal; the section stays hidden if the hub predates those keys.
    {
      // Empty position-holder; populated ONLY when the hub reports the
      // plans keys — so no stray .modal-segmented exists before then (the
      // status-bar control must stay the first segmented for tests/UX).
      const pdWrap = el('div');
      section.appendChild(pdWrap);

      let pdProjectDir = null;
      fetch(HUB + '/api/info')
        .then(r => r.json())
        .catch(() => ({}))
        .then(info => {
          pdProjectDir = (info && (info.projectDir || info.project_dir)) || null;
          const qs = pdProjectDir ? 'project_dir=' + encodeURIComponent(pdProjectDir) : '';
          return fetch(configReadUrl(qs));
        })
        .then(r => r.json())
        .then(cfg => {
          // Hub without the plans keys → leave the section hidden (deploy-
          // ordering guard: modals.js can ship before the hub binary).
          if (!cfg || typeof cfg.plansEnforce === 'undefined' || !pdProjectDir) return;
          const pdHeader = el('div', 'modal-section-header', 'Planning');
          pdHeader.style.cssText = 'margin-bottom:4px;font-weight:600;font-size:11px;text-transform:uppercase;opacity:0.6';
          pdWrap.appendChild(pdHeader);
          const pdHint = el('div', 'modal-row-detail',
            'Instructs AI agents to keep a live plan with tagged decisions and comment anchors. Applies at next session start.');
          pdHint.style.cssText = 'margin-bottom:8px;font-size:11px;opacity:0.7';
          pdWrap.appendChild(pdHint);
          const pdRow = el('div', 'modal-row');
          pdRow.appendChild(el('span', 'modal-row-label', 'Plan Discipline'));
          const pdSeg = el('div', 'modal-segmented');
          pdRow.appendChild(pdSeg);
          pdWrap.appendChild(pdRow);
          const globalDefault = !!cfg.globalPlansEnforce;
          const raw = (typeof cfg.projectPlansEnforce === 'boolean') ? cfg.projectPlansEnforce : null;
          const choices = [
            { value: null, label: 'Default (' + (globalDefault ? 'On' : 'Off') + ')' },
            { value: true, label: 'On' },
            { value: false, label: 'Off' },
          ];
          for (const c of choices) {
            const btn = document.createElement('button');
            btn.textContent = c.label;
            if (c.value === raw) btn.classList.add('active');
            btn.addEventListener('click', () => {
              pdSeg.querySelectorAll('button').forEach(b => b.classList.remove('active'));
              btn.classList.add('active');
              fetch(configProjectWriteUrl(), {
                method: 'PUT',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({
                  projectDir: pdProjectDir,
                  // Shallow hub merge: {} replaces the whole plans object →
                  // key absent → inherit global default.
                  plans: c.value === null ? {} : { enforce: c.value },
                }),
              }).catch(err => console.error('[plans-enforce] save failed', err));
            });
            pdSeg.appendChild(btn);
          }
        })
        .catch(err => console.warn('[plans-enforce] config load failed', err));
    }

    // Border toggle
    addToggleRow('Border', prefs.borderEnabled, (v) => {
      setPrefs({ borderEnabled: v });
      postMessage({ type: 'save-preference', key: 'borderEnabled', value: v });
      opacityRow.style.display = v ? 'flex' : 'none';
    });

    // Border opacity slider
    const opacityRow = el('div', 'modal-row');
    opacityRow.style.display = prefs.borderEnabled ? 'flex' : 'none';
    opacityRow.appendChild(el('span', 'modal-row-label', 'Border Opacity'));
    const rangeWrap = el('div', 'modal-range-row');
    rangeWrap.style.flex = '1';
    const slider = document.createElement('input');
    slider.type = 'range';
    slider.className = 'modal-range';
    slider.min = '0';
    slider.max = '100';
    slider.value = String(Math.round(prefs.borderOpacity * 100));
    const pctLabel = el('span', 'modal-range-value', slider.value + '%');
    slider.addEventListener('input', () => {
      const pct = parseInt(slider.value, 10);
      pctLabel.textContent = pct + '%';
      setPrefs({ borderOpacity: pct / 100 });
    });
    slider.addEventListener('change', () => {
      postMessage({ type: 'save-preference', key: 'borderOpacity', value: getPrefs().borderOpacity });
    });
    rangeWrap.appendChild(slider);
    rangeWrap.appendChild(pctLabel);
    opacityRow.appendChild(rangeWrap);
    section.appendChild(opacityRow);

    // Status bar mode (segmented: Always / Auto / Hidden)
    const sbRow = el('div', 'modal-row');
    sbRow.appendChild(el('span', 'modal-row-label', 'Status Bar'));
    const sbSeg = el('div', 'modal-segmented');
    ['always', 'auto', 'hidden'].forEach(mode => {
      const btn = document.createElement('button');
      btn.textContent = mode.charAt(0).toUpperCase() + mode.slice(1);
      if (mode === prefs.statusBarMode) btn.classList.add('active');
      btn.addEventListener('click', () => {
        sbSeg.querySelectorAll('button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        setPrefs({ statusBarMode: mode });
        postMessage({ type: 'save-preference', key: 'statusBarMode', value: mode });
      });
      sbSeg.appendChild(btn);
    });
    sbRow.appendChild(sbSeg);
    section.appendChild(sbRow);

    // Sessions sidebar mode (segmented: Show / Auto-reveal / Collapsed)
    const sessRow = el('div', 'modal-row');
    sessRow.appendChild(el('span', 'modal-row-label', 'Sessions'));
    const sessSeg = el('div', 'modal-segmented');
    const sidebarModes = [
      { value: 'show', label: 'Show' },
      { value: 'auto-reveal', label: 'Auto-reveal' },
      { value: 'collapsed', label: 'Collapsed' },
    ];
    sidebarModes.forEach(({ value, label }) => {
      const btn = document.createElement('button');
      btn.textContent = label;
      if (value === (prefs.sidebarMode || 'show')) btn.classList.add('active');
      btn.addEventListener('click', () => {
        sessSeg.querySelectorAll('button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        setPrefs({ sidebarMode: value }); // setViewMode inside persists — no separate post
      });
      sessSeg.appendChild(btn);
    });
    sessRow.appendChild(sessSeg);
    section.appendChild(sessRow);

    // File browser mode (segmented: Show / Auto-reveal / Collapsed /
    // Hidden) — left-side mirror of the sessions sidebar control above.
    // Collapsed keeps the floating expand button; Hidden removes the
    // panel entirely (only this control brings it back). Persistence is
    // PER-PROJECT via the hub's project config (setPrefs → host handler
    // PUTs /api/v1/config/project) — deliberately NOT save-preference,
    // which writes the global config.
    const filesRow = el('div', 'modal-row');
    filesRow.appendChild(el('span', 'modal-row-label', 'Files'));
    const filesSeg = el('div', 'modal-segmented');
    const fileBrowserModes = [
      { value: 'show', label: 'Show' },
      { value: 'auto-reveal', label: 'Auto-reveal' },
      { value: 'collapsed', label: 'Collapsed' },
      { value: 'hidden', label: 'Hidden' },
    ];
    fileBrowserModes.forEach(({ value, label }) => {
      const btn = document.createElement('button');
      btn.textContent = label;
      if (value === (prefs.fileBrowserMode || 'show')) btn.classList.add('active');
      btn.addEventListener('click', () => {
        filesSeg.querySelectorAll('button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        setPrefs({ fileBrowserMode: value });
      });
      filesSeg.appendChild(btn);
    });
    filesRow.appendChild(filesSeg);
    section.appendChild(filesRow);

    // Tasks panel mode (segmented: Show / Auto-reveal / Hidden)
    const tasksRow = el('div', 'modal-row');
    tasksRow.appendChild(el('span', 'modal-row-label', 'Tasks'));
    const tasksSeg = el('div', 'modal-segmented');
    const tasksModes = [
      { value: 'show', label: 'Show' },
      { value: 'auto-reveal', label: 'Auto-reveal' },
      { value: 'hidden', label: 'Hidden' },
    ];
    tasksModes.forEach(({ value, label }) => {
      const btn = document.createElement('button');
      btn.textContent = label;
      if (value === (prefs.tasksMode || 'show')) btn.classList.add('active');
      btn.addEventListener('click', () => {
        tasksSeg.querySelectorAll('button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        setPrefs({ tasksMode: value }); // setViewMode inside persists — no separate post
      });
      tasksSeg.appendChild(btn);
    });
    tasksRow.appendChild(tasksSeg);
    section.appendChild(tasksRow);

    // Workshops panel mode (segmented: Show / Auto-reveal / Hidden).
    // Mirrors Tasks UX. "Auto-reveal" reveals the panel on the first open_workshop
    // event and stays open; "Hidden" overrides everything (panel never renders).
    const workshopsRow = el('div', 'modal-row');
    workshopsRow.appendChild(el('span', 'modal-row-label', 'Workshops'));
    const workshopsSeg = el('div', 'modal-segmented');
    const workshopsModes = [
      { value: 'show', label: 'Show' },
      { value: 'auto-reveal', label: 'Auto-reveal' },
      { value: 'hidden', label: 'Hidden' },
    ];
    workshopsModes.forEach(({ value, label }) => {
      const btn = document.createElement('button');
      btn.textContent = label;
      if (value === (prefs.workshopsMode || 'show')) btn.classList.add('active');
      btn.addEventListener('click', () => {
        workshopsSeg.querySelectorAll('button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        setPrefs({ workshopsMode: value }); // setViewMode inside persists — no separate post
      });
      workshopsSeg.appendChild(btn);
    });
    workshopsRow.appendChild(workshopsSeg);
    section.appendChild(workshopsRow);

    // Animations toggle
    addToggleRow('Animations', prefs.animations, (v) => {
      setPrefs({ animations: v });
      postMessage({ type: 'save-preference', key: 'statusBarAnimations', value: v });
    });

    // AI Expression section header
    const exprHeader = el('div', 'modal-section-header', 'AI Expression');
    exprHeader.style.marginTop = '12px';
    exprHeader.style.marginBottom = '4px';
    exprHeader.style.fontWeight = '600';
    exprHeader.style.fontSize = '11px';
    exprHeader.style.textTransform = 'uppercase';
    exprHeader.style.opacity = '0.6';
    section.appendChild(exprHeader);

    addToggleRow('Expression Effects', prefs.expressionEffects, (v) => {
      setPrefs({ expressionEffects: v });
      postMessage({ type: 'save-preference', key: 'expressionEffects', value: v });
    });

    addToggleRow('Celebrations', prefs.celebrations, (v) => {
      setPrefs({ celebrations: v });
      postMessage({ type: 'save-preference', key: 'celebrations', value: v });
    });

    addToggleRow('Danger Effects', prefs.dangerEffects, (v) => {
      setPrefs({ dangerEffects: v });
      postMessage({ type: 'save-preference', key: 'dangerEffects', value: v });
    });

    addToggleRow('Text Animations', prefs.textAnimations, (v) => {
      setPrefs({ textAnimations: v });
      postMessage({ type: 'save-preference', key: 'textAnimations', value: v });
    });

    modalBody.appendChild(section);
  }

  // ── Diagnostics Modal ──

  function renderDiagnosticsModal() {
    modalBody.appendChild(modalSpinner('Running diagnostics...'));
    postMessage({ type: 'run-diagnostics' });
  }

  function handleDiagnosticsResult(checks) {
    clearEl(modalBody);
    const section = el('div', 'modal-section');
    let pass = 0, warn = 0, fail = 0;
    for (const c of checks) {
      section.appendChild(modalStatusRow(c.name, c.status, c.detail));
      if (c.status === 'pass') pass++;
      else if (c.status === 'warn') warn++;
      else fail++;
    }
    modalBody.appendChild(section);
    modalFooter.style.display = 'block';
    const parts = [];
    if (pass) parts.push(pass + ' passed');
    if (warn) parts.push(warn + ' warning' + (warn > 1 ? 's' : ''));
    if (fail) parts.push(fail + ' failed');
    clearEl(modalFooter);
    const summary = el('span', '', parts.join(' \u00b7 '));
    summary.style.fontSize = '12px';
    summary.style.color = fail ? '#f38ba8' : warn ? '#f9e2af' : '#a6e3a1';
    modalFooter.appendChild(summary);
  }

  // ── Insights Modal ──

  function renderInsightsModal() {
    modalBody.appendChild(modalSpinner('Loading insights...'));
    // Data fetch is triggered by gpu-terminal.html when the modal opens
  }

  function handleInsightsResult(data) {
    if (activeModal !== 'insights') return;
    clearEl(modalBody);

    if (data.error) {
      const msg = data.error.includes('ECONNREFUSED') || data.error.includes('fetch')
        ? 'Memory service unavailable. Run `immorterm serve` to start.'
        : data.error;
      modalBody.appendChild(el('div', 'modal-info error', msg));
      return;
    }

    // Overview cards
    const overview = el('div', 'modal-section');
    const overviewGrid = el('div', '');
    overviewGrid.style.cssText = 'display:grid;grid-template-columns:1fr 1fr;gap:8px;margin-bottom:12px;';

    function statCard(label, value, color) {
      const card = el('div', '');
      card.style.cssText = 'background:rgba(255,255,255,0.05);border-radius:6px;padding:8px 12px;';
      const val = el('div', '');
      val.style.cssText = 'font-size:18px;font-weight:700;color:' + (color || '#cdd6f4') + ';';
      val.textContent = String(value);
      card.appendChild(val);
      card.appendChild(el('div', 'modal-row-detail', label));
      return card;
    }

    const eng = data.engagement || {};
    const rate = eng.overall_rate != null ? eng.overall_rate.toFixed(1) + '%' : '0%';
    overviewGrid.appendChild(statCard('Suggestions Shown', eng.total_shown || 0));
    overviewGrid.appendChild(statCard('Acted On', eng.total_acted_on || 0, '#a6e3a1'));
    overviewGrid.appendChild(statCard('Engagement Rate', rate, '#89b4fa'));
    overviewGrid.appendChild(statCard('Active Guardrails', data.guardrails_active || 0, '#f9e2af'));
    overview.appendChild(overviewGrid);
    modalBody.appendChild(overview);

    // Signal effectiveness
    const signals = eng.by_signal || {};
    const signalKeys = Object.keys(signals);
    if (signalKeys.length > 0) {
      const section = el('div', 'modal-section');
      const hdr = el('div', 'modal-section-header', 'Signal Effectiveness');
      hdr.style.cssText = 'font-weight:600;font-size:11px;text-transform:uppercase;opacity:0.6;margin-bottom:8px;';
      section.appendChild(hdr);

      for (const key of signalKeys) {
        const s = signals[key];
        const signalRate = s.rate != null ? s.rate : (s.shown > 0 ? (s.acted_on / s.shown) * 100 : 0);
        const color = signalRate > 50 ? '#a6e3a1' : signalRate > 20 ? '#f9e2af' : '#f38ba8';
        const row = el('div', 'modal-row');
        row.style.cssText = 'display:flex;align-items:center;gap:8px;';
        const name = el('span', '');
        name.style.cssText = 'flex:1;font-size:12px;';
        name.textContent = key.replace(/_/g, ' ');
        row.appendChild(name);

        // Bar
        const barWrap = el('div', '');
        barWrap.style.cssText = 'flex:2;height:6px;background:rgba(255,255,255,0.08);border-radius:3px;overflow:hidden;';
        const barFill = el('div', '');
        barFill.style.cssText = 'height:100%;border-radius:3px;background:' + color + ';width:' + Math.min(signalRate, 100) + '%;';
        barWrap.appendChild(barFill);
        row.appendChild(barWrap);

        const pct = el('span', '');
        pct.style.cssText = 'font-size:11px;color:' + color + ';min-width:48px;text-align:right;';
        pct.textContent = signalRate.toFixed(1) + '% (' + (s.acted_on || 0) + '/' + (s.shown || 0) + ')';
        row.appendChild(pct);
        section.appendChild(row);
      }
      modalBody.appendChild(section);
    }

    // Failure patterns
    const patterns = data.failure_patterns || [];
    const patSection = el('div', 'modal-section');
    const patHdr = el('div', 'modal-section-header', 'Top Failure Patterns');
    patHdr.style.cssText = 'font-weight:600;font-size:11px;text-transform:uppercase;opacity:0.6;margin-bottom:8px;';
    patSection.appendChild(patHdr);
    if (patterns.length === 0) {
      patSection.appendChild(el('div', 'modal-row-detail', 'No recurring failure patterns detected yet'));
    } else {
      for (const p of patterns) {
        const row = el('div', 'modal-row');
        const badge = el('span', '');
        badge.style.cssText = 'background:rgba(243,139,168,0.2);color:#f38ba8;border-radius:4px;padding:1px 6px;font-size:10px;font-weight:600;margin-right:8px;';
        badge.textContent = 'x' + p.frequency;
        row.appendChild(badge);
        row.appendChild(el('span', '', p.description));
        patSection.appendChild(row);
      }
    }
    modalBody.appendChild(patSection);

    // Memory categories
    const cats = data.memory_categories || [];
    if (cats.length > 0) {
      const catSection = el('div', 'modal-section');
      const catHdr = el('div', 'modal-section-header', 'Memory Categories');
      catHdr.style.cssText = 'font-weight:600;font-size:11px;text-transform:uppercase;opacity:0.6;margin-bottom:8px;';
      catSection.appendChild(catHdr);
      for (const c of cats) {
        const row = el('div', 'modal-row');
        row.appendChild(el('span', 'modal-row-label', c.category));
        const count = el('span', '');
        count.style.cssText = 'font-size:11px;opacity:0.7;';
        count.textContent = String(c.count);
        row.appendChild(count);
        catSection.appendChild(row);
      }
      modalBody.appendChild(catSection);
    }

    // Footer: session + lessons summary
    modalFooter.style.display = 'block';
    clearEl(modalFooter);
    const ss = data.session_summary || {};
    const parts = [];
    if (ss.total_7d != null) parts.push(ss.total_7d + ' sessions (7d)');
    if (ss.active_today != null) parts.push(ss.active_today + ' today');
    if (data.lessons_count) parts.push(data.lessons_count + ' lessons');
    const summary = el('span', '', parts.join(' \u00b7 '));
    summary.style.cssText = 'font-size:12px;color:#a6adc8;';
    modalFooter.appendChild(summary);
  }

  // ── Digest LLM Modal (Phase A T11 — host-agnostic, works in VS Code AND standalone Tauri) ──
  // Routes through hub HTTP (/api/v1/config + PUT /api/v1/config/project) instead
  // of VS Code commands so the standalone immorterm-hub webview can use the same UI.

  // Static model lists per provider — mirrors libs/menu-data DIGEST_MODELS.
  // Order is intentional: subscription-backed CLIs and local engines come
  // first because the user is already paying for them. API providers go last
  // because they bill per token on top of any vendor subscription the user
  // already has. Never reorder so APIs lead — that double-bills users.
  const DIGEST_PROVIDERS = [
    { id: 'anthropic-cli', label: 'Anthropic CLI (claude)',         desc: 'Uses your Claude subscription. Requires `claude` on PATH. Recommended.' },
    { id: 'codex-cli',     label: 'OpenAI Codex CLI',                desc: 'Uses your ChatGPT Plus/Pro/Business sub. Auth via `codex login`.' },
    { id: 'cursor-cli',    label: 'Cursor (cursor-agent)',           desc: 'Uses your Cursor Pro sub — counts against monthly Cursor quota.' },
    { id: 'gemini-cli',    label: 'Google Gemini CLI',               desc: 'Uses your Google account / Gemini Advanced — needs prior `gemini` login.' },
    { id: 'copilot-cli',   label: 'GitHub Copilot CLI',              desc: 'Uses your Copilot Pro/Business/Enterprise sub — counts as premium request.' },
    { id: 'opencode-cli',  label: 'opencode',                        desc: 'Routes to whichever provider opencode has configured.' },
    { id: 'llm-cli',       label: "Simon Willison's `llm`",          desc: 'Universal CLI — routes to whichever vendor sub you have configured.' },
    { id: 'ollama',        label: 'Ollama (local)',                  desc: 'Free, runs on your machine via http://localhost:11434.' },
    { id: 'anthropic-api', label: 'Anthropic API (pay-per-token)',   desc: 'Direct API. Bills your ANTHROPIC_API_KEY on top of any subscription.' },
    { id: 'openai-api',    label: 'OpenAI API (pay-per-token)',      desc: 'Direct API. Bills your OPENAI_API_KEY on top of ChatGPT Plus.' },
    { id: 'gemini-api',    label: 'Gemini API (pay-per-token)',      desc: 'Google AI Studio. Bills your GEMINI_API_KEY on top of Gemini Advanced.' },
  ];
  // Mirrors libs/menu-data DIGEST_MODELS. Verified 2026-05-07 against:
  //   - platform.claude.com/docs/en/about-claude/models
  //   - developers.openai.com/api/docs/models/all
  //   - ai.google.dev/gemini-api/docs/models
  //   - docs.github.com/en/copilot/reference/ai-models/supported-models
  // NEVER include deprecated IDs \u2014 they ship as foot-guns for users who
  // don't track vendor announcements (gpt-5-chat-latest, o3-mini, o4-mini,
  // gemini-3-pro-preview, gemini-pro-latest, claude-sonnet-4-7 (doesn't
  // exist), etc. all removed in this revision).
  const DIGEST_MODELS = {
    'anthropic-cli': ['sonnet', 'haiku', 'opus', 'claude-opus-4-7', 'claude-sonnet-4-6', 'claude-haiku-4-5'],
    'codex-cli':     ['gpt-5.5', 'gpt-5.5-pro', 'gpt-5-mini', 'gpt-5-nano', 'gpt-5.4'],
    'cursor-cli':    ['sonnet', 'opus', 'claude-sonnet-4-6', 'claude-opus-4-7', 'gpt-5.5', 'gpt-5-mini'],
    'gemini-cli':    ['gemini-3.1-pro-preview', 'gemini-3-flash-preview', 'gemini-3.1-flash-lite-preview', 'gemini-2.5-flash', 'gemini-2.5-pro', 'gemini-flash-latest'],
    'copilot-cli':   ['claude-sonnet-4.5', 'claude-sonnet-4.6', 'claude-haiku-4.5', 'gpt-5.4', 'gpt-5-mini'],
    // opencode uses `provider/model-id` format and routes via the
    // models.dev catalog. opencode.ai/docs's "recommended" list admits
    // "not necessarily up to date" \u2014 verified against models.dev/api.json
    // directly (2026-05-07). User's full provider-configured set is
    // available at runtime via `opencode models`.
    'opencode-cli':  ['anthropic/claude-opus-4-7', 'anthropic/claude-sonnet-4-6', 'anthropic/claude-haiku-4-5', 'openai/gpt-5.4', 'openai/gpt-5.4-pro', 'google/gemini-3-pro-preview', 'google/gemini-3-flash-preview', 'opencode/gpt-5.1-codex'],
    'anthropic-api': ['claude-opus-4-7', 'claude-sonnet-4-6', 'claude-haiku-4-5'],
    'openai-api':    ['gpt-5.5', 'gpt-5.5-pro', 'gpt-5-mini', 'gpt-5-nano', 'gpt-5.4'],
    'gemini-api':    ['gemini-3.1-pro-preview', 'gemini-3-flash-preview', 'gemini-3.1-flash-lite-preview', 'gemini-2.5-flash', 'gemini-2.5-pro', 'gemini-flash-latest'],
    'ollama':        [],   // Dynamic \u2014 hub-side `ollama list` ideally; fallback to "Other / type"
    'llm-cli':       [],   // Dynamic \u2014 `llm models list`; fallback to "Other / type"
  };
  const DIGEST_DEFAULT_MODEL = {
    'anthropic-cli': 'sonnet',
    'codex-cli':     'gpt-5.5',
    'cursor-cli':    'sonnet',
    'gemini-cli':    'gemini-3-flash-preview',
    'copilot-cli':   'claude-sonnet-4.5',
    'opencode-cli':  'anthropic/claude-sonnet-4-6',
    'anthropic-api': 'claude-sonnet-4-6',
    'openai-api':    'gpt-5.5',
    'gemini-api':    'gemini-3-flash-preview',
    'ollama':        'llama3.2',
    'llm-cli':       'gpt-5-mini',
  };

  // Delivery method for the digest LLM call.
  //   direct       — call the vendor CLI from the hook process (legacy)
  //   immorterm-p  — run the vendor CLI inside a headless immorterm session
  //                  (subscription-safe; survives `claude -p` deprecation)
  //   auto         — pick immorterm-p when available + provider has a known
  //                  wrap template, else direct (default)
  // Matches IMMORTERM_DIGEST_DELIVERY values consumed by digest-llm-invoke.sh.
  const DIGEST_DELIVERIES = [
    { id: 'auto',        label: 'Auto',        desc: 'Use immorterm-p when available (recommended).' },
    { id: 'immorterm-p', label: 'immorterm-p', desc: 'Always wrap in a headless immorterm session — uses your subscription.' },
    { id: 'direct',      label: 'Direct',      desc: 'Call the vendor CLI in-process (legacy; needed for `claude -p` etc.).' },
  ];

  let _digestState = { provider: 'anthropic-cli', model: 'sonnet', delivery: 'auto', cmdTemplate: '', projectDir: null };

  function renderDigestLlmModal() {
    modalBody.appendChild(modalSpinner('Loading digest LLM config...'));
    // Resolve project_dir via /api/info (the same path other modals use), then
    // GET /api/v1/config?project_dir=... — the hub merges per-project services
    // into the response. Hub change shipped in commit ac61564e+.
    fetch(HUB + '/api/info')
      .then(r => r.json())
      .catch(() => ({}))
      .then(info => {
        const pd = (info && (info.projectDir || info.project_dir)) || '';
        _digestState.projectDir = pd || null;
        const qs = pd ? '?project_dir=' + encodeURIComponent(pd) : '';
        return fetch(configReadUrl(qs.replace(/^\?/, '')));
      })
      .then(r => r.text())  // text first — empty body tolerable; .json() errors on it
      .then(body => {
        let cfg = {};
        try { cfg = body ? JSON.parse(body) : {}; } catch { cfg = {}; }
        const services = (cfg && cfg.services) || {};
        const digest = services.digest || {};
        _digestState.provider = digest.provider || 'anthropic-cli';
        _digestState.model = digest.model || DIGEST_DEFAULT_MODEL[_digestState.provider] || '';
        _digestState.delivery = digest.delivery || 'auto';
        _digestState.cmdTemplate = digest.cmdTemplate || '';
        renderDigestLlmStep1();
      })
      .catch(err => {
        // Even if the hub call failed entirely, render with defaults so the
        // user can still pick + save. Save will fail loudly if projectDir is
        // missing.
        console.warn('[digest-llm] config load failed; rendering with defaults', err);
        _digestState.provider = _digestState.provider || 'anthropic-cli';
        _digestState.model = _digestState.model || DIGEST_DEFAULT_MODEL[_digestState.provider];
        renderDigestLlmStep1();
      });
  }

  function renderDigestLlmStep1() {
    clearEl(modalBody);
    // CSS default for #modal-footer is `display: none` (line 809 of
    // gpu-terminal.css) — must explicitly set 'block' to make footer
    // buttons (Test, Save, Cancel) visible. Empty string falls back
    // to the CSS rule which hides the footer.
    modalFooter.style.display = 'block';
    clearEl(modalFooter);

    const intro = el('div', 'modal-row-detail',
      'Choose which LLM ImmorTerm uses to digest your conversations into memories. ' +
      'This applies regardless of which AI tool you use to code.');
    intro.style.cssText = 'margin-bottom:12px;';
    modalBody.appendChild(intro);

    const providerSection = el('div', 'modal-section');
    providerSection.appendChild(el('div', 'modal-row-label', 'Provider'));
    for (const p of DIGEST_PROVIDERS) {
      const row = el('div', 'modal-row');
      row.style.cssText = 'cursor:pointer;padding:8px;border-radius:4px;' +
        (p.id === _digestState.provider ? 'background:rgba(180,190,254,0.15);' : '');
      const radio = el('span', '', p.id === _digestState.provider ? '\u25c9 ' : '\u25cb ');
      radio.style.cssText = 'margin-right:8px;color:#b4befe;';
      const label = el('span', '', p.label);
      label.style.cssText = 'font-weight:600;';
      const desc = el('div', 'modal-row-detail', p.desc);
      desc.style.cssText = 'margin-left:24px;font-size:11px;color:#a6adc8;';
      row.appendChild(radio);
      row.appendChild(label);
      row.appendChild(desc);
      row.addEventListener('click', () => {
        if (_digestState.provider !== p.id) {
          _digestState.provider = p.id;
          _digestState.model = DIGEST_DEFAULT_MODEL[p.id] || '';
        }
        renderDigestLlmStep1();  // re-render to update selection visual
      });
      providerSection.appendChild(row);
    }
    modalBody.appendChild(providerSection);

    // Model select for chosen provider
    const modelSection = el('div', 'modal-section');
    modelSection.style.marginTop = '16px';
    modelSection.appendChild(el('div', 'modal-row-label', 'Model'));
    const select = document.createElement('select');
    select.style.cssText = 'width:100%;padding:8px;background:#1e1e2e;color:#cdd6f4;border:1px solid #45475a;border-radius:4px;font-family:inherit;';
    const models = DIGEST_MODELS[_digestState.provider] || [];
    if (models.length === 0) {
      // Dynamic provider — give user free-text input
      const input = document.createElement('input');
      input.type = 'text';
      input.value = _digestState.model || '';
      input.placeholder = 'Type model name (e.g. ' + (DIGEST_DEFAULT_MODEL[_digestState.provider] || 'model') + ')';
      input.style.cssText = 'width:100%;padding:8px;background:#1e1e2e;color:#cdd6f4;border:1px solid #45475a;border-radius:4px;font-family:inherit;';
      input.addEventListener('input', () => { _digestState.model = input.value.trim(); });
      modelSection.appendChild(input);
      const hint = el('div', 'modal-row-detail',
        _digestState.provider === 'ollama'
          ? 'Run `ollama list` to see available local models.'
          : 'Run `llm models list` to see configured models.');
      hint.style.cssText = 'font-size:11px;color:#a6adc8;margin-top:4px;';
      modelSection.appendChild(hint);
    } else {
      for (const m of models) {
        const opt = document.createElement('option');
        opt.value = m;
        opt.textContent = m;
        if (m === _digestState.model) opt.selected = true;
        select.appendChild(opt);
      }
      // "Other" escape hatch
      const otherOpt = document.createElement('option');
      otherOpt.value = '__other__';
      otherOpt.textContent = 'Other / custom model name…';
      select.appendChild(otherOpt);
      select.addEventListener('change', () => {
        if (select.value === '__other__') {
          const custom = window.prompt('Enter custom model name:', _digestState.model);
          if (custom) {
            _digestState.model = custom.trim();
          }
          renderDigestLlmStep1();
        } else {
          _digestState.model = select.value;
        }
      });
      modelSection.appendChild(select);
    }
    modalBody.appendChild(modelSection);

    // Delivery method — direct vs immorterm-p wrap. See DIGEST_DELIVERIES above.
    const deliverySection = el('div', 'modal-section');
    deliverySection.style.marginTop = '16px';
    deliverySection.appendChild(el('div', 'modal-row-label', 'Delivery method'));
    for (const d of DIGEST_DELIVERIES) {
      const row = el('div', 'modal-row');
      row.style.cssText = 'cursor:pointer;padding:6px 8px;border-radius:4px;' +
        (d.id === _digestState.delivery ? 'background:rgba(180,190,254,0.15);' : '');
      const radio = el('span', '', d.id === _digestState.delivery ? '◉ ' : '○ ');
      radio.style.cssText = 'margin-right:8px;color:#b4befe;';
      const label = el('span', '', d.label);
      label.style.cssText = 'font-weight:600;';
      const desc = el('div', 'modal-row-detail', d.desc);
      desc.style.cssText = 'margin-left:24px;font-size:11px;color:#a6adc8;';
      row.appendChild(radio);
      row.appendChild(label);
      row.appendChild(desc);
      row.addEventListener('click', () => {
        if (_digestState.delivery !== d.id) {
          _digestState.delivery = d.id;
          renderDigestLlmStep1();
        }
      });
      deliverySection.appendChild(row);
    }
    // Optional custom-command template — only shown when delivery is
    // immorterm-p AND the user wants to override the built-in template
    // for their provider (or supply one for a provider that has none).
    if (_digestState.delivery !== 'direct') {
      const tplWrap = document.createElement('div');
      tplWrap.style.cssText = 'margin-top:8px;';
      const tplLabel = el('div', 'modal-row-detail',
        'Custom command template (optional). Placeholders: {INFILE}, {OUTFILE}, {SESSION_ID}, {SYSTEM_PROMPT}, {MODEL}.');
      tplLabel.style.cssText = 'font-size:11px;color:#a6adc8;margin-bottom:4px;';
      tplWrap.appendChild(tplLabel);
      const tplInput = document.createElement('input');
      tplInput.type = 'text';
      tplInput.value = _digestState.cmdTemplate || '';
      tplInput.placeholder = 'e.g. mycli --prompt-file {INFILE} --output {OUTFILE}';
      tplInput.style.cssText = 'width:100%;padding:6px 8px;background:#1e1e2e;color:#cdd6f4;border:1px solid #45475a;border-radius:4px;font-family:monospace;font-size:11px;';
      tplInput.addEventListener('input', () => { _digestState.cmdTemplate = tplInput.value; });
      tplWrap.appendChild(tplInput);
      deliverySection.appendChild(tplWrap);
    }
    modalBody.appendChild(deliverySection);

    // Test result panel — populated by clicking "Test connection". Lives
    // above the footer so the user can see the verdict next to the
    // Save button without scrolling. Built via safe DOM nodes (textContent
    // + appendChild) so no shim/error string ever lands in innerHTML.
    const testResult = document.createElement('div');
    testResult.className = 'modal-row-detail';
    testResult.style.cssText = 'margin-top:12px;font-size:12px;line-height:1.4;display:none;';
    modalBody.appendChild(testResult);

    function clearChildren(node) {
      while (node.firstChild) { node.removeChild(node.firstChild); }
    }
    function appendCode(parent, text) {
      const c = document.createElement('code');
      c.textContent = text;
      parent.appendChild(c);
    }
    function appendBold(parent, text) {
      const b = document.createElement('b');
      b.textContent = text;
      parent.appendChild(b);
    }
    function showTestVerdict(kind) {
      const color = kind === 'ok' ? '#a6e3a1'
                  : kind === 'err' ? '#f38ba8'
                  : '#94a3b8';
      testResult.style.color = color;
      testResult.style.display = 'block';
    }

    // Footer: Test + Save + Cancel. Test runs the canary against the
    // chosen provider/model via the hub's /api/v1/digest/test endpoint
    // — same shim path the digester uses in production, so any auth /
    // PATH / quota issue surfaces here too.
    const testBtn = el('button', 'modal-btn', 'Test connection');
    testBtn.style.cssText = 'margin-right:8px;';
    const saveBtn = el('button', 'modal-btn primary', 'Save');
    saveBtn.style.cssText = 'margin-right:8px;';
    const cancelBtn = el('button', 'modal-btn', 'Cancel');

    testBtn.addEventListener('click', () => {
      if (!_digestState.model) {
        window.alert('Please choose or enter a model name first.');
        return;
      }
      testBtn.disabled = true;
      saveBtn.disabled = true;
      const originalLabel = testBtn.textContent;
      testBtn.textContent = 'Testing\u2026';

      clearChildren(testResult);
      testResult.appendChild(document.createTextNode(
        'Running canary prompt via digest-llm-invoke.sh \u2014 up to 15s.'));
      showTestVerdict('info');

      fetch(HUB + '/api/v1/digest/test', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          provider: _digestState.provider,
          model: _digestState.model,
          delivery: _digestState.delivery || 'auto',
          cmdTemplate: _digestState.cmdTemplate || '',
        }),
      })
        .then(r => r.json().then(j => ({ status: r.status, body: j })))
        .then(({ status, body }) => {
          clearChildren(testResult);
          if (status >= 400) {
            const msg = (body && body.error) || ('HTTP ' + status);
            testResult.appendChild(document.createTextNode('\u274c Test failed: ' + msg));
            showTestVerdict('err');
            return;
          }
          const seconds = ((body.durationMs || 0) / 1000).toFixed(1);
          if (body.ok) {
            // Render: "\u2705 Got response in 14.1s via anthropic-cli / sonnet"
            //   or:    "\u2705 Got response in 14.1s via anthropic-cli via immorterm-p / sonnet"
            testResult.appendChild(document.createTextNode('\u2705 Got response in ' + seconds + 's via '));
            appendBold(testResult, _digestState.provider);
            const effectiveDelivery = body.delivery || _digestState.delivery;
            if (effectiveDelivery && effectiveDelivery !== 'direct') {
              testResult.appendChild(document.createTextNode(' via '));
              appendBold(testResult, effectiveDelivery);
            }
            testResult.appendChild(document.createTextNode(' / '));
            appendBold(testResult, _digestState.model);
            const excerpt = body.responseExcerpt ? String(body.responseExcerpt).slice(0, 80) : '';
            if (excerpt) {
              testResult.appendChild(document.createTextNode(' \u2014 response: '));
              appendCode(testResult, excerpt);
            }
            showTestVerdict('ok');
          } else {
            testResult.appendChild(document.createTextNode('\u274c Failed after ' + seconds + 's via '));
            appendBold(testResult, _digestState.provider);
            testResult.appendChild(document.createTextNode('. '));
            // Prefer responseExcerpt when present — many CLIs surface
            // actionable hints (auth, login) in the response envelope
            // rather than on stderr.
            const excerpt = (body.responseExcerpt || '').trim();
            const tail = (body.stderrTail || '').trim();
            if (excerpt) {
              testResult.appendChild(document.createTextNode('Provider says: '));
              appendCode(testResult, excerpt.slice(0, 300));
            } else if (tail) {
              testResult.appendChild(document.createTextNode('Error: '));
              appendCode(testResult, tail.slice(-300));
            } else {
              testResult.appendChild(document.createTextNode(
                'No diagnostic output captured. Check shim is installed and provider is reachable.'));
            }
            showTestVerdict('err');
          }
        })
        .catch(err => {
          clearChildren(testResult);
          testResult.appendChild(document.createTextNode(
            '\u274c Network error: ' + (err && err.message ? err.message : String(err))));
          showTestVerdict('err');
        })
        .finally(() => {
          testBtn.disabled = false;
          saveBtn.disabled = false;
          testBtn.textContent = originalLabel;
        });
    });

    saveBtn.addEventListener('click', () => {
      if (!_digestState.model) {
        window.alert('Please choose or enter a model name.');
        return;
      }
      saveBtn.disabled = true;
      testBtn.disabled = true;
      saveBtn.textContent = 'Saving...';
      saveDigestLlm()
        .then(() => {
          saveBtn.textContent = 'Saved \u2713';
          setTimeout(() => dismissModal(), 600);
        })
        .catch(err => {
          saveBtn.disabled = false;
          testBtn.disabled = false;
          saveBtn.textContent = 'Save';
          window.alert('Save failed: ' + err.message);
        });
    });

    cancelBtn.addEventListener('click', dismissModal);
    modalFooter.appendChild(testBtn);
    modalFooter.appendChild(saveBtn);
    modalFooter.appendChild(cancelBtn);
  }

  function saveDigestLlm() {
    if (!_digestState.projectDir) {
      return Promise.reject(new Error('No projectDir — open a workspace first.'));
    }
    const qs = '?project_dir=' + encodeURIComponent(_digestState.projectDir);
    return fetch(configReadUrl(qs.replace(/^\?/, '')))
      .then(r => r.text())
      .then(body => {
        let cfg = {};
        try { cfg = body ? JSON.parse(body) : {}; } catch { cfg = {}; }
        const services = (cfg && cfg.services) ? { ...cfg.services } : {};
        services.digest = {
          provider: _digestState.provider,
          model: _digestState.model,
          delivery: _digestState.delivery || 'auto',
          cmdTemplate: _digestState.cmdTemplate || '',
        };
        return fetch(configProjectWriteUrl(), {
          method: 'PUT',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ services, projectDir: _digestState.projectDir }),
        });
      })
      .then(r => r.json())
      .then(res => {
        if (res && res.error) throw new Error(res.error);
      });
  }

  // ── Digest LLM TEST modal — auto-runs the canary against the saved
  //    services.digest.{provider, model} and shows the verdict only.
  //    No provider/model picker. Reuses POST /api/v1/digest/test, the
  //    same endpoint the configure modal's Test button calls. ──

  function renderDigestLlmTestModal() {
    clearEl(modalBody);
    clearEl(modalFooter);
    // CSS default is display:none — must set 'block' or the footer
    // (Retry / Close / Open configure buttons) is invisible.
    modalFooter.style.display = 'block';
    modalBody.appendChild(modalSpinner('Loading saved digest LLM config\u2026'));

    // Match the digester's actual defaults — see digest-llm-invoke.sh
    // line 602: provider="${IMMORTERM_DIGEST_PROVIDER:-anthropic-cli}",
    // model="${IMMORTERM_DIGEST_MODEL:-sonnet}". When users haven't
    // run the configure picker, the digester silently uses these, so
    // the test modal must too — refusing to test would lie about what
    // the digester is actually doing.
    const SHIM_DEFAULT_PROVIDER = 'anthropic-cli';
    const SHIM_DEFAULT_MODEL = 'sonnet';

    let projectDir = null;
    let provider = null;
    let model = null;
    let delivery = 'auto';
    let cmdTemplate = '';
    let usingDefaults = false;

    function showVerdict(kind, lines) {
      clearEl(modalBody);
      const wrap = document.createElement('div');
      wrap.className = 'modal-section';
      wrap.style.cssText = 'padding:12px;line-height:1.5;';
      const color = kind === 'ok' ? '#a6e3a1'
                  : kind === 'err' ? '#f38ba8'
                  : '#94a3b8';
      wrap.style.color = color;
      for (const ln of lines) {
        if (typeof ln === 'string') {
          wrap.appendChild(document.createTextNode(ln));
          wrap.appendChild(document.createElement('br'));
        } else if (ln && ln.code) {
          const c = document.createElement('code');
          c.textContent = ln.code;
          c.style.cssText = 'display:block;padding:6px;background:#1e1e2e;color:#cdd6f4;border-radius:4px;margin-top:4px;font-size:11px;';
          wrap.appendChild(c);
        } else if (ln && ln.bold) {
          const b = document.createElement('b');
          b.textContent = ln.bold;
          wrap.appendChild(b);
          wrap.appendChild(document.createElement('br'));
        }
      }
      modalBody.appendChild(wrap);

      // Footer: Configure (re-run wizard) + Close.
      clearEl(modalFooter);
      const configureBtn = el('button', 'modal-btn', 'Open configure\u2026');
      configureBtn.style.cssText = 'margin-right:8px;';
      configureBtn.addEventListener('click', () => showModal('digest-llm'));
      const closeBtn = el('button', 'modal-btn primary', 'Close');
      closeBtn.addEventListener('click', dismissModal);
      modalFooter.appendChild(configureBtn);
      modalFooter.appendChild(closeBtn);
    }

    // Shown when the test endpoint returns 403 (no hub) or fetch
    // network-errors. Asks the extension to (re)spawn the sidecar and
    // listens for the response, then auto-re-runs the test. End users
    // never see "cargo run" \u2014 just a spinner + Retry button.
    function showHubFailedWithRetry() {
      clearEl(modalBody);
      const wrap = document.createElement('div');
      wrap.className = 'modal-section';
      wrap.style.cssText = 'padding:12px;line-height:1.5;color:#f38ba8;';
      wrap.appendChild(document.createTextNode(
        '\u274c The local hub isn\u2019t responding. Click Retry below to (re)start it.'
      ));
      modalBody.appendChild(wrap);

      clearEl(modalFooter);
      const retryBtn = el('button', 'modal-btn primary', 'Retry');
      retryBtn.style.cssText = 'margin-right:8px;';
      const closeBtn = el('button', 'modal-btn', 'Close');
      closeBtn.addEventListener('click', dismissModal);
      retryBtn.addEventListener('click', () => {
        retryBtn.disabled = true;
        retryBtn.textContent = 'Retrying\u2026';
        clearEl(modalBody);
        modalBody.appendChild(modalSpinner('Restarting the hub\u2026'));

        let timedOut = false;
        const timer = setTimeout(() => {
          timedOut = true;
          showVerdict('err', [
            '\u274c Hub didn\u2019t respond to the retry within 8s.',
            'Reload the window and try again, or check the ImmorTerm output channel for spawn errors.',
          ]);
        }, 8000);

        const onMsg = (ev) => {
          const data = ev.data || {};
          if (data.type !== 'hub-status') return;
          if (timedOut) return;
          clearTimeout(timer);
          window.removeEventListener('message', onMsg);
          if (data.status && data.status.running) {
            // Hub is back \u2014 re-run the original test.
            runTest();
          } else {
            const reason = (data.status && data.status.reason) || 'Hub failed to start.';
            const details = (data.status && data.status.details) || '';
            const lines = ['\u274c ' + reason];
            if (details) lines.push({ code: details });
            lines.push('Reload the window after fixing the issue.');
            showVerdict('err', lines);
          }
        };
        window.addEventListener('message', onMsg);
        try {
          postMessage({ type: 'retry-hub' });
        } catch (e) {
          clearTimeout(timer);
          window.removeEventListener('message', onMsg);
          showVerdict('err', [
            '\u274c Couldn\u2019t reach the extension to retry.',
            { code: (e && e.message) || String(e) },
          ]);
        }
      });
      modalFooter.appendChild(retryBtn);
      modalFooter.appendChild(closeBtn);
    }

    function runTest() {
      clearEl(modalBody);
      const banner = document.createElement('div');
      banner.style.cssText = 'padding:8px 12px;font-size:12px;color:' +
        (usingDefaults ? '#fab387' : '#a6adc8') + ';';
      banner.textContent = usingDefaults
        ? 'No saved config \u2014 testing the digester\u2019s default: ' + provider + ' / ' + model
        : 'Testing your saved config: ' + provider + ' / ' + model;
      modalBody.appendChild(banner);
      modalBody.appendChild(modalSpinner('Running canary prompt \u2014 up to 15s\u2026'));
      fetch(HUB + '/api/v1/digest/test', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          provider: provider,
          model: model,
          delivery: delivery,
          cmdTemplate: cmdTemplate,
        }),
      })
        // Tolerant parse — the hub may 404 with an empty body if it's
        // running an old binary that pre-dates the digest/test route.
        // r.json() throws "Unexpected end of JSON input" on empty body,
        // which is unhelpful — surface the HTTP status instead.
        .then(r => r.text().then(text => {
          let parsed = null;
          try { parsed = text ? JSON.parse(text) : null; } catch { /* keep null */ }
          return { status: r.status, body: parsed, raw: text };
        }))
        .then(({ status, body, raw }) => {
          if (status === 404 || status === 405) {
            showVerdict('err', [
              '\u274c Hub doesn\u2019t expose the test endpoint.',
              'The running immorterm-hub binary predates the /api/v1/digest/test route. Restart the hub (or relaunch the standalone app) to pick up the latest build.',
              { code: 'POST /api/v1/digest/test \u2192 HTTP ' + status },
            ]);
            return;
          }
          if (status === 403) {
            // 403 here means no hub is reachable (webview\u2019s static-
            // resource server returns 403 for non-allowlisted POSTs).
            // Show a friendly retry — the extension\u2019s sidecar will
            // re-attempt the spawn, surfacing any real reason inline.
            showHubFailedWithRetry();
            return;
          }
          if (status >= 400 || body === null) {
            const msg = (body && body.error) || ('HTTP ' + status + (raw ? ' \u2014 ' + raw.slice(0, 200) : ' (empty body)'));
            showVerdict('err', [
              '\u274c Test request failed.',
              { code: msg },
            ]);
            return;
          }
          const seconds = ((body.durationMs || 0) / 1000).toFixed(1);
          if (body.ok) {
            // Header line: "anthropic-cli / sonnet" or, when wrapped:
            // "anthropic-cli via immorterm-p / sonnet".
            const effective = body.delivery || delivery;
            const headerBold = (effective && effective !== 'direct')
              ? (provider + ' via ' + effective + ' / ' + model)
              : (provider + ' / ' + model);
            const lines = [
              '\u2705 Digest LLM is working.',
              { bold: headerBold },
              'Round-trip in ' + seconds + 's.',
            ];
            if (body.responseExcerpt) {
              lines.push('Sample response:');
              lines.push({ code: String(body.responseExcerpt).slice(0, 200) });
            }
            showVerdict('ok', lines);
          } else {
            const lines = [
              '\u274c Digest LLM failed after ' + seconds + 's.',
              { bold: provider + ' / ' + model },
            ];
            // Prefer responseExcerpt when present — many CLIs (claude
            // "Not logged in", codex auth failures, etc.) emit the
            // actionable hint inside the response envelope rather than
            // on stderr. Fall through to stderr tail, then a generic
            // hint if neither is populated.
            const excerpt = (body.responseExcerpt || '').trim();
            const tail = (body.stderrTail || '').trim();
            if (excerpt) {
              lines.push('Provider says:');
              lines.push({ code: excerpt.slice(0, 400) });
            }
            if (tail) {
              lines.push('Error output:');
              lines.push({ code: tail.slice(-400) });
            }
            if (!excerpt && !tail) {
              lines.push('No diagnostic output captured. Check the shim is installed and the provider is reachable.');
            }
            showVerdict('err', lines);
          }
        })
        .catch(err => {
          // fetch outright failed — almost always means no hub. Same
          // retry path as 403; user gets a button, not a cargo command.
          console.warn('[digest-llm-test] fetch failed:', err);
          showHubFailedWithRetry();
        });
    }

    fetch(HUB + '/api/info')
      .then(r => r.json())
      .catch(() => ({}))
      .then(info => {
        projectDir = (info && (info.projectDir || info.project_dir)) || '';
        const qs = projectDir ? '?project_dir=' + encodeURIComponent(projectDir) : '';
        return fetch(configReadUrl(qs.replace(/^\?/, '')));
      })
      .then(r => r.text())
      .then(body => {
        let cfg = {};
        try { cfg = body ? JSON.parse(body) : {}; } catch { cfg = {}; }
        const services = (cfg && cfg.services) || {};
        const digest = services.digest || {};
        if (digest.provider && digest.model) {
          provider = digest.provider;
          model = digest.model;
          delivery = digest.delivery || 'auto';
          cmdTemplate = digest.cmdTemplate || '';
          usingDefaults = false;
        } else {
          // Mirror what digest-llm-invoke.sh does when env is unset:
          // anthropic-cli + sonnet. The digester is already running
          // with these silently for users who never opened the picker;
          // the test must reveal the same thing.
          provider = SHIM_DEFAULT_PROVIDER;
          model = SHIM_DEFAULT_MODEL;
          usingDefaults = true;
        }
        runTest();
      })
      .catch(err => {
        showVerdict('err', [
          '\u274c Could not load saved digest LLM config.',
          { code: (err && err.message) || String(err) },
        ]);
      });
  }

  // ── Services Modal ──

  function renderServicesModal() {
    modalBody.appendChild(modalSpinner('Loading services...'));
    postMessage({ type: 'get-service-status' });
  }

  function handleServiceStatus(services) {
    clearEl(modalBody);
    for (const svc of services) {
      const card = el('div', 'modal-section');
      const hdr = el('div', 'modal-row');
      hdr.style.marginBottom = '8px';
      hdr.appendChild(el('span', 'modal-status-dot ' + (svc.healthy ? 'pass' : svc.enabled ? 'warn' : 'unknown')));
      const nameEl = el('span', 'modal-row-label');
      const strong = el('strong', '', svc.name);
      nameEl.appendChild(strong);
      hdr.appendChild(nameEl);
      const toggleWrap = el('div', 'modal-toggle-wrap');
      const toggle = el('div', 'modal-toggle' + (svc.enabled ? ' on' : ''));
      toggleWrap.appendChild(toggle);
      toggleWrap.addEventListener('click', () => {
        postMessage({ type: 'service-toggle', serviceId: svc.id, enabled: !svc.enabled });
      });
      hdr.appendChild(toggleWrap);
      card.appendChild(hdr);
      card.appendChild(el('div', 'modal-row-detail', svc.desc));
      const btnRow = el('div', '');
      btnRow.style.cssText = 'display:flex;gap:8px;margin-top:8px;';
      if (svc.canStartStop) {
        const startBtn = el('button', 'modal-btn', 'Start');
        startBtn.disabled = svc.healthy;
        startBtn.addEventListener('click', () => {
          startBtn.disabled = true;
          startBtn.textContent = 'Starting...';
          postMessage({ type: 'service-action', serviceId: svc.id, action: 'start' });
        });
        btnRow.appendChild(startBtn);
        const stopBtn = el('button', 'modal-btn', 'Stop');
        stopBtn.disabled = !svc.healthy;
        stopBtn.addEventListener('click', () => {
          stopBtn.disabled = true;
          stopBtn.textContent = 'Stopping...';
          postMessage({ type: 'service-action', serviceId: svc.id, action: 'stop' });
        });
        btnRow.appendChild(stopBtn);
        const restartBtn = el('button', 'modal-btn', 'Restart');
        restartBtn.disabled = !svc.healthy;
        restartBtn.addEventListener('click', () => {
          restartBtn.disabled = true;
          restartBtn.textContent = 'Restarting...';
          postMessage({ type: 'service-action', serviceId: svc.id, action: 'restart' });
        });
        btnRow.appendChild(restartBtn);
      }
      if (svc.hasGraph !== undefined) {
        const graphWrap = el('div', 'modal-toggle-wrap');
        graphWrap.style.marginLeft = 'auto';
        graphWrap.appendChild(el('span', '', 'Graph'));
        const graphToggle = el('div', 'modal-toggle' + (svc.graphEnabled ? ' on' : ''));
        graphWrap.appendChild(graphToggle);
        graphWrap.addEventListener('click', () => {
          postMessage({ type: 'service-toggle', serviceId: svc.id + ':graph', enabled: !svc.graphEnabled });
        });
        btnRow.appendChild(graphWrap);
      }
      if (svc.hasDashboard) {
        const dashBtn = el('button', 'modal-btn', 'Dashboard');
        dashBtn.addEventListener('click', () => {
          postMessage({ type: 'service-action', serviceId: svc.id, action: 'dashboard' });
        });
        btnRow.appendChild(dashBtn);
      }
      card.appendChild(btnRow);
      modalBody.appendChild(card);
    }
  }

  // ── License Modal ──

  function renderLicenseModal() {
    modalBody.appendChild(modalSpinner('Loading license info...'));
    postMessage({ type: 'get-license-status' });
  }

  function handleLicenseStatus(license) {
    clearEl(modalBody);
    const section = el('div', 'modal-section');
    const badgeRow = el('div', '');
    badgeRow.style.marginBottom = '12px';
    badgeRow.appendChild(el('span', 'modal-badge ' + (license.isPro ? 'pro' : 'free'),
      license.isPro ? (license.tier === 'memory-pro' ? 'Memory Pro' : 'Pro') : 'Free'));
    section.appendChild(badgeRow);

    if (license.isPro) {
      if (license.email) section.appendChild(modalStatusRow('Email', 'pass', license.email));
      if (license.expiresAt) section.appendChild(modalStatusRow('Expires', 'pass', license.expiresAt));
      if (license.key) section.appendChild(modalStatusRow('Key', 'pass', license.key.slice(0, 8) + '...'));
      const btnRow = el('div', '');
      btnRow.style.cssText = 'display:flex;gap:8px;margin-top:12px;';
      const validateBtn = el('button', 'modal-btn', 'Validate');
      validateBtn.addEventListener('click', () => {
        validateBtn.disabled = true;
        validateBtn.textContent = 'Validating...';
        postMessage({ type: 'license-validate' });
      });
      btnRow.appendChild(validateBtn);
      const deactivateBtn = el('button', 'modal-btn danger', 'Deactivate');
      deactivateBtn.addEventListener('click', () => {
        if (confirm('Are you sure you want to deactivate your license?')) {
          deactivateBtn.disabled = true;
          postMessage({ type: 'license-deactivate' });
        }
      });
      btnRow.appendChild(deactivateBtn);
      section.appendChild(btnRow);
    } else {
      section.appendChild(el('div', 'modal-info info',
        'Enter a license key to unlock Pro features \u2014 more themes, graph memory, and priority support.'));
      const inputRow = el('div', '');
      inputRow.style.cssText = 'display:flex;gap:8px;margin-top:12px;';
      const input = el('input', 'modal-input');
      input.type = 'text';
      input.placeholder = 'Paste your license key...';
      input.style.flex = '1';
      inputRow.appendChild(input);
      const activateBtn = el('button', 'modal-btn primary', 'Activate');
      activateBtn.addEventListener('click', () => {
        const key = input.value.trim();
        if (!key) return;
        activateBtn.disabled = true;
        activateBtn.textContent = 'Activating...';
        postMessage({ type: 'license-activate', key });
      });
      inputRow.appendChild(activateBtn);
      section.appendChild(inputRow);
      const linkRow = el('div', '');
      linkRow.style.cssText = 'margin-top:12px;text-align:center;';
      const link = el('a', '', 'View Pricing \u2192');
      link.style.cssText = 'color:#7c3aed;cursor:pointer;font-size:12px;text-decoration:none;';
      link.addEventListener('click', () => postMessage({ type: 'open-external', url: 'https://immorterm.dev/pricing' }));
      linkRow.appendChild(link);
      section.appendChild(linkRow);
    }
    modalBody.appendChild(section);
  }

  // ── Logs Modal ──

  function renderLogsModal() {
    modalBody.appendChild(modalSpinner('Scanning session logs...'));
    postMessage({ type: 'get-session-logs' });
  }

  function handleSessionLogs(logSessions) {
    clearEl(modalBody);
    if (!logSessions || logSessions.length === 0) {
      modalBody.appendChild(el('div', 'modal-info info',
        'No session logs found. Start a terminal session to begin logging.'));
      return;
    }
    const section = el('div', 'modal-section');
    for (const s of logSessions) {
      const row = el('div', 'modal-row');
      row.style.cursor = 'pointer';
      row.appendChild(el('span', 'modal-status-dot ' + (s.alive ? 'pass' : 'unknown')));
      const label = el('span', 'modal-row-label');
      const nameStrong = el('strong', '', s.name);
      label.appendChild(nameStrong);
      if (s.types && s.types.length) {
        const typesSpan = el('span', '', ' ' + s.types.join(' \u00b7 '));
        typesSpan.style.cssText = 'font-size:10px;opacity:0.6;';
        label.appendChild(typesSpan);
      }
      row.appendChild(label);
      const detail = el('span', 'modal-row-detail');
      const parts = [];
      if (s.age) parts.push(s.age);
      if (s.size) parts.push(s.size);
      detail.textContent = parts.join(' \u00b7 ');
      row.appendChild(detail);
      row.addEventListener('click', () => {
        postMessage({ type: 'open-session-log', sessionName: s.name, logType: s.types?.[0] || 'grid' });
      });
      section.appendChild(row);
    }
    modalBody.appendChild(section);
  }

  // ── Wizard Modal ──

  let wizardStep = 0;
  // Phase A: vendor selection step inserted after welcome. Lets users
  // tick which AI tools they use (Claude/Codex/Cursor/Windsurf/Cline/
  // opencode/Gemini/Copilot/Aider). Defaults to all-enabled per the
  // opt-OUT model; users can disable vendors they don\u2019t want config
  // files generated for.
  const WIZARD_STEPS = ['welcome', 'vendors', 'binary', 'docker', 'services', 'license', 'complete'];

  // Vendor catalog for the wizard step. Each vendor card shows a brand
  // icon (Simple Icons CDN, MIT-licensed), display name, "\u2713 Detected"
  // badge if the CLI is on PATH, and "\u2713 Configured" if auth-config
  // files exist. id matches the VendorId in libs/config so toggling
  // maps directly to services.vendors.{id}.enabled.
  const WIZARD_VENDORS = [
    { id: 'claudeCode', name: 'Claude Code',     subtitle: 'Anthropic Claude CLI',         icon: 'claude',          color: 'd97757' },
    { id: 'codex',      name: 'Codex',           subtitle: 'OpenAI Codex CLI',             icon: 'openai',          color: '10a37f' },
    { id: 'cursor',     name: 'Cursor',          subtitle: 'Cursor agent (cursor-agent)',  icon: 'cursor',          color: 'ffffff' },
    { id: 'windsurf',   name: 'Windsurf',        subtitle: 'Codeium Windsurf',             icon: 'codeium',         color: '09b6a2' },
    { id: 'cline',      name: 'Cline',           subtitle: 'Cline VS Code agent',          icon: 'anthropic',       color: '7e6dff' },
    { id: 'opencode',   name: 'opencode',        subtitle: 'sst/opencode (multi-provider)', icon: 'sst',            color: 'eea001' },
    { id: 'gemini',     name: 'Gemini CLI',      subtitle: 'Google Gemini CLI',            icon: 'googlegemini',    color: '4285f4' },
    { id: 'copilot',    name: 'GitHub Copilot',  subtitle: 'Copilot CLI (agentic)',        icon: 'githubcopilot',   color: 'ffffff' },
    { id: 'aider',      name: 'Aider',           subtitle: 'Aider AI pair programmer',     icon: 'python',          color: 'f7b731' },
  ];

  // Wizard-scope state: which vendors are enabled + detection result map.
  let _wizardVendorsEnabled = null;  // Set<vendorId> | null when not yet loaded
  let _wizardVendorsProbed = null;   // {vendorId: {installed, configured, configPath}} | null

  function renderWizardModal() {
    wizardStep = 0;
    renderWizardStep();
  }

  // Standalone re-entry point for the vendor step. Clears probed/enabled
  // state so we re-fetch fresh detection + saved config on each open.
  function renderWizardVendorsOnly() {
    wizardStep = WIZARD_STEPS.indexOf('vendors');
    _wizardVendorsProbed = null;
    _wizardVendorsEnabled = null;
    renderWizardStep();
  }

  function renderWizardStepDots(parent) {
    const dots = el('div', 'modal-step-dots');
    for (let i = 0; i < WIZARD_STEPS.length; i++) {
      dots.appendChild(el('span', 'modal-step-dot' + (i < wizardStep ? ' done' : i === wizardStep ? ' active' : '')));
    }
    parent.appendChild(dots);
  }

  // ── Vendor selection step (Phase A) ──
  //
  // Renders a 3\u00d73 grid of brand-iconed vendor cards. Click a card to
  // toggle enable. Cards with a CLI on PATH show a "Detected" badge;
  // cards with cached auth files show "Configured". On Next, persists
  // services.vendors.{id}.enabled to the per-project config via the hub
  // PUT /api/v1/config/project endpoint. Re-entry from the Sessions
  // popup ("Vendors" entry) jumps users straight to this step.
  function renderWizardVendorsStep() {
    // Self-clearing: this step re-renders itself on card toggles and async
    // probe completion — without this, each re-render appends a second grid.
    clearEl(modalBody);
    renderWizardStepDots(modalBody);

    // Lazy-load detection probe + saved state on first render.
    if (_wizardVendorsProbed === null) {
      modalBody.appendChild(modalSpinner('Detecting installed AI tools\u2026'));
      // Use HUB_BASE_URL so VS Code webview hits 127.0.0.1:1440 cross-
      // origin (same pattern as the digest-llm modal).
      Promise.all([
        fetch(HUB + '/api/v1/vendors/detect').then(r => r.json()).catch(() => ({ vendors: [] })),
        fetch(HUB + '/api/info').then(r => r.json()).catch(() => ({})),
      ]).then(([probe, info]) => {
        _wizardVendorsProbed = {};
        for (const v of (probe.vendors || [])) {
          _wizardVendorsProbed[v.id] = v;
        }
        const pd = (info && (info.projectDir || info.project_dir)) || '';
        const qs = pd ? '?project_dir=' + encodeURIComponent(pd) : '';
        return fetch(configReadUrl(qs.replace(/^\?/, ''))).then(r => r.text());
      }).then(body => {
        let cfg = {};
        try { cfg = body ? JSON.parse(body) : {}; } catch { cfg = {}; }
        const vendors = (cfg && cfg.services && cfg.services.vendors) || {};
        _wizardVendorsEnabled = new Set();
        // Default rule: Claude Code only when config is absent (opt-IN —
        // mirrors defaultVendorsConfig). Stored values are respected.
        for (const v of WIZARD_VENDORS) {
          const stored = vendors[v.id];
          if (stored ? stored.enabled !== false : v.id === 'claudeCode') _wizardVendorsEnabled.add(v.id);
        }
        renderWizardVendorsStep();
      }).catch(err => {
        console.warn('[wizard-vendors] load failed; rendering with Claude-only default', err);
        _wizardVendorsProbed = {};
        _wizardVendorsEnabled = new Set(['claudeCode']);
        renderWizardVendorsStep();
      });
      return;
    }

    // Headline + sub-copy
    const head = document.createElement('div');
    head.style.cssText = 'margin-bottom:14px;';
    const title = el('div', 'modal-row-label', 'Which AI tools do you use?');
    title.style.cssText = 'font-size:15px;font-weight:600;margin-bottom:4px;';
    head.appendChild(title);
    const sub = el('div', 'modal-row-detail',
      'ImmorTerm pre-configures hooks for every selected vendor, so any AI tool you run will feed into memory. ' +
      'Detected = CLI is on PATH. Configured = signed in.');
    sub.style.cssText = 'font-size:12px;color:#a6adc8;line-height:1.4;';
    head.appendChild(sub);
    modalBody.appendChild(head);

    // Grid container
    const grid = document.createElement('div');
    grid.style.cssText = 'display:grid;grid-template-columns:repeat(3, 1fr);gap:10px;';
    for (const v of WIZARD_VENDORS) {
      const probe = _wizardVendorsProbed[v.id] || {};
      const enabled = _wizardVendorsEnabled.has(v.id);

      const card = document.createElement('div');
      card.style.cssText = [
        'border:2px solid ' + (enabled ? '#b4befe' : '#313244'),
        'border-radius:8px',
        'padding:10px 12px',
        'cursor:pointer',
        'background:' + (enabled ? 'rgba(180,190,254,0.08)' : 'rgba(255,255,255,0.02)'),
        'transition:border-color 120ms ease, background 120ms ease',
        'display:flex',
        'flex-direction:column',
        'gap:6px',
        'min-height:74px',
      ].join(';');

      // Top row: icon + name + checkbox state
      const topRow = document.createElement('div');
      topRow.style.cssText = 'display:flex;align-items:center;gap:8px;';

      const icon = document.createElement('img');
      // Simple Icons CDN \u2014 free, MIT, no auth, brand-colored. 24px so
      // the card scales nicely across themes. CSP allows https: img-src.
      icon.src = 'https://cdn.simpleicons.org/' + v.icon + '/' + v.color;
      icon.alt = v.name;
      icon.width = 22;
      icon.height = 22;
      icon.style.cssText = 'flex-shrink:0;';
      icon.onerror = () => {
        // Fallback: replace with a tiny letter badge if the CDN icon
        // doesn\u2019t exist for this vendor (e.g. 'sst' for opencode).
        const fallback = document.createElement('span');
        fallback.textContent = v.name[0];
        fallback.style.cssText = 'display:inline-block;width:22px;height:22px;line-height:22px;text-align:center;font-weight:700;color:#cdd6f4;background:#45475a;border-radius:4px;';
        icon.replaceWith(fallback);
      };
      topRow.appendChild(icon);

      const nameWrap = document.createElement('div');
      nameWrap.style.cssText = 'flex:1;min-width:0;';
      const name = document.createElement('div');
      name.textContent = v.name;
      name.style.cssText = 'font-weight:600;font-size:13px;color:#cdd6f4;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;';
      nameWrap.appendChild(name);
      topRow.appendChild(nameWrap);

      const tickBox = document.createElement('span');
      tickBox.textContent = enabled ? '\u2713' : '';
      tickBox.style.cssText = [
        'flex-shrink:0',
        'width:18px;height:18px;line-height:16px',
        'border:2px solid ' + (enabled ? '#b4befe' : '#585b70'),
        'border-radius:4px',
        'text-align:center',
        'color:' + (enabled ? '#b4befe' : 'transparent'),
        'font-weight:700;font-size:12px',
      ].join(';');
      topRow.appendChild(tickBox);

      card.appendChild(topRow);

      const subline = document.createElement('div');
      subline.style.cssText = 'font-size:10.5px;color:#a6adc8;line-height:1.3;display:flex;gap:6px;flex-wrap:wrap;align-items:center;';
      subline.appendChild(document.createTextNode(v.subtitle));
      if (probe.installed || probe.configured) {
        const badge = document.createElement('span');
        const tag = probe.configured ? '\u2713 Configured' : '\u2713 Detected';
        const bgColor = probe.configured ? '#a6e3a1' : '#fab387';
        badge.textContent = tag;
        badge.style.cssText = 'font-size:9.5px;padding:1px 6px;border-radius:8px;background:rgba(0,0,0,0.3);color:' + bgColor + ';';
        subline.appendChild(badge);
      }
      card.appendChild(subline);

      card.addEventListener('click', () => {
        if (_wizardVendorsEnabled.has(v.id)) {
          _wizardVendorsEnabled.delete(v.id);
        } else {
          _wizardVendorsEnabled.add(v.id);
        }
        renderWizardVendorsStep();
      });

      grid.appendChild(card);
    }
    modalBody.appendChild(grid);

    // Footer: Back + Next. Next persists the selection via PUT
    // /api/v1/config/project (read-merge-write).
    clearEl(modalFooter);
    const row = document.createElement('div');
    row.style.cssText = 'display:flex;justify-content:space-between;align-items:center;gap:8px;';
    const backBtn = el('button', 'modal-btn', 'Back');
    backBtn.addEventListener('click', () => { wizardStep--; renderWizardStep(); });
    row.appendChild(backBtn);

    const summary = document.createElement('span');
    summary.style.cssText = 'font-size:11px;color:#a6adc8;';
    summary.textContent = _wizardVendorsEnabled.size + ' of ' + WIZARD_VENDORS.length + ' enabled';
    row.appendChild(summary);

    const nextBtn = el('button', 'modal-btn primary', 'Save & Next');
    nextBtn.addEventListener('click', () => {
      nextBtn.disabled = true;
      nextBtn.textContent = 'Saving\u2026';
      persistWizardVendors().then(() => {
        wizardStep++;
        renderWizardStep();
      }).catch(err => {
        nextBtn.disabled = false;
        nextBtn.textContent = 'Save & Next';
        window.alert('Save failed: ' + (err && err.message ? err.message : err));
      });
    });
    row.appendChild(nextBtn);
    modalFooter.appendChild(row);
  }

  function persistWizardVendors() {
    // Read-merge-write so we don\u2019t clobber other services.* keys.
    return fetch(HUB + '/api/info')
      .then(r => r.json()).catch(() => ({}))
      .then(info => {
        const pd = (info && (info.projectDir || info.project_dir)) || '';
        if (!pd) return Promise.reject(new Error('No projectDir \u2014 open a workspace first.'));
        const qs = '?project_dir=' + encodeURIComponent(pd);
        return fetch(configReadUrl(qs.replace(/^\?/, '')))
          .then(r => r.text())
          .then(body => {
            let cfg = {};
            try { cfg = body ? JSON.parse(body) : {}; } catch { cfg = {}; }
            const services = (cfg && cfg.services) ? { ...cfg.services } : {};
            const vendors = { ...(services.vendors || {}) };
            for (const v of WIZARD_VENDORS) {
              vendors[v.id] = { enabled: _wizardVendorsEnabled.has(v.id) };
            }
            services.vendors = vendors;
            return fetch(configProjectWriteUrl(), {
              method: 'PUT',
              headers: { 'content-type': 'application/json' },
              body: JSON.stringify({ projectDir: pd, services }),
            });
          })
          .then(r => r.json())
          .then(res => {
            if (res && res.error) throw new Error(res.error);
          });
      });
  }

  function renderWizardStep() {
    clearEl(modalBody);
    modalFooter.style.display = 'block';
    clearEl(modalFooter);
    renderWizardStepDots(modalBody);

    const step = WIZARD_STEPS[wizardStep];
    if (step === 'welcome') {
      modalBody.appendChild(el('div', 'modal-info info',
        'Welcome to ImmorTerm! This wizard will help you set up persistent terminals, AI memory, and MCP gateway.'));
      addWizardNav(null, 'Get Started');
    } else if (step === 'vendors') {
      renderWizardVendorsStep();
    } else if (step === 'binary') {
      modalBody.appendChild(modalSpinner('Checking ImmorTerm binary...'));
      postMessage({ type: 'wizard-check', step: 'binary' });
    } else if (step === 'docker') {
      modalBody.appendChild(modalSpinner('Checking Docker...'));
      postMessage({ type: 'wizard-check', step: 'docker' });
    } else if (step === 'services') {
      modalBody.appendChild(modalSpinner('Loading services...'));
      postMessage({ type: 'wizard-check', step: 'services' });
    } else if (step === 'license') {
      const section = el('div', 'modal-section');
      section.appendChild(el('div', 'modal-section-title', 'License'));
      section.appendChild(el('div', 'modal-info info',
        'ImmorTerm works great on Free tier. Upgrade to Pro for extra themes, graph memory, and priority support.'));
      const inputRow = el('div', '');
      inputRow.style.cssText = 'display:flex;gap:8px;margin-top:12px;';
      const input = el('input', 'modal-input');
      input.type = 'text';
      input.placeholder = 'License key (optional)';
      input.style.flex = '1';
      input.id = 'wizard-license-input';
      inputRow.appendChild(input);
      section.appendChild(inputRow);
      modalBody.appendChild(section);
      addWizardNav('Back', 'Skip / Next');
    } else if (step === 'complete') {
      modalBody.appendChild(el('div', 'modal-info success',
        'Setup complete! Your ImmorTerm environment is ready. Terminal sessions will now persist across VS Code restarts.'));
      clearEl(modalFooter);
      const doneBtn = el('button', 'modal-btn primary', 'Done');
      doneBtn.addEventListener('click', dismissModal);
      modalFooter.appendChild(doneBtn);
    }
  }

  function addWizardNav(backLabel, nextLabel) {
    clearEl(modalFooter);
    const row = el('div', '');
    row.style.cssText = 'display:flex;justify-content:space-between;';
    if (backLabel) {
      const backBtn = el('button', 'modal-btn', backLabel);
      backBtn.addEventListener('click', () => { wizardStep--; renderWizardStep(); });
      row.appendChild(backBtn);
    } else {
      row.appendChild(el('span', ''));
    }
    const nextBtn = el('button', 'modal-btn primary', nextLabel || 'Next');
    nextBtn.addEventListener('click', () => {
      if (WIZARD_STEPS[wizardStep] === 'license') {
        const input = document.getElementById('wizard-license-input');
        const key = input ? input.value.trim() : '';
        if (key) postMessage({ type: 'license-activate', key });
      }
      wizardStep++;
      renderWizardStep();
    });
    row.appendChild(nextBtn);
    modalFooter.appendChild(row);
  }

  function handleWizardCheckResult(step, result) {
    if (WIZARD_STEPS[wizardStep] !== step) return;
    clearEl(modalBody);
    renderWizardStepDots(modalBody);

    const section = el('div', 'modal-section');
    section.appendChild(el('div', 'modal-section-title', step.charAt(0).toUpperCase() + step.slice(1)));

    if (step === 'binary') {
      if (result.found) {
        section.appendChild(modalStatusRow('ImmorTerm Binary', 'pass', result.version || 'Found'));
        if (result.path) section.appendChild(modalStatusRow('Path', 'pass', result.path));
      } else {
        section.appendChild(modalStatusRow('ImmorTerm Binary', 'fail', 'Not found'));
        section.appendChild(el('div', 'modal-info warning',
          'The ImmorTerm binary was not found. Run: brew install immorterm or download from immorterm.dev'));
      }
    } else if (step === 'docker') {
      section.appendChild(modalStatusRow('Docker', result.status === 'running' ? 'pass' : 'fail',
        result.status === 'running' ? 'Running' : result.status === 'installed' ? 'Installed (not running)' : 'Not installed'));
      if (result.status !== 'running') {
        const fixBtn = el('button', 'modal-btn primary', result.status === 'installed' ? 'Start Docker' : 'Install Docker');
        fixBtn.style.marginTop = '8px';
        fixBtn.addEventListener('click', () => {
          if (result.status === 'installed') {
            fixBtn.disabled = true;
            fixBtn.textContent = 'Starting...';
            postMessage({ type: 'wizard-action', action: 'start-docker' });
          } else {
            postMessage({ type: 'open-external', url: 'https://docs.docker.com/get-docker/' });
          }
        });
        section.appendChild(fixBtn);
      }
    } else if (step === 'services') {
      for (const svc of (result.services || [])) {
        section.appendChild(modalStatusRow(svc.name,
          svc.enabled ? (svc.healthy ? 'pass' : 'warn') : 'unknown',
          svc.enabled ? (svc.healthy ? 'Running' : 'Enabled (not running)') : 'Disabled'));
      }
    }
    modalBody.appendChild(section);
    addWizardNav(wizardStep > 0 ? 'Back' : null, 'Next');
  }

  // ── Inline message handlers (keep modal-scoped state internal) ──

  function handleLicenseActionResult(msg) {
    if (msg.success) {
      postMessage({ type: 'get-license-status' });
    } else if (msg.error) {
      if (activeModal === 'license' || activeModal === 'wizard') {
        const errDiv = el('div', 'modal-info error', msg.error);
        modalBody.appendChild(errDiv);
      }
    }
  }

  function handleWizardActionResult() {
    postMessage({ type: 'wizard-check', step: WIZARD_STEPS[wizardStep] });
  }

  // ── Session Summary modal ──

  // Search state
  let searchMode = false;
  let searchQuery = '';
  let searchResults = [];
  let searchOffset = 0;
  let searchHasMore = false;
  let searchLoading = false;
  let searchScopeAll = false; // true = all sessions, false = current immorterm only
  let savedSessionContent = null; // stash the normal session content
  const SEARCH_LIMIT = 10;

  function renderSessionSummaryModal() {
    modalBody.appendChild(modalSpinner('Loading session context...'));
    // Reset search state when modal opens fresh
    searchMode = false;
    searchQuery = '';
    searchResults = [];
    searchOffset = 0;
    savedSessionContent = null;
  }

  // ── Session Info modal ────────────────────────────────────────────
  // Diagnostic dump triggered from the right-click menu on a session tab.
  // Shows: identity (immorterm_id, claude UUID), runtime (active vendor,
  // daemon PID/alive, descendants), files (structured_log_dir, sizes),
  // and ai.jsonl health (event counts, last event). Useful for answering
  // "why is claude_session_id null?" without grepping multiple files.
  function renderSessionInfoModal() {
    modalBody.appendChild(modalSpinner('Collecting session info...'));
  }

  function handleSessionInfoResult(info) {
    if (activeModal !== 'session-info') return;
    clearEl(modalBody);
    if (!info) {
      modalBody.appendChild(el('div', 'modal-error', 'No session info returned'));
      return;
    }
    const root = el('div', 'session-info');
    root.style.fontFamily = 'var(--font-mono, monospace)';
    root.style.fontSize = '12px';
    root.style.lineHeight = '1.5';
    // Read-only diagnostic dump — explicit user-select so click-drag picks
    // up text. Some ancestors apply `user-select: none` for drag handles;
    // override here so all values (UUIDs, paths, PIDs) are copyable.
    root.style.userSelect = 'text';
    root.style.cursor = 'text';

    const fmtBytes = (n) => {
      if (typeof n !== 'number' || n < 0) return '?';
      if (n < 1024) return `${n} B`;
      if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
      return `${(n / 1024 / 1024).toFixed(2)} MB`;
    };
    const dimText = (s) => { const e = el('span', null, s); e.style.opacity = '0.55'; return e; };
    const codeText = (s) => {
      const e = el('span', null, s);
      e.style.fontFamily = 'var(--font-mono, monospace)';
      e.style.background = 'rgba(255,255,255,0.06)';
      e.style.padding = '1px 5px';
      e.style.borderRadius = '3px';
      return e;
    };

    const section = (title) => {
      const wrap = el('div', 'si-section');
      wrap.style.marginBottom = '14px';
      const h = el('div', 'si-section-title', title);
      h.style.fontSize = '10px';
      h.style.textTransform = 'uppercase';
      h.style.letterSpacing = '0.1em';
      h.style.opacity = '0.55';
      h.style.marginBottom = '4px';
      wrap.appendChild(h);
      return wrap;
    };
    const row = (key, value) => {
      const r = el('div', 'si-row');
      r.style.display = 'grid';
      // Wider key column so long keys like `registry.claude_session_id`
      // don't wrap and bleed into the value column. minmax + minWidth:0
      // on cells lets long values (UUIDs, paths) wrap inside their cell.
      r.style.gridTemplateColumns = 'minmax(220px, max-content) minmax(0, 1fr)';
      r.style.gap = '12px';
      r.style.padding = '2px 0';
      r.style.alignItems = 'baseline';
      const k = el('span', null, key);
      k.style.opacity = '0.7';
      k.style.whiteSpace = 'nowrap';
      r.appendChild(k);
      const valWrap = el('span');
      valWrap.style.minWidth = '0';
      valWrap.style.overflowWrap = 'anywhere';
      if (typeof value === 'string' || typeof value === 'number') {
        valWrap.appendChild(codeText(String(value)));
      } else if (value === null || value === undefined) {
        valWrap.appendChild(dimText('—'));
      } else if (value instanceof HTMLElement) {
        valWrap.appendChild(value);
      } else {
        const j = el('span');
        j.textContent = JSON.stringify(value);
        j.style.fontFamily = 'var(--font-mono, monospace)';
        j.style.opacity = '0.85';
        valWrap.appendChild(j);
      }
      r.appendChild(valWrap);
      return r;
    };
    const kv = (sectionEl, key, value) => sectionEl.appendChild(row(key, value));

    // Identity
    const ident = section('Identity');
    kv(ident, 'immorterm_id', info.immortermId);
    kv(ident, 'session_name', info.sessionName);
    kv(ident, 'display_name', info.displayName);
    kv(ident, 'session_type', info.sessionType);
    kv(ident, 'project_dir', info.projectDir);
    kv(ident, 'title_locked', String(info.titleLocked));
    if (info.theme) kv(ident, 'theme', info.theme);
    if (info.speakMode) kv(ident, 'speak_mode', info.speakMode);
    root.appendChild(ident);

    // Vendor / AI
    const vendor = section('AI / Vendor');
    const vendorBadge = el('span');
    if (info.activeVendor) {
      vendorBadge.textContent = info.activeVendor + ' (live in process tree)';
      vendorBadge.style.color = '#7ee787';
    } else {
      vendorBadge.textContent = 'none detected in process tree';
      vendorBadge.style.opacity = '0.55';
    }
    kv(vendor, 'active_vendor', vendorBadge);
    kv(vendor, 'registry.claude_session_id', info.registryClaudeSessionId);
    kv(vendor, 'session-status.claude_resume_id', info.sessionStatusClaudeResumeId);
    if (info.claudeStats) {
      const cs = info.claudeStats;
      const pretty = `pid=${cs.pid ?? '?'} rss=${cs.rss_kb ? Math.round(cs.rss_kb / 1024) + 'MB' : '?'} uptime=${cs.runtime_secs ?? '?'}s`;
      kv(vendor, 'claude_stats', pretty);
    }
    root.appendChild(vendor);

    // State
    const state = section('State');
    kv(state, 'registry_source', info.registrySource);
    kv(state, 'log_dir_status', info.logDirStatus);
    kv(state, 'is_soft_shelved', String(info.isSoftShelved));
    if (info.softInfo) {
      kv(state, 'soft_shelved_at', info.softInfo.softShelvedAt);
      const ttlMin = (info.softInfo.ttlElapsedMs / 60000).toFixed(1);
      kv(state, 'ttl_elapsed', `${ttlMin} min`);
    }
    if (info.sessionStatus) {
      kv(state, 'session_status', info.sessionStatus);
    }
    root.appendChild(state);

    // Process
    const proc = section('Process');
    const pidValue = info.daemonPid != null
      ? `${info.daemonPid} ${info.daemonAlive ? '(alive)' : '(dead)'}`
      : null;
    kv(proc, 'daemon_pid', pidValue);
    kv(proc, 'ws_port', info.wsPort);
    if (Array.isArray(info.descendants) && info.descendants.length > 0) {
      const tree = el('div');
      tree.style.fontFamily = 'var(--font-mono, monospace)';
      tree.style.fontSize = '11px';
      for (const d of info.descendants) {
        const line = el('div', null, `${d.pid}  ${d.cmd}`);
        line.style.opacity = '0.85';
        tree.appendChild(line);
      }
      kv(proc, 'descendants', tree);
    } else {
      kv(proc, 'descendants', dimText(info.daemonAlive ? '(none)' : '(daemon dead)'));
    }
    root.appendChild(proc);

    // Files
    const files = section('Files');
    kv(files, 'structured_log_dir', info.structuredLogDir);
    if (info.files) {
      for (const [name, stat] of Object.entries(info.files)) {
        if (!stat) {
          kv(files, name, dimText('not present'));
        } else {
          const s = stat;
          const desc = `${fmtBytes(s.bytes)} · ${new Date(s.mtime).toLocaleString()}`;
          kv(files, name, desc);
        }
      }
    }
    root.appendChild(files);

    // AI extractor health (key signal for "why no UUID")
    if (info.aiJsonlInfo) {
      const ai = section('ai.jsonl health');
      if (info.aiJsonlInfo.error) {
        kv(ai, 'error', info.aiJsonlInfo.error);
      } else {
        kv(ai, 'total_lines', info.aiJsonlInfo.totalLines);
        if (info.aiJsonlInfo.eventCounts) {
          const ec = Object.entries(info.aiJsonlInfo.eventCounts)
            .map(([k, v]) => `${k}=${v}`).join(', ');
          kv(ai, 'event_counts', ec || dimText('(empty)'));
        }
        if (info.aiJsonlInfo.lastEvent) {
          const le = info.aiJsonlInfo.lastEvent;
          kv(ai, 'last_event', `${le.event} role=${le.role || '-'} ts=${le.ts || '-'}`);
          if (le.contentPreview) {
            const p = el('span', null, le.contentPreview);
            p.style.opacity = '0.7';
            kv(ai, 'last_content', p);
          }
        }
      }
      root.appendChild(ai);
    }

    modalBody.appendChild(root);
  }

  /** Render the search bar at the top of the session summary modal body. */
  function renderSearchBar(container) {
    const bar = el('div', 'ss-search-bar');
    const input = el('input', 'ss-search-input');
    input.type = 'text';
    input.placeholder = 'Search memories...';
    input.value = searchQuery;
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && input.value.trim()) {
        e.preventDefault();
        searchQuery = input.value.trim();
        searchOffset = 0;
        searchResults = [];
        enterSearchMode();
        performSearch();
      }
    });
    bar.appendChild(input);
    container.prepend(bar);
    requestAnimationFrame(() => input.focus());
  }

  /** Build the scope toggle button. */
  function buildScopeToggle() {
    const btn = el('button', 'ss-search-scope');
    btn.textContent = searchScopeAll ? 'All' : 'This session';
    btn.title = searchScopeAll ? 'Searching all sessions — click to search this session only' : 'Searching this session — click to search all';
    btn.addEventListener('click', () => {
      searchScopeAll = !searchScopeAll;
      btn.textContent = searchScopeAll ? 'All' : 'This session';
      btn.title = searchScopeAll ? 'Searching all sessions — click to search this session only' : 'Searching this session — click to search all';
      // Re-search if there's a query
      if (searchQuery) {
        searchOffset = 0;
        searchResults = [];
        const container = document.getElementById('ss-search-results');
        if (container) { container.textContent = ''; container.appendChild(modalSpinner('Searching...')); }
        performSearch();
      }
    });
    return btn;
  }

  /** Switch to search results mode, stashing the current session content. */
  function enterSearchMode() {
    if (!searchMode) {
      savedSessionContent = document.createDocumentFragment();
      while (modalBody.firstChild) savedSessionContent.appendChild(modalBody.firstChild);
      searchMode = true;
    }
    clearEl(modalBody);

    // Back button + scope toggle + search input
    const bar = el('div', 'ss-search-bar');
    const backBtn = el('button', 'ss-search-back');
    backBtn.textContent = '\u2190 Back';
    backBtn.addEventListener('click', exitSearchMode);
    bar.appendChild(backBtn);
    bar.appendChild(buildScopeToggle());

    const input = el('input', 'ss-search-input');
    input.type = 'text';
    input.placeholder = 'Search memories...';
    input.value = searchQuery;
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && input.value.trim()) {
        e.preventDefault();
        searchQuery = input.value.trim();
        searchOffset = 0;
        searchResults = [];
        enterSearchMode();
        performSearch();
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        exitSearchMode();
      }
    });
    bar.appendChild(input);
    modalBody.appendChild(bar);
    requestAnimationFrame(() => input.focus());

    // Results container
    const resultsContainer = el('div', 'ss-search-results');
    resultsContainer.id = 'ss-search-results';
    modalBody.appendChild(resultsContainer);
  }

  /** Exit search mode and restore the stashed session content. */
  function exitSearchMode() {
    searchMode = false;
    searchQuery = '';
    searchResults = [];
    searchOffset = 0;
    clearEl(modalBody);
    if (savedSessionContent) {
      modalBody.appendChild(savedSessionContent);
      savedSessionContent = null;
    }
    renderSearchBar(modalBody);
  }

  /** Send search request to the extension backend. */
  function performSearch() {
    searchLoading = true;
    const container = document.getElementById('ss-search-results');
    if (container && searchOffset === 0) {
      container.textContent = '';
      container.appendChild(modalSpinner('Searching...'));
    }
    postMessage({
      type: 'memory-search',
      query: searchQuery,
      limit: SEARCH_LIMIT,
      offset: searchOffset,
      scopeAll: searchScopeAll,
    });
  }

  /** Handle search results from the extension. */
  function handleMemorySearchResult(data) {
    searchLoading = false;
    const container = document.getElementById('ss-search-results');
    if (!container) return;

    if (searchOffset === 0) container.textContent = '';

    // Remove existing timing / load-more
    const existingBtn = container.querySelector('.ss-search-load-more');
    if (existingBtn) existingBtn.remove();

    if (data.error) {
      container.appendChild(el('div', 'ss-search-status', data.error));
      return;
    }

    // Timing badge
    if (data.elapsed_ms !== undefined && searchOffset === 0) {
      const timing = el('div', 'ss-search-timing');
      timing.textContent = data.count + ' result' + (data.count !== 1 ? 's' : '') + ' in ' + data.elapsed_ms + 'ms';
      container.appendChild(timing);
    }

    const results = data.results || [];
    if (results.length === 0 && searchOffset === 0) {
      container.appendChild(el('div', 'ss-search-status', 'No memories found.'));
      return;
    }

    searchResults = searchResults.concat(results);
    searchHasMore = results.length >= SEARCH_LIMIT;

    for (const mem of results) {
      container.appendChild(renderSearchResult(mem));
    }

    if (searchHasMore) {
      const loadMoreBtn = el('button', 'ss-search-load-more', 'Load more...');
      loadMoreBtn.addEventListener('click', () => {
        searchOffset += SEARCH_LIMIT;
        loadMoreBtn.textContent = 'Loading...';
        loadMoreBtn.disabled = true;
        performSearch();
      });
      container.appendChild(loadMoreBtn);
    }
  }

  /** Simple markdown-to-DOM renderer for memory content. */
  function renderMarkdown(text) {
    const container = el('div', 'ss-search-md');
    const lines = text.split('\n');
    let currentList = null;

    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed) {
        currentList = null;
        continue;
      }
      // Headings
      const headingMatch = trimmed.match(/^(#{1,4})\s+(.+)$/);
      if (headingMatch) {
        currentList = null;
        const level = Math.min(headingMatch[1].length + 2, 6); // h3-h6 inside card
        const heading = el('h' + level, 'ss-search-md-heading', headingMatch[2]);
        container.appendChild(heading);
        continue;
      }
      // List items
      if (trimmed.startsWith('- ') || trimmed.startsWith('* ')) {
        if (!currentList) {
          currentList = el('ul', 'ss-search-md-list');
          container.appendChild(currentList);
        }
        const li = el('li', '', trimmed.slice(2));
        currentList.appendChild(li);
        continue;
      }
      // Regular paragraph
      currentList = null;
      container.appendChild(el('p', 'ss-search-md-p', trimmed));
    }
    return container;
  }

  /** Render a single search result as a collapsible card. */
  function renderSearchResult(mem) {
    const card = el('div', 'ss-search-result');
    const header = el('div', 'ss-search-result-header');

    const chevron = el('span', 'ss-chevron', '\u25B6');
    header.appendChild(chevron);

    // Category badge in header (visible when collapsed)
    const headerType = mem.type || (mem.metadata && mem.metadata.categories && mem.metadata.categories[0]) || mem.category;
    if (headerType) {
      header.appendChild(el('span', 'ss-category-badge', headerType));
    }

    const text = mem.content || mem.data || mem.memory || mem.text || '';
    // For session summaries, show session_title or extract first real content line
    let previewText;
    if (mem.type === 'session_summary') {
      const title = mem.session_title || (mem.metadata && mem.metadata.session_title);
      if (title) {
        previewText = title;
      } else {
        // Skip markdown headings (## Goals, ## Done) — find first content line
        const lines = text.split('\n');
        const contentLine = lines.find(l => l.trim() && !l.startsWith('#') && !l.startsWith('---')) || lines[0];
        previewText = contentLine.replace(/^[-*]\s*/, '').trim();
      }
    } else {
      const firstLine = text.split('\n')[0];
      previewText = firstLine;
    }
    const preview = el('span', 'ss-search-result-text', previewText.slice(0, 100) + (previewText.length > 100 ? '...' : ''));
    header.appendChild(preview);

    // Right side: score + timestamp
    const headerRight = el('span', 'ss-search-header-right');
    if (mem.score !== undefined) {
      const pct = Math.min(Math.round(mem.score * 100), 999);
      headerRight.appendChild(el('span', 'ss-search-result-score', pct + '%'));
    }
    if (mem.created_at) {
      headerRight.appendChild(el('span', 'ss-timestamp', formatTimestamp(mem.created_at)));
    }
    header.appendChild(headerRight);

    card.appendChild(header);

    // Body — markdown-rendered content + metadata
    const body = el('div', 'ss-search-result-body');
    const hasMarkdown = text.includes('## ') || text.includes('- ') || text.includes('\n');
    if (hasMarkdown) {
      body.appendChild(renderMarkdown(text));
    } else {
      body.appendChild(el('div', '', text));
    }

    // Metadata badges row — prefer metadata.categories (array) over top-level category
    const meta = el('div', 'ss-search-meta');
    const cats = (mem.metadata && mem.metadata.categories) || (mem.category ? [mem.category] : []);
    for (const cat of cats) meta.appendChild(el('span', 'ss-category-badge', cat));
    if (mem.type) meta.appendChild(el('span', 'ss-category-badge', mem.type));
    if (mem.created_at) {
      meta.appendChild(el('span', 'ss-timestamp', formatTimestamp(mem.created_at)));
    }
    if (meta.childNodes.length > 0) body.appendChild(meta);

    // IDs row — memory_id, session_id (Claude Code UUID), immorterm_id
    const ids = el('div', 'ss-search-ids');
    if (mem.id) ids.appendChild(el('span', 'ss-search-id', 'mem:' + mem.id.slice(0, 8)));
    const sessionId = mem.session_id || (mem.metadata && mem.metadata.session_id);
    if (sessionId) ids.appendChild(el('span', 'ss-search-id', 'claude:' + sessionId.slice(0, 8)));
    const immortermId = mem.immorterm_id || (mem.metadata && mem.metadata.immorterm_id);
    if (immortermId) ids.appendChild(el('span', 'ss-search-id', 'term:' + immortermId));
    // Session title (injected by extension from session lookup)
    if (mem.session_title) ids.appendChild(el('span', 'ss-search-session-title', mem.session_title));
    if (ids.childNodes.length > 0) body.appendChild(ids);

    card.appendChild(body);

    header.addEventListener('click', () => {
      chevron.classList.toggle('open');
      body.classList.toggle('open');
    });

    return card;
  }

  /** Format ISO timestamp to relative time string. */
  function formatTimestamp(iso) {
    if (!iso) return '';
    const diff = Date.now() - new Date(iso).getTime();
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return 'just now';
    if (mins < 60) return mins + 'm ago';
    const hrs = Math.floor(mins / 60);
    if (hrs < 24) return hrs + 'h ago';
    const days = Math.floor(hrs / 24);
    if (days < 7) return days + 'd ago';
    return new Date(iso).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  /** Create a collapsible section with clickable header. */
  function createCollapsible(title, contentElements, defaultOpen) {
    const wrapper = el('div', 'ss-collapsible');
    const header = el('div', 'ss-collapsible-header');
    const chevron = el('span', 'ss-chevron' + (defaultOpen ? ' open' : ''), '\u25B6');
    header.appendChild(chevron);
    header.appendChild(document.createTextNode(title));
    wrapper.appendChild(header);
    const body = el('div', 'ss-collapsible-body' + (defaultOpen ? ' open' : ''));
    for (const child of contentElements) body.appendChild(child);
    wrapper.appendChild(body);
    header.addEventListener('click', () => {
      chevron.classList.toggle('open');
      body.classList.toggle('open');
    });
    return wrapper;
  }

  function handleSessionSummaryResult(data) {
    if (activeModal !== 'session-summary') return;
    clearEl(modalBody);

    if (data.error) {
      const msg = data.error.includes('ECONNREFUSED') || data.error.includes('fetch')
        ? 'Memory service unavailable. Run `immorterm serve` to start.'
        : data.error;
      modalBody.appendChild(el('div', 'modal-info error', msg));
      return;
    }

    const hasSummary = data.summary && data.summary !== 'null';
    const hasFacts = data.facts && data.facts.length > 0;
    const hasDecisions = data.pending_decisions && data.pending_decisions.length > 0;
    const hasGlance = data.at_a_glance && data.at_a_glance.length > 0;
    const hasPrompts = data.recent_prompts && data.recent_prompts.length > 0;
    const hasHistory = data.title_history && data.title_history.length > 1;
    const hasTasks = data.tasks && data.tasks.length > 0;

    if (!hasSummary && !hasFacts && !hasDecisions && !hasGlance && !hasTasks) {
      modalBody.appendChild(el('div', 'ss-empty', 'No session data yet. Start working and check back.'));
      renderSearchBar(modalBody);
      return;
    }

    // Override modal title with session title if available
    if (data.title) {
      modalTitle.textContent = data.title;
    }

    // Extract timeline from structured summary and show in header
    const oldBadge = modalTitle.parentElement.querySelector('.ss-timeline-badge');
    if (oldBadge) oldBadge.remove();
    if (hasSummary && data.summary.includes('## Timeline')) {
      const tlMatch = data.summary.match(/## Timeline\n([^\n#]+)/);
      if (tlMatch) {
        const timelineText = tlMatch[1].replace(/^- /, '').trim();
        const badge = el('span', 'ss-timeline-badge', timelineText);
        // Insert after title, before close button
        modalTitle.after(badge);
      }
    }

    // At a Glance — always visible, not collapsible
    if (hasGlance) {
      const card = el('div', 'ss-at-a-glance');
      const list = el('ul', 'ss-glance-list');
      for (const bullet of data.at_a_glance) {
        list.appendChild(el('li', 'ss-glance-item', bullet));
      }
      card.appendChild(list);
      modalBody.appendChild(card);
    }

    // Title History — collapsible, default closed
    if (hasHistory) {
      const rows = data.title_history.map(entry => {
        const row = el('div', 'ss-title-history-row');
        row.appendChild(el('span', 'ss-th-title', entry.title));
        const time = entry.since
          ? formatTimestamp(entry.since) + ' \u2013 ' + formatTimestamp(entry.at)
          : formatTimestamp(entry.at);
        row.appendChild(el('span', 'ss-th-time', time));
        return row;
      });
      modalBody.appendChild(createCollapsible(
        'Title History \u00B7 ' + data.title_history.length, rows, false
      ));
    }

    // Summary — render structured sections or fallback to plain text
    if (hasSummary) {
      const isStructured = data.summary.includes('## ');
      if (isStructured) {
        // Parse markdown sections into individual collapsibles
        const sections = data.summary.split(/^## /m).filter(Boolean);
        for (const section of sections) {
          const nlIdx = section.indexOf('\n');
          const heading = nlIdx > 0 ? section.slice(0, nlIdx).trim() : section.trim();
          if (heading === 'Timeline') continue; // rendered in header
          const body = nlIdx > 0 ? section.slice(nlIdx + 1).trim() : '';
          if (!body) continue;
          const lines = body.split('\n').filter(l => l.trim());
          const items = lines.map(line => {
            const text = line.replace(/^- /, '');
            return el('div', 'ss-summary-bullet', text);
          });
          const label = heading + ' \u00B7 ' + lines.length;
          modalBody.appendChild(createCollapsible(label, items, false));
        }
      } else {
        // Legacy prose fallback
        const content = [el('div', 'ss-summary-text', data.summary)];
        modalBody.appendChild(createCollapsible('Summary', content, false));
      }
    }

    // Recent Prompts — collapsible, default closed
    if (hasPrompts) {
      const items = data.recent_prompts.map(p => el('div', 'ss-prompt-item', p));
      modalBody.appendChild(createCollapsible(
        'Recent Prompts \u00B7 ' + data.recent_prompts.length, items, false
      ));
    }

    // Tasks — collapsible, default closed
    if (hasTasks) {
      const items = data.tasks.map(t => {
        const item = el('div', 'ss-task-item');
        const dot = el('span', 'ss-decision-dot');
        dot.classList.add(t.status || 'planned');
        item.appendChild(dot);
        const rawSubject = t.subject || t.content || '';
        const cleanSubject = rawSubject
          .replace(/^TASK\s*#\d+:\s*/i, '')
          .replace(/\s*\[(completed|planned|in_progress|pending)\]\s*/gi, '')
          .replace(/\s*—\s*$/, '');
        item.appendChild(el('span', 'ss-task-subject', cleanSubject));
        const ts = t.status === 'completed' ? (t.updated_at || t.created_at) : t.created_at;
        if (ts) item.appendChild(el('span', 'ss-timestamp', formatTimestamp(ts)));
        return item;
      });
      modalBody.appendChild(createCollapsible(
        'Tasks \u00B7 ' + data.task_count, items, false
      ));
    }

    // Key Facts — collapsible, default closed
    if (hasFacts) {
      const label = data.has_more_facts
        ? 'Key Facts \u00B7 ' + data.fact_count + ' of ' + data.total_facts
        : 'Key Facts \u00B7 ' + data.fact_count;
      const items = data.facts.map(fact => {
        const item = el('div', 'ss-fact-item');
        item.style.display = 'flex'; item.style.alignItems = 'baseline';
        if (fact.category) {
          item.appendChild(el('span', 'ss-category-badge', fact.category));
        }
        item.appendChild(el('span', '', fact.content));
        if (fact.created_at) item.appendChild(el('span', 'ss-timestamp', formatTimestamp(fact.created_at)));
        return item;
      });
      modalBody.appendChild(createCollapsible(label, items, false));
    }

    // Pending Decisions — collapsible, default closed
    if (hasDecisions) {
      const items = data.pending_decisions.map(d => {
        const item = el('div', 'ss-decision-item');
        const dot = el('span', 'ss-decision-dot');
        dot.classList.add(d.status || 'planned');
        item.appendChild(dot);
        item.appendChild(el('span', '', d.content));
        const ts = d.created_at;
        if (ts) item.appendChild(el('span', 'ss-timestamp', formatTimestamp(ts)));
        return item;
      });
      modalBody.appendChild(createCollapsible(
        'Pending Decisions \u00B7 ' + data.decision_count, items, false
      ));
    }

    // Add search bar at the top of the content
    renderSearchBar(modalBody);
  }

  // ── Shelved Sessions Modal ──

  function renderShelvedSessionsModal() {
    modalBody.appendChild(modalSpinner('Loading shelved sessions...'));
    postMessage({ type: 'get-shelved-sessions' });
  }

  function handleShelvedSessionsResult(sessions) {
    clearEl(modalBody);

    if (!sessions || sessions.length === 0) {
      const empty = el('div', 'modal-section');
      empty.appendChild(el('p', 'modal-empty-text', 'No shelved sessions'));
      const hint = el('p', 'modal-hint-text', 'Click the \u00D7 button on a session tab to shelve it. Shelved sessions can be reattached later with their scrollback and Claude conversation restored.');
      hint.style.opacity = '0.6';
      hint.style.fontSize = '12px';
      hint.style.marginTop = '8px';
      empty.appendChild(hint);
      modalBody.appendChild(empty);
      return;
    }

    // Sort by shelvedAt descending (most recent first)
    sessions.sort((a, b) => (b.shelvedAt || 0) - (a.shelvedAt || 0));

    const list = el('div', 'modal-section');
    for (const s of sessions) {
      const row = el('div', 'modal-row shelved-row');
      row.style.padding = '8px 12px';
      row.style.borderRadius = '6px';
      row.style.transition = 'background 0.15s';
      row.style.position = 'relative';

      // Left side: type badge + name
      const left = el('div', 'shelved-left');
      left.style.display = 'flex';
      left.style.alignItems = 'center';
      left.style.gap = '8px';
      left.style.flex = '1';
      left.style.minWidth = '0';

      const typeLabel = s.sessionType === 'ai' ? '[AI]' : '[Screen]';
      const badge = el('span', 'shelved-type-badge', typeLabel);
      badge.style.fontSize = '10px';
      badge.style.opacity = '0.5';
      badge.style.fontFamily = 'monospace';
      badge.style.flexShrink = '0';
      left.appendChild(badge);

      const nameEl = el('span', 'modal-row-label', s.name);
      nameEl.style.overflow = 'hidden';
      nameEl.style.textOverflow = 'ellipsis';
      nameEl.style.whiteSpace = 'nowrap';
      left.appendChild(nameEl);

      // Claude indicator
      if (s.claudeSessionId) {
        const claude = el('span', 'shelved-claude-badge', '\u2728');
        claude.title = 'Claude conversation will resume';
        claude.style.flexShrink = '0';
        left.appendChild(claude);
      }

      row.appendChild(left);

      // Right side: time + action buttons
      const right = el('div', 'shelved-right');
      right.style.display = 'flex';
      right.style.alignItems = 'center';
      right.style.gap = '6px';
      right.style.flexShrink = '0';

      const timeStr = formatShelvedTime(s.shelvedAt);
      const detail = el('span', 'modal-row-detail', timeStr);
      detail.style.opacity = '0.5';
      detail.style.fontSize = '11px';
      right.appendChild(detail);

      // Button container — all buttons show on hover
      const btnGroup = el('div', 'shelved-btn-group');
      btnGroup.style.cssText = 'opacity:0; transition:opacity 0.15s; display:flex; gap:4px; align-items:center;';

      const btnStyle = 'padding:3px 8px; border:1px solid rgba(255,255,255,0.2); border-radius:4px; background:rgba(255,255,255,0.08); color:inherit; cursor:pointer; font-size:11px; white-space:nowrap;';

      // Summary button
      const summaryBtn = el('button', 'shelved-summary-btn', '\u{1F4CB}');
      summaryBtn.title = 'View session summary';
      summaryBtn.style.cssText = btnStyle;
      summaryBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        // Push current modal onto stack so dismiss returns here
        modalStack.push('shelved-sessions');
        // Switch modal context so handleSessionSummaryResult accepts the result
        activeModal = 'session-summary';
        modalTitle.textContent = 'Session Summary';
        clearEl(modalBody);
        modalBody.appendChild(modalSpinner('Loading session summary...'));
        postMessage({ type: 'get-session-summary-by-id', windowId: s.windowId });
      });
      btnGroup.appendChild(summaryBtn);

      // Delete button
      const deleteBtn = el('button', 'shelved-delete-btn', '\u{1F5D1}');
      deleteBtn.title = 'Delete session permanently';
      deleteBtn.style.cssText = btnStyle;
      deleteBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        // VS Code webviews block window.confirm() — use inline confirmation
        if (deleteBtn.dataset.confirming) return;
        deleteBtn.dataset.confirming = '1';
        const origText = deleteBtn.textContent;
        deleteBtn.textContent = 'Sure?';
        deleteBtn.style.background = 'rgba(255,80,80,0.3)';
        deleteBtn.style.borderColor = 'rgba(255,80,80,0.5)';
        // Click again to confirm, or revert after 3s
        const revert = () => {
          delete deleteBtn.dataset.confirming;
          deleteBtn.textContent = origText;
          deleteBtn.style.background = 'rgba(255,255,255,0.08)';
          deleteBtn.style.borderColor = 'rgba(255,255,255,0.2)';
        };
        const timer = setTimeout(revert, 3000);
        deleteBtn.addEventListener('click', function onConfirm(e2) {
          e2.stopPropagation();
          deleteBtn.removeEventListener('click', onConfirm);
          clearTimeout(timer);
          row.style.opacity = '0.3';
          row.style.pointerEvents = 'none';
          postMessage({ type: 'delete-shelved-session', windowId: s.windowId });
        }, { once: true });
      });
      btnGroup.appendChild(deleteBtn);

      // Restore button
      const restoreBtn = el('button', 'shelved-restore-btn', '\u25B6 Restore');
      restoreBtn.style.cssText = btnStyle;
      restoreBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        row.style.opacity = '0.5';
        row.style.pointerEvents = 'none';
        restoreBtn.textContent = 'Restoring...';
        postMessage({ type: 'reattach-session', windowId: s.windowId });
        dismissModal();
      });
      btnGroup.appendChild(restoreBtn);

      right.appendChild(btnGroup);
      row.appendChild(right);

      // Hover effects — show buttons, no row-click action
      row.addEventListener('mouseenter', () => {
        row.style.background = 'rgba(255,255,255,0.05)';
        btnGroup.style.opacity = '1';
      });
      row.addEventListener('mouseleave', () => {
        row.style.background = 'none';
        btnGroup.style.opacity = '0';
      });

      list.appendChild(row);
    }

    modalBody.appendChild(list);
  }

  function formatShelvedTime(ts) {
    if (!ts) return '';
    const diff = Math.floor(Date.now() / 1000) - ts;
    if (diff < 60) return 'just now';
    if (diff < 3600) return Math.floor(diff / 60) + 'm ago';
    if (diff < 86400) return Math.floor(diff / 3600) + 'h ago';
    const days = Math.floor(diff / 86400);
    if (days < 7) return days + 'd ago';
    return new Date(ts * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  // ── Settings Hub Modal ──

  function renderSettingsModal() {
    const section = el('div', 'modal-section');

    const items = [
      { icon: '\u2728', label: 'Personalize', desc: 'Speak mode, border, status bar, sidebar', kind: 'appearance' },
      { icon: '\uD83C\uDFA8', label: 'Theme', desc: 'Terminal color scheme', kind: 'theme-picker' },
      { icon: '\uD83D\uDCC2', label: 'Shelved Sessions', desc: 'Restore or delete shelved sessions', kind: 'shelved-sessions' },
      { icon: '\u26A1', label: 'Performance', desc: 'Background session memory mode', kind: 'performance' },
      { icon: '\uD83D\uDD11', label: 'License', desc: 'Manage your license key', kind: 'license' },
    ];

    for (const item of items) {
      const row = el('div', 'settings-row');
      row.appendChild(el('span', 'settings-icon', item.icon));
      const text = el('div', 'settings-text');
      text.appendChild(el('div', 'settings-label', item.label));
      text.appendChild(el('div', 'settings-desc', item.desc));
      row.appendChild(text);
      row.appendChild(el('span', 'settings-chevron', '\u203A'));
      row.addEventListener('click', () => {
        if (item.kind === 'theme-picker') {
          dismissModal();
          if (onShowThemePicker) setTimeout(onShowThemePicker, 50);
          return;
        }
        modalStack.push('settings');
        showModal(item.kind, true);
      });
      section.appendChild(row);
    }

    modalBody.appendChild(section);
  }

  // ── Performance Modal ──

  function renderPerformanceModal() {
    const section = el('div', 'modal-section');

    // Background Mode segmented control
    const modeRow = el('div', 'modal-row');
    modeRow.appendChild(el('span', 'modal-row-label', 'Background Mode'));
    const modeSeg = el('div', 'modal-segmented');
    const isControl = getBackgroundControlMode ? getBackgroundControlMode() : true;

    const modes = [
      { value: true, label: 'Control' },
      { value: false, label: 'Raw' },
    ];
    modes.forEach(({ value, label }) => {
      const btn = document.createElement('button');
      btn.textContent = label;
      if (value === isControl) btn.classList.add('active');
      btn.addEventListener('click', () => {
        modeSeg.querySelectorAll('button').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        if (setBackgroundControlMode) setBackgroundControlMode(value);
        updateStats();
      });
      modeSeg.appendChild(btn);
    });
    modeRow.appendChild(modeSeg);
    section.appendChild(modeRow);

    // Description
    const desc = el('div', 'modal-help-text');
    const controlStrong = el('strong', null, 'Control');
    const rawStrong = el('strong', null, 'Raw');
    desc.appendChild(controlStrong);
    desc.appendChild(document.createTextNode(' (recommended): background sessions receive metadata only. Prevents WASM memory growth that causes V8 crashes. '));
    desc.appendChild(rawStrong);
    desc.appendChild(document.createTextNode(': all sessions maintain live terminal state in WASM. Uses more memory.'));
    section.appendChild(desc);

    // Stats
    const statsDiv = el('div', 'performance-stats');
    section.appendChild(statsDiv);

    function updateStats() {
      statsDiv.textContent = '';
      const count = getSessionCount ? getSessionCount() : 0;
      const mode = getBackgroundControlMode ? getBackgroundControlMode() : true;
      statsDiv.appendChild(el('div', 'modal-stat-row', 'Active Sessions: ' + count));
      statsDiv.appendChild(el('div', 'modal-stat-row', 'WASM Terminals: ' + (mode ? '1 (active only)' : count)));
    }
    updateStats();

    modalBody.appendChild(section);
  }

  // ── Public API ──

  return {
    showModal,
    dismissModal,
    isActive,
    handleDiagnosticsResult,
    handleInsightsResult,
    handleServiceStatus,
    handleLicenseStatus,
    handleLicenseActionResult,
    handleSessionLogs,
    handleWizardCheckResult,
    handleWizardActionResult,
    handleSessionSummaryResult,
    handleMemorySearchResult,
    handleShelvedSessionsResult,
    handleSessionInfoResult,
    setPomodoroModule(mod) { pomodoroModule = mod; },
  };
}
