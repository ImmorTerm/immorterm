import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

import { inlineWebviewStyles } from '../webview-styles';

describe('inlineWebviewStyles', () => {
  const shell = [
    '<head>',
    '<link rel="stylesheet" href="__CSS_URI__">',
    '<link rel="stylesheet" href="__CODICON_CSS_URI__">',
    '</head>',
  ].join('\n');

  it('embeds both stylesheets and the Codicon font without external style URLs', () => {
    const result = inlineWebviewStyles(
      shell,
      'html, body { display: block; }',
      '@font-face { src: url("./codicon.ttf?hash") format("truetype"); }',
      Buffer.from('font bytes'),
    );

    expect(result).toContain('data-immorterm-style="main"');
    expect(result).toContain('html, body { display: block; }');
    expect(result).toContain('data-immorterm-style="codicons"');
    expect(result).toContain('data:font/ttf;base64,Zm9udCBieXRlcw==');
    expect(result).not.toContain('__CSS_URI__');
    expect(result).not.toContain('__CODICON_CSS_URI__');
    expect(result).not.toContain('./codicon.ttf');
  });

  it('fails closed when the HTML contract changes', () => {
    expect(() => inlineWebviewStyles('<head></head>', '', '', Buffer.alloc(0))).toThrow(
      'WebView stylesheet placeholders are missing',
    );
  });

  it('prevents CSS text from terminating its generated style element', () => {
    const result = inlineWebviewStyles(
      shell,
      '/* </style><script>bad()</script> */',
      '@font-face { src: url(./codicon.ttf); }',
      Buffer.alloc(0),
    );

    expect(result).not.toContain('</style><script>');
    expect(result).toContain('<\\/style><script>');
  });

  it('makes the real packaged WebView self-contained', () => {
    const resources = path.resolve(__dirname, '../../resources');
    const mainCss = fs.readFileSync(path.join(resources, 'gpu-terminal.css'), 'utf8');
    const result = inlineWebviewStyles(
      fs.readFileSync(path.join(resources, 'gpu-terminal.html'), 'utf8'),
      mainCss,
      fs.readFileSync(path.join(resources, 'vendor/codicons/codicon.css'), 'utf8'),
      fs.readFileSync(path.join(resources, 'vendor/codicons/codicon.ttf')),
    );

    expect(result).toContain('* { margin: 0; padding: 0; box-sizing: border-box; }');
    expect(result).toContain('.codicon[class*=\'codicon-\']');
    expect(result).not.toContain('<link rel="stylesheet"');
    expect(result).not.toContain('__CSS_URI__');
    expect(result).not.toContain('__CODICON_CSS_URI__');

    // A duplicated opener here makes modern Chromium treat the remainder of
    // the file as nested under the canvas. The terminal still renders, but all
    // following sidebar/file/status rules silently stop matching.
    expect(mainCss.match(/^#terminal-canvas\s*\{/gm)).toHaveLength(1);

    // Summary cards contain a header, title, source, and preview. Keep an
    // explicit content-height floor so a crowded Inbox scrolls the list
    // instead of clipping each card's title inside a compressed row.
    expect(mainCss).toMatch(/\.inbox-card-summary\s*\{[^}]*min-block-size:\s*120px[^}]*height:\s*auto/s);
    expect(mainCss).toMatch(/\.inbox-card-summary \.ss-search-md\s*\{[^}]*-webkit-line-clamp:\s*1/s);
  });
});

describe('packaged WebView CSS structure', () => {
  const extensionRoot = path.resolve(__dirname, '../..');
  const validator = path.join(extensionRoot, 'scripts/validate-webview-css.mjs');

  it('rejects the nested-rule shape caused by a missing closing brace', () => {
    const result = spawnSync(process.execPath, [validator, '-'], {
      cwd: extensionRoot,
      encoding: 'utf8',
      input: [
        '#terminal-canvas { display: block;',
        '#sidebar { display: flex; }',
        '}',
      ].join('\n'),
    });

    expect(result.status).toBe(1);
    expect(result.stderr).toContain('#sidebar nested under #terminal-canvas');
  });
});
