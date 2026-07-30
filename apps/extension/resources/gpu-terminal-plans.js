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

// §6B wire MIME for PLANS-row → Spaces-grid native drag-in. Single source of
// truth (mirrors utils.SESSION_MIME): the drop handler in gpu-terminal.html
// reads it as `plansModule.PLAN_MIME`.
export const PLAN_MIME = 'application/x-immorterm-plan';

// ── Plan-html sanitizer ──────────────────────────────────────────────
// Plan html is untrusted (any vendor/agent authors it) and renders in a
// shadow root with NO CSP backstop on the hub (standalone/Tauri) path.
// Shadow DOM does NOT sandbox execution, so all active content must be
// neutralized. This is a hardened denylist, not DOMPurify — DOMPurify is
// not a dependency of this repo, and plan bodies are semi-trusted +
// isolated; vendor DOMPurify if that threat model tightens. Covers the
// vectors an automated review flagged: dangerous elements, every URL-
// bearing attribute, and data: URIs (only safe raster images allowed —
// SVG excluded because it can carry script).
const PLAN_BLOCK_TAGS =
  'script,iframe,object,embed,meta,base,link,form,param,applet,frame,frameset,noscript';
const PLAN_URL_ATTRS = [
  'href', 'src', 'xlink:href', 'srcdoc', 'formaction', 'action',
  'data', 'background', 'poster', 'ping',
];
const PLAN_SAFE_DATA_IMG = /^data:image\/(png|jpe?g|gif|webp)[;,]/;
/** Strip agent-authoring wrappers that corrupt HTML parsing BEFORE the parser
 *  sees them — the fix must precede parseFromString, not sanitizePlanDoc (by
 *  then the damage is done). Two seen in the wild:
 *   • XML CDATA (`<![CDATA[ … ]]>`) — HTML has no CDATA in the html namespace,
 *     so `<!` opens a bogus comment that runs to the first `>` and EATS the
 *     leading `<style>` tag, dumping the CSS as visible body text.
 *   • Markdown code fences (```html … ```) — an agent pasting a fenced block.
 *  Returns cleaned html. Idempotent + safe on already-clean input. */
export function unwrapPlanHtml(html) {
  let s = String(html == null ? '' : html).trim();
  // Markdown code fence: ```html\n … \n```  (language tag optional)
  const fm = /^```[a-zA-Z-]*\s*\n([\s\S]*?)\n?```$/.exec(s);
  if (fm) s = fm[1].trim();
  // XML CDATA wrapper (leading; trailing ]]> stripped only if present)
  if (s.startsWith('<![CDATA[')) {
    s = s.slice(9);
    if (s.endsWith(']]>')) s = s.slice(0, -3);
    s = s.trim();
  }
  return s;
}

export function sanitizePlanDoc(doc) {
  // Whole document (head + body): a leading <style> is routed to <head> and
  // carried across, so head must be scrubbed too.
  doc.querySelectorAll(PLAN_BLOCK_TAGS).forEach((n) => n.remove());
  for (const node of doc.querySelectorAll('*')) {
    for (const attr of [...node.attributes]) {
      const n = attr.name.toLowerCase();
      if (n.startsWith('on')) { node.removeAttribute(attr.name); continue; }
      if (PLAN_URL_ATTRS.includes(n)) {
        const v = (attr.value || '').replace(/\s+/g, '').toLowerCase();
        const bad = v.startsWith('javascript:') || v.startsWith('vbscript:')
          || (v.startsWith('data:') && !PLAN_SAFE_DATA_IMG.test(v));
        if (bad) node.removeAttribute(attr.name);
      }
    }
  }
}

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
  + 'font:inherit;font-size:12px;padding:6px;resize:vertical;min-height:40px}'
  // Persisted comments shown on open (thread above the input slot).
  + '.plan-comment-prior{background:var(--sidebar-hover,#1e1e2e);border:1px solid var(--sidebar-border,#313244);'
  + 'border-radius:8px;padding:7px 9px;margin-bottom:6px}'
  + '.plan-comment-prior-meta{font-size:10px;color:var(--sidebar-muted,#a6adc8);margin-bottom:3px;'
  + 'font-family:ui-monospace,Menlo,monospace}'
  + '.plan-comment-prior-text{font-size:12.5px;color:var(--sidebar-text,#cdd6f4);white-space:pre-wrap;line-height:1.45}';

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

/** Mount an already-sanitized plan doc's body into a fresh shadow root on
 *  `shadowHost`, in the PROJECT's brand. Shared by the sidebar overlay and the
 *  Spaces plan tile. Caller runs sanitizePlanDoc(doc) first (the overlay injects
 *  comment slots into doc.body between sanitize and mount). attachShadow is
 *  single-shot — pass a fresh host each render. Returns the shadow root. */
export function renderPlanBodyInto(shadowHost, doc) {
  const bodyShadow = shadowHost.attachShadow({ mode: 'open' });
  const slotStyle = document.createElement('style');
  slotStyle.textContent = COMMENT_SLOT_CSS;
  bodyShadow.appendChild(slotStyle); // first → author's <style> wins ties (repo brand primary)
  for (const st of doc.head.querySelectorAll('style')) bodyShadow.appendChild(st);
  while (doc.body.firstChild) bodyShadow.appendChild(doc.body.firstChild);
  return bodyShadow;
}

/** Render a plan's html as a SANDBOXED IFRAME artifact — the claude.ai model:
 *  a real document with full CSS + JS, isolated in an opaque origin
 *  (sandbox=allow-scripts, NO allow-same-origin → it cannot reach the parent
 *  webview or Tauri's native IPC). A strict in-frame CSP blocks ALL external
 *  network (default-src 'none', no connect-src) so an untrusted artifact stays
 *  self-contained and can't exfiltrate — no script-stripping needed. The frame
 *  auto-sizes to its content via a postMessage height bridge (capped, so a tall
 *  plan scrolls internally). Returns the iframe. */
export function renderPlanArtifactIframe(host, html) {
  const body = unwrapPlanHtml(html) || '<p style="font-family:system-ui;opacity:.6;padding:16px">(empty plan)</p>';
  const csp = "default-src 'none'; img-src data: blob:; media-src data: blob:; "
    + "style-src 'unsafe-inline'; script-src 'unsafe-inline'; font-src data:;";
  const bridge =
    '<script>(function(){function p(){try{parent.postMessage({__immPlanFrame:1,'
    + 'h:document.documentElement.scrollHeight},"*")}catch(e){}}'
    + 'addEventListener("load",p);setTimeout(p,60);setTimeout(p,350);'
    + 'if(window.ResizeObserver){new ResizeObserver(p).observe(document.documentElement)}})();<\/script>';
  const srcdoc = '<!doctype html><html><head><meta charset="utf-8">'
    + '<meta http-equiv="Content-Security-Policy" content="' + csp + '">'
    + '<meta name="color-scheme" content="dark light">'
    + '<style>html,body{margin:0}body{overflow:auto}</style></head><body>'
    + body + bridge + '</body></html>';

  const iframe = document.createElement('iframe');
  iframe.className = 'plan-artifact-frame';
  iframe.setAttribute('sandbox', 'allow-scripts');
  iframe.setAttribute('referrerpolicy', 'no-referrer');
  iframe.style.cssText = 'width:100%;border:0;display:block;background:transparent;min-height:140px';
  iframe.srcdoc = srcdoc;

  // Height bridge — self-cleans once the overlay (and iframe) are gone.
  const onMsg = (e) => {
    if (!iframe.isConnected) { window.removeEventListener('message', onMsg); return; }
    if (e.source !== iframe.contentWindow || !e.data || e.data.__immPlanFrame !== 1) return;
    const cap = Math.round((window.innerHeight || 800) * 0.78);
    iframe.style.height = Math.max(140, Math.min(Number(e.data.h) || 0, cap)) + 'px';
  };
  window.addEventListener('message', onMsg);
  host.appendChild(iframe);
  return iframe;
}

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
export function createPlansPanel({ plansHeaderEl, plansListEl, requestPlans, getPlansMode, onHasContent, submitPlan, wakeAgent, enableGridDrag, onConsumeDragState, onConsumeToTerminal }) {
  let _plans = [];
  const _submittedIds = new Set(); // plans submitted with no live agent to wake
  let _pendingSubmit = null;       // { planId, onResult } for the open overlay
  // Unsubmitted comment drafts, kept across overlay close/reopen so typing is
  // never lost. Key: `${planId} ${slotKey}`. Cleared on successful submit.
  const _planDrafts = new Map();

  // ── Plan consume-drag (body, not the grip) ──────────────────────────
  // Grip = spatial drag into the Spaces grid (native PLAN_MIME, §6B). Body =
  // semantic consume: drop onto a terminal/tile to queue the plan as context,
  // exactly like session/task/file drops. Synthetic (mousemove) like the
  // sessions list, because the drop target is the GPU canvas. In a Space the
  // host's consume path resolves the dropped-on tile (N4) — free, same pill.
  let _planDrag = null;   // { plan, startX, startY, row, dragging }
  let _planDragged = false; // true after a real drag → suppress the row's click
  let _planDragWired = false;
  function attachPlanDragListeners() {
    if (_planDragWired) return;
    _planDragWired = true;
    document.addEventListener('mousemove', (e) => {
      if (!_planDrag) return;
      if (!_planDrag.dragging) {
        if (Math.abs(e.clientX - _planDrag.startX) <= 4 && Math.abs(e.clientY - _planDrag.startY) <= 4) return;
        _planDrag.dragging = true;
        _planDragged = true;
        _planDrag.row.classList.add('dragging');
      }
      // Raise the consume drop zone; the N4 hover-ring keys off its visibility.
      if (onConsumeDragState) onConsumeDragState(true, _planDrag.plan.title || _planDrag.plan.id);
    });
    document.addEventListener('mouseup', (e) => {
      if (!_planDrag) return;
      const drag = _planDrag; _planDrag = null;
      if (!drag.dragging) return;
      drag.row.classList.remove('dragging');
      if (onConsumeDragState) onConsumeDragState(false, null);
      const sidebar = plansListEl.closest('#sidebar');
      const rect = sidebar ? sidebar.getBoundingClientRect() : null;
      const outside = rect && e.clientX < rect.left;   // dropped over the terminal area
      if (outside && onConsumeToTerminal) onConsumeToTerminal(drag.plan);
    });
  }

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

    // §6B drag SOURCE — a native-drag grip carrying PLAN_MIME, so a plan row
    // drops into the Spaces grid as a plan tile. Mirrors utils §6A. The grip's
    // mousedown/click stopPropagation keeps a grip drag from opening the overlay.
    if (enableGridDrag) {
      const grip = el('span', 'tile-grip plan-grip');
      grip.setAttribute('aria-hidden', 'true');
      grip.textContent = '⠿';
      grip.draggable = true;
      grip.addEventListener('mousedown', (e) => e.stopPropagation());
      grip.addEventListener('click', (e) => e.stopPropagation()); // grip click must not open the overlay
      grip.addEventListener('dragstart', (e) => {
        e.dataTransfer.setData(PLAN_MIME, plan.id);
        e.dataTransfer.effectAllowed = 'copy';
        row.classList.add('dragging');
        const ghost = row.cloneNode(true);
        ghost.style.cssText = 'position:absolute;top:-9999px;left:-9999px;width:200px;opacity:0.9;'
          + 'transform:scale(1.03) rotate(-0.8deg);border-radius:6px;'
          + 'box-shadow:0 8px 18px rgba(0,0,0,0.45),0 0 0 1px var(--sidebar-accent,#b482ff);'
          + 'background:var(--sidebar-bg,#181825);pointer-events:none';
        document.body.appendChild(ghost);
        try { e.dataTransfer.setDragImage(ghost, 16, 12); } catch (_) { /* jsdom */ }
        setTimeout(() => ghost.remove(), 0);
      });
      grip.addEventListener('dragend', () => row.classList.remove('dragging'));
      row.appendChild(grip); // first child → leftmost
    }

    row.appendChild(el('span', 'plan-title', plan.title || plan.id));

    const n = unresolvedCount(plan);
    if (n > 0) row.appendChild(el('span', 'plan-decisions-badge', n + (n === 1 ? ' decision' : ' decisions')));

    // No-wake submissions leave a badge until an agent picks the plan up
    // (next session start surfaces it via the discipline hook).
    if (_submittedIds.has(plan.id)) row.appendChild(el('span', 'plan-decisions-badge', 'submitted'));

    row.appendChild(el('span', 'plan-status-pill status-' + status, status));
    row.appendChild(el('span', 'plan-updated', relativeTime(plan.updatedAt || 0)));

    // Body drag → consume into a terminal (grip already owns the grid drag).
    if (onConsumeToTerminal) {
      attachPlanDragListeners();
      row.addEventListener('mousedown', (e) => {
        if (e.button !== 0) return;
        if (e.target.closest('.plan-grip')) return;    // grip owns the native grid drag
        _planDragged = false;
        _planDrag = { plan, startX: e.clientX, startY: e.clientY, row, dragging: false };
      });
    }

    // A real drag suppresses the click (browsers usually drop the click after a
    // move, but the flag makes it deterministic; the next mousedown resets it).
    row.addEventListener('click', () => {
      if (_planDragged) { _planDragged = false; return; }
      openPlanOverlay(plan);
    });
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

    // ── Local form state — plain in-memory, discarded on close. Selecting
    //    and typing never wakes anyone; only Submit persists. ──
    const formState = {
      selections: new Map(), // decisionId -> chosen option text
      comments: new Map(),   // 'section:<id>' | 'decision:<id>' | 'general' -> text
    };

    // Persisted comments render on OPEN so a user always sees what they (and
    // the agent) previously wrote — plan comments are durable, not ephemeral.
    // decision:<id> → that decision's slot; everything else (general + section-
    // anchored) → the general slot for now, so nothing a user wrote is hidden
    // (section-anchored ones move inline once the geometry bridge lands).
    const priorComments = Array.isArray(plan.comments) ? plan.comments : [];
    function priorFor(key) {
      return priorComments.filter(c => key.startsWith('decision:')
        ? c.decisionId === key.slice(9)
        : (key === 'general' ? !c.decisionId : c.sectionId === key.slice(8)));
    }

    function commentCount() {
      let n = 0;
      for (const t of formState.comments.values()) if (t.trim()) n++;
      return n;
    }

    function makeCommentSlot(key, placeholder, addLabel) {
      const slot = el('div', 'plan-comment-slot');
      // Existing thread first — so reopening a plan shows prior comments.
      for (const c of priorFor(key)) {
        const bubble = el('div', 'plan-comment-prior');
        const who = (c.author && String(c.author).split('@')[0]) || 'user';
        let metaText = who + ' · ' + relativeTime(c.ts || 0);
        if (key === 'general' && c.sectionId) metaText += ' · on ' + c.sectionId;
        bubble.appendChild(el('div', 'plan-comment-prior-meta', metaText));
        bubble.appendChild(el('div', 'plan-comment-prior-text', c.text || ''));
        slot.appendChild(bubble);
      }
      const input = el('textarea', 'plan-comment-input');
      input.placeholder = placeholder;
      // Restore an unsubmitted draft (kept across close/reopen) so typing is
      // never lost. Seed formState so Submit reflects it immediately on open.
      const draftKey = plan.id + ' ' + key;
      const draftVal = _planDrafts.get(draftKey) || '';
      if (draftVal) { input.value = draftVal; formState.comments.set(key, draftVal); }
      if (addLabel) {
        const add = el('button', 'plan-comment-add', addLabel);
        add.type = 'button';
        if (draftVal) { add.hidden = true; } else { input.hidden = true; } // a draft forces the box open
        add.addEventListener('click', () => { add.hidden = true; input.hidden = false; input.focus(); });
        slot.appendChild(add);
      }
      input.addEventListener('input', () => {
        formState.comments.set(key, input.value);
        if (input.value.trim()) _planDrafts.set(draftKey, input.value);
        else _planDrafts.delete(draftKey);
        updateSubmitBar();
      });
      // Keep terminal keybindings out of the textarea (Escape still closes).
      input.addEventListener('keydown', (e) => { if (e.key !== 'Escape') e.stopPropagation(); });
      slot.appendChild(input);
      return slot;
    }

    // The plan BODY renders as a SANDBOXED IFRAME — a real document with full
    // JS, isolated in an opaque origin (no allow-same-origin), the claude.ai
    // artifact model. A strict per-frame CSP keeps it self-contained. The
    // decision + general-comment + submit chrome below is native and unchanged.
    // (Section-anchored comments move to a geometry bridge next — a sandboxed
    // frame can't be injected into.)
    const bodyHost = el('div', 'plan-body-host');
    renderPlanArtifactIframe(bodyHost, plan.html);
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
    updateSubmitBar(); // reflect any restored drafts immediately on open

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
          // Submitted successfully → drop this plan's saved drafts (they're
          // now persisted as real comments).
          for (const k of [..._planDrafts.keys()]) {
            if (k.indexOf(plan.id + ' ') === 0) _planDrafts.delete(k);
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
