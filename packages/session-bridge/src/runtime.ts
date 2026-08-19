import {
  type AgentReply,
  type AttachmentReference,
  type BridgeEvent,
  type BridgeIdentity,
  type BridgeMessage,
  correlationId,
  DELIVERY_STATES,
  eventCursor,
  installationId,
  messageId,
  projectId,
  type SessionDirectory,
  windowId,
} from "./contracts.js";

export class ProtocolValidationError extends Error {
  constructor(message: string, readonly value: unknown) {
    super(message);
    this.name = "ProtocolValidationError";
  }
}

function object(value: unknown, label: string): Record<string, any> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ProtocolValidationError(`${label} must be an object`, value);
  }
  return value as Record<string, any>;
}

function string(value: unknown, label: string): string {
  if (typeof value !== "string" || !value) {
    throw new ProtocolValidationError(`${label} must be a non-empty string`, value);
  }
  return value;
}

function number(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new ProtocolValidationError(`${label} must be a finite number`, value);
  }
  return value;
}

function iso(value: unknown, label: string): string {
  if (typeof value === "number") return new Date(value).toISOString();
  const candidate = string(value, label);
  if (Number.isNaN(Date.parse(candidate))) {
    throw new ProtocolValidationError(`${label} must be an ISO timestamp`, value);
  }
  return candidate;
}

function optionalIso(value: unknown, label: string): string | undefined {
  return value === null || value === undefined ? undefined : iso(value, label);
}

function attachment(value: unknown): AttachmentReference {
  const raw = object(value, "attachment");
  return {
    attachmentId: string(raw.attachment_id ?? raw.attachmentId, "attachmentId"),
    fileName: string(raw.file_name ?? raw.fileName, "fileName"),
    mediaType: string(raw.media_type ?? raw.mediaType, "mediaType"),
    sha256: string(raw.sha256, "sha256"),
    size: number(raw.size, "size"),
  };
}

export function parseAgentReply(value: unknown): AgentReply {
  const raw = object(value, "reply");
  return {
    replyId: string(raw.reply_id ?? raw.replyId, "replyId") as AgentReply["replyId"],
    messageId: messageId(string(raw.message_id ?? raw.messageId, "messageId")),
    correlationId: correlationId(
      string(raw.correlation_id ?? raw.correlationId, "correlationId"),
    ),
    sessionWindowId: windowId(
      string(raw.session_window_id ?? raw.sessionWindowId, "sessionWindowId"),
    ),
    message: string(raw.message, "message"),
    createdAt: iso(raw.created_at ?? raw.createdAt, "createdAt"),
  };
}

export function parseBridgeMessage(value: unknown): BridgeMessage {
  const raw = object(value, "message");
  const state = string(raw.state, "state");
  if (!(DELIVERY_STATES as readonly string[]).includes(state)) {
    throw new ProtocolValidationError(`Unknown delivery state: ${state}`, value);
  }
  const history = Array.isArray(raw.history) ? raw.history : [];
  return {
    messageId: messageId(string(raw.message_id ?? raw.messageId, "messageId")),
    correlationId: correlationId(
      string(raw.correlation_id ?? raw.correlationId, "correlationId"),
    ),
    projectId: projectId(string(raw.project_id ?? raw.projectId, "projectId")),
    targetWindowId: windowId(
      string(raw.target_window_id ?? raw.targetWindowId, "targetWindowId"),
    ),
    targetSessionName: string(
      raw.target_session_name ?? raw.targetSessionName,
      "targetSessionName",
    ),
    location: object(raw.location, "location") as BridgeMessage["location"],
    message: string(raw.message, "message"),
    attachments: (Array.isArray(raw.attachments) ? raw.attachments : []).map(attachment),
    state: state as BridgeMessage["state"],
    attempt: number(raw.attempt ?? 0, "attempt"),
    expiresAt: iso(raw.expires_at ?? raw.expiresAt, "expiresAt"),
    createdAt: iso(raw.created_at ?? raw.createdAt, "createdAt"),
    updatedAt: iso(raw.updated_at ?? raw.updatedAt, "updatedAt"),
    history: history.map((entry) => {
      const item = object(entry, "history entry");
      const entryState = string(item.state, "history state");
      if (!(DELIVERY_STATES as readonly string[]).includes(entryState)) {
        throw new ProtocolValidationError(`Unknown history state: ${entryState}`, entry);
      }
      return {
        state: entryState as BridgeMessage["state"],
        changedAt: iso(item.changed_at ?? item.changedAt ?? item.at, "changedAt"),
        attempt: number(item.attempt ?? 0, "attempt"),
        ...(typeof item.error === "string" ? { error: item.error } : {}),
        ...(typeof item.error_code === "string" ? { errorCode: item.error_code } : {}),
        ...(typeof item.retryable === "boolean" ? { retryable: item.retryable } : {}),
      };
    }),
    replies: (Array.isArray(raw.replies) ? raw.replies : []).map(parseAgentReply),
    ...(typeof raw.error === "string" ? { error: raw.error } : {}),
  };
}

export function parseDirectory(value: unknown): SessionDirectory {
  const raw = object(value, "directory");
  const id = projectId(string(raw.project_id ?? raw.projectId, "projectId"));
  const sessions = raw.sessions ?? raw.agents;
  if (!Array.isArray(sessions)) throw new ProtocolValidationError("agents must be an array", value);
  return {
    projectId: id,
    revision: string(raw.revision, "revision"),
    generatedAt: iso(raw.generated_at ?? raw.generatedAt, "generatedAt"),
    agents: sessions.map((session) => {
      const agent = object(session, "directory agent");
      const status = string(agent.status, "status");
      if (!(["active", "idle", "offline"] as const).includes(status as any)) {
        throw new ProtocolValidationError(`Unknown directory status: ${status}`, session);
      }
      return {
        windowId: windowId(string(agent.window_id ?? agent.windowId, "windowId")),
        projectId: projectId(string(agent.project_id ?? agent.projectId ?? id, "projectId")),
        sessionName: string(agent.session_name ?? agent.sessionName, "sessionName"),
        ...(typeof (agent.display_name ?? agent.displayName) === "string"
          ? { displayName: agent.display_name ?? agent.displayName }
          : {}),
        ...(typeof (agent.tool ?? agent.agentTool) === "string"
          ? { agentTool: agent.tool ?? agent.agentTool }
          : {}),
        location: object(agent.location, "location") as any,
        status: status as "active" | "idle" | "offline",
        connectedAt: optionalIso(agent.connected_at ?? agent.connectedAt, "connectedAt"),
        lastSeenAt: iso(agent.last_seen_at ?? agent.lastSeenAt, "lastSeenAt"),
        lastActiveAt: optionalIso(agent.last_active_at ?? agent.lastActiveAt, "lastActiveAt"),
        isWorking: Boolean(agent.is_working ?? agent.isWorking),
        needsAttention: Boolean(agent.needs_attention ?? agent.needsAttention),
        capabilities: Array.isArray(agent.capabilities)
          ? agent.capabilities.filter((item: unknown): item is string => typeof item === "string")
          : [],
        protocolVersion: number(agent.protocol_version ?? agent.protocolVersion ?? 0, "protocolVersion"),
      };
    }),
  };
}

export function parseBridgeEvent(value: unknown): BridgeEvent {
  const raw = object(value, "event");
  if (raw.version !== 1) throw new ProtocolValidationError("Unsupported event version", value);
  const type = string(raw.type, "event type");
  const project = projectId(string(raw.projectId, "projectId"));
  const base = {
    version: 1 as const,
    eventId: string(raw.eventId, "eventId"),
    projectId: project,
    sequence: number(raw.sequence, "sequence"),
    cursor: eventCursor(string(raw.cursor, "cursor")),
    occurredAt: iso(raw.occurredAt, "occurredAt"),
    ...(typeof raw.messageId === "string" ? { messageId: messageId(raw.messageId) } : {}),
    ...(typeof raw.correlationId === "string"
      ? { correlationId: correlationId(raw.correlationId) }
      : {}),
    ...(typeof raw.causationId === "string" ? { causationId: raw.causationId } : {}),
  };
  const payload = object(raw.payload, "event payload");
  if (type === "message_state_changed") {
    return { ...base, type, payload: { message: parseBridgeMessage(payload.message) } };
  }
  if (type === "agent_reply") {
    return { ...base, type, payload: { reply: parseAgentReply(payload.reply) } };
  }
  if (type === "directory_snapshot") {
    return {
      ...base,
      type,
      payload: {
        directory: parseDirectory({
          projectId: project,
          revision: payload.revision,
          generatedAt: payload.generatedAt,
          sessions: payload.sessions,
        }),
      },
    };
  }
  if (type === "snapshot") {
    return {
      ...base,
      type,
      payload: {
        directory: parseDirectory({
          projectId: project,
          revision: payload.directoryRevision,
          generatedAt: payload.generatedAt,
          sessions: payload.sessions,
        }),
        messages: (Array.isArray(payload.messages) ? payload.messages : []).map(parseBridgeMessage),
      },
    };
  }
  if (type === "cursor_expired") {
    return {
      ...base,
      type,
      payload: {
        ...(typeof payload.requestedCursor === "string"
          ? { requestedCursor: eventCursor(payload.requestedCursor) }
          : {}),
        oldestAvailableCursor: eventCursor(
          string(payload.oldestAvailableCursor, "oldestAvailableCursor"),
        ),
        resnapshotRequired: true,
      },
    };
  }
  throw new ProtocolValidationError(`Unknown Session Bridge event: ${type}`, value);
}

export function parseBridgeIdentity(value: unknown): BridgeIdentity {
  const raw = object(value, "bridge identity");
  const protocolRaw = object(raw.protocol, "bridge identity protocol");
  const protocol = {
    version: number(protocolRaw.version, "protocol version"),
    capabilities: Array.isArray(protocolRaw.capabilities)
      ? protocolRaw.capabilities.filter((item: unknown): item is string => typeof item === "string")
      : [],
  };
  if (raw.kind === "administrator") return { kind: "administrator", protocol };
  if (raw.kind !== "installation") {
    throw new ProtocolValidationError("Unknown bridge credential kind", value);
  }
  const operations = Array.isArray(raw.operations)
    ? raw.operations.filter(
        (item: unknown): item is "directory:read" | "message:send" | "events:subscribe" =>
          item === "directory:read" || item === "message:send" || item === "events:subscribe",
      )
    : [];
  return {
    kind: "installation",
    installationId: installationId(
      string(raw.installation_id ?? raw.installationId, "installationId"),
    ),
    projectId: projectId(string(raw.project_id ?? raw.projectId, "projectId")),
    tokenId: string(raw.token_id ?? raw.tokenId, "tokenId"),
    audience: string(raw.audience, "audience"),
    operations,
    expiresAt: iso(raw.expires_at ?? raw.expiresAt, "expiresAt"),
    protocol,
  } as BridgeIdentity;
}
