/**
 * PlansStorage — read-only mirror of ~/.immorterm/plans/<projectId>/<planId>/current.json
 * (written by the daemon's immorterm_plan MCP tools, S3 commit 9443489).
 * Watches the tree and emits 'change' so the webview Plans sidebar stays live.
 * Mirrors tasks/storage.ts watchFile().
 */
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { EventEmitter } from 'events';

export class PlansStorage extends EventEmitter {
  private dir: string;
  private watcher: fs.FSWatcher | null = null;
  private debounce: ReturnType<typeof setTimeout> | null = null;

  constructor(projectId: string) {
    super();
    this.dir = path.join(os.homedir(), '.immorterm', 'plans', projectId);
    // Create so fs.watch doesn't ENOENT before the first plan is written.
    try { fs.mkdirSync(this.dir, { recursive: true }); } catch { /* exists */ }
    this.watch();
  }

  /** All current.json records for this project (corrupt/non-plan entries skipped). */
  list(): unknown[] {
    const plans: unknown[] = [];
    let ids: string[] = [];
    try { ids = fs.readdirSync(this.dir); } catch { return plans; }
    for (const id of ids) {
      try {
        plans.push(JSON.parse(fs.readFileSync(path.join(this.dir, id, 'current.json'), 'utf-8')));
      } catch { /* .lock, history-only dir, or corrupt — daemon sidelines corrupt on next write */ }
    }
    return plans;
  }

  private watch(): void {
    try {
      // ponytail: {recursive:true} works on macOS/Windows; Linux throws —
      // fall back to top-level (new plan dirs still fire; content rewrites
      // catch up via the daemon's plan_changed WS event in the webview).
      this.watcher = fs.watch(this.dir, { recursive: true }, () => this.schedule());
    } catch {
      try { this.watcher = fs.watch(this.dir, () => this.schedule()); } catch { /* no live sync */ }
    }
  }

  private schedule(): void {
    if (this.debounce) clearTimeout(this.debounce);
    this.debounce = setTimeout(() => this.emit('change'), 150); // coalesce tmp+rename bursts
  }

  dispose(): void {
    this.watcher?.close();
    this.watcher = null;
    if (this.debounce) clearTimeout(this.debounce);
    this.removeAllListeners();
  }
}
