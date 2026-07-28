/**
 * Vendor detection — which AI CLIs are actually installed on this machine.
 *
 * Backs the vendor picker in `immorterm init` so the default selection is
 * already right and the user only has to confirm.
 *
 * TS twin of `services/hub/src/routes/vendors_api.rs`, which serves the same
 * probe over HTTP to the webview wizard. The CLI can't use that endpoint —
 * `immorterm init` runs before any service is up — so the table lives in both
 * places. Keep them in sync; the Rust side additionally probes `llm`/`ollama`
 * (digest providers, not vendors) and Codex's per-project hook trust state.
 *
 * Deliberately cheap: a PATH scan plus a stat of each vendor's state dir. No
 * subprocess, no `--version` call — nine cold CLI spawns would stall the
 * wizard for seconds.
 */

import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { VendorId } from '@immorterm/config';

export interface VendorProbe {
  id: VendorId;
  display: string;
  /** Executable name looked up on PATH. */
  bin: string;
  installed: boolean;
  /**
   * A state path exists under $HOME — the tool has been run at least once.
   * Presence-only; we never read contents. False negatives are fine, false
   * positives are not, so the paths are the dirs each tool creates on first run.
   */
  configured: boolean;
}

const VENDORS: ReadonlyArray<{
  id: VendorId;
  display: string;
  bin: string;
  state: readonly string[];
}> = [
  { id: 'claudeCode', display: 'Claude Code', bin: 'claude', state: ['.claude/sessions', '.claude/projects', '.claude/history.jsonl'] },
  { id: 'codex', display: 'OpenAI Codex', bin: 'codex', state: ['.codex/sessions', '.codex/auth.json', '.codex/log'] },
  { id: 'cursor', display: 'Cursor', bin: 'cursor-agent', state: ['.cursor/auth.json', 'Library/Application Support/cursor-agent'] },
  { id: 'windsurf', display: 'Windsurf', bin: 'windsurf', state: ['.windsurf/auth.json', '.codeium'] },
  { id: 'cline', display: 'Cline', bin: 'cline', state: ['.cline/auth.json', '.clinerules'] },
  { id: 'opencode', display: 'opencode', bin: 'opencode', state: ['.local/share/opencode/auth.json', '.local/share/opencode'] },
  { id: 'gemini', display: 'Gemini CLI', bin: 'gemini', state: ['.gemini/oauth_creds.json', '.gemini'] },
  { id: 'copilot', display: 'GitHub Copilot', bin: 'copilot', state: ['.copilot/auth.json', '.copilot'] },
  { id: 'aider', display: 'Aider', bin: 'aider', state: ['.aider.chat.history.md', '.aider'] },
];

/** Whether `bin` resolves to an executable on PATH. */
export function isOnPath(bin: string): boolean {
  const dirs = (process.env.PATH ?? '').split(path.delimiter).filter(Boolean);
  for (const dir of dirs) {
    try {
      fs.accessSync(path.join(dir, bin), fs.constants.X_OK);
      return true;
    } catch {
      // next
    }
  }
  return false;
}

/** Probe every vendor. Never throws — an unreadable $HOME just yields all-false. */
export function detectVendors(): VendorProbe[] {
  const home = os.homedir();
  return VENDORS.map(({ id, display, bin, state }) => ({
    id,
    display,
    bin,
    installed: isOnPath(bin),
    configured: state.some((rel) => {
      try {
        return fs.existsSync(path.join(home, rel));
      } catch {
        return false;
      }
    }),
  }));
}

/**
 * The vendors a first run should pre-tick: installed AND used at least once.
 *
 * Installed-but-never-run is deliberately excluded — enabling a vendor drops
 * config files into the project root, and a binary that came along for the ride
 * with some other install is not a signal the user wants that.
 */
export function detectedVendorIds(probes: VendorProbe[] = detectVendors()): VendorId[] {
  return probes.filter((p) => p.installed && p.configured).map((p) => p.id);
}
