export type Brand<Value, Name extends string> = Value & { readonly __brand: Name };

export type ProjectId = Brand<string, "ProjectId">;
export type InstallationId = Brand<string, "InstallationId">;
export type WindowId = Brand<string, "WindowId">;
export type MessageId = Brand<string, "MessageId">;
export type CorrelationId = Brand<string, "CorrelationId">;
export type ReplyId = Brand<string, "ReplyId">;
export type EventCursor = Brand<string, "EventCursor">;
export type BridgeOperation = "directory:read" | "message:send" | "events:subscribe";

export const DELIVERY_STATES = [
  "queued",
  "routing",
  "accepted_by_daemon",
  "presented_to_agent_input",
  "acknowledged_by_agent",
  "replied",
  "failed",
  "expired",
  "cancelled",
] as const;

export type DeliveryState = (typeof DELIVERY_STATES)[number];
export type DirectoryStatus = "active" | "idle" | "offline";

export type SessionLocation =
  | { kind: "local" }
  | { kind: "remote"; name: string };

export interface DirectoryAgent {
  windowId: WindowId;
  projectId: ProjectId;
  sessionName: string;
  displayName?: string;
  agentTool?: string;
  location: SessionLocation;
  status: DirectoryStatus;
  connectedAt?: string;
  lastSeenAt: string;
  lastActiveAt?: string;
  isWorking: boolean;
  needsAttention: boolean;
  capabilities: string[];
  protocolVersion: number;
}

export interface SessionDirectory {
  projectId: ProjectId;
  revision: string;
  generatedAt: string;
  agents: DirectoryAgent[];
}

export interface DeliveryHistoryEntry {
  state: DeliveryState;
  changedAt: string;
  attempt: number;
  error?: string;
  errorCode?: string;
  retryable?: boolean;
}

export interface AttachmentReference {
  attachmentId: string;
  fileName: string;
  mediaType: string;
  sha256: string;
  size: number;
}

export interface AgentReply {
  replyId: ReplyId;
  messageId: MessageId;
  correlationId: CorrelationId;
  sessionWindowId: WindowId;
  message: string;
  createdAt: string;
}

export interface BridgeMessage {
  messageId: MessageId;
  correlationId: CorrelationId;
  projectId: ProjectId;
  targetWindowId: WindowId;
  targetSessionName: string;
  location: SessionLocation;
  message: string;
  attachments: AttachmentReference[];
  state: DeliveryState;
  attempt: number;
  expiresAt: string;
  createdAt: string;
  updatedAt: string;
  history: DeliveryHistoryEntry[];
  replies: AgentReply[];
  error?: string;
}

export interface SendMessageInput {
  targetWindowId: WindowId;
  message: string;
  messageId: MessageId;
  correlationId: CorrelationId;
  expiresAt: string;
  attachments?: AttachmentReference[];
  traceContext?: Record<string, string>;
  signal?: AbortSignal;
}

export interface BridgeEventBase<Type extends string, Payload> {
  version: 1;
  eventId: string;
  type: Type;
  projectId: ProjectId;
  sequence: number;
  cursor: EventCursor;
  occurredAt: string;
  messageId?: MessageId;
  correlationId?: CorrelationId;
  causationId?: string;
  payload: Payload;
}

export type BridgeEvent =
  | BridgeEventBase<
      "snapshot",
      {
        directory: SessionDirectory;
        messages: BridgeMessage[];
      }
    >
  | BridgeEventBase<"directory_snapshot", { directory: SessionDirectory }>
  | BridgeEventBase<"message_state_changed", { message: BridgeMessage }>
  | BridgeEventBase<"agent_reply", { reply: AgentReply }>
  | BridgeEventBase<
      "cursor_expired",
      {
        requestedCursor?: EventCursor;
        oldestAvailableCursor: EventCursor;
        resnapshotRequired: true;
      }
    >;

export interface BridgeContract {
  version: number;
  protocol: {
    version: number;
    capabilities: string[];
  };
  provision: string;
  revoke: string;
  identity: string;
  directory: string;
  send: string;
  cancel: string;
  acknowledge: string;
  reply: string;
  events: string;
  states: DeliveryState[];
  addressing: string;
  idempotency: string;
  sdk: string;
  host_operations: string[];
  agent_authority: string;
  event_delivery: string;
  limits: Record<string, number>;
}

export type BridgeIdentity =
  | {
      kind: "administrator";
      protocol: { version: number; capabilities: string[] };
    }
  | {
      kind: "installation";
      installationId: InstallationId;
      projectId: ProjectId;
      tokenId: string;
      audience: string;
      operations: BridgeOperation[];
      expiresAt: string;
      protocol: { version: number; capabilities: string[] };
    };

export interface ExpectedBridgeIdentity {
  installationId: InstallationId;
  projectId: ProjectId;
  audience: string;
  requiredOperations?: BridgeOperation[];
  signal?: AbortSignal;
}

export interface InstallationCredentials {
  token: string;
  tokenId?: string;
  expiresAt?: string;
}

export interface ProvisionInstallationInput {
  installationId: InstallationId;
  projectId: ProjectId;
  audience: string;
  operations?: BridgeOperation[];
  ttlSeconds?: number;
  signal?: AbortSignal;
}

export interface ProvisionedInstallationCredential {
  token: string;
  tokenId: string;
  installationId: InstallationId;
  projectId: ProjectId;
  audience: string;
  operations: string[];
  expiresAt: string;
}

const ID_PATTERN = /^[A-Za-z0-9._-]{1,128}$/;

export function brandedId<Name extends string>(value: string, name: Name): Brand<string, Name> {
  if (!ID_PATTERN.test(value)) throw new TypeError(`Invalid ${name}`);
  return value as Brand<string, Name>;
}

export const projectId = (value: string) => brandedId(value, "ProjectId");
export const installationId = (value: string) => brandedId(value, "InstallationId");
export const windowId = (value: string) => brandedId(value, "WindowId");
export const messageId = (value: string) => brandedId(value, "MessageId");
export const correlationId = (value: string) => brandedId(value, "CorrelationId");
export const eventCursor = (value: string) => value as EventCursor;
