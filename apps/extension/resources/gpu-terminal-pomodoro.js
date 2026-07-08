/**
 * GPU Terminal — Pomodoro Focus Tracker.
 *
 * Self-contained, IDE-agnostic module: timer engine, state management,
 * all modal tab renderers, floating pill, Web Audio sounds, and animations.
 *
 * The host (VS Code, standalone, web) provides only two messages:
 *   - pomodoro-load-state  (host → webview, on init)
 *   - pomodoro-save-state  (webview → host, on state change)
 *
 * Imported by gpu-terminal.html via dynamic import (same pattern as modals).
 */

// ── Pure helpers ────────────────────────────────────────────────

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function clearEl(parent) { parent.textContent = ''; }

function uid() {
  return Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
}

function pad2(n) { return String(n).padStart(2, '0'); }

function fmtTime(seconds) {
  const m = Math.floor(Math.max(0, seconds) / 60);
  const s = Math.max(0, seconds) % 60;
  return pad2(m) + ':' + pad2(s);
}

function todayStr() {
  const d = new Date();
  return d.getFullYear() + '-' + pad2(d.getMonth() + 1) + '-' + pad2(d.getDate());
}

/** Build a DOM tree from a descriptor: [tag, {attrs}, ...children|text] */
function h(tag, attrs, ...children) {
  const e = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (k === 'className') e.className = v;
      else if (k === 'style' && typeof v === 'object') Object.assign(e.style, v);
      else if (k.startsWith('on')) e.addEventListener(k.slice(2).toLowerCase(), v);
      else e.setAttribute(k, v);
    }
  }
  for (const c of children) {
    if (c == null) continue;
    if (typeof c === 'string' || typeof c === 'number') e.appendChild(document.createTextNode(String(c)));
    else e.appendChild(c);
  }
  return e;
}

// ── Defaults ────────────────────────────────────────────────────

// pillMode: 'always' | 'hidden' | 'auto-reveal' | 'minimal' | 'periodic'
const DEFAULT_SETTINGS = {
  workDuration: 1500,       // 25 min
  shortBreakDuration: 300,  // 5 min
  longBreakDuration: 900,   // 15 min
  longBreakInterval: 4,
  soundEnabled: true,
  autoStartBreak: false,
  pillMode: 'always',
};

const DEFAULT_TIMER = {
  phase: 'idle',
  remaining: 0,
  totalDuration: 0,
  activeTaskId: null,
  pomodorosCompletedToday: 0,
  consecutivePomodoros: 0,
  running: false,
  startedAt: null,
};

function defaultData() {
  return {
    version: 1,
    tasks: [],
    settings: { ...DEFAULT_SETTINGS },
    timer: { ...DEFAULT_TIMER },
    history: [],
  };
}

// ── Sound Engine (Web Audio API) ────────────────────────────────

function createSoundEngine() {
  let ctx = null;
  let muted = false;

  function getCtx() {
    if (!ctx) ctx = new (window.AudioContext || window.webkitAudioContext)();
    if (ctx.state === 'suspended') ctx.resume();
    return ctx;
  }

  function tone(freq, duration, type, volume, delay) {
    if (muted) return;
    const c = getCtx();
    const t = c.currentTime + (delay || 0);
    const osc = c.createOscillator();
    const gain = c.createGain();
    osc.type = type || 'sine';
    osc.frequency.value = freq;
    gain.gain.setValueAtTime(volume || 0.25, t);
    gain.gain.exponentialRampToValueAtTime(0.001, t + duration);
    osc.connect(gain).connect(c.destination);
    osc.start(t);
    osc.stop(t + duration);
  }

  return {
    chime()   { tone(880, 0.15); tone(1100, 0.2, 'sine', 0.25, 0.16); },
    ping()    { tone(660, 0.12, 'triangle', 0.2); },
    fanfare() { tone(523, 0.15); tone(659, 0.15, 'sine', 0.25, 0.16); tone(784, 0.25, 'sine', 0.3, 0.32); },
    click()   { tone(1000, 0.03, 'square', 0.08); },
    setMuted(m) { muted = m; },
    isMuted()   { return muted; },
  };
}

// ── Confetti animation ──────────────────────────────────────────

function spawnConfetti(container) {
  const colors = ['#7c3aed', '#a6e3a1', '#f9e2af', '#89b4fa', '#f38ba8', '#E0B0FF'];
  for (let i = 0; i < 24; i++) {
    const p = el('div', 'pomo-confetti');
    p.style.left = (15 + Math.random() * 70) + '%';
    p.style.setProperty('--fall-x', (Math.random() - 0.5) * 120 + 'px');
    p.style.background = colors[i % colors.length];
    p.style.animationDelay = (Math.random() * 0.4) + 's';
    p.style.width = (4 + Math.random() * 4) + 'px';
    p.style.height = (4 + Math.random() * 4) + 'px';
    container.appendChild(p);
    setTimeout(() => p.remove(), 2500);
  }
}

// ── SVG Ring builder (safe DOM, no innerHTML) ───────────────────

function buildRingSvg(size, stroke, radius, circumference, offset, color) {
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('width', size);
  svg.setAttribute('height', size);
  svg.setAttribute('viewBox', '0 0 ' + size + ' ' + size);
  svg.setAttribute('class', 'pomo-ring-svg');

  const cx = size / 2, cy = size / 2;

  // Background circle
  const bgCircle = document.createElementNS(NS, 'circle');
  bgCircle.setAttribute('cx', cx);
  bgCircle.setAttribute('cy', cy);
  bgCircle.setAttribute('r', radius);
  bgCircle.setAttribute('fill', 'none');
  bgCircle.setAttribute('stroke', 'rgba(255,255,255,0.08)');
  bgCircle.setAttribute('stroke-width', stroke);
  svg.appendChild(bgCircle);

  // Progress circle
  const progCircle = document.createElementNS(NS, 'circle');
  progCircle.setAttribute('cx', cx);
  progCircle.setAttribute('cy', cy);
  progCircle.setAttribute('r', radius);
  progCircle.setAttribute('fill', 'none');
  progCircle.setAttribute('stroke', color);
  progCircle.setAttribute('stroke-width', stroke);
  progCircle.setAttribute('stroke-linecap', 'round');
  progCircle.setAttribute('stroke-dasharray', circumference);
  progCircle.setAttribute('stroke-dashoffset', offset);
  progCircle.setAttribute('transform', 'rotate(-90 ' + cx + ' ' + cy + ')');
  progCircle.setAttribute('class', 'pomo-ring-progress');
  svg.appendChild(progCircle);

  return svg;
}

// ── Factory ─────────────────────────────────────────────────────

/**
 * Create the Pomodoro system.
 *
 * @param {Object} deps
 * @param {Function} deps.postMessage - send message to host (for file I/O)
 */
export function createPomodoroSystem({ postMessage, onOpenModal }) {

  // ── State ──

  let data = defaultData();
  let tickInterval = null;
  let activeTab = 'timer';
  let fiveMinFired = false;
  let modalBodyEl = null;
  let modalFooterEl = null;
  let pillEl = null;
  const sound = createSoundEngine();

  // ── Persistence ──

  let saveTimeout = null;
  function requestSave() {
    if (saveTimeout) clearTimeout(saveTimeout);
    saveTimeout = setTimeout(() => {
      postMessage({ type: 'pomodoro-save-state', data });
    }, 300);
  }

  function loadState(incoming) {
    if (!incoming || incoming.version !== 1) {
      data = defaultData();
    } else {
      data = incoming;
      data.settings = { ...DEFAULT_SETTINGS, ...data.settings };
      data.timer = { ...DEFAULT_TIMER, ...data.timer };
      if (!data.tasks) data.tasks = [];
      if (!data.history) data.history = [];
    }
    // Recover timer if it was running when webview was destroyed
    if (data.timer.running && data.timer.startedAt) {
      const elapsed = Math.floor((Date.now() - data.timer.startedAt) / 1000);
      const newRemaining = data.timer.totalDuration - elapsed;
      if (newRemaining > 0) {
        data.timer.remaining = newRemaining;
      } else {
        handlePhaseComplete();
      }
    }
    checkDayRollover();
    sound.setMuted(!data.settings.soundEnabled);
    startTickIfNeeded();
    updatePill();
    if (modalBodyEl) renderActiveTab();
  }

  // ── Day rollover ──

  function checkDayRollover() {
    const today = todayStr();
    const lastEntry = data.history.length > 0 ? data.history[data.history.length - 1] : null;
    if (!lastEntry || lastEntry.date !== today) {
      data.timer.pomodorosCompletedToday = 0;
    }
  }

  function accumulateStats(type) {
    const today = todayStr();
    let entry = data.history.find(h => h.date === today);
    if (!entry) {
      entry = { date: today, pomodorosCompleted: 0, focusMinutes: 0, tasksCompleted: 0 };
      data.history.push(entry);
    }
    if (type === 'pomodoro') {
      entry.pomodorosCompleted++;
      entry.focusMinutes += Math.round(data.settings.workDuration / 60);
    } else if (type === 'task') {
      entry.tasksCompleted++;
    }
    if (data.history.length > 30) data.history = data.history.slice(-30);
  }

  // ── Timer engine ──

  function startTickIfNeeded() {
    if (tickInterval) return;
    if (!data.timer.running) return;
    tickInterval = setInterval(tick, 1000);
  }

  function stopTick() {
    if (tickInterval) { clearInterval(tickInterval); tickInterval = null; }
  }

  function tick() {
    if (!data.timer.running) { stopTick(); return; }

    // Wall-clock drift recovery
    if (data.timer.startedAt) {
      const elapsed = Math.floor((Date.now() - data.timer.startedAt) / 1000);
      data.timer.remaining = Math.max(0, data.timer.totalDuration - elapsed);
    } else {
      data.timer.remaining = Math.max(0, data.timer.remaining - 1);
    }

    // 5-minute warning
    if (!fiveMinFired && data.timer.remaining <= 300 && data.timer.remaining > 0 && data.timer.phase === 'work') {
      fiveMinFired = true;
      sound.ping();
      if (pillEl) pillEl.classList.add('warning', 'pulse');
    }

    // Phase complete
    if (data.timer.remaining <= 0) {
      handlePhaseComplete();
      return;
    }

    updatePill();
    if (modalBodyEl && activeTab === 'timer') updateTimerDisplay();
  }

  function handlePhaseComplete() {
    stopTick();
    data.timer.running = false;
    data.timer.startedAt = null;

    if (data.timer.phase === 'work') {
      data.timer.consecutivePomodoros++;
      data.timer.pomodorosCompletedToday++;
      accumulateStats('pomodoro');

      if (data.timer.activeTaskId) {
        const task = data.tasks.find(t => t.id === data.timer.activeTaskId);
        if (task) task.completedPomodoros++;
      }

      sound.chime();
      if (modalBodyEl) spawnConfetti(modalBodyEl);

      if (data.timer.pomodorosCompletedToday % data.settings.longBreakInterval === 0) {
        setTimeout(() => sound.fanfare(), 500);
      }

      const isLongBreak = data.timer.consecutivePomodoros >= data.settings.longBreakInterval;
      if (isLongBreak) {
        data.timer.phase = 'long-break';
        data.timer.totalDuration = data.settings.longBreakDuration;
        data.timer.consecutivePomodoros = 0;
      } else {
        data.timer.phase = 'short-break';
        data.timer.totalDuration = data.settings.shortBreakDuration;
      }
      data.timer.remaining = data.timer.totalDuration;
      fiveMinFired = false;

      if (data.settings.autoStartBreak) {
        data.timer.running = true;
        data.timer.startedAt = Date.now();
        startTickIfNeeded();
      }

    } else {
      sound.chime();
      data.timer.phase = 'idle';
      data.timer.remaining = 0;
      data.timer.totalDuration = 0;
    }

    requestSave();
    updatePill();
    if (modalBodyEl) renderActiveTab();
  }

  // ── Timer controls ──

  function startTimer(taskId) {
    if (data.timer.phase === 'work' && data.timer.running) return; // already running
    if (data.timer.phase === 'work' && !data.timer.running) { resumeTimer(); return; }
    if (taskId) data.timer.activeTaskId = taskId;
    data.timer.phase = 'work';
    data.timer.totalDuration = data.settings.workDuration;
    data.timer.remaining = data.settings.workDuration;
    data.timer.running = true;
    data.timer.startedAt = Date.now();
    fiveMinFired = false;
    sound.click();
    startTickIfNeeded();
    requestSave();
    updatePill();
    if (modalBodyEl) renderActiveTab();
  }

  function pauseTimer() {
    if (!data.timer.running) return;
    data.timer.running = false;
    if (data.timer.startedAt) {
      const elapsed = Math.floor((Date.now() - data.timer.startedAt) / 1000);
      data.timer.remaining = Math.max(0, data.timer.totalDuration - elapsed);
    }
    data.timer.startedAt = null;
    stopTick();
    requestSave();
    updatePill();
    if (modalBodyEl && activeTab === 'timer') renderActiveTab();
  }

  function resumeTimer() {
    if (data.timer.running || data.timer.phase === 'idle') return;
    data.timer.running = true;
    data.timer.totalDuration = data.timer.remaining;
    data.timer.startedAt = Date.now();
    startTickIfNeeded();
    requestSave();
    updatePill();
  }

  function skipPhase() {
    handlePhaseComplete();
  }

  function resetTimer() {
    stopTick();
    const todayCount = data.timer.pomodorosCompletedToday;
    data.timer = { ...DEFAULT_TIMER, pomodorosCompletedToday: todayCount };
    fiveMinFired = false;
    requestSave();
    updatePill();
    if (modalBodyEl) renderActiveTab();
  }

  // ── Task CRUD ──

  function addTask(name, estimate) {
    const task = {
      id: uid(),
      name: name.trim(),
      estimatedPomodoros: estimate || 1,
      completedPomodoros: 0,
      done: false,
      createdAt: Date.now(),
    };
    data.tasks.push(task);
    requestSave();
    return task;
  }

  function editTask(id, updates) {
    const task = data.tasks.find(t => t.id === id);
    if (!task) return;
    if (updates.name !== undefined) task.name = updates.name.trim();
    if (updates.estimatedPomodoros !== undefined) task.estimatedPomodoros = updates.estimatedPomodoros;
    requestSave();
  }

  function removeTask(id) {
    data.tasks = data.tasks.filter(t => t.id !== id);
    if (data.timer.activeTaskId === id) data.timer.activeTaskId = null;
    requestSave();
  }

  function completeTask(id) {
    const task = data.tasks.find(t => t.id === id);
    if (!task) return;
    task.done = !task.done;
    if (task.done) {
      accumulateStats('task');
      sound.click();
    }
    requestSave();
  }

  function setActiveTask(id) {
    data.timer.activeTaskId = id;
    requestSave();
  }

  // ── Settings ──

  function updateSettings(partial) {
    Object.assign(data.settings, partial);
    sound.setMuted(!data.settings.soundEnabled);
    requestSave();
  }

  // ── Floating Pill ──

  let pillUpcoming = null;
  let pillControls = null;
  let pillProximity = false;      // mouse is near bottom-right
  let hoverExtrasTimeout = null;  // delayed hover extras for auto-reveal
  let periodicFlashVisible = false;
  let periodicFlashTimeout = null;
  let lastPeriodicFlash = 0;      // timestamp of last 5-min flash

  function createPill(container) {
    // Wrapper holds upcoming list + pill row
    const wrapper = el('div', 'pomo-pill-wrapper');
    wrapper.style.display = 'none';

    // Upcoming tasks dropdown (above pill, shown on hover)
    pillUpcoming = el('div', 'pomo-pill-upcoming');
    wrapper.appendChild(pillUpcoming);

    // The pill itself
    pillEl = el('div', 'pomodoro-pill');
    pillEl.addEventListener('click', () => {
      if (onOpenModal) onOpenModal();
    });
    wrapper.appendChild(pillEl);

    // Inline controls (right side of pill, shown on hover)
    pillControls = el('div', 'pomo-pill-controls');
    pillEl.appendChild(pillControls);

    // Hover: show controls + upcoming (delayed in auto-reveal mode)
    wrapper.addEventListener('mouseenter', () => {
      const isAutoReveal = data.settings.pillMode === 'auto-reveal';
      if (isAutoReveal) {
        // In auto-reveal, delay extras by 5s so the pill appears alone first
        if (hoverExtrasTimeout) clearTimeout(hoverExtrasTimeout);
        hoverExtrasTimeout = setTimeout(() => {
          wrapper.classList.add('hover');
          updatePillControls();
          updatePillUpcoming();
          hoverExtrasTimeout = null;
        }, 5000);
      } else {
        wrapper.classList.add('hover');
        updatePillControls();
        updatePillUpcoming();
      }
    });
    wrapper.addEventListener('mouseleave', () => {
      if (hoverExtrasTimeout) { clearTimeout(hoverExtrasTimeout); hoverExtrasTimeout = null; }
      wrapper.classList.remove('hover');
    });

    container.appendChild(wrapper);
    // Store wrapper ref on pillEl for display toggling
    pillEl._wrapper = wrapper;
    pillEl._container = container;

    // Mouse proximity detection for auto-reveal mode
    container.addEventListener('mousemove', (e) => {
      if (data.settings.pillMode !== 'auto-reveal') return;
      if (data.timer.phase === 'idle') return;
      const rect = container.getBoundingClientRect();
      const nearRight = (rect.right - e.clientX) < 200;
      const nearBottom = (rect.bottom - e.clientY) < 100;
      const isNear = nearRight && nearBottom;
      if (isNear !== pillProximity) {
        pillProximity = isNear;
        updatePillVisibility();
      }
    });
    container.addEventListener('mouseleave', () => {
      if (pillProximity) { pillProximity = false; updatePillVisibility(); }
    });

    updatePill();
  }

  function updatePillControls() {
    if (!pillControls) return;
    clearEl(pillControls);
    const t = data.timer;

    const pauseBtn = el('button', 'pomo-pill-ctrl-btn', t.running ? '\u23F8' : '\u25B6');
    pauseBtn.title = t.running ? 'Pause' : 'Resume';
    pauseBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      if (t.running) pauseTimer(); else resumeTimer();
    });
    pillControls.appendChild(pauseBtn);

    const skipBtn = el('button', 'pomo-pill-ctrl-btn', '\u23ED');
    skipBtn.title = 'Skip';
    skipBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      skipPhase();
    });
    pillControls.appendChild(skipBtn);
  }

  function updatePillUpcoming() {
    if (!pillUpcoming) return;
    clearEl(pillUpcoming);
    const t = data.timer;
    const upcoming = data.tasks.filter(tk => !tk.done && tk.id !== t.activeTaskId);
    if (upcoming.length === 0) return;

    const label = el('div', 'pomo-pill-upcoming-label', 'Up next');
    pillUpcoming.appendChild(label);
    for (const tk of upcoming.slice(0, 5)) {
      const row = el('div', 'pomo-pill-upcoming-task');
      row.appendChild(el('span', 'pomo-pill-upcoming-name', tk.name));
      if (tk.estimatedPomodoros > 0) {
        row.appendChild(el('span', 'pomo-pill-upcoming-est',
          '\uD83C\uDF45'.repeat(Math.min(tk.estimatedPomodoros, 4))
          + (tk.estimatedPomodoros > 4 ? '+' + (tk.estimatedPomodoros - 4) : '')));
      }
      pillUpcoming.appendChild(row);
    }
  }

  // Stable span refs so tick updates don't rebuild DOM (avoids animation restarts)
  let pillIconSpan = null, pillTimeSpan = null, pillTaskSpan = null;
  let pillBuilt = false; // true once initial spans exist

  function updatePillVisibility() {
    if (!pillEl) return;
    const wrapper = pillEl._wrapper;
    if (!wrapper) return;
    const t = data.timer;
    const mode = data.settings.pillMode || 'always';

    if (t.phase === 'idle' || mode === 'hidden') {
      wrapper.style.display = 'none';
      return;
    }

    switch (mode) {
      case 'always':
      case 'minimal':
        wrapper.style.display = '';
        break;
      case 'auto-reveal':
        wrapper.style.display = pillProximity ? '' : 'none';
        break;
      case 'periodic': {
        const under5 = t.phase === 'work' && t.remaining <= 300 && t.remaining > 0;
        wrapper.style.display = (periodicFlashVisible || under5) ? '' : 'none';
        // Trigger flash every 5 minutes
        const now = Date.now();
        if (!periodicFlashVisible && !under5 && (now - lastPeriodicFlash >= 300000)) {
          periodicFlashVisible = true;
          lastPeriodicFlash = now;
          if (periodicFlashTimeout) clearTimeout(periodicFlashTimeout);
          periodicFlashTimeout = setTimeout(() => {
            periodicFlashVisible = false;
            updatePillVisibility();
          }, 5000); // show for 5 seconds
          wrapper.style.display = '';
        }
        break;
      }
    }
  }

  function updatePill() {
    if (!pillEl) return;
    const t = data.timer;
    const wrapper = pillEl._wrapper;
    const mode = data.settings.pillMode || 'always';

    if (t.phase === 'idle') {
      if (wrapper) wrapper.style.display = 'none';
      pillBuilt = false;
      return;
    }

    updatePillVisibility();
    pillEl.style.display = 'flex';

    const icon = t.phase === 'work' ? '\uD83C\uDF45' : '\u2615';
    const taskName = t.activeTaskId ? (data.tasks.find(tk => tk.id === t.activeTaskId)?.name || '') : '';
    const showTask = mode !== 'minimal';
    const truncName = showTask && taskName.length > 18 ? taskName.slice(0, 16) + '\u2026'
                    : showTask ? taskName : '';
    const pct = t.totalDuration > 0 ? ((t.totalDuration - t.remaining) / t.totalDuration * 100) : 0;

    if (!pillBuilt) {
      // First build — create spans
      clearEl(pillEl);
      pillIconSpan = el('span', 'pomo-pill-icon', icon);
      pillTimeSpan = el('span', 'pomo-pill-time', fmtTime(t.remaining));
      pillTaskSpan = el('span', 'pomo-pill-task', truncName);
      pillEl.appendChild(pillIconSpan);
      pillEl.appendChild(pillTimeSpan);
      if (truncName) pillEl.appendChild(pillTaskSpan);
      if (pillControls) pillEl.appendChild(pillControls);
      pillBuilt = true;
    } else {
      // Update existing spans in place — no DOM churn
      pillIconSpan.textContent = icon;
      pillTimeSpan.textContent = fmtTime(t.remaining);
      if (truncName) {
        pillTaskSpan.textContent = truncName;
        if (!pillTaskSpan.parentNode) pillEl.insertBefore(pillTaskSpan, pillControls);
      } else if (pillTaskSpan.parentNode) {
        pillTaskSpan.remove();
      }
    }
    pillEl.style.setProperty('--pomo-progress', pct + '%');

    if (t.phase === 'work' && t.remaining <= 300 && t.remaining > 0) {
      pillEl.classList.add('warning', 'pulse');
    } else {
      pillEl.classList.remove('warning', 'pulse');
    }
    pillEl.classList.toggle('paused', !t.running);
  }

  // ── Modal Rendering ──

  function renderInModal(bodyEl, footerEl) {
    modalBodyEl = bodyEl;
    modalFooterEl = footerEl;
    renderActiveTab();
  }

  function detachModal() {
    modalBodyEl = null;
    modalFooterEl = null;
  }

  function renderActiveTab() {
    if (!modalBodyEl) return;
    clearEl(modalBodyEl);
    if (modalFooterEl) { clearEl(modalFooterEl); modalFooterEl.style.display = 'none'; }

    // Tab bar
    const tabBar = el('div', 'pomo-tabs');
    const tabs = [
      { id: 'timer', label: 'Timer' },
      { id: 'tasks', label: 'Tasks' },
      { id: 'stats', label: 'Stats' },
      { id: 'settings', label: 'Settings' },
      { id: 'help', label: '?' },
    ];
    for (const tab of tabs) {
      const btn = el('button', 'pomo-tab' + (tab.id === activeTab ? ' active' : ''), tab.label);
      btn.addEventListener('click', () => { activeTab = tab.id; renderActiveTab(); });
      tabBar.appendChild(btn);
    }
    modalBodyEl.appendChild(tabBar);

    // Tab content
    const content = el('div', 'pomo-content');
    if (activeTab === 'timer') renderTimerTab(content);
    else if (activeTab === 'tasks') renderTasksTab(content);
    else if (activeTab === 'stats') renderStatsTab(content);
    else if (activeTab === 'settings') renderSettingsTab(content);
    else if (activeTab === 'help') renderHelpTab(content);
    modalBodyEl.appendChild(content);

    // Break background tint
    modalBodyEl.classList.toggle('pomo-break',
      data.timer.phase === 'short-break' || data.timer.phase === 'long-break');
  }

  // ── Timer Tab ──

  function renderTimerTab(container) {
    const t = data.timer;

    const ringSize = 160;
    const ringStroke = 6;
    const radius = (ringSize - ringStroke) / 2;
    const circumference = 2 * Math.PI * radius;
    const progress = t.totalDuration > 0 ? (t.totalDuration - t.remaining) / t.totalDuration : 0;
    const offset = circumference * (1 - progress);
    const ringColor = t.phase === 'work'
      ? (t.remaining <= 300 ? '#f9e2af' : '#7c3aed')
      : '#89b4fa';

    // Ring wrapper
    const ringWrap = el('div', 'pomo-ring-wrap');
    ringWrap.appendChild(buildRingSvg(ringSize, ringStroke, radius, circumference, offset, ringColor));

    const inner = el('div', 'pomo-ring-inner');
    inner.appendChild(el('div', 'pomo-ring-phase', phaseLabel(t.phase)));
    inner.appendChild(el('div', 'pomo-ring-time', fmtTime(t.remaining)));
    ringWrap.appendChild(inner);
    container.appendChild(ringWrap);

    // Active task label
    if (t.activeTaskId) {
      const task = data.tasks.find(tk => tk.id === t.activeTaskId);
      if (task) {
        const taskLabel = el('div', 'pomo-active-task', task.name);
        taskLabel.addEventListener('click', () => { activeTab = 'tasks'; renderActiveTab(); });
        container.appendChild(taskLabel);
      }
    }

    // Pomodoro dots
    const dotsWrap = el('div', 'pomo-dots');
    const interval = data.settings.longBreakInterval;
    for (let i = 0; i < interval; i++) {
      dotsWrap.appendChild(el('span', 'pomo-dot' + (i < t.consecutivePomodoros ? ' filled' : '')));
    }
    dotsWrap.appendChild(el('span', 'pomo-dot-count', t.pomodorosCompletedToday + ' today'));
    container.appendChild(dotsWrap);

    // Controls
    const controls = el('div', 'pomo-controls');
    if (t.phase === 'idle') {
      const startBtn = el('button', 'modal-btn primary', '\u25B6 Start Focus');
      startBtn.addEventListener('click', () => startTimer());
      controls.appendChild(startBtn);
    } else {
      if (t.running) {
        const pauseBtn = el('button', 'modal-btn', '\u23F8 Pause');
        pauseBtn.addEventListener('click', () => pauseTimer());
        controls.appendChild(pauseBtn);
      } else {
        const resumeBtn = el('button', 'modal-btn primary', '\u25B6 Resume');
        resumeBtn.addEventListener('click', () => resumeTimer());
        controls.appendChild(resumeBtn);
      }
      const skipBtn = el('button', 'modal-btn', '\u23ED Skip');
      skipBtn.addEventListener('click', () => skipPhase());
      controls.appendChild(skipBtn);
      const stopBtn = el('button', 'modal-btn danger', '\u23F9 Stop');
      stopBtn.addEventListener('click', () => resetTimer());
      controls.appendChild(stopBtn);
    }
    container.appendChild(controls);
  }

  function updateTimerDisplay() {
    if (!modalBodyEl) return;
    const timeEl = modalBodyEl.querySelector('.pomo-ring-time');
    if (timeEl) timeEl.textContent = fmtTime(data.timer.remaining);
    const ringEl = modalBodyEl.querySelector('.pomo-ring-progress');
    if (ringEl) {
      const radius = 77; // (160 - 6) / 2
      const circumference = 2 * Math.PI * radius;
      const progress = data.timer.totalDuration > 0
        ? (data.timer.totalDuration - data.timer.remaining) / data.timer.totalDuration : 0;
      ringEl.setAttribute('stroke-dashoffset', String(circumference * (1 - progress)));
      const ringColor = data.timer.phase === 'work'
        ? (data.timer.remaining <= 300 ? '#f9e2af' : '#7c3aed')
        : '#89b4fa';
      ringEl.setAttribute('stroke', ringColor);
    }
  }

  function phaseLabel(phase) {
    if (phase === 'work') return 'Focus';
    if (phase === 'short-break') return 'Short Break';
    if (phase === 'long-break') return 'Long Break';
    return 'Ready';
  }

  // ── Tasks Tab ──

  function renderTasksTab(container) {
    // Add task input
    const input = el('input', 'modal-input pomo-task-input');
    input.placeholder = 'Add a task...';
    input.type = 'text';
    const estInput = el('input', 'modal-input pomo-est-input');
    estInput.placeholder = '\uD83C\uDF45';
    estInput.type = 'number';
    estInput.min = '0';
    estInput.max = '7';
    estInput.title = 'Estimated pomodoros (optional)';
    const addBtn = el('button', 'modal-btn primary', '+');
    addBtn.title = 'Add task';
    function doAdd() {
      const name = input.value.trim();
      if (!name) return;
      addTask(name, parseInt(estInput.value) || 1);
      input.value = '';
      estInput.value = '';
      renderActiveTab();
      // Re-focus the input for rapid entry
      setTimeout(() => {
        const newInput = modalBodyEl?.querySelector('.pomo-task-input');
        if (newInput) newInput.focus();
      }, 10);
    }
    addBtn.addEventListener('click', doAdd);
    input.addEventListener('keydown', (e) => { if (e.key === 'Enter') doAdd(); });

    const addRow = h('div', { className: 'pomo-add-row' }, input, estInput, addBtn);
    container.appendChild(addRow);

    // Task list
    const list = el('div', 'pomo-task-list');
    const pendingTasks = data.tasks.filter(t => !t.done);
    const doneTasks = data.tasks.filter(t => t.done);

    for (const task of pendingTasks) list.appendChild(renderTaskRow(task));
    if (doneTasks.length > 0) {
      list.appendChild(el('div', 'pomo-section-label', 'Completed'));
      for (const task of doneTasks) list.appendChild(renderTaskRow(task));
    }
    if (data.tasks.length === 0) {
      list.appendChild(el('div', 'pomo-empty', 'No tasks yet. Add one above to get started.'));
    }
    container.appendChild(list);
  }

  function renderTaskRow(task) {
    const row = el('div', 'pomo-task-row' + (task.done ? ' done' : '') +
      (task.id === data.timer.activeTaskId ? ' active' : ''));

    // Checkbox
    const check = el('button', 'pomo-task-check', task.done ? '\u2713' : '');
    check.addEventListener('click', (e) => { e.stopPropagation(); completeTask(task.id); renderActiveTab(); });
    row.appendChild(check);

    // Name
    const name = el('span', 'pomo-task-name', task.name);
    name.addEventListener('click', () => {
      if (!task.done) {
        setActiveTask(task.id);
        if (data.timer.phase === 'idle') startTimer(task.id);
        renderActiveTab();
      }
    });
    name.addEventListener('dblclick', () => {
      if (task.done) return;
      name.contentEditable = 'true';
      name.focus();
      const save = () => {
        name.contentEditable = 'false';
        const newName = name.textContent.trim();
        if (newName && newName !== task.name) editTask(task.id, { name: newName });
      };
      name.addEventListener('blur', save, { once: true });
      name.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') { e.preventDefault(); name.blur(); }
        if (e.key === 'Escape') { name.textContent = task.name; name.blur(); }
      });
    });
    row.appendChild(name);

    // Pomodoro dots
    if (task.estimatedPomodoros > 0) {
      const dots = el('span', 'pomo-task-dots');
      for (let i = 0; i < task.estimatedPomodoros; i++) {
        dots.appendChild(el('span', 'pomo-mini-dot' + (i < task.completedPomodoros ? ' filled' : '')));
      }
      row.appendChild(dots);
    }

    // Delete
    const del = el('button', 'pomo-task-del', '\u00D7');
    del.title = 'Remove task';
    del.addEventListener('click', (e) => { e.stopPropagation(); removeTask(task.id); renderActiveTab(); });
    row.appendChild(del);

    return row;
  }

  // ── Stats Tab ──

  function renderStatsTab(container) {
    const today = todayStr();
    const todayEntry = data.history.find(h => h.date === today) || {
      pomodorosCompleted: data.timer.pomodorosCompletedToday,
      focusMinutes: data.timer.pomodorosCompletedToday * Math.round(data.settings.workDuration / 60),
      tasksCompleted: 0,
    };

    // Today's card
    const card = el('div', 'pomo-stats-card');
    card.appendChild(el('div', 'pomo-stats-title', 'Today'));
    const grid = el('div', 'pomo-stats-grid');
    grid.appendChild(statBlock(todayEntry.pomodorosCompleted, '\uD83C\uDF45 Pomodoros'));
    grid.appendChild(statBlock(todayEntry.focusMinutes, '\u23F1 Minutes'));
    grid.appendChild(statBlock(todayEntry.tasksCompleted, '\u2713 Tasks'));
    card.appendChild(grid);
    container.appendChild(card);

    // 7-day bar chart
    const chartWrap = el('div', 'pomo-chart-wrap');
    chartWrap.appendChild(el('div', 'pomo-section-label', 'Last 7 Days'));
    const chart = el('div', 'pomo-chart');
    const last7 = getLast7Days();
    const maxPomo = Math.max(1, ...last7.map(d => d.pomodorosCompleted));
    for (const day of last7) {
      const col = el('div', 'pomo-chart-col');
      const bar = el('div', 'pomo-chart-bar');
      bar.style.height = (day.pomodorosCompleted / maxPomo * 100) + '%';
      if (day.date === today) bar.classList.add('today');
      bar.title = day.pomodorosCompleted + ' pomodoros';
      col.appendChild(bar);
      col.appendChild(el('div', 'pomo-chart-label', day.date.slice(5)));
      chart.appendChild(col);
    }
    chartWrap.appendChild(chart);
    container.appendChild(chartWrap);

    // History list
    if (data.history.length > 0) {
      container.appendChild(el('div', 'pomo-section-label', 'History'));
      const histList = el('div', 'pomo-hist-list');
      for (const entry of [...data.history].reverse()) {
        const row = el('div', 'modal-row');
        row.appendChild(el('span', 'modal-row-label', entry.date));
        row.appendChild(el('span', 'modal-row-detail',
          entry.pomodorosCompleted + '\uD83C\uDF45  ' + entry.focusMinutes + 'min  ' + entry.tasksCompleted + ' tasks'));
        histList.appendChild(row);
      }
      container.appendChild(histList);
    }
  }

  function statBlock(value, label) {
    const block = el('div', 'pomo-stat');
    block.appendChild(el('div', 'pomo-stat-value', String(value)));
    block.appendChild(el('div', 'pomo-stat-label', label));
    return block;
  }

  function getLast7Days() {
    const days = [];
    for (let i = 6; i >= 0; i--) {
      const d = new Date();
      d.setDate(d.getDate() - i);
      const dateStr = d.getFullYear() + '-' + pad2(d.getMonth() + 1) + '-' + pad2(d.getDate());
      const entry = data.history.find(h => h.date === dateStr);
      days.push(entry || { date: dateStr, pomodorosCompleted: 0, focusMinutes: 0, tasksCompleted: 0 });
    }
    return days;
  }

  // ── Settings Tab ──

  function renderSettingsTab(container) {
    const s = data.settings;

    container.appendChild(settingSlider('Focus duration', s.workDuration / 60, 1, 60, 'min', (v) => {
      updateSettings({ workDuration: v * 60 });
    }));
    container.appendChild(settingSlider('Short break', s.shortBreakDuration / 60, 1, 15, 'min', (v) => {
      updateSettings({ shortBreakDuration: v * 60 });
    }));
    container.appendChild(settingSlider('Long break', s.longBreakDuration / 60, 5, 30, 'min', (v) => {
      updateSettings({ longBreakDuration: v * 60 });
    }));

    // Long break interval
    const intervalRow = el('div', 'modal-row');
    intervalRow.appendChild(el('span', 'modal-row-label', 'Long break every'));
    const segmented = el('div', 'modal-segmented');
    for (let i = 2; i <= 6; i++) {
      const btn = el('button', i === s.longBreakInterval ? 'active' : '', i + '\uD83C\uDF45');
      btn.addEventListener('click', () => { updateSettings({ longBreakInterval: i }); renderActiveTab(); });
      segmented.appendChild(btn);
    }
    intervalRow.appendChild(segmented);
    container.appendChild(intervalRow);

    // Pill visibility mode
    const pillModeRow = el('div', 'modal-row');
    pillModeRow.style.flexDirection = 'column';
    pillModeRow.style.alignItems = 'stretch';
    pillModeRow.style.gap = '6px';
    pillModeRow.appendChild(el('span', 'modal-row-label', 'Timer pill'));
    const modeGroup = el('div', 'pomo-pill-mode-group');
    const modes = [
      { id: 'always',      label: 'Always',      desc: 'Always visible' },
      { id: 'minimal',     label: 'Minimal',     desc: 'Timer only, no task' },
      { id: 'auto-reveal', label: 'Auto-reveal', desc: 'Show near cursor' },
      { id: 'periodic',    label: 'Periodic',    desc: 'Flash every 5 min' },
      { id: 'hidden',      label: 'Hidden',      desc: 'Never show pill' },
    ];
    for (const m of modes) {
      const opt = el('button', 'pomo-pill-mode-opt' + (s.pillMode === m.id ? ' active' : ''));
      opt.appendChild(el('span', 'pomo-pill-mode-label', m.label));
      opt.appendChild(el('span', 'pomo-pill-mode-desc', m.desc));
      opt.addEventListener('click', () => {
        updateSettings({ pillMode: m.id });
        pillBuilt = false; // force rebuild for minimal/always switch
        updatePill();
        renderActiveTab();
      });
      modeGroup.appendChild(opt);
    }
    pillModeRow.appendChild(modeGroup);
    container.appendChild(pillModeRow);

    container.appendChild(settingToggle('Sounds', s.soundEnabled, (v) => {
      updateSettings({ soundEnabled: v });
    }));
    container.appendChild(settingToggle('Auto-start breaks', s.autoStartBreak, (v) => {
      updateSettings({ autoStartBreak: v });
    }));
  }

  function settingSlider(label, value, min, max, unit, onChange) {
    const row = el('div', 'modal-row pomo-setting-row');
    row.appendChild(el('span', 'modal-row-label', label));
    const right = el('div', 'pomo-setting-right');
    const valEl = el('span', 'pomo-setting-value', value + ' ' + unit);
    const range = document.createElement('input');
    range.type = 'range';
    range.className = 'modal-range';
    range.min = String(min);
    range.max = String(max);
    range.value = String(value);
    range.addEventListener('input', () => {
      const v = parseInt(range.value);
      valEl.textContent = v + ' ' + unit;
      onChange(v);
    });
    right.appendChild(range);
    right.appendChild(valEl);
    row.appendChild(right);
    return row;
  }

  function settingToggle(label, value, onChange) {
    const row = el('div', 'modal-row');
    row.appendChild(el('span', 'modal-row-label', label));
    const toggle = el('div', 'modal-toggle-wrap');
    const knob = el('div', 'modal-toggle' + (value ? ' on' : ''));
    knob.addEventListener('click', () => {
      const newVal = !knob.classList.contains('on');
      knob.classList.toggle('on', newVal);
      onChange(newVal);
    });
    toggle.appendChild(knob);
    row.appendChild(toggle);
    return row;
  }

  // ── Help Tab ──

  function renderHelpTab(container) {
    const help = el('div', 'pomo-help');

    // Info box
    const info = el('div', 'modal-info info');
    const strong = el('strong', null, 'The Pomodoro Technique');
    info.appendChild(strong);
    info.appendChild(document.createTextNode(' \u2014 a time management method that uses focused work intervals separated by breaks.'));
    help.appendChild(info);

    // Steps
    const steps = el('div', 'pomo-help-steps');
    const stepTexts = [
      'Pick a task from your task list',
      'Start the timer (default: 25 minutes)',
      'Focus on the task until the timer rings',
      'Take a short break (5 minutes)',
      'After 4 pomodoros, take a longer break',
    ];
    for (let i = 0; i < stepTexts.length; i++) {
      const step = el('div', 'pomo-help-step');
      step.appendChild(el('span', 'pomo-help-num', String(i + 1)));
      step.appendChild(document.createTextNode(' ' + stepTexts[i]));
      steps.appendChild(step);
    }
    help.appendChild(steps);

    // Tips
    const tipsSection = el('div', 'pomo-help-tips');
    tipsSection.appendChild(el('div', 'pomo-section-label', 'Tips'));
    const tipList = el('ul');
    const tips = [
      ['Click a task name', ' to start focusing on it'],
      ['Double-click', ' a task name to edit it'],
      ['', 'A sound plays at 5 minutes remaining and on completion'],
      ['', 'Track your daily progress in the Stats tab'],
    ];
    for (const [bold, rest] of tips) {
      const li = el('li');
      if (bold) li.appendChild(el('strong', null, bold));
      li.appendChild(document.createTextNode(rest));
      tipList.appendChild(li);
    }
    tipsSection.appendChild(tipList);
    help.appendChild(tipsSection);

    // Shortcut
    const shortcut = el('div', 'pomo-help-shortcut');
    shortcut.appendChild(document.createTextNode('Keyboard shortcut: '));
    shortcut.appendChild(el('kbd', null, 'Ctrl'));
    shortcut.appendChild(document.createTextNode('+'));
    shortcut.appendChild(el('kbd', null, 'Alt'));
    shortcut.appendChild(document.createTextNode('+'));
    shortcut.appendChild(el('kbd', null, '`'));
    help.appendChild(shortcut);

    container.appendChild(help);
  }

  // ── Public API ──

  return {
    loadState,
    renderInModal,
    detachModal,
    createPill,
    updatePill,
    getState() { return data; },
    isRendering() { return modalBodyEl !== null; },
    startTimer,
    pauseTimer,
    resumeTimer,
    dispose() {
      stopTick();
      if (saveTimeout) { clearTimeout(saveTimeout); saveTimeout = null; }
      postMessage({ type: 'pomodoro-save-state', data });
    },
  };
}
