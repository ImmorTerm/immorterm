import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

interface RestoreSemaphoreOptions {
  rootDir?: string;
  slots?: number;
  pollMs?: number;
  staleMs?: number;
  timeoutMs?: number;
  ownerPid?: number;
  isProcessAlive?: (pid: number) => boolean;
}

interface SlotOwner {
  pid: number;
  token: string;
  acquiredAt: number;
}

function defaultIsProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function readOwner(slotDir: string): SlotOwner | undefined {
  try {
    return JSON.parse(fs.readFileSync(path.join(slotDir, 'owner.json'), 'utf8')) as SlotOwner;
  } catch {
    return undefined;
  }
}

/**
 * Cross-extension-host semaphore for cold-boot daemon respawns.
 *
 * Every VS Code window owns a separate extension-host process, so an in-memory
 * concurrency cap is multiplied by the number of restored windows. Atomic
 * directory creation provides a small machine-wide budget without requiring a
 * coordinator service to already be alive during boot.
 */
export async function acquireGlobalRestoreSlot(
  options: RestoreSemaphoreOptions = {},
): Promise<() => void> {
  const rootDir = options.rootDir ?? path.join(os.homedir(), '.immorterm', 'restore-slots');
  const slots = Math.max(1, options.slots ?? 4);
  const pollMs = Math.max(10, options.pollMs ?? 150);
  const staleMs = Math.max(1_000, options.staleMs ?? 10 * 60_000);
  const timeoutMs = Math.max(staleMs, options.timeoutMs ?? 15 * 60_000);
  const ownerPid = options.ownerPid ?? process.pid;
  const isProcessAlive = options.isProcessAlive ?? defaultIsProcessAlive;
  const token = crypto.randomUUID();
  const deadline = Date.now() + timeoutMs;

  fs.mkdirSync(rootDir, { recursive: true });

  while (Date.now() < deadline) {
    for (let index = 0; index < slots; index++) {
      const slotDir = path.join(rootDir, `slot-${index}`);
      try {
        fs.mkdirSync(slotDir);
        const owner: SlotOwner = { pid: ownerPid, token, acquiredAt: Date.now() };
        fs.writeFileSync(path.join(slotDir, 'owner.json'), JSON.stringify(owner), { mode: 0o600 });
        let released = false;
        return () => {
          if (released) return;
          released = true;
          const current = readOwner(slotDir);
          if (current?.token === token) {
            fs.rmSync(slotDir, { recursive: true, force: true });
          }
        };
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
      }

      const owner = readOwner(slotDir);
      let expired = false;
      try {
        const ageMs = Date.now() - fs.statSync(slotDir).mtimeMs;
        expired = ageMs > staleMs || (!!owner && !isProcessAlive(owner.pid));
      } catch {
        continue;
      }
      if (!expired) continue;

      // Rename first so cleanup cannot delete a slot another process just
      // acquired between our stale check and removal.
      const quarantine = path.join(
        rootDir,
        `.stale-slot-${index}-${ownerPid}-${crypto.randomUUID()}`,
      );
      try {
        fs.renameSync(slotDir, quarantine);
        fs.rmSync(quarantine, { recursive: true, force: true });
      } catch {
        // Another extension host won the stale-slot race.
      }
    }
    await new Promise<void>((resolve) => setTimeout(resolve, pollMs));
  }

  throw new Error(`Timed out waiting for an ImmorTerm restore slot after ${timeoutMs}ms`);
}
