#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import postcss from 'postcss';

const requestedPath = process.argv[2] || 'resources/gpu-terminal.css';
const sourcePath = requestedPath === '-' ? '<stdin>' : path.resolve(process.cwd(), requestedPath);
const css = requestedPath === '-' ? fs.readFileSync(0, 'utf8') : fs.readFileSync(sourcePath, 'utf8');

let root;
try {
  root = postcss.parse(css, { from: sourcePath });
} catch (error) {
  console.error(`[webview-css] parse failed: ${error.message}`);
  process.exit(1);
}

// ImmorTerm's WebView stylesheet intentionally uses flat qualified rules.
// CSS nesting is valid in modern Chromium, which means a missing closing brace
// can silently turn the rest of the file into nested rules instead of raising a
// parse error. Reject every rule-within-rule so that failure mode is impossible.
const nestedRules = [];
root.walkRules((rule) => {
  if (rule.parent?.type !== 'rule') return;
  nestedRules.push({
    selector: rule.selector,
    parent: rule.parent.selector,
    line: rule.source?.start?.line ?? 0,
    column: rule.source?.start?.column ?? 0,
  });
});

if (nestedRules.length > 0) {
  console.error('[webview-css] nested qualified rules are forbidden:');
  for (const rule of nestedRules.slice(0, 20)) {
    console.error(
      `  ${sourcePath}:${rule.line}:${rule.column} ${rule.selector} nested under ${rule.parent}`,
    );
  }
  if (nestedRules.length > 20) {
    console.error(`  ... ${nestedRules.length - 20} more`);
  }
  process.exit(1);
}

const requiredSelectors = ['#container', '#terminal-canvas', '#sidebar', '#file-browser'];
const selectors = new Set();
root.walkRules((rule) => {
  for (const selector of rule.selectors || []) selectors.add(selector.trim());
});

const missing = requiredSelectors.filter((selector) => !selectors.has(selector));
if (missing.length > 0) {
  console.error(`[webview-css] required structural selectors missing: ${missing.join(', ')}`);
  process.exit(1);
}

console.log(
  `[webview-css] ${sourcePath}: ${root.nodes.length} top-level nodes; no nested qualified rules`,
);
