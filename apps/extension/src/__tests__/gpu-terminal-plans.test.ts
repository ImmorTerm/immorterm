// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { unwrapPlanHtml } from '../../resources/gpu-terminal-plans.js';

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
    // @ts-expect-error — exercising the null guard
    expect(unwrapPlanHtml(null)).toBe('');
  });
});
