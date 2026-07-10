#!/usr/bin/env node
// Drives the immorterm_browser_* scenario over the daemon's stdio MCP server
// (`immorterm-ai mcp serve`, newline-delimited JSON-RPC 2.0). The browser is
// process-global inside the MCP server, so ALL calls go through this single
// spawned process. We never signal the server — closing its stdin makes the
// serve loop exit on EOF.
//
// This exercises the HARDENED, ref-based surface:
//   open → read_page (assert ref_N handles) → find → click{ref}
//        → form_input{ref} → screenshot(>10KB) → close (exact pid gone).
// No coord clicks, no `type`, no raw `eval` — those are removed / gated off.
//
// Usage: node browser-e2e-scenario.js <immorterm-ai-bin> <fixture-url>
// Emits "OK <check>" / "BAD <check>" lines (parsed by browser-e2e.sh);
// exit code = number of failed checks.
'use strict';
const { spawn, execFileSync } = require('child_process');
const readline = require('readline');

const [BIN, URL] = process.argv.slice(2);
if (!BIN || !URL) {
  console.error('usage: browser-e2e-scenario.js <immorterm-ai-bin> <fixture-url>');
  process.exit(1);
}

const srv = spawn(BIN, ['mcp', 'serve'], { stdio: ['pipe', 'pipe', 'ignore'] });
const pending = new Map();
readline.createInterface({ input: srv.stdout }).on('line', (l) => {
  let m;
  try { m = JSON.parse(l); } catch { return; }
  const resolve = pending.get(m.id);
  if (resolve) { pending.delete(m.id); resolve(m); }
});

let nextId = 1;
function rpc(method, params, timeoutMs) {
  const id = nextId++;
  srv.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`${method} timed out after ${timeoutMs}ms`));
    }, timeoutMs);
    pending.set(id, (m) => { clearTimeout(t); resolve(m); });
  });
}

async function call(tool, args = {}, timeoutMs = 30000) {
  const resp = await rpc('tools/call', { name: tool, arguments: args }, timeoutMs);
  if (resp.error) throw new Error(`${tool}: rpc error ${JSON.stringify(resp.error)}`);
  const result = resp.result || {};
  if (result.isError) throw new Error(`${tool}: ${JSON.stringify(result.content).slice(0, 300)}`);
  return result.content || [];
}
const text = (content) => content.filter((c) => c.type === 'text').map((c) => c.text).join('\n');
const image = (content) => content.find((c) => c.type === 'image');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Parse a read_page/find listing into { ref, role, name } rows. Lines look
// like: [ref_3]  textbox  "Name"   value:""
function parseRefs(listing) {
  const rows = [];
  for (const line of listing.split('\n')) {
    const m = line.match(/^\[(ref_\d+)\]\s+(\S+)\s+"([^"]*)"/);
    if (m) rows.push({ ref: m[1], role: m[2], name: m[3] });
  }
  return rows;
}

let fails = 0;
async function step(name, fn) {
  try {
    await fn();
    console.log(`OK ${name}`);
    return true;
  } catch (e) {
    fails++;
    console.log(`BAD ${name} — ${String(e.message || e).slice(0, 200)}`);
    return false;
  }
}

// A process counts as gone when ps no longer lists it, or lists it as a
// zombie (a forgotten browser child that was never reaped).
function pidGone(pid) {
  try {
    const state = execFileSync('ps', ['-o', 'state=', '-p', String(pid)]).toString().trim();
    return state === '' || state.startsWith('Z');
  } catch {
    return true; // ps exits non-zero when the pid doesn't exist
  }
}

(async () => {
  let refs = [];
  let browserPid = null;

  const opened = await step('open returns caption + image', async () => {
    const c = await call('immorterm_browser_open', { url: URL }, 90000); // cold launch is slow
    if (!image(c)) throw new Error('no image block in response');
  });
  if (!opened) {
    console.log('BAD remaining steps skipped — browser never opened');
    fails++;
    srv.stdin.end();
    process.exit(fails);
  }

  await step('read_page lists untrusted-framed refs', async () => {
    const t = text(await call('immorterm_browser_read_page', { interactive_only: true }));
    if (!t.includes('[Untrusted web-page content follows')) throw new Error('missing untrusted frame');
    if (!t.includes('[end of untrusted web-page content]')) throw new Error('missing untrusted close');
    refs = parseRefs(t);
    if (refs.length === 0) throw new Error(`no ref_N rows in:\n${t.slice(0, 300)}`);
  });

  await step('find ranks a submit control', async () => {
    const t = text(await call('immorterm_browser_find', { query: 'submit' }));
    const hits = parseRefs(t);
    if (hits.length === 0) throw new Error(`find returned no refs:\n${t.slice(0, 200)}`);
  });

  await step('form_input{ref} fills the name field', async () => {
    const field = refs.find((r) => r.role === 'textbox');
    if (!field) throw new Error(`no textbox ref among: ${refs.map((r) => r.role).join(',')}`);
    const c = await call('immorterm_browser_form_input', { ref: field.ref, value: 'mort' });
    if (!image(c)) throw new Error('no screenshot after form_input');
  });

  await step('form_input{ref} checks the checkbox', async () => {
    const box = refs.find((r) => r.role === 'checkbox');
    if (!box) throw new Error('no checkbox ref');
    await call('immorterm_browser_form_input', { ref: box.ref, value: 'checked' });
  });

  await step('click{ref} submits the form', async () => {
    // Re-read so the submit button's ref is fresh, then click it by handle.
    const t = text(await call('immorterm_browser_find', { query: 'submit' }));
    const rows = parseRefs(t);
    const submit = rows.find((r) => r.role === 'button') || rows[0];
    if (!submit) throw new Error('no submit ref to click');
    const c = await call('immorterm_browser_click', { ref: submit.ref });
    if (!image(c)) throw new Error('no screenshot after click');
  });

  await step('page shows SUBMITTED marker', async () => {
    const t = text(await call('immorterm_browser_read_page', { interactive_only: false }));
    // Fixture writes SUBMITTED:<name>:<flavor>:<checked>; assert name + checked.
    if (!/SUBMITTED:mort:.*:checked/.test(t)) throw new Error(`no SUBMITTED marker in: ...${t.slice(-200)}`);
  });

  await step('stale ref returns a recoverable error', async () => {
    const resp = await rpc('tools/call', { name: 'immorterm_browser_click', arguments: { ref: 'ref_9999' } }, 15000);
    const r = resp.result || {};
    if (!r.isError) throw new Error('expected isError for bogus ref');
    const msg = text(r.content || []);
    if (!/read_page again/.test(msg)) throw new Error(`no recovery hint in: ${msg}`);
  });

  await step('screenshot is a >10KB png', async () => {
    const img = image(await call('immorterm_browser_screenshot'));
    if (!img) throw new Error('no image block');
    if (img.mimeType !== 'image/png') throw new Error(`mimeType=${img.mimeType}`);
    const bytes = Buffer.from(img.data, 'base64').length;
    if (bytes <= 10240) throw new Error(`png only ${bytes} bytes`);
  });

  await step('eval is gated off by default', async () => {
    // With IMMORTERM_BROWSER_EVAL unset, the tool must not appear in tools/list.
    const listed = await rpc('tools/list', {}, 15000);
    const names = ((listed.result && listed.result.tools) || []).map((t) => t.name);
    if (names.includes('immorterm_browser_eval')) throw new Error('eval tool exposed without the gate');
  });

  await step('close reports the browser pid', async () => {
    const t = text(await call('immorterm_browser_close'));
    const m = t.match(/pid (\d+)/);
    if (!m) throw new Error(`no pid in: ${t}`);
    browserPid = Number(m[1]);
  });

  await step('browser child pid is gone (exact pid)', async () => {
    if (!browserPid) throw new Error('no pid captured from close');
    for (let i = 0; i < 20; i++) { // up to 5s
      if (pidGone(browserPid)) return;
      await sleep(250);
    }
    throw new Error(`pid ${browserPid} still alive after 5s`);
  });

  srv.stdin.end(); // EOF → serve loop exits; we never kill it
  process.exit(fails);
})().catch((e) => {
  fails++;
  console.log(`BAD scenario crashed — ${String(e.message || e).slice(0, 200)}`);
  srv.stdin.end();
  process.exit(fails);
});
