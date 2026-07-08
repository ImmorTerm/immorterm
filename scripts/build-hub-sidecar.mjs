#!/usr/bin/env node
/**
 * Build immorterm-hub and stage it as a Tauri externalBin.
 *
 * Tauri's externalBin resolver expects `<name>-<target-triple>[.exe]`
 * next to the path declared in tauri.conf.json. At bundle time it
 * copies that file into the .app/Resources (or equivalent) with the
 * platform-local name (`.exe` on Windows, bare elsewhere).
 *
 * This script is cross-platform (Node runs everywhere the Tauri CLI
 * does) — no bash dependency, no PowerShell variant needed. Wired to
 * `beforeBundleCommand` in tauri.conf.json so `tauri build` always
 * stages a fresh hub.
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, chmodSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..");

// Accept an optional --target <triple> argument so CI can cross-compile
// the hub for each Tauri bundle platform (see 01-build.yml build-tauri-app
// matrix). Without it, we build for the host triple — the dev default.
function parseArgs(argv) {
  const out = { target: null };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--target" && argv[i + 1]) {
      out.target = argv[i + 1];
      i++;
    }
  }
  return out;
}

function hostTriple() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const m = out.match(/^host:\s*(\S+)/m);
  if (!m) throw new Error("could not parse host triple from rustc -vV");
  return m[1];
}

const { target: argTarget } = parseArgs(process.argv.slice(2));
const triple = argTarget || hostTriple();
const exe = triple.includes("windows") ? ".exe" : "";
const crossCompile = argTarget && argTarget !== hostTriple();

console.log(`[hub-sidecar] triple=${triple}${crossCompile ? " (cross)" : ""}`);
console.log(`[hub-sidecar] building release hub...`);
execFileSync(
  "cargo",
  [
    "build",
    "--release",
    "-p",
    "immorterm-hub",
    ...(argTarget ? ["--target", argTarget] : []),
    "--manifest-path",
    join(REPO_ROOT, "Cargo.toml"),
  ],
  { stdio: "inherit" },
);

const binDir = argTarget
  ? join(REPO_ROOT, "target", argTarget, "release")
  : join(REPO_ROOT, "target", "release");
const src = join(binDir, `immorterm-hub${exe}`);
const destDir = join(
  REPO_ROOT,
  "apps",
  "immorterm-app",
  "src-tauri",
  "binaries",
);
const dest = join(destDir, `immorterm-hub-${triple}${exe}`);

mkdirSync(destDir, { recursive: true });
copyFileSync(src, dest);
if (!exe) chmodSync(dest, 0o755);
console.log(`[hub-sidecar] staged ${dest}`);
