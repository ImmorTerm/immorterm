/**
 * GPU Terminal — Plans Panel (S4).
 *
 * Read-only sidebar list of project plans (visual briefs written by the
 * daemon's immorterm_plan MCP tools, ~/.immorterm/plans/<project>/<id>/current.json)
 * plus a full-size overlay of a plan's html on click.
 *
 * The host provides messages:
 *   - plans-load  (host → webview, full records incl. html)
 *   - get-plans   (webview → host, request/refresh)
 * Live updates: daemon `plan_changed` workshop envelope → refresh() (wired in
 * gpu-terminal.html handleWorkshopEvent); VS Code additionally pushes on fs.watch.
 *
 * Imported by gpu-terminal.html via dynamic import.
 */

const STATUSES = ['draft', 'active', 'decided', 'superseded', 'done'];

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function relativeTime(ts) {
  const diff = Date.now() - ts;
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return 'just now';
  if (mins < 60) return mins + 'm ago';
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return hrs + 'h ago';
  const days = Math.floor(hrs / 24);
  if (days === 1) return 'yesterday';
  return days + 'd ago';
}

// Scrollbar rules duplicated from the draw_html shadow-DOM path
// (gpu-terminal.html ~9228-9231) — shadow DOM can't see page styles.
const OVERLAY_SHADOW_CSS =
  '.ai-html-content{max-height:85vh;max-width:85vw;overflow:auto}'
  + '.ai-html-content::-webkit-scrollbar{width:10px;height:10px}'
  + '.ai-html-content::-webkit-scrollbar-thumb{background:color-mix(in srgb,var(--sidebar-muted,#a6adc8) 28%,transparent);border-radius:6px;border:3px solid transparent;background-clip:padding-box}'
  + '.ai-html-content:hover::-webkit-scrollbar-thumb{background:color-mix(in srgb,var(--sidebar-muted,#a6adc8) 45%,transparent);background-clip:padding-box}';

/**
 * Creates the plans panel. Returns { setPlans, refresh, applyVisibility, dispose }.
 */
export function createPlansPanel({ plansHeaderEl, plansListEl, requestPlans, getPlansMode }) {
  let _plans = [];

  function sorted() {
    // Brief rule: active first, then newest updated. superseded greys via CSS.
    return [..._plans].sort((a, b) => {
      const aa = a.status === 'active' ? 0 : 1;
      const bb = b.status === 'active' ? 0 : 1;
      if (aa !== bb) return aa - bb;
      return (b.updatedAt || 0) - (a.updatedAt || 0);
    });
  }

  function unresolvedCount(plan) {
    return (plan.decisions || []).filter(d => !d.resolved).length;
  }

  function render() {
    plansListEl.textContent = '';
    for (const plan of sorted()) plansListEl.appendChild(buildPlanRow(plan));
  }

  function buildPlanRow(plan) {
    const status = STATUSES.includes(plan.status) ? plan.status : 'draft';
    const row = el('div', 'plan-item' + (status === 'superseded' ? ' superseded' : ''));
    row.dataset.planId = plan.id;
    row.title = plan.summary || plan.title || '';

    row.appendChild(el('span', 'plan-title', plan.title || plan.id));

    const n = unresolvedCount(plan);
    if (n > 0) row.appendChild(el('span', 'plan-decisions-badge', n + (n === 1 ? ' decision' : ' decisions')));

    row.appendChild(el('span', 'plan-status-pill status-' + status, status));
    row.appendChild(el('span', 'plan-updated', relativeTime(plan.updatedAt || 0)));

    row.addEventListener('click', () => openPlanOverlay(plan));
    return row;
  }

  // ── Full-size overlay: scrim + shadow-DOM html, ESC/scrim dismiss.
  // Same scrim skin as .task-board-overlay; same shadow-DOM isolation and
  // S1 inner-scroll caps as the draw_html ai-overlay card path.
  function openPlanOverlay(plan) {
    const existing = document.querySelector('.plan-overlay');
    if (existing) existing.remove();

    const overlay = el('div', 'plan-overlay');
    const card = el('div', 'plan-overlay-card');

    const shadow = card.attachShadow({ mode: 'open' });
    const style = document.createElement('style');
    style.textContent = OVERLAY_SHADOW_CSS;
    shadow.appendChild(style);

    const wrapper = el('div', 'ai-html-content');
    const doc = new DOMParser().parseFromString(plan.html || '<p>(empty plan)</p>', 'text/html');
    // ponytail: plans are static briefs — scripts stripped, not executed.
    // Reuse runScriptInOverlay parity if interactive plans ever appear.
    doc.body.querySelectorAll('script').forEach(s => s.remove());
    while (doc.body.firstChild) wrapper.appendChild(doc.body.firstChild);
    shadow.appendChild(wrapper);

    const closeBtn = el('button', 'plan-overlay-close', '×');
    closeBtn.title = 'Close (Esc)';
    closeBtn.addEventListener('click', close);

    overlay.appendChild(card);
    overlay.appendChild(closeBtn);
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
    document.body.appendChild(overlay);

    function onKey(e) { if (e.key === 'Escape') close(); }
    document.addEventListener('keydown', onKey);
    function close() { overlay.remove(); document.removeEventListener('keydown', onKey); }
  }

  // ── Visibility: mirror of tasks setTasks — header/list show only when the
  // project has plans AND plansMode isn't 'hidden'.
  function applyVisibility() {
    const mode = typeof getPlansMode === 'function' ? getPlansMode() : 'show';
    const visible = mode !== 'hidden' && _plans.length > 0;
    if (plansHeaderEl) plansHeaderEl.style.display = visible ? '' : 'none';
    if (plansListEl) plansListEl.style.display = visible ? '' : 'none';
  }

  function setPlans(plans) {
    _plans = Array.isArray(plans) ? plans : [];
    render();
    applyVisibility();
  }

  function dispose() { /* no persistent listeners */ }

  return { setPlans, refresh: requestPlans, applyVisibility, dispose };
}
