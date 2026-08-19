export interface ToolHistoryRecord {
  tool?: string;
  session_id?: string;
  transcript_path?: string;
  ts?: string;
}

export interface RestoreIdentitySource {
  tool?: string;
  ai_session_id?: string;
  ai_transcript_path?: string;
  tool_history?: ToolHistoryRecord[];
}

export interface RestoreIdentity {
  tool?: string;
  sessionId?: string;
  transcriptPath?: string;
  source: 'tool-history' | 'registry';
}

/**
 * Resolve the agent identity to restore after a daemon restart.
 *
 * `tool`, `ai_session_id`, and `ai_transcript_path` are separate hot fields
 * and can be observed from different writers during a registry race. The
 * append-only tool history is a single tuple, so its newest valid row is the
 * authoritative restore target. Array order breaks ties and also supports
 * legacy rows with a missing or malformed timestamp.
 */
export function resolveRestoreIdentity(entry: RestoreIdentitySource): RestoreIdentity {
  let newest: { record: ToolHistoryRecord; timestamp: number; index: number } | undefined;
  for (const [index, record] of (entry.tool_history ?? []).entries()) {
    if (!record?.tool?.trim() || !record.session_id?.trim()) continue;
    const parsed = record.ts ? Date.parse(record.ts) : Number.NaN;
    const timestamp = Number.isFinite(parsed) ? parsed : Number.NEGATIVE_INFINITY;
    if (!newest || timestamp > newest.timestamp || (timestamp === newest.timestamp && index > newest.index)) {
      newest = { record, timestamp, index };
    }
  }

  if (newest) {
    return {
      tool: newest.record.tool!.trim(),
      sessionId: newest.record.session_id!.trim(),
      transcriptPath: newest.record.transcript_path?.trim() || undefined,
      source: 'tool-history',
    };
  }

  return {
    tool: entry.tool?.trim() || undefined,
    sessionId: entry.ai_session_id?.trim() || undefined,
    transcriptPath: entry.ai_transcript_path?.trim() || undefined,
    source: 'registry',
  };
}
