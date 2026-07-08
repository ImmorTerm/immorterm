import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { IncomingMessage, ClientRequest } from 'http';
import { EventEmitter } from 'events';

// ── Mocks ────────────────────────────────────────────────────────

// Mock vscode (transitive dep from gateway-manager → isServiceEnabled)
vi.mock('vscode', () => ({
  workspace: { workspaceFolders: [] },
  window: { showErrorMessage: vi.fn() },
  env: { openExternal: vi.fn() },
  Uri: { parse: vi.fn() },
  commands: { executeCommand: vi.fn() },
}));

// Mock config
const mockReadGlobalConfig = vi.fn();
const mockWriteGlobalConfig = vi.fn();
vi.mock('../utils/immorterm-config', () => ({
  readGlobalConfig: (...args: unknown[]) => mockReadGlobalConfig(...args),
  writeGlobalConfig: (...args: unknown[]) => mockWriteGlobalConfig(...args),
}));

// Mock gateway-manager
const mockStopGateway = vi.fn();
const mockStartGateway = vi.fn();
vi.mock('../services/mcp-gateway/gateway-manager', () => ({
  stopGateway: (...args: unknown[]) => mockStopGateway(...args),
  startGateway: (...args: unknown[]) => mockStartGateway(...args),
}));

// Mock gateway-config
vi.mock('../services/mcp-gateway/gateway-config', () => ({
  getHealthUrl: (port: number) => `http://localhost:${port}/health`,
  GATEWAY_PORT: 9100,
}));

// Mock child_process
const mockExecFile = vi.fn();
vi.mock('child_process', () => ({
  execFile: (...args: unknown[]) => mockExecFile(...args),
}));

// Mock http and https
const mockHttpGet = vi.fn();
const mockHttpsGet = vi.fn();
vi.mock('http', () => ({
  get: (...args: unknown[]) => mockHttpGet(...args),
}));
vi.mock('https', () => ({
  get: (...args: unknown[]) => mockHttpsGet(...args),
}));

// ── Helper to create mock HTTP response ──────────────────────────

function createMockResponse(statusCode: number, body: string): IncomingMessage {
  const res = new EventEmitter() as IncomingMessage;
  res.statusCode = statusCode;
  // Emit data + end on next tick
  process.nextTick(() => {
    res.emit('data', body);
    res.emit('end');
  });
  return res;
}

function createMockRequest(): ClientRequest {
  const req = new EventEmitter() as ClientRequest;
  req.destroy = vi.fn();
  return req;
}

// ── Tests ────────────────────────────────────────────────────────

describe('isNewerVersion', () => {
  // Import the exported pure function directly
  let isNewerVersion: (latest: string, current: string) => boolean;

  beforeEach(async () => {
    const mod = await import('../services/update-checker');
    isNewerVersion = mod.isNewerVersion;
  });

  it('detects major version bump', () => {
    expect(isNewerVersion('2.0.0', '1.0.0')).toBe(true);
  });

  it('detects minor version bump', () => {
    expect(isNewerVersion('1.1.0', '1.0.0')).toBe(true);
  });

  it('detects patch version bump', () => {
    expect(isNewerVersion('1.0.1', '1.0.0')).toBe(true);
  });

  it('returns false for same version', () => {
    expect(isNewerVersion('1.0.0', '1.0.0')).toBe(false);
  });

  it('returns false when current is newer', () => {
    expect(isNewerVersion('1.0.0', '2.0.0')).toBe(false);
  });

  it('handles v prefix', () => {
    expect(isNewerVersion('v2.0.0', 'v1.0.0')).toBe(true);
    expect(isNewerVersion('v1.0.0', '1.0.0')).toBe(false);
  });

  it('handles different segment counts', () => {
    expect(isNewerVersion('1.0.1', '1.0')).toBe(true);
    expect(isNewerVersion('1.0', '1.0.0')).toBe(false);
  });

  it('handles double-digit versions', () => {
    expect(isNewerVersion('1.10.0', '1.9.0')).toBe(true);
    expect(isNewerVersion('1.2.0', '1.10.0')).toBe(false);
  });

  it('compares 0.x versions correctly', () => {
    expect(isNewerVersion('0.2.0', '0.1.0')).toBe(true);
    expect(isNewerVersion('0.1.0', '0.2.0')).toBe(false);
  });
});

describe('startUpdateChecker', () => {
  let startUpdateChecker: (logger: (msg: string) => void) => void;
  let stopUpdateChecker: () => void;
  const logMessages: string[] = [];
  const logFn = (msg: string) => logMessages.push(msg);

  beforeEach(async () => {
    vi.useFakeTimers();
    logMessages.length = 0;

    // Reset module state by re-importing
    vi.resetModules();

    // Re-apply mocks after module reset
    vi.doMock('vscode', () => ({
      workspace: { workspaceFolders: [] },
      window: { showErrorMessage: vi.fn() },
      env: { openExternal: vi.fn() },
      Uri: { parse: vi.fn() },
      commands: { executeCommand: vi.fn() },
    }));

    const mod = await import('../services/update-checker');
    startUpdateChecker = mod.startUpdateChecker;
    stopUpdateChecker = mod.stopUpdateChecker;
  });

  afterEach(() => {
    stopUpdateChecker();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('logs disabled message when autoUpdate.enabled is false', () => {
    mockReadGlobalConfig.mockReturnValue({
      autoUpdate: { enabled: false, checkIntervalHours: 6, lastCheckedAt: null },
    });

    startUpdateChecker(logFn);

    expect(logMessages).toContain('[auto-update] Disabled by config');
  });

  it('starts with configured interval when enabled', () => {
    mockReadGlobalConfig.mockReturnValue({
      autoUpdate: { enabled: true, checkIntervalHours: 12, lastCheckedAt: null },
    });

    startUpdateChecker(logFn);

    expect(logMessages.some(m => m.includes('Started (interval: 12h)'))).toBe(true);
  });

  it('uses default config when autoUpdate is missing', () => {
    mockReadGlobalConfig.mockReturnValue({});

    startUpdateChecker(logFn);

    expect(logMessages.some(m => m.includes('Started (interval: 6h)'))).toBe(true);
  });
});

describe('checkForUpdates (integration)', () => {
  let startUpdateChecker: (logger: (msg: string) => void) => void;
  let stopUpdateChecker: () => void;
  const logMessages: string[] = [];
  const logFn = (msg: string) => logMessages.push(msg);

  beforeEach(async () => {
    vi.useFakeTimers();
    logMessages.length = 0;

    vi.resetModules();
    vi.doMock('vscode', () => ({
      workspace: { workspaceFolders: [] },
      window: { showErrorMessage: vi.fn() },
      env: { openExternal: vi.fn() },
      Uri: { parse: vi.fn() },
      commands: { executeCommand: vi.fn() },
    }));

    // Default config: enabled, never checked
    mockReadGlobalConfig.mockReturnValue({
      autoUpdate: { enabled: true, checkIntervalHours: 6, lastCheckedAt: null },
    });

    mockWriteGlobalConfig.mockImplementation(() => {});
    mockStopGateway.mockResolvedValue(undefined);
    mockStartGateway.mockResolvedValue({});

    const mod = await import('../services/update-checker');
    startUpdateChecker = mod.startUpdateChecker;
    stopUpdateChecker = mod.stopUpdateChecker;
  });

  afterEach(() => {
    stopUpdateChecker();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('skips check when lastCheckedAt is too recent', async () => {
    mockReadGlobalConfig.mockReturnValue({
      autoUpdate: {
        enabled: true,
        checkIntervalHours: 6,
        lastCheckedAt: new Date().toISOString(), // just now
      },
    });

    // Mock http to return no gateway version (gateway not running)
    const req = createMockRequest();
    mockHttpGet.mockImplementation((_url: string, _opts: unknown, cb: (res: IncomingMessage) => void) => {
      cb(createMockResponse(500, ''));
      return req;
    });

    startUpdateChecker(logFn);

    // Advance past 60s startup delay
    await vi.advanceTimersByTimeAsync(61_000);

    expect(logMessages.some(m => m.includes('Skipping'))).toBe(true);
    expect(mockWriteGlobalConfig).not.toHaveBeenCalled();
  });

  it('persists lastCheckedAt after successful check', async () => {
    // Mock http to return no gateway (not running)
    const req = createMockRequest();
    mockHttpGet.mockImplementation((_url: string, _opts: unknown, cb: (res: IncomingMessage) => void) => {
      cb(createMockResponse(500, ''));
      return req;
    });

    startUpdateChecker(logFn);

    // Advance past 60s startup delay
    await vi.advanceTimersByTimeAsync(61_000);

    expect(mockWriteGlobalConfig).toHaveBeenCalledTimes(1);
    const savedConfig = mockWriteGlobalConfig.mock.calls[0][0];
    expect(savedConfig.autoUpdate.lastCheckedAt).toBeTruthy();
    expect(new Date(savedConfig.autoUpdate.lastCheckedAt).getTime()).toBeGreaterThan(0);
  });

  it('detects and installs gateway update when newer version available', async () => {
    // Mock gateway health → returns current version
    const gatewayReq = createMockRequest();
    mockHttpGet.mockImplementation((_url: string, _opts: unknown, cb: (res: IncomingMessage) => void) => {
      cb(createMockResponse(200, JSON.stringify({ version: '0.1.0', status: 'ok' })));
      return gatewayReq;
    });

    // Mock npm registry → returns newer version
    const npmReq = createMockRequest();
    mockHttpsGet.mockImplementation((_url: string, _opts: unknown, cb: (res: IncomingMessage) => void) => {
      cb(createMockResponse(200, JSON.stringify({ version: '0.2.0' })));
      return npmReq;
    });

    // Mock npm install
    mockExecFile.mockImplementation((_cmd: string, _args: string[], _opts: unknown, callback: Function) => {
      callback(null, { stdout: '', stderr: '' });
    });

    startUpdateChecker(logFn);
    await vi.advanceTimersByTimeAsync(61_000);

    expect(logMessages.some(m => m.includes('Gateway update available: v0.1.0'))).toBe(true);
    expect(mockStopGateway).toHaveBeenCalled();
    expect(mockStartGateway).toHaveBeenCalled();
  });

  it('skips gateway update when already up to date', async () => {
    const gatewayReq = createMockRequest();
    mockHttpGet.mockImplementation((_url: string, _opts: unknown, cb: (res: IncomingMessage) => void) => {
      cb(createMockResponse(200, JSON.stringify({ version: '0.2.0', status: 'ok' })));
      return gatewayReq;
    });

    const npmReq = createMockRequest();
    mockHttpsGet.mockImplementation((_url: string, _opts: unknown, cb: (res: IncomingMessage) => void) => {
      cb(createMockResponse(200, JSON.stringify({ version: '0.2.0' }))); // same version
      return npmReq;
    });

    startUpdateChecker(logFn);
    await vi.advanceTimersByTimeAsync(61_000);

    expect(logMessages.some(m => m.includes('Gateway is up to date'))).toBe(true);
    expect(mockStopGateway).not.toHaveBeenCalled();
  });

  it('handles network failures gracefully', async () => {
    // Gateway health fails
    const req = createMockRequest();
    mockHttpGet.mockImplementation((_url: string, _opts: unknown, _cb: Function) => {
      process.nextTick(() => req.emit('error', new Error('ECONNREFUSED')));
      return req;
    });

    startUpdateChecker(logFn);
    await vi.advanceTimersByTimeAsync(61_000);

    // Should complete without throwing
    expect(logMessages.some(m => m.includes('Check complete'))).toBe(true);
  });
});
