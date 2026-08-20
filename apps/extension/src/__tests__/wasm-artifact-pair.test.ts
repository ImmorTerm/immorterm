import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

describe('packaged WASM artifacts', () => {
  it('provides a JavaScript callable for every binary function import', () => {
    const artifactDir = path.resolve(__dirname, '../../resources/wasm');
    const wasm = fs.readFileSync(path.join(artifactDir, 'immorterm_wasm_bg.wasm'));
    const glue = fs.readFileSync(path.join(artifactDir, 'immorterm_wasm.js'), 'utf8');
    const imports = WebAssembly.Module.imports(new WebAssembly.Module(wasm));

    const missing = imports
      .filter((entry) => entry.kind === 'function')
      .map((entry) => entry.name)
      .filter((name) => !glue.includes(`${name}: function`));

    expect(missing).toEqual([]);
  });
});
