import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { mkdtempSync, rmSync, writeFileSync, readFileSync, mkdirSync } from 'fs';
import { tmpdir } from 'os';

describe('AutoUpdateConfig in GlobalConfig', () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = mkdtempSync(path.join(tmpdir(), 'immorterm-config-test-'));
    mkdirSync(path.join(tmpDir, '.immorterm'), { recursive: true });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    rmSync(tmpDir, { recursive: true, force: true });
  });

  it('returns default autoUpdate config when no config file exists', async () => {
    vi.resetModules();
    vi.doMock('os', async () => {
      const actual = await vi.importActual('os');
      return { ...actual as object, homedir: () => tmpDir };
    });

    const { readGlobalConfig } = await import('../utils/immorterm-config');
    const config = readGlobalConfig();

    expect(config.autoUpdate).toEqual({
      enabled: true,
      checkIntervalHours: 6,
      lastCheckedAt: null,
    });
  });

  it('persists autoUpdate config through write/read cycle', async () => {
    vi.resetModules();
    vi.doMock('os', async () => {
      const actual = await vi.importActual('os');
      return { ...actual as object, homedir: () => tmpDir };
    });

    const { readGlobalConfig, writeGlobalConfig } = await import('../utils/immorterm-config');

    const config = readGlobalConfig();
    config.autoUpdate = {
      enabled: false,
      checkIntervalHours: 24,
      lastCheckedAt: '2026-03-01T00:00:00.000Z',
    };
    writeGlobalConfig(config);

    const reloaded = readGlobalConfig();
    expect(reloaded.autoUpdate).toEqual({
      enabled: false,
      checkIntervalHours: 24,
      lastCheckedAt: '2026-03-01T00:00:00.000Z',
    });
  });

  it('preserves existing config fields when adding autoUpdate', async () => {
    // Write a config without autoUpdate (legacy format)
    const configPath = path.join(tmpDir, '.immorterm', 'config.json');
    writeFileSync(configPath, JSON.stringify({
      version: 1,
      license: { key: 'test-key', status: 'active' },
      defaults: { services: { memory: { enabled: true } } },
    }));

    vi.resetModules();
    vi.doMock('os', async () => {
      const actual = await vi.importActual('os');
      return { ...actual as object, homedir: () => tmpDir };
    });

    const { readGlobalConfig, writeGlobalConfig } = await import('../utils/immorterm-config');

    const config = readGlobalConfig();
    // License should be preserved from file
    expect(config.license.key).toBe('test-key');
    // autoUpdate comes from spread defaults
    expect(config.autoUpdate).toEqual({
      enabled: true,
      checkIntervalHours: 6,
      lastCheckedAt: null,
    });

    // Write it back with autoUpdate modified
    config.autoUpdate!.lastCheckedAt = '2026-03-02T12:00:00.000Z';
    writeGlobalConfig(config);

    // Re-read raw and verify both fields survive
    const raw = JSON.parse(readFileSync(configPath, 'utf-8'));
    expect(raw.license.key).toBe('test-key');
    expect(raw.autoUpdate.lastCheckedAt).toBe('2026-03-02T12:00:00.000Z');
  });

  it('throws on corrupted config — never silently falls back to defaults', async () => {
    // Regression: a silent default fallback on parse error caused the next
    // writer (e.g. update-checker) to commit defaults to disk, wiping the
    // license block. The contract now is: corruption surfaces, callers
    // decide whether to overwrite.
    const configPath = path.join(tmpDir, '.immorterm', 'config.json');
    writeFileSync(configPath, 'not valid json{{{');

    vi.resetModules();
    vi.doMock('os', async () => {
      const actual = await vi.importActual('os');
      return { ...actual as object, homedir: () => tmpDir };
    });

    const { readGlobalConfig } = await import('../utils/immorterm-config');
    expect(() => readGlobalConfig()).toThrow(/failed to parse/);
  });

  it('write is atomic — no .tmp file lingers and inode changes', async () => {
    // Regression: writeFileSync truncates-then-writes; a concurrent reader
    // could see an empty/partial file → parse fail → silent-default cascade.
    // Atomic via tmp + rename means: (1) no .tmp.* leftover after write,
    // (2) the inode changes across writes (proof it was renamed, not edited
    // in place — an in-place writeFileSync keeps the same inode).
    vi.resetModules();
    vi.doMock('os', async () => {
      const actual = await vi.importActual('os');
      return { ...actual as object, homedir: () => tmpDir };
    });

    const { readGlobalConfig, writeGlobalConfig } = await import('../utils/immorterm-config');
    const config = readGlobalConfig();
    config.license.devTierOverride = 'pro';
    writeGlobalConfig(config);

    const configPath = path.join(tmpDir, '.immorterm', 'config.json');
    const inode1 = fs.statSync(configPath).ino;

    config.license.key = 'TEST-KEY';
    writeGlobalConfig(config);
    const inode2 = fs.statSync(configPath).ino;

    expect(inode2).not.toBe(inode1);

    const dirEntries = fs.readdirSync(path.dirname(configPath));
    expect(dirEntries.filter(f => f.includes('.tmp.'))).toEqual([]);

    // Final contents are valid JSON with both edits preserved.
    const final = JSON.parse(fs.readFileSync(configPath, 'utf-8'));
    expect(final.license.devTierOverride).toBe('pro');
    expect(final.license.key).toBe('TEST-KEY');
  });

  it('license fields survive write/read cycle — regression for the wipe bug', async () => {
    // The actual bug: update-checker.ts read the config, modified
    // autoUpdate.lastCheckedAt, wrote back — and the license block came
    // back all-null. Verify the round-trip preserves every license field.
    vi.resetModules();
    vi.doMock('os', async () => {
      const actual = await vi.importActual('os');
      return { ...actual as object, homedir: () => tmpDir };
    });

    const { readGlobalConfig, writeGlobalConfig } = await import('../utils/immorterm-config');
    const config = readGlobalConfig();
    config.license.key = 'TEST-KEY';
    config.license.instanceId = 'inst-123';
    config.license.status = 'active';
    config.license.tier = 'pro';
    config.license.lastValidatedAt = '2026-05-21T10:00:00.000Z';
    config.license.devTierOverride = 'pro';
    config.license.customerEmail = 'user@example.com';
    writeGlobalConfig(config);

    // Simulate update-checker's read-modify-write
    const reloaded = readGlobalConfig();
    if (!reloaded.autoUpdate) {
      reloaded.autoUpdate = { enabled: true, checkIntervalHours: 6, lastCheckedAt: null };
    }
    reloaded.autoUpdate.lastCheckedAt = '2026-05-21T11:00:00.000Z';
    writeGlobalConfig(reloaded);

    const final = readGlobalConfig();
    expect(final.license.key).toBe('TEST-KEY');
    expect(final.license.instanceId).toBe('inst-123');
    expect(final.license.status).toBe('active');
    expect(final.license.tier).toBe('pro');
    expect(final.license.lastValidatedAt).toBe('2026-05-21T10:00:00.000Z');
    expect(final.license.devTierOverride).toBe('pro');
    expect(final.license.customerEmail).toBe('user@example.com');
  });

  it('config file has secure permissions (600)', async () => {
    vi.resetModules();
    vi.doMock('os', async () => {
      const actual = await vi.importActual('os');
      return { ...actual as object, homedir: () => tmpDir };
    });

    const { readGlobalConfig, writeGlobalConfig } = await import('../utils/immorterm-config');

    const config = readGlobalConfig();
    writeGlobalConfig(config);

    const configPath = path.join(tmpDir, '.immorterm', 'config.json');
    const stat = fs.statSync(configPath);
    // 0o600 = owner read/write only
    expect(stat.mode & 0o777).toBe(0o600);
  });
});
