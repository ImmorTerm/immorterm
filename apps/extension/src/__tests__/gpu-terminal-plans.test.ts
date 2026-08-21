// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { createPlansPanel, unwrapPlanHtml, renderPlanArtifactIframe } from '../../resources/gpu-terminal-plans.js';

describe('unwrapPlanHtml — strip authoring wrappers that corrupt HTML parsing', () => {
  it('strips a leading <![CDATA[ … ]]> wrapper (the delulus break)', () => {
    const cdata = '<![CDATA[\n<style>\n  .lim { color: red }\n</style>\n<div>hi</div>\n]]>';
    const out = unwrapPlanHtml(cdata);
    expect(out.startsWith('<style>')).toBe(true);
    expect(out.includes('<![CDATA[')).toBe(false);
    expect(out.endsWith(']]>')).toBe(false);
  });

  it('strips a markdown code fence (```html … ```)', () => {
    expect(unwrapPlanHtml('```html\n<style>.a{}</style>\n<p>x</p>\n```'))
      .toBe('<style>.a{}</style>\n<p>x</p>');
  });

  it('leaves clean html untouched and is idempotent', () => {
    const clean = '<style>.a{color:red}</style><div>hi</div>';
    expect(unwrapPlanHtml(clean)).toBe(clean);
    expect(unwrapPlanHtml(unwrapPlanHtml(clean))).toBe(clean);
  });

  it('is safe on empty / null', () => {
    expect(unwrapPlanHtml('')).toBe('');
    expect(unwrapPlanHtml(null as unknown as string)).toBe('');
  });
});

describe('renderPlanArtifactIframe — sandboxed artifact + wake bridge', () => {
  it('renders a sandboxed iframe carrying the wake bridge', () => {
    const host = document.createElement('div');
    const frame = renderPlanArtifactIframe(host, '<button data-plan-action="go">Go</button>');
    expect(frame.tagName).toBe('IFRAME');
    // opaque origin: allow-scripts WITHOUT allow-same-origin
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts');
    const doc = frame.getAttribute('srcdoc') || '';
    expect(doc).toContain('data-plan-action');   // author's button survives
    expect(doc).toContain('__immPlanFrame');      // bridge present
    // the security gate: only genuine user clicks are forwarded
    expect(doc).toContain('ev.isTrusted');
  });

  it('embeds a strict CSP that blocks external network', () => {
    const host = document.createElement('div');
    const frame = renderPlanArtifactIframe(host, '<p>hi</p>');
    const doc = frame.getAttribute('srcdoc') || '';
    expect(doc).toContain("default-src 'none'");
    expect(doc).not.toContain('connect-src');     // no network egress granted
  });
});

describe('Plans sidebar lifecycle controls', () => {
  it('separates current and archived plans and exposes reversible archive plus confirmed delete', () => {
    document.body.innerHTML = '<div id="plans-header"><button id="archive-plans-btn"></button></div><div id="plans-list"></div>';
    const header = document.getElementById('plans-header')!;
    const list = document.getElementById('plans-list')!;
    const archived: Array<[string, boolean]> = [];
    const deleted: string[] = [];
    const panel = createPlansPanel({
      plansHeaderEl: header,
      plansListEl: list,
      requestPlans: () => {},
      getPlansMode: () => 'visible',
      onHasContent: () => {},
      submitPlan: () => {},
      archivePlan: (id: string, value: boolean) => archived.push([id, value]),
      deletePlan: (id: string) => deleted.push(id),
      wakeAgent: () => false,
      enableGridDrag: false,
    });
    panel.setPlans([
      { id: 'current', title: 'Current', status: 'active', updatedAt: 2 },
      { id: 'old', title: 'Old', status: 'done', updatedAt: 1, _archived: true },
    ]);

    expect(list.textContent).toContain('Current');
    expect(list.textContent).not.toContain('Old');
    (list.querySelector('.codicon-archive') as HTMLButtonElement).click();
    expect(archived).toEqual([['current', true]]);

    const archiveToggle = document.getElementById('archive-plans-btn') as HTMLButtonElement;
    archiveToggle.click();
    expect(list.textContent).toContain('Old');
    expect(list.textContent).not.toContain('Current');
    (list.querySelector('.codicon-debug-restart') as HTMLButtonElement).click();
    expect(archived).toEqual([['current', true], ['old', false]]);

    const deleteButton = list.querySelector('.plan-delete') as HTMLButtonElement;
    deleteButton.click();
    expect(deleteButton.classList.contains('confirming')).toBe(true);
    expect(deleteButton.title).toBe('Click again to permanently delete');
    expect(deleted).toEqual([]);
    deleteButton.click();
    expect(deleted).toEqual(['old']);
  });
});
