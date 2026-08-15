import WebSocket from "ws";

import {
  type BridgeContract,
  type BridgeEvent,
  type BridgeIdentity,
  type BridgeMessage,
  type EventCursor,
  type ExpectedBridgeIdentity,
  type InstallationCredentials,
  type InstallationId,
  type MessageId,
  type ProjectId,
  type ProvisionInstallationInput,
  type ProvisionedInstallationCredential,
  type SendMessageInput,
  type SessionDirectory,
} from "./contracts.js";
import {
  parseBridgeEvent,
  parseBridgeIdentity,
  parseBridgeMessage,
  parseDirectory,
  ProtocolValidationError,
} from "./runtime.js";

export type CredentialProvider =
  | InstallationCredentials
  | (() => InstallationCredentials | Promise<InstallationCredentials>);

export interface WebSocketLike {
  addEventListener(type: "open", listener: () => void): void;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
  addEventListener(type: "close", listener: () => void): void;
  addEventListener(type: "error", listener: (event: unknown) => void): void;
  close(): void;
}

export type WebSocketFactory = (input: {
  url: string;
  credentials: InstallationCredentials;
}) => WebSocketLike;

interface ClientOptions {
  baseUrl: string;
  credentials: CredentialProvider;
  fetch?: typeof globalThis.fetch;
  webSocketFactory?: WebSocketFactory;
}

export interface SessionBridgeClientOptions extends ClientOptions {}
export interface SessionBridgeAdminClientOptions extends ClientOptions {}

export type ConnectionState = "connecting" | "connected" | "disconnected" | "reconnecting";

export interface EventsOptions {
  cursor?: EventCursor;
  signal?: AbortSignal;
}

export interface SubscribeOptions extends EventsOptions {
  reconnect?: boolean;
  minReconnectDelayMs?: number;
  maxReconnectDelayMs?: number;
  onEvent: (event: BridgeEvent) => void | Promise<void>;
  onConnectionChange?: (state: ConnectionState) => void;
  onError?: (error: unknown) => void;
}

export interface BridgeSubscription {
  readonly ready: Promise<void>;
  close(): void;
}

export class SessionBridgeError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string,
    readonly body: unknown,
  ) {
    super(message);
    this.name = "SessionBridgeError";
  }
}

export class AuthenticationError extends SessionBridgeError {}
export class AuthorizationError extends SessionBridgeError {}
export class ConflictError extends SessionBridgeError {}
export class RateLimitError extends SessionBridgeError {
  readonly retryAfterSeconds?: number;
  constructor(message: string, status: number, code: string, body: unknown) {
    super(message, status, code, body);
    const retryAfter = objectBody(body)?.retry_after_seconds;
    this.retryAfterSeconds = typeof retryAfter === "number" ? retryAfter : undefined;
  }
}
export class DeliveryError extends SessionBridgeError {}
export class CursorExpiredError extends Error {
  constructor(
    readonly requestedCursor: EventCursor | undefined,
    readonly oldestAvailableCursor: EventCursor,
  ) {
    super("Session Bridge event cursor expired; a fresh snapshot is required");
    this.name = "CursorExpiredError";
  }
}

function objectBody(body: unknown): Record<string, any> | undefined {
  return body && typeof body === "object" && !Array.isArray(body)
    ? (body as Record<string, any>)
    : undefined;
}

function bridgeError(status: number, body: unknown): SessionBridgeError {
  const object = objectBody(body);
  const code = typeof object?.error === "string" ? object.error : "bridge_request_failed";
  const message = code.replaceAll("_", " ");
  if (status === 401) return new AuthenticationError(message, status, code, body);
  if (status === 403) return new AuthorizationError(message, status, code, body);
  if (status === 409) return new ConflictError(message, status, code, body);
  if (status === 429) return new RateLimitError(message, status, code, body);
  if (status === 503) return new DeliveryError(message, status, code, body);
  return new SessionBridgeError(message, status, code, body);
}

function defaultWebSocketFactory({
  url,
  credentials,
}: {
  url: string;
  credentials: InstallationCredentials;
}): WebSocketLike {
  return new WebSocket(url, {
    headers: { Authorization: `Bearer ${credentials.token}` },
  }) as WebSocketLike;
}

function normalizeBaseUrl(value: string): string {
  const url = new URL(value);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new TypeError("Session Bridge baseUrl must use http: or https:");
  }
  url.pathname = url.pathname.replace(/\/+$/, "");
  url.search = "";
  url.hash = "";
  return url.toString().replace(/\/$/, "");
}

function websocketBaseUrl(value: string): string {
  const url = new URL(value);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString().replace(/\/$/, "");
}

class Transport {
  readonly baseUrl: string;
  private readonly credentialProvider: CredentialProvider;
  private readonly fetchImpl: typeof globalThis.fetch;
  readonly webSocketFactory: WebSocketFactory;

  constructor(options: ClientOptions) {
    this.baseUrl = normalizeBaseUrl(options.baseUrl);
    this.credentialProvider = options.credentials;
    this.fetchImpl = options.fetch ?? globalThis.fetch;
    if (!this.fetchImpl) throw new TypeError("A fetch implementation is required");
    this.webSocketFactory = options.webSocketFactory ?? defaultWebSocketFactory;
  }

  async credentials(): Promise<InstallationCredentials> {
    const credentials =
      typeof this.credentialProvider === "function"
        ? await this.credentialProvider()
        : this.credentialProvider;
    if (!credentials?.token) throw new TypeError("Session Bridge credentials are empty");
    return credentials;
  }

  async request<T>(
    method: string,
    path: string,
    options: { body?: unknown; signal?: AbortSignal; parse: (value: unknown) => T },
  ): Promise<T> {
    const credentials = await this.credentials();
    const response = await this.fetchImpl(`${this.baseUrl}${path}`, {
      method,
      signal: options.signal,
      headers: {
        Authorization: `Bearer ${credentials.token}`,
        ...(options.body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });
    const text = await response.text();
    let payload: unknown;
    try {
      payload = text ? JSON.parse(text) : undefined;
    } catch {
      payload = text;
    }
    if (!response.ok) throw bridgeError(response.status, payload);
    return options.parse(payload);
  }

  socketUrl(expectedProjectId: ProjectId | undefined, cursor: EventCursor | undefined): string {
    const url = new URL(`${websocketBaseUrl(this.baseUrl)}/api/v1/bridge/events`);
    if (expectedProjectId) url.searchParams.set("project_id", expectedProjectId);
    if (cursor) url.searchParams.set("cursor", cursor);
    return url.toString();
  }
}

export class SessionBridgeClient {
  private readonly transport: Transport;
  constructor(options: SessionBridgeClientOptions) {
    this.transport = new Transport(options);
  }

  project(expectedProjectId?: ProjectId): SessionBridgeProjectClient {
    return new SessionBridgeProjectClient(this.transport, expectedProjectId);
  }

  identity(signal?: AbortSignal): Promise<BridgeIdentity> {
    return this.transport.request("GET", "/api/v1/bridge/identity", {
      signal,
      parse: parseBridgeIdentity,
    });
  }

  async assertIdentity(expected: ExpectedBridgeIdentity): Promise<BridgeIdentity> {
    const identity = await this.identity(expected.signal);
    const denied = (code: string, detail: unknown): never => {
      throw new AuthorizationError(code.replaceAll("_", " "), 403, code, detail);
    };
    if (identity.kind !== "installation") {
      return denied("installation_credential_required", identity);
    }
    if (identity.installationId !== expected.installationId) {
      return denied("installation_scope_mismatch", identity);
    }
    if (identity.projectId !== expected.projectId) {
      return denied("project_scope_mismatch", identity);
    }
    if (identity.audience !== expected.audience) {
      return denied("audience_mismatch", identity);
    }
    const missing = (expected.requiredOperations ?? []).filter(
      (operation) => !identity.operations.includes(operation),
    );
    if (missing.length) return denied("operation_scope_mismatch", { identity, missing });
    return identity;
  }

  contract(signal?: AbortSignal): Promise<BridgeContract> {
    return this.transport.request("GET", "/api/v1/bridge/contract", {
      signal,
      parse(value) {
        if (!value || typeof value !== "object") {
          throw new ProtocolValidationError("Bridge contract must be an object", value);
        }
        return value as BridgeContract;
      },
    });
  }
}

export class SessionBridgeAdminClient {
  private readonly transport: Transport;
  constructor(options: SessionBridgeAdminClientOptions) {
    this.transport = new Transport(options);
  }

  provision(input: ProvisionInstallationInput): Promise<ProvisionedInstallationCredential> {
    return this.transport.request("POST", "/api/v1/bridge/installations/credentials", {
      signal: input.signal,
      body: {
        installation_id: input.installationId,
        project_id: input.projectId,
        audience: input.audience,
        operations: input.operations,
        ttl_seconds: input.ttlSeconds,
      },
      parse(value) {
        const raw = objectBody(value);
        if (!raw || typeof raw.token !== "string" || typeof raw.token_id !== "string") {
          throw new ProtocolValidationError("Invalid provision response", value);
        }
        return {
          token: raw.token,
          tokenId: raw.token_id,
          installationId: raw.installation_id,
          projectId: raw.project_id,
          audience: raw.audience,
          operations: raw.operations,
          expiresAt: new Date(raw.expires_at).toISOString(),
        } as ProvisionedInstallationCredential;
      },
    });
  }

  revoke(installationId: InstallationId, tokenId: string, signal?: AbortSignal): Promise<void> {
    return this.transport.request(
      "DELETE",
      `/api/v1/bridge/installations/${encodeURIComponent(installationId)}/credentials/${encodeURIComponent(tokenId)}`,
      { signal, parse: () => undefined },
    );
  }
}

export class SessionBridgeProjectClient {
  constructor(
    private readonly transport: Transport,
    readonly expectedProjectId?: ProjectId,
  ) {}

  directory(options: { status?: "active" | "idle" | "offline"; signal?: AbortSignal } = {}): Promise<SessionDirectory> {
    const query = this.expectedProjectId
      ? `?project_id=${encodeURIComponent(this.expectedProjectId)}`
      : "";
    return this.transport
      .request("GET", `/api/v1/bridge/directory${query}`, {
        signal: options.signal,
        parse: parseDirectory,
      })
      .then((directory) =>
        options.status
          ? { ...directory, agents: directory.agents.filter((agent) => agent.status === options.status) }
          : directory,
      );
  }

  send(input: SendMessageInput): Promise<BridgeMessage> {
    return this.transport.request("POST", "/api/v1/bridge/messages", {
      signal: input.signal,
      body: {
        ...(this.expectedProjectId ? { project_id: this.expectedProjectId } : {}),
        target_window_id: input.targetWindowId,
        message_id: input.messageId,
        correlation_id: input.correlationId,
        message: input.message,
        expires_at: input.expiresAt,
        attachments: input.attachments?.map((attachment) => ({
          attachment_id: attachment.attachmentId,
          file_name: attachment.fileName,
          media_type: attachment.mediaType,
          sha256: attachment.sha256,
          size: attachment.size,
        })),
        trace_context: input.traceContext,
      },
      parse: parseBridgeMessage,
    });
  }

  cancel(messageId: MessageId, signal?: AbortSignal): Promise<BridgeMessage> {
    return this.transport.request(
      "POST",
      `/api/v1/bridge/messages/${encodeURIComponent(messageId)}/cancel`,
      {
        signal,
        body: this.expectedProjectId ? { project_id: this.expectedProjectId } : {},
        parse: parseBridgeMessage,
      },
    );
  }

  events(options: EventsOptions = {}): AsyncIterable<BridgeEvent> {
    const project = this;
    return {
      async *[Symbol.asyncIterator]() {
        const queue: BridgeEvent[] = [];
        let wake: (() => void) | undefined;
        let failure: unknown;
        let finished = false;
        const subscription = project.subscribe({
          ...options,
          onEvent(event) {
            queue.push(event);
            wake?.();
          },
          onError(error) {
            failure = error;
            wake?.();
          },
        });
        options.signal?.addEventListener(
          "abort",
          () => {
            finished = true;
            wake?.();
          },
          { once: true },
        );
        try {
          await subscription.ready;
          while (!finished) {
            if (failure) throw failure;
            const event = queue.shift();
            if (event) {
              yield event;
              continue;
            }
            await new Promise<void>((resolve) => {
              wake = resolve;
            });
            wake = undefined;
          }
        } finally {
          subscription.close();
        }
      },
    };
  }

  subscribe(options: SubscribeOptions): BridgeSubscription {
    const reconnect = options.reconnect ?? true;
    const minimum = Math.max(50, options.minReconnectDelayMs ?? 500);
    const maximum = Math.max(minimum, options.maxReconnectDelayMs ?? 10_000);
    let currentCursor = options.cursor;
    let delay = minimum;
    let socket: WebSocketLike | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let closed = false;
    let opened = false;
    let readyResolve!: () => void;
    let readyReject!: (error: unknown) => void;
    const ready = new Promise<void>((resolve, reject) => {
      readyResolve = resolve;
      readyReject = reject;
    });

    const close = () => {
      closed = true;
      if (timer) clearTimeout(timer);
      socket?.close();
      options.onConnectionChange?.("disconnected");
    };
    const connect = async () => {
      if (closed || options.signal?.aborted) return;
      options.onConnectionChange?.(opened ? "reconnecting" : "connecting");
      try {
        const credentials = await this.transport.credentials();
        socket = this.transport.webSocketFactory({
          url: this.transport.socketUrl(this.expectedProjectId, currentCursor),
          credentials,
        });
        socket.addEventListener("open", () => {
          delay = minimum;
          options.onConnectionChange?.("connected");
          if (!opened) {
            opened = true;
            readyResolve();
          }
        });
        socket.addEventListener("message", (event) => {
          try {
            const raw = typeof event.data === "string" ? event.data : event.data?.toString();
            const parsed = parseBridgeEvent(JSON.parse(raw ?? ""));
            // A gap marker is immediately followed by a repair snapshot. Do
            // not advance the reconnect cursor until that snapshot arrives.
            if (parsed.type !== "cursor_expired") currentCursor = parsed.cursor;
            void Promise.resolve(options.onEvent(parsed)).catch(options.onError);
          } catch (error) {
            options.onError?.(error);
          }
        });
        socket.addEventListener("error", (error) => {
          if (!opened) readyReject(error);
          options.onError?.(error);
        });
        socket.addEventListener("close", () => {
          options.onConnectionChange?.("disconnected");
          if (!reconnect || closed || options.signal?.aborted) return;
          timer = setTimeout(() => void connect(), delay);
          delay = Math.min(maximum, delay * 2);
        });
      } catch (error) {
        if (!opened) readyReject(error);
        options.onError?.(error);
        if (reconnect && !closed && !options.signal?.aborted) {
          timer = setTimeout(() => void connect(), delay);
          delay = Math.min(maximum, delay * 2);
        }
      }
    };

    options.signal?.addEventListener("abort", close, { once: true });
    void connect();
    return { ready, close };
  }
}

export { ProtocolValidationError };
