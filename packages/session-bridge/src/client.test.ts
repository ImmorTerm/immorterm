import { describe, expect, test } from "bun:test";

import {
  AuthenticationError,
  AuthorizationError,
  ConflictError,
  type BridgeEvent,
  type InstallationCredentials,
  RateLimitError,
  SessionBridgeAdminClient,
  SessionBridgeClient,
  correlationId,
  eventCursor,
  installationId,
  messageId,
  projectId,
  windowId,
  type WebSocketLike,
} from "./index.js";

class FakeSocket implements WebSocketLike {
  private listeners = new Map<string, Array<(event: any) => void>>();
  closed = false;

  addEventListener(type: "open" | "message" | "close" | "error", listener: (event: any) => void) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  emit(type: "open" | "message" | "close" | "error", event: any = {}) {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }

  close() {
    this.closed = true;
  }
}

const directoryPayload = {
  project_id: "project-1",
  revision: "revision-1",
  generated_at: "2026-08-14T12:00:00.000Z",
  sessions: [],
};

const messagePayload = {
  message_id: "message-1",
  correlation_id: "correlation-1",
  project_id: "project-1",
  target_window_id: "41103-66e4a36b",
  target_session_name: "flam-ai-41103-66e4a36b",
  location: { kind: "local" },
  message: "Review this",
  attachments: [],
  state: "presented_to_agent_input",
  attempt: 1,
  expires_at: "2026-08-14T13:00:00.000Z",
  created_at: "2026-08-14T12:00:00.000Z",
  updated_at: "2026-08-14T12:00:01.000Z",
  history: [
    { state: "queued", changed_at: "2026-08-14T12:00:00.000Z", attempt: 0 },
    { state: "routing", changed_at: "2026-08-14T12:00:00.500Z", attempt: 1 },
    {
      state: "presented_to_agent_input",
      changed_at: "2026-08-14T12:00:01.000Z",
      attempt: 1,
    },
  ],
  replies: [],
};

const credentials: InstallationCredentials = { token: "installation-secret" };

describe("SessionBridgeClient", () => {
  test("derives project scope from installation credentials by default", async () => {
    const requests: Array<{ url: string; init?: RequestInit }> = [];
    const client = new SessionBridgeClient({
      baseUrl: "https://hub.example.test/",
      credentials,
      fetch: (async (url: string | URL | Request, init?: RequestInit) => {
        requests.push({ url: String(url), init });
        return Response.json(directoryPayload);
      }) as typeof fetch,
    });

    const directory = await client.project().directory();
    expect(directory.projectId).toBe("project-1");
    expect(requests[0]?.url).toBe("https://hub.example.test/api/v1/bridge/directory");
    expect((requests[0]?.init?.headers as Record<string, string>).Authorization).toBe(
      "Bearer installation-secret",
    );
  });

  test("uses an expected project only as a server-enforced assertion", async () => {
    let requestedUrl = "";
    const client = new SessionBridgeClient({
      baseUrl: "https://hub.example.test",
      credentials,
      fetch: (async (url: string | URL | Request) => {
        requestedUrl = String(url);
        return Response.json(directoryPayload);
      }) as typeof fetch,
    });

    await client.project(projectId("project-1")).directory();
    expect(requestedUrl).toEndWith("/api/v1/bridge/directory?project_id=project-1");
  });

  test("preflights installation, project, audience, and operations", async () => {
    const client = new SessionBridgeClient({
      baseUrl: "https://hub.example.test",
      credentials,
      fetch: (() =>
        Promise.resolve(
          Response.json({
            kind: "installation",
            installation_id: "flam-production",
            project_id: "project-1",
            token_id: "token-1",
            audience: "flam-team-runtime",
            operations: ["directory:read", "message:send", "events:subscribe"],
            expires_at: "2026-08-14T13:00:00.000Z",
            protocol: { version: 1, capabilities: ["credential_identity.v1"] },
          }),
        )) as unknown as typeof fetch,
    });

    const identity = await client.assertIdentity({
      installationId: installationId("flam-production"),
      projectId: projectId("project-1"),
      audience: "flam-team-runtime",
      requiredOperations: ["directory:read", "message:send", "events:subscribe"],
    });
    expect(identity.kind).toBe("installation");
    await expect(
      client.assertIdentity({
        installationId: installationId("flam-production"),
        projectId: projectId("project-1"),
        audience: "wrong-audience",
      }),
    ).rejects.toBeInstanceOf(AuthorizationError);
  });

  test("sends the complete stable, bounded envelope and forwards AbortSignal", async () => {
    let payload: any;
    let signal: AbortSignal | null | undefined;
    const controller = new AbortController();
    const client = new SessionBridgeClient({
      baseUrl: "http://127.0.0.1:1440",
      credentials,
      fetch: (async (_url: string | URL | Request, init?: RequestInit) => {
        payload = JSON.parse(String(init?.body));
        signal = init?.signal;
        return Response.json(messagePayload, { status: 202 });
      }) as typeof fetch,
    });

    await client.project().send({
      targetWindowId: windowId("41103-66e4a36b"),
      messageId: messageId("message-1"),
      correlationId: correlationId("correlation-1"),
      message: "Review this",
      expiresAt: "2026-08-14T13:00:00.000Z",
      attachments: [
        {
          attachmentId: "attachment-1",
          fileName: "review.png",
          mediaType: "image/png",
          sha256: "a".repeat(64),
          size: 123,
        },
      ],
      traceContext: { traceparent: "00-abc-def-01" },
      signal: controller.signal,
    });

    expect(payload).toEqual({
      target_window_id: "41103-66e4a36b",
      message_id: "message-1",
      correlation_id: "correlation-1",
      message: "Review this",
      expires_at: "2026-08-14T13:00:00.000Z",
      attachments: [
        {
          attachment_id: "attachment-1",
          file_name: "review.png",
          media_type: "image/png",
          sha256: "a".repeat(64),
          size: 123,
        },
      ],
      trace_context: { traceparent: "00-abc-def-01" },
    });
    expect(signal).toBe(controller.signal);
  });

  test("subscribes with refreshed credentials, cursor resume, and versioned events", async () => {
    const socket = new FakeSocket();
    let connection: { url: string; credentials: InstallationCredentials } | undefined;
    let credentialCalls = 0;
    const events: BridgeEvent[] = [];
    const client = new SessionBridgeClient({
      baseUrl: "https://hub.example.test",
      credentials: async () => {
        credentialCalls += 1;
        return { token: `installation-secret-${credentialCalls}` };
      },
      fetch: (() => Promise.reject(new Error("not used"))) as unknown as typeof fetch,
      webSocketFactory: (input) => {
        connection = input;
        queueMicrotask(() => socket.emit("open"));
        return socket;
      },
    });

    const subscription = client.project().subscribe({
      cursor: eventCursor("v1.41"),
      reconnect: false,
      onEvent: (event) => events.push(event),
    });
    await subscription.ready;
    socket.emit("message", {
      data: JSON.stringify({
        version: 1,
        eventId: "event-42",
        type: "message_state_changed",
        projectId: "project-1",
        sequence: 42,
        cursor: "v1.42",
        occurredAt: "2026-08-14T12:00:01.000Z",
        messageId: "message-1",
        correlationId: "correlation-1",
        causationId: "message-1",
        payload: { message: messagePayload },
      }),
    });

    expect(connection).toEqual({
      url: "wss://hub.example.test/api/v1/bridge/events?cursor=v1.41",
      credentials: { token: "installation-secret-1" },
    });
    expect(events[0]?.type).toBe("message_state_changed");
    expect(events[0]?.sequence).toBe(42);
    subscription.close();
    expect(socket.closed).toBe(true);
  });

  test("delivers cursor_expired followed by its repair snapshot without terminating", async () => {
    const socket = new FakeSocket();
    const received: BridgeEvent[] = [];
    const errors: unknown[] = [];
    const client = new SessionBridgeClient({
      baseUrl: "https://hub.example.test",
      credentials,
      fetch: (() => Promise.reject(new Error("not used"))) as unknown as typeof fetch,
      webSocketFactory: () => {
        queueMicrotask(() => socket.emit("open"));
        return socket;
      },
    });
    const subscription = client.project(projectId("project-1")).subscribe({
      cursor: eventCursor("v1:1"),
      reconnect: false,
      onEvent: (event) => received.push(event),
      onError: (error) => errors.push(error),
    });
    await subscription.ready;
    socket.emit("message", {
      data: JSON.stringify({
        version: 1,
        eventId: "gap-1",
        type: "cursor_expired",
        projectId: "project-1",
        sequence: 41,
        cursor: "v1:41",
        occurredAt: "2026-08-14T12:00:00.000Z",
        payload: {
          requestedCursor: "v1:1",
          oldestAvailableCursor: "v1:20",
          resnapshotRequired: true,
        },
      }),
    });
    socket.emit("message", {
      data: JSON.stringify({
        version: 1,
        eventId: "snapshot-41",
        type: "snapshot",
        projectId: "project-1",
        sequence: 41,
        cursor: "v1:41",
        occurredAt: "2026-08-14T12:00:00.001Z",
        payload: {
          directoryRevision: "revision-1",
          generatedAt: "2026-08-14T12:00:00.001Z",
          sessions: [],
          messages: [],
        },
      }),
    });
    await Promise.resolve();
    expect(received.map((event) => event.type)).toEqual(["cursor_expired", "snapshot"]);
    expect(errors).toEqual([]);
    subscription.close();
  });

  test("async iterable yields a gap marker and then the repair snapshot", async () => {
    const socket = new FakeSocket();
    const controller = new AbortController();
    const client = new SessionBridgeClient({
      baseUrl: "https://hub.example.test",
      credentials,
      fetch: (() => Promise.reject(new Error("not used"))) as unknown as typeof fetch,
      webSocketFactory: () => {
        queueMicrotask(() => socket.emit("open"));
        return socket;
      },
    });
    const iterator = client
      .project(projectId("project-1"))
      .events({ cursor: eventCursor("v1:1"), signal: controller.signal })
      [Symbol.asyncIterator]();
    const first = iterator.next();
    await new Promise((resolve) => setTimeout(resolve, 0));
    socket.emit("message", {
      data: JSON.stringify({
        version: 1,
        eventId: "gap-1",
        type: "cursor_expired",
        projectId: "project-1",
        sequence: 41,
        cursor: "v1:41",
        occurredAt: "2026-08-14T12:00:00.000Z",
        payload: {
          requestedCursor: "v1:1",
          oldestAvailableCursor: "v1:20",
          resnapshotRequired: true,
        },
      }),
    });
    expect((await first).value?.type).toBe("cursor_expired");
    const second = iterator.next();
    socket.emit("message", {
      data: JSON.stringify({
        version: 1,
        eventId: "snapshot-41",
        type: "snapshot",
        projectId: "project-1",
        sequence: 41,
        cursor: "v1:41",
        occurredAt: "2026-08-14T12:00:00.001Z",
        payload: {
          directoryRevision: "revision-1",
          generatedAt: "2026-08-14T12:00:00.001Z",
          sessions: [],
          messages: [],
        },
      }),
    });
    expect((await second).value?.type).toBe("snapshot");
    controller.abort();
    await iterator.return?.();
  });

  test("maps stable HTTP failures to typed SDK errors", async () => {
    const cases = [
      [401, { error: "unauthorized" }, AuthenticationError],
      [409, { error: "idempotency_conflict" }, ConflictError],
      [429, { error: "rate_limited", retry_after_seconds: 60 }, RateLimitError],
    ] as const;
    for (const [status, body, ErrorType] of cases) {
      const client = new SessionBridgeClient({
        baseUrl: "http://127.0.0.1:1440",
        credentials,
        fetch: (() => Promise.resolve(Response.json(body, { status }))) as unknown as typeof fetch,
      });
      await expect(client.contract()).rejects.toBeInstanceOf(ErrorType);
    }
  });

  test("keeps deployment authority in a separate admin client", async () => {
    let payload: any;
    const admin = new SessionBridgeAdminClient({
      baseUrl: "https://hub.example.test",
      credentials: { token: "deployment-secret" },
      fetch: (async (_url: string | URL | Request, init?: RequestInit) => {
        payload = JSON.parse(String(init?.body));
        return Response.json({
          token: "new-installation-secret",
          token_id: "token-1",
          installation_id: "flam-production",
          project_id: "project-1",
          audience: "flam-team-runtime",
          operations: ["directory:read", "message:send", "events:subscribe"],
          expires_at: "2026-08-14T13:00:00.000Z",
        });
      }) as typeof fetch,
    });

    const provisioned = await admin.provision({
      installationId: installationId("flam-production"),
      projectId: projectId("project-1"),
      audience: "flam-team-runtime",
    });
    expect(payload.project_id).toBe("project-1");
    expect(provisioned.tokenId).toBe("token-1");
  });
});
