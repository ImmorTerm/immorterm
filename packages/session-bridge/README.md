# `@immorterm/session-bridge`

Typed, channel-neutral access to an ImmorTerm Hub serving local or remotely
deployed terminals. ImmorTerm owns stable identity, presence, delivery state,
agent acknowledgement, replies, and remote aggregation. A SaaS host owns its
tenant authentication, human allowlists, product UI, audit, and notifications.

## Provision once, use scoped credentials

The deployment credential is an administrative secret. Use it only to issue or
revoke short-lived installation credentials:

```ts
import {
  SessionBridgeAdminClient,
  installationId,
  projectId,
} from "@immorterm/session-bridge";

const admin = new SessionBridgeAdminClient({
  baseUrl: process.env.IMMORTERM_HUB_URL!,
  credentials: { token: process.env.IMMORTERM_BRIDGE_ADMIN_TOKEN! },
});

const issued = await admin.provision({
  installationId: installationId("flam-production"),
  projectId: projectId("794e3fa2-f27d-41b6-9c67-ec1ef7b06301"),
  audience: "flam-team-runtime",
  ttlSeconds: 3600,
});
// Persist issued.token in the installation's secret store. It is shown once.
```

An installation credential is bound server-side to one installation, project,
audience, expiry, token ID, and operation set. A caller-supplied project ID can
only assert that scope; it cannot select or widen it.

## Host API

```ts
import {
  SessionBridgeClient,
  correlationId,
  installationId,
  messageId,
  projectId,
  windowId,
} from "@immorterm/session-bridge";

const bridge = new SessionBridgeClient({
  baseUrl: process.env.IMMORTERM_HUB_URL!,
  credentials: async () => ({ token: await refreshInstallationToken() }),
});

await bridge.assertIdentity({
  installationId: installationId("flam-production"),
  projectId: projectId("794e3fa2-f27d-41b6-9c67-ec1ef7b06301"),
  audience: "flam-team-runtime",
  requiredOperations: ["directory:read", "message:send", "events:subscribe"],
});

const project = bridge.project(); // project comes from the credential
const directory = await project.directory({ status: "active", signal });
const sent = await project.send({
  targetWindowId: windowId(selected.windowId),
  messageId: messageId(`review-${review.id}`),
  correlationId: correlationId(`slack-thread-${thread.id}`),
  message: "A founder requested review of Factory #27.",
  expiresAt: new Date(Date.now() + 60 * 60_000).toISOString(),
  signal,
});
```

`messageId` is the idempotency key. An identical retry returns the existing
record. Reuse with a different canonical project, target, correlation, content,
attachments, expiry, or trace metadata returns `409 Conflict`.

The host client exposes directory, send, cancel, delivery observation, and
correlated replies. It intentionally has no acknowledgement or reply mutation:
only the receiving ImmorTerm agent may perform those actions, bound to the
incoming message.

## Event-driven updates

The primary API is a resumable async iterable. Delivery is project-ordered and
at-least-once; persist each cursor only after handling the event and deduplicate
by `eventId`.

```ts
for await (const event of project.events({ cursor: savedCursor, signal })) {
  await handle(event);
  // A cursor_expired marker announces a gap. The following snapshot repairs
  // it and supplies the cursor that is safe to persist.
  if (event.type !== "cursor_expired") await saveCursor(event.cursor);
}
```

A callback wrapper is also available:

```ts
const subscription = project.subscribe({
  cursor: savedCursor,
  signal,
  onEvent: handle,
  onConnectionChange: state => observeConnection(state),
  onError: error => report(error),
});
await subscription.ready;
subscription.close();
```

If retention no longer contains the requested cursor, the stream emits an
explicit `cursor_expired` event followed by a fresh snapshot. Both subscription
APIs deliver both events without terminating. Do not persist the gap marker's
cursor; reconcile and persist the following snapshot. Connections refresh
installation credentials on reconnect and never poll.

## Content and topology

Messages are UTF-8 plain text, at most 64 KiB. Session Bridge v1 rejects
non-empty `attachments` with `422 attachments_not_supported` until ImmorTerm
ships an immutable, hash-verified resolver. Bytes, secret values, temporary
URLs, and shell commands are never accepted. The Hub limits request rate,
pending queues, and expiry. The protocol permits 100 pending messages per
installation and 20 per target; a consumer may impose a one-pending-job product
policy, but it is not a Session Bridge invariant.

Run a served Hub behind TLS/private networking for remote deployments:

```bash
IMMORTERM_HUB_HOST=0.0.0.0 \
IMMORTERM_BRIDGE_TOKEN="$ADMIN_SECRET" \
immorterm-hub serve --port 1440
```

The SDK does not make a loopback desktop Hub reachable from a SaaS control
plane. That topology requires a served Hub/private tunnel or ImmorTerm's
outbound connector/reverse relay.
