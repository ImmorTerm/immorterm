#!/usr/bin/env node
// Drives the immorterm_browser_* scenario over the daemon's stdio MCP server
// (`immorterm-ai mcp serve`, newline-delimited JSON-RPC 2.0). The browser is
// process-global inside the MCP server, so ALL calls go through this single
// spawned process. We never signal the server — closing its stdin makes the
// serve loop exit on EOF.
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

// Center coordinates of the fixture's interactive elements, in CSS pixels
// (the same space browser_click takes — DPR is pinned to 1).
const COORDS_JS = `(() => {
  const c = {};
  for (const id of ['name', 'check', 'go']) {
    const r = document.getElementById(id).getBoundingClientRect();
    c[id] = { x: Math.round(r.x + r.width / 2), y: Math.round(r.y + r.height / 2) };
  }
  return JSON.stringify(c);
})()`;

// A process counts as gone when ps no longer lists it, or lists it as a
// zombie (the MCP server never reaps the browser child it forgot).
function pidGone(pid) {
  try {
    const state = execFileSync('ps', ['-o', 'state=', '-p', String(pid)]).toString().trim();
    return state === '' || state.startsWith('Z');
  } catch {
    return true; // ps exits non-zero when the pid doesn't exist
  }
}

(async () => {
  let coords = null;
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

  await step('read shows fixture title', async () => {
    const t = text(await call('immorterm_browser_read'));
    if (!t.includes('ImmorTerm Browser Fixture')) throw new Error(`title missing in: ${t.slice(0, 120)}`);
  });

  await step('eval locates form coords', async () => {
    coords = JSON.parse(text(await call('immorterm_browser_eval', { js: COORDS_JS })));
    if (!coords.name || !coords.check || !coords.go) throw new Error('missing coords');
  });

  await step('click focuses text input', async () => {
    const c = await call('immorterm_browser_click', coords.name);
    if (!image(c)) throw new Error('no screenshot after click');
    const active = text(await call('immorterm_browser_eval', { js: 'document.activeElement.id' }));
    if (active !== 'name') throw new Error(`activeElement=${active}`);
  });

  await step('type fills input', async () => {
    await call('immorterm_browser_type', { text: 'mort' });
    const v = text(await call('immorterm_browser_eval', { js: "document.getElementById('name').value" }));
    if (v !== 'mort') throw new Error(`input value=${v}`);
  });

  await step('click checks checkbox', async () => {
    await call('immorterm_browser_click', coords.check);
    const v = text(await call('immorterm_browser_eval', { js: "String(document.getElementById('check').checked)" }));
    if (v !== 'true') throw new Error(`checked=${v}`);
  });

  await step('submit renders SUBMITTED', async () => {
    await call('immorterm_browser_click', coords.go);
    const t = text(await call('immorterm_browser_read'));
    if (!t.includes('SUBMITTED:mort:checked')) throw new Error(`no SUBMITTED marker in: ...${t.slice(-200)}`);
  });

  await step('screenshot is a >10KB png', async () => {
    const img = image(await call('immorterm_browser_screenshot'));
    if (!img) throw new Error('no image block');
    if (img.mimeType !== 'image/png') throw new Error(`mimeType=${img.mimeType}`);
    const bytes = Buffer.from(img.data, 'base64').length;
    if (bytes <= 10240) throw new Error(`png only ${bytes} bytes`);
  });

  await step('close reports the browser pid', async () => {
    const t = text(await call('immorterm_browser_close'));
    const m = t.match(/pid (\d+)/);
    if (!m) throw new Error(`no pid in: ${t}`);
    browserPid = Number(m[1]);
  });

  await step('browser child pid is gone', async () => {
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
