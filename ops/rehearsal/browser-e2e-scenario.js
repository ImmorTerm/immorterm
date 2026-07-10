#!/usr/bin/env node
// Drives the immorterm_browser_* scenario over the daemon's stdio MCP server
// (`immorterm-ai mcp serve`, newline-delimited JSON-RPC 2.0). The browser is
// process-global inside the MCP server, so ALL calls go through this single
// spawned process. We never signal the server — closing its stdin makes the
// serve loop exit on EOF.
//
// Ref-based surface (the current contract, docs/browser-tools.md):
//   read_page → "[ref_N] role \"name\"" lines; find(query) → ranked ref list;
//   click{ref}; form_input{ref,value} for textbox/checkbox/<select>;
//   wait_for resolves on a delayed element. This harness NEVER touches
//   browser_eval — it's gated off by default (IMMORTERM_BROWSER_EVAL=1), so a
//   rehearsal that depends on it would be testing a non-default surface.
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

// "[ref_7] textbox \"Name\"  value:\"\"" → { ref:"ref_7", role:"textbox",
// name:"Name" }. Returns every ref-bearing line, in listed order.
function parseRefs(listing) {
  const out = [];
  for (const line of listing.split('\n')) {
    const m = line.match(/\[(ref_\d+)\]\s+(\S+)\s+"([^"]*)"/);
    if (m) out.push({ ref: m[1], role: m[2], name: m[3], line });
  }
  return out;
}
// First ref whose role/name matches — used to pin form fields by their label.
function pickRef(refs, roleRe, nameRe) {
  return refs.find((r) => roleRe.test(r.role) && nameRe.test(r.name));
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
// Optional step: skips (no OK/BAD, informational) when a tool isn't in this
// deploy. Used for wait_for, which post-dates the read_page/find/form_input core.
async function optionalStep(name, fn) {
  try {
    await fn();
    console.log(`OK ${name}`);
  } catch (e) {
    const msg = String(e.message || e);
    if (/Method not found|Unknown tool|not found|no such tool|-32601/i.test(msg)) {
      console.log(`SKIP ${name} — tool absent in this deploy`);
    } else {
      fails++;
      console.log(`BAD ${name} — ${msg.slice(0, 200)}`);
    }
  }
}

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
  let refs = [];
  let nameRef = null, flavorRef = null, checkRef = null, submitRef = null;
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

  await step('read_page returns [ref_N] lines', async () => {
    const listing = text(await call('immorterm_browser_read_page', { interactive_only: true }));
    refs = parseRefs(listing);
    if (refs.length < 3) throw new Error(`only ${refs.length} refs in: ${listing.slice(0, 200)}`);
    if (!/ref_\d+/.test(listing)) throw new Error('no ref_N handle in listing');
    // Pin the fields we act on by role + accessible name.
    nameRef = pickRef(refs, /textbox|text/i, /name/i);
    flavorRef = pickRef(refs, /combobox|listbox|select|menu/i, /flavor/i);
    checkRef = pickRef(refs, /checkbox/i, /agree/i);
    if (!nameRef) throw new Error(`no name textbox ref in: ${refs.map((r) => r.line).join(' | ')}`);
  });

  await step('find locates the submit button by text→ref', async () => {
    const listing = text(await call('immorterm_browser_find', { query: 'the Submit button' }));
    const found = parseRefs(listing);
    submitRef = pickRef(found, /button/i, /submit/i) || found[0];
    if (!submitRef) throw new Error(`no ref in find result: ${listing.slice(0, 200)}`);
    if (!/button/i.test(submitRef.role)) throw new Error(`top result is ${submitRef.role}, not a button`);
  });

  await step('form_input fills the name textbox by ref', async () => {
    if (!nameRef) throw new Error('no name ref from read_page');
    const c = await call('immorterm_browser_form_input', { ref: nameRef.ref, value: 'mort' });
    if (!image(c) && !text(c)) throw new Error('no caption/screenshot back');
  });

  await step('form_input sets the <select> by ref', async () => {
    if (!flavorRef) throw new Error('no flavor <select> ref from read_page');
    const c = await call('immorterm_browser_form_input', { ref: flavorRef.ref, value: 'mango' });
    if (!image(c) && !text(c)) throw new Error('no caption/screenshot back');
    // Confirm the option actually took via a fresh AX snapshot's value:"…".
    const listing = text(await call('immorterm_browser_read_page', { interactive_only: true }));
    if (!/flavor/i.test(listing) || !/mango/i.test(listing)) {
      throw new Error(`select value not 'mango' in snapshot: ${listing.slice(0, 200)}`);
    }
  });

  await step('form_input checks the checkbox by ref', async () => {
    if (!checkRef) throw new Error('no checkbox ref from read_page');
    await call('immorterm_browser_form_input', { ref: checkRef.ref, value: 'checked' });
  });

  await step('click{ref} submits → SUBMITTED marker', async () => {
    await call('immorterm_browser_click', { ref: submitRef.ref });
    const t = text(await call('immorterm_browser_read_page', { interactive_only: false }));
    // Reflects name + selected flavor + checkbox state — proves form_input on
    // the textbox, the <select>, AND the checkbox all landed.
    if (!/SUBMITTED:mort:mango:checked/.test(t)) {
      throw new Error(`no SUBMITTED:mort:mango:checked in: ...${t.slice(-220)}`);
    }
  });

  // wait_for post-dates the ref core — assert only if this deploy has it.
  await optionalStep('wait_for resolves on the delayed element', async () => {
    const c = await call('immorterm_browser_wait_for', { query: 'Delayed Ready Marker' }, 15000);
    const body = text(c);
    if (!/Delayed Ready Marker|ref_\d+/.test(body)) {
      throw new Error(`wait_for gave nothing resolvable: ${body.slice(0, 200)}`);
    }
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
