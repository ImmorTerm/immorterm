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

// Comment-slot chrome — shared by the outer form scope (decision/general
// slots) AND the isolated body scope (section slots live inside the body's
// own shadow). Neutral so it reads as an annotation over ANY project brand.
const COMMENT_SLOT_CSS =
  '.plan-comment-slot{margin:6px 0 14px}'
  + '.plan-comment-add{background:none;border:none;color:var(--sidebar-muted,#a6adc8);'
  + 'font-size:11px;cursor:pointer;padding:0}'
  + '.plan-comment-add:hover{color:var(--sidebar-text,#cdd6f4)}'
  + '.plan-comment-input{width:100%;box-sizing:border-box;background:var(--sidebar-hover,#1e1e2e);'
  + 'border:1px solid var(--sidebar-border,#313244);border-radius:6px;color:var(--sidebar-text,#cdd6f4);'
  + 'font:inherit;font-size:12px;padding:6px;resize:vertical;min-height:40px}';

// Scrollbar rules duplicated from the draw_html shadow-DOM path
// (gpu-terminal.html ~9228-9231) — shadow DOM can't see page styles.
const OVERLAY_SHADOW_CSS =
  '.ai-html-content{max-height:85vh;max-width:85vw;overflow:auto}'
  + '.ai-html-content::-webkit-scrollbar{width:10px;height:10px}'
  + '.ai-html-content::-webkit-scrollbar-thumb{background:color-mix(in srgb,var(--sidebar-muted,#a6adc8) 28%,transparent);border-radius:6px;border:3px solid transparent;background-clip:padding-box}'
  + '.ai-html-content:hover::-webkit-scrollbar-thumb{background:color-mix(in srgb,var(--sidebar-muted,#a6adc8) 45%,transparent);background-clip:padding-box}'
  // ── Decision form + comment slots (trusted panel chrome, shadow-scoped;
  //    theme vars only, house .plan-* idiom of gpu-terminal.css:2283+) ──
  + '.plan-form{margin-top:16px;border-top:1px solid var(--sidebar-border,#313244);padding-top:12px;'
  + 'font-size:13px;color:var(--sidebar-text,#cdd6f4)}'
  + '.plan-form-head{font-size:11px;font-weight:600;text-transform:uppercase;letter-spacing:.4px;'
  + 'color:var(--sidebar-muted,#a6adc8);margin-bottom:6px}'
  + '.plan-decision{margin:10px 0;padding:10px;border:1px solid var(--sidebar-border,#313244);border-radius:8px}'
  + '.plan-decision.answered{border-color:color-mix(in srgb,var(--sidebar-accent,#b482ff) 45%,transparent)}'
  + '.plan-decision.resolved{opacity:.6}'
  + '.plan-decision-label{font-weight:600;margin-bottom:8px}'
  + '.plan-decision-resolution{font-size:12px;color:var(--sidebar-accent,#b482ff)}'
  + '.plan-decision-options{display:flex;flex-wrap:wrap;gap:6px}'
  + '.plan-opt{background:none;border:1px solid var(--sidebar-border,#313244);border-radius:6px;'
  + 'padding:4px 10px;color:var(--sidebar-text,#cdd6f4);cursor:pointer;font:inherit;font-size:12px}'
  + '.plan-opt:hover{background:var(--sidebar-hover,#1e1e2e)}'
  + '.plan-opt.selected{border-color:var(--sidebar-accent,#b482ff);'
  + 'background:color-mix(in srgb,var(--sidebar-accent,#b482ff) 18%,transparent)}'
  + '.plan-rec-tag{font-size:9px;text-transform:uppercase;letter-spacing:.4px;'
  + 'color:var(--sidebar-accent,#b482ff);margin-left:6px}'
  + COMMENT_SLOT_CSS
  + '.plan-submit-bar{position:sticky;bottom:0;display:flex;align-items:center;gap:10px;'
  + 'justify-content:flex-end;padding:10px 0 2px;margin-top:14px;'
  + 'background:var(--sidebar-bg,#1e1e2e);border-top:1px solid var(--sidebar-border,#313244)}'
  + '.plan-submit-summary{font-size:11px;color:var(--sidebar-muted,#a6adc8)}'
  + '.plan-submit-error{font-size:11px;color:var(--status-error,#f38ba8)}'
  + '.plan-submit-btn{background:color-mix(in srgb,var(--sidebar-accent,#b482ff) 25%,transparent);'
  + 'border:1px solid var(--sidebar-accent,#b482ff);color:var(--sidebar-text,#cdd6f4);'
  + 'border-radius:6px;padding:5px 14px;cursor:pointer;font:inherit;font-size:12px}'
  + '.plan-submit-btn:disabled{opacity:.4;cursor:default}';

/** Compact wake summary typed into the agent's input box — cap ~300 chars;
 *  comment texts are referenced by count, the agent reads the record. */
// Security: planId, decision ids, and option labels are free-form MCP args
// (validated nowhere upstream) that get TYPED INTO THE TERMINAL. Strip every
// control char — ESC (0x1b) could forge a bracketed-paste end marker and
// break out; CR/LF could forge prompt submission. Collapse to spaces.
function scrubForPty(str) {
  return String(str).replace(/[\x00-\x1f\x7f]/g, ' ');
}
function buildWakeSummary(planId, selections, nComments) {
  const parts = [];
  for (const [id, opt] of selections) parts.push(scrubForPty(id) + '→' + scrubForPty(opt));
  const pid = scrubForPty(planId);
  let s = 'Plan ' + pid + ' submitted';
  if (parts.length) s += ': ' + parts.join('; ');
  if (nComments > 0) s += (parts.length ? '. ' : ': ') + nComments + ' comment' + (nComments === 1 ? '' : 's');
  s += ' — read the full record via immorterm_list_plans id=' + pid + '.';
  // Final belt-and-suspenders scrub of the assembled string, then cap.
  s = scrubForPty(s);
  return s.length > 300 ? s.slice(0, 297) + '…' : s;
}

/**
 * Creates the plans panel.
 * Returns { setPlans, refresh, applyVisibility, handleSubmitResult, dispose }.
 *
 * submitPlan({planId, resolutions, comments}) — posts the batch to the host;
 *   the host replies with a 'plans-submit-result' message which the embedder
 *   routes back via handleSubmitResult(msg).
 * wakeAgent(sessionName, text) — types `text` into the plan's attached (or
 *   active) Claude session; returns true if a session was woken.
 */
export function createPlansPanel({ plansHeaderEl, plansListEl, requestPlans, getPlansMode, onHasContent, submitPlan, wakeAgent }) {
  let _plans = [];
  const _submittedIds = new Set(); // plans submitted with no live agent to wake
  let _pendingSubmit = null;       // { planId, onResult } for the open overlay

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

    // No-wake submissions leave a badge until an agent picks the plan up
    // (next session start surfaces it via the discipline hook).
    if (_submittedIds.has(plan.id)) row.appendChild(el('span', 'plan-decisions-badge', 'submitted'));

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
    // Plan html is untrusted (any vendor/agent authors it) — neutralize ALL
    // active content, not just <script>: inline on*= handlers and
    // javascript:/data: URLs execute even with scripts removed, and the hub
    // (standalone/Tauri) has no CSP backstop like the VS Code webview does.
    // Whole document (head + body): head <style> is carried across, so a
    // <script> or on*= there must be neutralized too.
    doc.querySelectorAll('script').forEach(s => s.remove());
    for (const node of doc.querySelectorAll('*')) {
      for (const attr of [...node.attributes]) {
        const n = attr.name.toLowerCase();
        const v = (attr.value || '').replace(/\s+/g, '').toLowerCase();
        if (n.startsWith('on') || ((n === 'href' || n === 'src' || n === 'xlink:href')
            && (v.startsWith('javascript:') || v.startsWith('data:text/html')))) {
          node.removeAttribute(attr.name);
        }
      }
    }

    // ── Local form state — plain in-memory, discarded on close. Selecting
    //    and typing never wakes anyone; only Submit persists. ──
    const formState = {
      selections: new Map(), // decisionId -> chosen option text
      comments: new Map(),   // 'section:<id>' | 'decision:<id>' | 'general' -> text
    };

    function commentCount() {
      let n = 0;
      for (const t of formState.comments.values()) if (t.trim()) n++;
      return n;
    }

    function makeCommentSlot(key, placeholder, addLabel) {
      const slot = el('div', 'plan-comment-slot');
      const input = el('textarea', 'plan-comment-input');
      input.placeholder = placeholder;
      if (addLabel) {
        const add = el('button', 'plan-comment-add', addLabel);
        add.type = 'button';
        input.hidden = true;
        add.addEventListener('click', () => { add.hidden = true; input.hidden = false; input.focus(); });
        slot.appendChild(add);
      }
      input.addEventListener('input', () => { formState.comments.set(key, input.value); updateSubmitBar(); });
      // Keep terminal keybindings out of the textarea (Escape still closes).
      input.addEventListener('keydown', (e) => { if (e.key !== 'Escape') e.stopPropagation(); });
      slot.appendChild(input);
      return slot;
    }

    // Comment affordance under every data-plan-section anchor (agent-authored
    // per the discipline hook).
    for (const sectionEl of doc.body.querySelectorAll('[data-plan-section]')) {
      const sectionId = sectionEl.getAttribute('data-plan-section') || '';
      sectionEl.insertAdjacentElement('afterend', makeCommentSlot('section:' + sectionId, 'Comment on this section…', '+ comment'));
    }
    // The plan BODY renders in its OWN shadow root, in the PROJECT's brand
    // (repo tokens/fonts) — isolated so its CSS can neither clobber nor be
    // clobbered by the ImmorTerm decision-form chrome below. The design
    // contract: body = project brand, chrome = ImmorTerm frame.
    const bodyHost = el('div', 'plan-body-host');
    const bodyShadow = bodyHost.attachShadow({ mode: 'open' });
    const slotStyle = document.createElement('style');
    slotStyle.textContent = COMMENT_SLOT_CSS;
    bodyShadow.appendChild(slotStyle); // first → the body's own <style> wins ties (repo brand is primary)
    // DOMParser routes a leading <style>/<link> into <head>; carry the
    // author's <style> across (external <link> deliberately dropped — no
    // remote CSS from untrusted plan html), else a rich body renders bare.
    for (const st of doc.head.querySelectorAll('style')) bodyShadow.appendChild(st);
    while (doc.body.firstChild) bodyShadow.appendChild(doc.body.firstChild);
    wrapper.appendChild(bodyHost);

    // ── Decision form (from structured decisions[], never plan html) ──
    const decisions = Array.isArray(plan.decisions) ? plan.decisions : [];
    let submitBtn = null, summaryLabel = null, errorLabel = null;
    const form = el('div', 'plan-form');
    {
      if (decisions.length > 0) form.appendChild(el('div', 'plan-form-head', 'Decisions'));

      for (const d of decisions) {
        if (d.resolved) {
          const block = el('div', 'plan-decision resolved');
          block.appendChild(el('div', 'plan-decision-label', d.label || d.id));
          block.appendChild(el('div', 'plan-decision-resolution', '→ ' + (d.resolution || '(resolved)')));
          form.appendChild(block);
          continue;
        }
        const block = el('div', 'plan-decision');
        block.dataset.decisionId = d.id;
        block.appendChild(el('div', 'plan-decision-label', d.label || d.id));
        const opts = el('div', 'plan-decision-options');
        for (const opt of (Array.isArray(d.options) ? d.options : [])) {
          const isRec = d.recommendation === opt;
          const btn = el('button', 'plan-opt' + (isRec ? ' is-rec' : ''), opt);
          btn.type = 'button';
          btn.dataset.option = opt;
          if (isRec) btn.appendChild(el('span', 'plan-rec-tag', 'recommended'));
          btn.addEventListener('click', () => {
            const already = formState.selections.get(d.id) === opt;
            opts.querySelectorAll('.plan-opt').forEach(b => b.classList.remove('selected'));
            if (already) {
              formState.selections.delete(d.id);
            } else {
              btn.classList.add('selected');
              formState.selections.set(d.id, opt);
            }
            block.classList.toggle('answered', formState.selections.has(d.id));
            updateSubmitBar();
          });
          opts.appendChild(btn);
        }
        block.appendChild(opts);
        block.appendChild(makeCommentSlot('decision:' + d.id, 'Note on this decision…', '+ note'));
        form.appendChild(block);
      }

      const general = makeCommentSlot('general', 'General comment…', null);
      general.classList.add('plan-comment-general');
      form.appendChild(general);

      const bar = el('div', 'plan-submit-bar');
      summaryLabel = el('span', 'plan-submit-summary', '');
      errorLabel = el('span', 'plan-submit-error', '');
      submitBtn = el('button', 'plan-submit-btn', 'Submit');
      submitBtn.type = 'button';
      submitBtn.disabled = true;
      submitBtn.addEventListener('click', doSubmit);
      bar.appendChild(errorLabel);
      bar.appendChild(summaryLabel);
      bar.appendChild(submitBtn);
      form.appendChild(bar);
      wrapper.appendChild(form);
    }

    // Mount the fully-built content into the card's shadow root. (Without
    // this the card renders empty — the form/comments live on `wrapper`.)
    shadow.appendChild(wrapper);

    function updateSubmitBar() {
      if (!submitBtn) return;
      const nSel = formState.selections.size;
      const nCom = commentCount();
      submitBtn.disabled = nSel === 0 && nCom === 0;
      summaryLabel.textContent =
        (nSel ? nSel + ' decision' + (nSel === 1 ? '' : 's') : '')
        + (nSel && nCom ? ' · ' : '')
        + (nCom ? nCom + ' comment' + (nCom === 1 ? '' : 's') : '');
    }

    function doSubmit() {
      if (typeof submitPlan !== 'function') return;
      const resolutions = [];
      for (const [decisionId, opt] of formState.selections) {
        resolutions.push({ decision_id: decisionId, resolution: opt });
      }
      const comments = [];
      for (const [key, text] of formState.comments) {
        if (!text.trim()) continue;
        const c = { text: text.trim() };
        if (key.startsWith('section:')) c.sectionId = key.slice(8);
        else if (key.startsWith('decision:')) c.decisionId = key.slice(9);
        comments.push(c);
      }
      submitBtn.disabled = true;
      submitBtn.textContent = 'Submitting…';
      errorLabel.textContent = '';
      _pendingSubmit = {
        planId: plan.id,
        onResult(msg) {
          if (!msg.ok) {
            // Keep state; let the user retry. (Overlay may be closed already —
            // guard the DOM writes.)
            if (overlay.isConnected) {
              submitBtn.disabled = false;
              submitBtn.textContent = 'Submit';
              errorLabel.textContent = msg.error || 'Submit failed';
            }
            return;
          }
          // The wake + sidebar refresh must fire even if the user closed the
          // overlay after submitting — the write already persisted, so the
          // agent must still be notified. DOM freeze only if still open.
          const updated = msg.plan || plan;
          const summary = buildWakeSummary(plan.id, formState.selections, comments.length);
          const woke = typeof wakeAgent === 'function' && wakeAgent(updated.sessionName, summary);
          if (!woke) { _submittedIds.add(plan.id); render(); }
          if (overlay.isConnected) {
            form.querySelectorAll('.plan-opt, .plan-comment-add').forEach(b => { b.disabled = true; });
            form.querySelectorAll('.plan-comment-input').forEach(t => { t.readOnly = true; });
            submitBtn.textContent = woke ? 'Submitted ✓ — agent notified' : 'Submitted ✓';
          }
        },
      };
      submitPlan({ planId: plan.id, resolutions, comments });
    }

    const closeBtn = el('button', 'plan-overlay-close', '×');
    closeBtn.title = 'Close (Esc)';
    closeBtn.addEventListener('click', close);

    overlay.appendChild(card);
    overlay.appendChild(closeBtn);
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
    document.body.appendChild(overlay);

    function onKey(e) { if (e.key === 'Escape') close(); }
    document.addEventListener('keydown', onKey);
    // ponytail: Escape/scrim close discards draft selections + comments (v1);
    // add localStorage draft persistence if users report losing long comments.
    function close() {
      overlay.remove();
      document.removeEventListener('keydown', onKey);
      // Deliberately DON'T clear _pendingSubmit here: if a submit is in flight,
      // handleSubmitResult must still fire the agent wake (the write persisted).
      // It self-clears on result; onResult is now overlay-close-safe.
    }
  }

  /** Route a host 'plans-submit-result' message to the open overlay's form. */
  function handleSubmitResult(msg) {
    if (_pendingSubmit && _pendingSubmit.planId === msg.planId) {
      _pendingSubmit.onResult(msg);
      if (msg.ok) _pendingSubmit = null;
    }
  }

  // ── Visibility: report has-content to the S5a accordion — the mode gate
  // ('hidden') and all style.display writes live in applySectionLayout.
  function applyVisibility() {
    if (typeof onHasContent === 'function') onHasContent(_plans.length > 0);
  }

  function setPlans(plans) {
    _plans = Array.isArray(plans) ? plans : [];
    render();
    applyVisibility();
  }

  function dispose() { /* no persistent listeners */ }

  return { setPlans, refresh: requestPlans, applyVisibility, handleSubmitResult, dispose };
}
