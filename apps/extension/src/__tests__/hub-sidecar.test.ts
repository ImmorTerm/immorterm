import { describe, expect, it, vi } from 'vitest';

vi.mock('vscode', () => ({
  workspace: { workspaceFolders: [] },
  window: { createOutputChannel: vi.fn() },
}));

vi.mock('../utils/logger', () => ({
  logger: {
    debug: vi.fn(),
    info: vi.fn(),
    warn: vi.fn(),
    error: vi.fn(),
  },
}));

import {
  HUB_API_VERSION,
  REQUIRED_HUB_CAPABILITIES,
  isCompatibleHubInfo,
} from '../hub-sidecar';

describe('Hub sidecar compatibility guard', () => {
  it('accepts the exact Hub identity and required capabilities', () => {
    expect(isCompatibleHubInfo({
      service: 'immorterm-hub',
      apiVersion: HUB_API_VERSION,
      capabilities: [...REQUIRED_HUB_CAPABILITIES, 'future-capability'],
    })).toBe(true);
  });

  it('rejects the legacy response that caused the wrong-checkout reuse', () => {
    expect(isCompatibleHubInfo({
      projectName: 'immorterm',
      projectDir: '/Users/shaisnir/Development/immorterm',
    })).toBe(false);
  });

  it('rejects another service, API revision, or missing capability', () => {
    expect(isCompatibleHubInfo({
      service: 'another-service',
      apiVersion: HUB_API_VERSION,
      capabilities: [...REQUIRED_HUB_CAPABILITIES],
    })).toBe(false);
    expect(isCompatibleHubInfo({
      service: 'immorterm-hub',
      apiVersion: HUB_API_VERSION + 1,
      capabilities: [...REQUIRED_HUB_CAPABILITIES],
    })).toBe(false);
    expect(isCompatibleHubInfo({
      service: 'immorterm-hub',
      apiVersion: HUB_API_VERSION,
      capabilities: ['bridge-v1'],
    })).toBe(false);
  });
});
