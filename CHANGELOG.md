# Changelog

## Unreleased

### Added

- Authenticated, project-scoped Session Bridge directory and message ledger,
  addressed by stable project UUID and terminal window ID.
- Explicit delivery states (`queued`, `routing`, `accepted_by_daemon`,
  `presented_to_agent_input`, `acknowledged_by_agent`, `replied`, `failed`,
  `expired`, `cancelled`) plus message-bound replies and a durable, resumable,
  project-ordered WebSocket event ledger.
- Local and configured remote ImmorTerm session aggregation, persisted daemon
  heartbeat/activity fields, terminal MCP discovery/ack/reply tools, and a
  configurable Session Bridge panel with enable/disable, credential rotation,
  and remote add/test/remove controls. The bearer credential is stored at
  `~/.immorterm/bridge-token` with mode `0600` and is never rendered in the UI.
- `@immorterm/session-bridge`, a typed Node SDK for SaaS host adapters, with
  branded IDs, runtime validation, AbortSignal support, typed errors, and
  reconnecting AsyncIterable/callback subscriptions without polling. Agent
  acknowledgement/reply authority is intentionally absent from the host SDK.
- Deployment administrator credentials provision short-lived, revocable
  installation credentials scoped to installation, project, audience,
  operations, expiry, and token ID. Hosted credentials are stored hashed and
  cannot widen their project scope.
- Canonical retry idempotency, bounded queues/rates/expiry, and `Retry-After`.
- Hardened Session Bridge agent authority with daemon-only, message-bound
  receipts; added authenticated credential identity preflight; made cursor-gap
  repair non-terminating; and gated attachments until an immutable resolver is
  available.

### Known limitations

- Configured remote forwarding is implemented but still needs a remote
  end-to-end acknowledgement test.
- The loopback hub has no outbound connector or reverse tunnel to FLAM
  Factory/team-runtime in k3s. The local bridge must not be described as
  k3s-reachable until that transport and its reconnect tests are implemented.
