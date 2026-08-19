import { describe, expect, it } from 'vitest';
import { resolveRestoreIdentity } from '../restore-identity';

describe('resolveRestoreIdentity', () => {
  it('prefers the newest complete tool-history tuple over stale mixed hot fields', () => {
    expect(resolveRestoreIdentity({
      tool: 'codex',
      ai_session_id: 'older-claude-id',
      ai_transcript_path: '/tmp/latest-codex.jsonl',
      tool_history: [
        { tool: 'claude-code', session_id: 'older-claude-id', transcript_path: '/tmp/claude.jsonl', ts: '2026-08-15T08:07:05Z' },
        { tool: 'codex', session_id: 'latest-codex-id', transcript_path: '/tmp/latest-codex.jsonl', ts: '2026-08-18T13:37:32Z' },
      ],
    })).toEqual({
      tool: 'codex',
      sessionId: 'latest-codex-id',
      transcriptPath: '/tmp/latest-codex.jsonl',
      source: 'tool-history',
    });
  });

  it('uses array order for legacy history rows without timestamps', () => {
    expect(resolveRestoreIdentity({
      tool_history: [
        { tool: 'claude-code', session_id: 'claude-id' },
        { tool: 'codex', session_id: 'codex-id' },
      ],
    })).toMatchObject({ tool: 'codex', sessionId: 'codex-id' });
  });

  it('falls back to the registry fields when history has no complete tuple', () => {
    expect(resolveRestoreIdentity({
      tool: 'codex',
      ai_session_id: 'registry-id',
      ai_transcript_path: '/tmp/registry.jsonl',
      tool_history: [{ tool: 'codex', ts: '2026-08-18T13:37:32Z' }],
    })).toEqual({
      tool: 'codex',
      sessionId: 'registry-id',
      transcriptPath: '/tmp/registry.jsonl',
      source: 'registry',
    });
  });
});
