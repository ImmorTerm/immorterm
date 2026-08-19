# ImmorTerm Session Bridge v1 — continuation handoff

**Prepared:** 2026-08-15 (Asia/Jerusalem)
**Outgoing ImmorTerm ID:** `57921-8ffb4aa1`
**Outgoing Codex session UUID:** `019ff6c0-ada7-7b42-bd28-c4bde4a91689`
**Repository:** `/Users/shaisnir/Development/immorterm-org/immorterm`
**Working branch at handoff:** `feat/task-grouping`
**Consumer baselines:** Longstory PR #70 at `2a89b678`; FLAM PR #85 at `9ddcb514`
**Canonical external blocker:** `task-1786740703871`
**Completed local hardening task:** `task-1786740833707`

## Read this first

This is a shared and heavily dirty worktree. At handoff time it contains roughly 142 modified
tracked files and 18 untracked paths. Those edits are not all owned by one agent. Do **not** stash,
reset, restore, clean, or stage the whole repository. Do not discard another agent's changes. Use
path/hunk-scoped commits or an isolated integration worktree based on a known commit.

The Session Bridge source changes described below are implemented and tested, but they were
intentionally **not deployed** and the npm package was **not published**. Consequently, the Hub and
daemons currently running on the user's machine may still implement the older contract. A live
`409 message is not presented to this agent session` from an old daemon is not evidence that the
new source failed; rebuild/restart is required before live validation.

Do not claim that a desktop loopback Hub is reachable from FLAM k3s. The secure outbound
connector/reverse relay is still the next major ImmorTerm-owned deliverable.

## Product intent and ownership boundary

ImmorTerm owns the channel-neutral terminal/session infrastructure:

- stable project and terminal-window identity;
- project-scoped discovery across local and configured remote ImmorTerm Hubs;
- presence, heartbeat, `is_working`, `needs_attention`, and persisted `last_activity_at`;
- durable external-message ledger, ordering, cursors, retries, delivery states, cancellation,
  acknowledgement, and correlated replies;
- daemon presentation and receiving-agent authority;
- the TypeScript Session Bridge SDK;
- secure remote/served-Hub topology and the desktop-loopback-to-SaaS outbound connector;
- future durable file/image reference resolution.

FLAM owns only its product integration:

- Slack UI and founder workflow;
- tenant/founder allowlists;
- product audit and notification policy;
- selecting a window returned by ImmorTerm's authenticated directory;
- consuming the published SDK and event contract.

FLAM must not copy or reimplement the Hub, daemon, relay, acknowledgement, reply, cursor, event,
terminal, remote-session, or filesystem behavior. Longstory owns reviewed memory and permissioned
knowledge, not terminal transport. Do not put Session Bridge transport into `immorterm-memory`.

The committed FLAM project UUID is:

```text
794e3fa2-f27d-41b6-9c67-ec1ef7b06301
```

## Corrected Session Bridge v1 contract now implemented in source

### Credential roles

The deployment credential comes from `IMMORTERM_BRIDGE_TOKEN` or the mode-`0600` local file
`~/.immorterm/bridge-token`. It is a control-plane credential only. It may provision and revoke
short-lived installation credentials. It is rejected for directory, send, cancel, contract/event
host operations, acknowledgement, and reply.

Installation credentials are stored by SHA-256 digest and bound server-side to:

- `installation_id`;
- canonical `project_id`;
- `audience`;
- allowed operations (`directory:read`, `message:send`, `events:subscribe`);
- expiry;
- token ID and revocation state.

A request-supplied project ID is only an assertion. It cannot select or widen scope.

### Identity preflight

`GET /api/v1/bridge/identity` returns the non-secret claims of the credential that authenticated
the request. The SDK exposes:

```ts
await bridge.assertIdentity({
  installationId: installationId("flam-production"),
  projectId: projectId("794e3fa2-f27d-41b6-9c67-ec1ef7b06301"),
  audience: "flam-team-runtime",
  requiredOperations: ["directory:read", "message:send", "events:subscribe"],
});
```

This fails closed on an administrator credential, wrong installation, wrong project, wrong
audience, or missing operation.

### Directory and targeting

`GET /api/v1/bridge/directory` derives the project from the installation credential. Targets are
stable `window_id` values from that authenticated directory. Arbitrary shell names, session names,
filesystem paths, commands, and PTY targets are not part of the external API.

Directory records expose the stable project/window IDs, display/tool/location information,
`active | idle | offline`, heartbeat timestamps, persisted activity, working/attention flags,
capabilities, and protocol version. Configured remote entries retain their remote identity.

### Send and idempotency

`POST /api/v1/bridge/messages` accepts bounded UTF-8 plain text plus stable `message_id`,
`correlation_id`, `target_window_id`, and expiry. The canonical idempotency comparison covers
project, target, correlation, content, attachments metadata, expiry, and trace metadata:

- identical retry returns the existing message;
- conflicting reuse returns `409`.

Current hard limits are:

- 64 KiB message text;
- 60 requests/minute per principal;
- 100 pending messages per installation;
- 20 pending messages per target;
- expiry no more than 24 hours out;
- 10,000 retained project events.

One pending engineering job is a possible FLAM product policy, **not** a Session Bridge invariant.

### Delivery states

The ledger uses:

1. `queued` — durably stored by the Hub;
2. `routing` — a delivery attempt began;
3. `accepted_by_daemon` — the exact daemon accepted the envelope;
4. `presented_to_agent_input` — ImmorTerm presented it to the receiving agent input;
5. `acknowledged_by_agent` — the receiving agent explicitly accepted it;
6. `replied` — a correlated response was recorded;
7. `failed`, `expired`, or `cancelled` — terminal outcomes.

PTY/IPC write success is never agent acknowledgement.

### Receiving-agent authority

The old implementation was insecure because daemon MCP acknowledgement/reply read the deployment
administrator token. The corrected source does this instead:

1. The Hub generates a high-entropy `imsr_...` receipt for each local delivery.
2. Only its SHA-256 digest is stored in the project message store.
3. The plaintext receipt goes only to the exact target daemon over its Unix socket in
   `AcceptExternalMessage`.
4. After successful presentation, the daemon retains the receipt in a bounded in-memory map.
5. `immorterm_acknowledge_message` and `immorterm_reply_to_message` retrieve it from the current
   daemon via `GetExternalMessageReceipt`.
6. Ack/reply use that receipt as bearer authority. The Hub scans its ledgers for the matching
   message/receipt and derives project, target window, correlation, and reply destination from the
   stored message. Request JSON cannot override them.
7. Deployment and installation credentials do not authorize ack/reply.

This survives Codex/Claude context compaction as long as the same daemon remains alive. It is not
persisted across daemon restart; a restarted daemon must receive a new delivery/retry under the
normal message state/expiry rules.

### Event contract and cursor repair

`WS /api/v1/bridge/events` is project-ordered, durable, resumable, and at-least-once. Consumers
deduplicate by `eventId` and persist handled cursors.

If the requested cursor is invalid or too old, the Hub sends:

1. `cursor_expired` describing the gap;
2. a complete repair `snapshot`.

The SDK now delivers both events through both callback and `AsyncIterable` APIs. It does not throw
and close on `cursor_expired`, and it does not advance its reconnect cursor to the gap marker.
Consumers must not persist the gap cursor; reconcile the following snapshot and then persist the
snapshot cursor.

### Configured remote Hubs

Deployment admin authority is no longer used for remote message/event traffic. The local Hub uses
the remote deployment credential only to provision a one-project, exact-operation, one-hour relay
installation credential. These credentials are cached by remote/project/operation and refreshed
before expiry to avoid provisioning a new credential on every reconnect.

Remote directory aggregation and message forwarding are implemented. Real configured-remote
acknowledgement/reply and latency proof is still outstanding.

### Attachments

Although attachment hash metadata had been validated previously, no durable resolver delivered a
usable object/path to the receiving agent. Session Bridge v1 therefore now rejects any non-empty
`attachments` array with:

```text
422 attachments_not_supported
```

The first FLAM slice is text-only. Do not re-enable the advertised attachment capability until an
immutable, hash-verified resolver is built and tested for both local and remote targets. Never
accept raw image bytes, secret-bearing URLs, temporary signed URLs, arbitrary absolute paths, or
shell commands. Existing project files should eventually be referenced without duplication; new
clipboard screenshots may use the existing ImmorTerm paste-image mechanism only after they are
turned into a durable bridge-owned reference.

## Important source files

Session Bridge Hub:

- `services/hub/src/routes/bridge.rs` — credentials, directory, ledger, receipt authority, states,
  events, limits, contract and tests;
- `services/hub/src/routes/mod.rs` — route registration;
- `services/hub/src/routes/remote_api.rs` — configured-remote forwarding, SSH tunnel and scoped
  relay credential provisioning/cache;
- `services/hub/Cargo.toml` and root `Cargo.lock` — SHA-256 dependency.

Daemon/MCP:

- `apps/immorterm-ai/immorterm-daemon/src/ipc.rs` — `AcceptExternalMessage`,
  `PresentExternalMessage`, `GetExternalMessageReceipt` wire types and tests;
- `apps/immorterm-ai/immorterm-daemon/src/daemon.rs` — bounded pending messages, receipt retention,
  and presentation;
- `apps/immorterm-ai/immorterm-daemon/src/mcp.rs` — discovery, scoped local directory credential,
  ack/reply receipt retrieval and calls.

SDK:

- `packages/session-bridge/src/contracts.ts`;
- `packages/session-bridge/src/runtime.ts`;
- `packages/session-bridge/src/client.ts`;
- `packages/session-bridge/src/client.test.ts`;
- `packages/session-bridge/README.md`;
- root `package.json` and `bun.lock` workspace registration;
- generated `packages/session-bridge/dist/*`.

Docs/UI:

- `apps/docs/content/docs/session-bridge.mdx`;
- `apps/docs/content/docs/hub-api.mdx`;
- `CHANGELOG.md`;
- `apps/extension/resources/gpu-terminal-modals.js` and its tests contain the Session Bridge
  settings panel, but that file also contains other concurrent UI work. Audit hunks before commit.

## Verification already completed

The following passed after the corrected consumer baselines were inspected:

```text
cargo test -p immorterm-hub
  68 Hub unit tests + 7 webview contract tests passed

cargo test -p immorterm-daemon
  86 daemon tests passed

packages/session-bridge:
  9 SDK tests passed
  TypeScript typecheck passed
  TypeScript build passed
  bun pm pack --dry-run passed (18 files, ~82 KiB unpacked)

apps/docs:
  production MDX/Next build passed (30 static pages)

cargo check -p immorterm-hub -p immorterm-daemon
  passed
```

No deployment, installed-extension rebuild, Hub restart, daemon restart, npm publish, or real
desktop-to-k3s delivery was performed after the final hardening.

## What must happen next, in order

### 1. Finish a clean, reviewable commit and merge

The dirty worktree prevents a safe blanket `git add -A`. Create path/hunk-scoped commits for the
Session Bridge and this handoff. Do not include unrelated renderer, digest, vendor, Files, Inbox,
task-grouping, or other agents' changes merely because they share a file.

Where a required Session Bridge hunk depends on an uncommitted shared-file refactor, reproduce the
minimal hunk in an isolated integration worktree rather than staging the whole file. Re-run all
verification above on the resulting commit. Merge into the intended integration branch only after
checking that branch's current tip; at handoff `main` was `80adeb8` and `feat/task-grouping` was
`f21f1d6` before the new commit.

### 2. Publish and pin the corrected SDK

After code review and version decision:

- bump `@immorterm/session-bridge` from its current pre-publication `0.1.0` as appropriate;
- run `prepublishOnly`, inspect `bun pm pack --dry-run`, and publish to the approved registry;
- record the immutable package version/integrity;
- notify FLAM so PR #85 can pin that exact version;
- do not let FLAM copy wire types or add temporary workarounds.

### 3. Land and deploy the secure outbound desktop-loopback-to-FLAM-k3s connector

The generic connector source is now implemented on `codex/session-bridge-main`. See
`docs/session-bridge-outbound-connector.md`. It adds the authenticated outbound WebSocket,
persisted offline directory, durable queue drain, strict connector source binding, credential
expiry/revocation checks, and loopback-only plaintext development rule. It still needs review,
merge, FLAM deployment configuration, and a real desktop-to-staging proof.

The remaining work from `task-1786740703871` is review/merge, FLAM deployment configuration,
credential provisioning/rotation, and the real desktop-to-staging proof.

Required properties:

- desktop Hub remains loopback-only;
- desktop/daemon initiates an authenticated outbound TLS/WebSocket connection to a served Hub or
  relay reachable by FLAM k3s;
- stable installation/project identity and least-privilege operations;
- explicit enrollment, rotation, revocation, audience and expiry;
- resumable project events with durable cursor/outbox behavior;
- bounded offline queue and expiry;
- no polling;
- no arbitrary shell/session/PTY targeting;
- at-least-once delivery with `eventId` deduplication;
- reconnect after network loss and credential rotation;
- reply destination and correlation derived from the original message;
- observability without logging secrets, raw prompts, private reasoning, or customer memory tokens.

Use the same Session Bridge Hub/daemon contract. Do not create a FLAM-specific presence or message
store. A served remote Hub behind TLS/private networking is already a supported topology, but it
does not solve desktop loopback → k3s by itself.

Local two-Hub proof completed on 18 Aug 2026:

- a served Hub listed a session supplied by a loopback-only desktop Hub;
- an online message reached `presented_to_agent_input` through the real Unix-socket delivery path;
- a second message stayed `queued` while the desktop Hub was stopped and reached
  `presented_to_agent_input` after restart;
- the served Hub was then stopped while the desktop agent acknowledged and replied locally;
- after the served Hub restarted, cursor replay restored state `replied` and the exact correlated
  reply text;
- the proof used distinct served/desktop homes and ports plus a minimal daemon socket double; it did
  not use FLAM staging, so the real deployment proof below remains required.

### 4. Prove real end-to-end delivery

Test at least:

- FLAM k3s/team-runtime lists the FLAM project directory through the published SDK;
- stable target selection by `window_id`;
- wrong deployment/installation/project/audience/operation/window is denied;
- deterministic send reaches `queued`, `routing`, `accepted_by_daemon`, and
  `presented_to_agent_input`;
- receiving agent alone acknowledges, and admin/host attempts are denied;
- correlated reply reaches FLAM;
- duplicate send returns the same record; conflicting reuse returns `409`;
- disconnect/offline queue/reconnect works without polling;
- cursor retention gap yields marker + repair snapshot in order;
- remote configured-Hub acknowledgement/reply works;
- measured send→present, send→ack and send→reply latency is recorded;
- revoked/expired credentials stop new work;
- no raw transcript or Longstory credential is copied through the terminal bridge.

Do not mark the canonical blocker complete until this proof exists.

### 5. Rebuild/restart only when deployment is authorized

When the user explicitly requests deployment:

- build Hub and daemon binaries from the merged commit;
- rebuild/package the VS Code extension if its UI/resources are part of the merged change;
- restart the Hub and reconnect/restart target daemons so they advertise protocol v1;
- verify the installed resources are the newly built ones;
- run a live ack/reply after an AI context compaction to prove the daemon-held receipt survives;
- clearly tell the user what was restarted and whether active terminal sessions were interrupted.

## Adjacent work from the larger ImmorTerm session

The repository contains other incomplete or separately owned changes. Do not silently fold them
into the Session Bridge commit:

- **Remote Files legacy restoration fix:** isolated worktree
  `/Users/shaisnir/Development/immorterm-org/immorterm-remote-files-fix`, branch
  `fix/remote-files-legacy`, commit `78c5ee5a`. Preserve it and do not duplicate it in Longstory.
- **Human Inbox:** `services/hub/src/routes/inbox.rs` plus status-bar/mail UI and correlated action
  work is present but interleaved. It is conceptually built on the same wake/message channel but is
  a separate product surface and needs its own scoped review/tests/commit.
- **Shared Activity/session-to-session messaging and files/images:** source/UI work exists in the
  dirty tree. Verify installed-version behavior separately. Sharing must remain the authorization
  boundary. Existing local files should be referenced, not copied.
- **Vendor wrappers and wizard:** custom executable fields (for example `codex-trust`) and vendor
  save/model-list changes exist in the dirty tree. Reconnect must actually use the configured
  wrapper; do not infer completion from the settings field alone.
- **Codex digestion:** prior work attempted to add Codex auto-digest/at-a-glance support. Verify a
  real FLAM Codex session produces goals/memories after the merged extension/daemon is installed.
- **Codex scroll/selection anchoring:** the user reported that scrolling stopped but selection
  coordinates still drifted/disappeared as new output arrived. This remained unsatisfactory and
  needs a renderer/model-level fix and regression test; do not claim it solved.
- **Immediate new-session creation/resume identity:** several fixes are already committed on
  `feat/task-grouping`, including `f21f1d6` for Codex resume-ID detection. The user previously saw a
  fresh terminal inherit the wrong resume UUID and a temporary system overload. Re-test fresh
  creation, explicit resume, and wrapper launch without OrbStack/Eve interference before release.
- **Repository size:** `target/debug` was previously responsible for tens of gigabytes of build
  artifacts. It is disposable build output, but removal is a separate destructive action and must
  be explicitly scoped. Bun's global package store does not replace Cargo target artifacts.

## Communication state

The Longstory agent's current stable ImmorTerm ID during coordination was `77802-27028c82` and its
Codex thread ID was `01a000ed-c105-7070-a448-063aaa410f79`. Earlier IDs were confused, so always
confirm through Session Info/directory before targeting.

The former FLAM Team target was `41103-b78ffb92`; another relevant FLAM/Longstory terminal was
`41103-6338d801`. These may be stale. Discover through the authenticated project directory rather
than assuming they are alive.

The latest consumer update said Longstory PR #70 and FLAM PR #85 include all five corrections and
FLAM will not add workarounds or pin before the corrected SDK. Their acknowledgement/reply attempt
against the old live implementation returned `409` after Codex compaction, so it was explicitly a
best-effort terminal handoff, not correlated proof.

## Definition of done for the next agent

The next agent is done only when:

- the Session Bridge/handoff commit is clean and merged without other agents' changes;
- the corrected SDK is reviewed, published, and immutably pinned by FLAM (when publication is
  authorized);
- the secure outbound connector exists and is documented/configurable;
- a real FLAM k3s → desktop ImmorTerm → receiving agent acknowledgement/reply round trip passes;
- configured remote delivery passes the same proof;
- cursor repair, offline retry, revocation, idempotency, authorization, and latency evidence is
  recorded;
- docs/API/changelog match the exact deployed contract;
- no one claims completion based only on PTY write success or local unit tests.
