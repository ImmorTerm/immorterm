import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { acquireGlobalRestoreSlot } from '../restore-semaphore';

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

function options(rootDir: string) {
  return {
    rootDir,
    slots: 2,
    pollMs: 10,
    staleMs: 1_000,
    timeoutMs: 2_000,
    ownerPid: process.pid,
  };
}

describe('acquireGlobalRestoreSlot', () => {
  it('caps concurrent owners across callers and releases a slot', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'immorterm-restore-slots-'));
    roots.push(root);
    const release1 = await acquireGlobalRestoreSlot(options(root));
    const release2 = await acquireGlobalRestoreSlot(options(root));

    let thirdAcquired = false;
    const third = acquireGlobalRestoreSlot(options(root)).then((release) => {
      thirdAcquired = true;
      return release;
    });
    await new Promise((resolve) => setTimeout(resolve, 40));
    expect(thirdAcquired).toBe(false);

    release1();
    const release3 = await third;
    expect(thirdAcquired).toBe(true);
    release2();
    release3();
  });

  it('reclaims a slot owned by a dead extension host', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'immorterm-restore-slots-'));
    roots.push(root);
    const slot = path.join(root, 'slot-0');
    fs.mkdirSync(slot);
    fs.writeFileSync(path.join(slot, 'owner.json'), JSON.stringify({ pid: 999_999, token: 'dead', acquiredAt: 0 }));

    const release = await acquireGlobalRestoreSlot({
      ...options(root),
      slots: 1,
      isProcessAlive: () => false,
    });
    expect(JSON.parse(fs.readFileSync(path.join(slot, 'owner.json'), 'utf8')).token).not.toBe('dead');
    release();
  });
});
