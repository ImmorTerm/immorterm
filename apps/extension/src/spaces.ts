/**
 * SpacesStorage — read/WRITE mirror of ~/.immorterm/spaces/<projectId>/index.json
 * (the SP2 docking-grid model). Unlike PlansStorage (read-only, daemon writes),
 * the webview owns spaces, so this both reads for the sidebar and persists the
 * index the webview posts on every debounced change.
 *
 * Mirrors PlansStorage's watch()/dispose() shape; the atomic write follows the
 * extension write precedent (registry-client.ts:196 — tmp + rename).
 */
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { EventEmitter } from 'events';

/** Default index for a project that has never created a space. */
function emptyIndex(): Record<string, unknown> {
  return { version: 1, order: [], active: null, spaces: {} };
}

export class SpacesStorage extends EventEmitter {
  private dir: string;
  private file: string;
  private watcher: fs.FSWatcher | null = null;
  private debounce: ReturnType<typeof setTimeout> | null = null;
  /** Set while WE are writing so our own fs.watch event doesn't echo back. */
  private writing = false;

  constructor(projectId: string) {
    super();
    this.dir = path.join(os.homedir(), '.immorterm', 'spaces', projectId);
    this.file = path.join(this.dir, 'index.json');
    try { fs.mkdirSync(this.dir, { recursive: true }); } catch { /* exists */ }
    this.watch();
  }

  /** The whole index.json (or an empty index if none/corrupt). */
  load(): Record<string, unknown> {
    try {
      return JSON.parse(fs.readFileSync(this.file, 'utf-8'));
    } catch {
      return emptyIndex();
    }
  }

  /** Atomic write (tmp + rename) of the full index blob the webview owns. */
  save(index: unknown): void {
    try {
      this.writing = true;
      const tmp = `${this.file}.${process.pid}.tmp`;
      fs.writeFileSync(tmp, JSON.stringify(index, null, 2), 'utf-8');
      fs.renameSync(tmp, this.file);
    } catch {
      /* best effort — an unwritable spaces dir shouldn't crash the webview */
    } finally {
      // Release the self-write guard after the fs.watch burst settles.
      setTimeout(() => { this.writing = false; }, 200);
    }
  }

  private watch(): void {
    const onEvent = () => { if (!this.writing) this.schedule(); };
    try {
      // ponytail: single dir, no recursion needed (one index.json).
      this.watcher = fs.watch(this.dir, onEvent);
    } catch {
      /* no live sync — sidebar still loads on demand */
    }
  }

  private schedule(): void {
    if (this.debounce) clearTimeout(this.debounce);
    this.debounce = setTimeout(() => this.emit('change'), 150);
  }

  dispose(): void {
    this.watcher?.close();
    this.watcher = null;
    if (this.debounce) clearTimeout(this.debounce);
    this.removeAllListeners();
  }
}
