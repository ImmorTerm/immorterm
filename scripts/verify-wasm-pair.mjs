#!/usr/bin/env node

import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import path from 'node:path';

const artifactDir = path.resolve(process.argv[2] ?? 'apps/extension/resources/wasm');
const gluePath = path.join(artifactDir, 'immorterm_wasm.js');
const wasmPath = path.join(artifactDir, 'immorterm_wasm_bg.wasm');

const bytes = await readFile(wasmPath);
const module = new WebAssembly.Module(bytes);
const glue = await import(`${pathToFileURL(gluePath).href}?pair-proof=${Date.now()}`);

await glue.default({ module_or_path: bytes });

if (typeof glue.WasmTerminal !== 'function') {
  throw new Error('WASM initialized but did not export a callable WasmTerminal');
}

const imports = WebAssembly.Module.imports(module);
console.log(
  `Verified matched WASM pair: ${imports.length} imports, WasmTerminal callable`
);
