// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { unwrapPlanHtml, renderPlanArtifactIframe } from '../../resources/gpu-terminal-plans.js';

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
