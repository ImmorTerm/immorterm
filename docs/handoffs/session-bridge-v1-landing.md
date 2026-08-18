# ImmorTerm Session Bridge v1 — landing handoff (continuation 2)

**Prepared:** 2026-08-18 (Asia/Jerusalem)
**Outgoing agent:** Claude (ImmorTerm ID `27274-8ffb4aa1` era session `1be40f79-de75-4b92-95e3-799e7844b0bc`, immorterm_id `27274-d8a92945`)
**Predecessor handoff:** `docs/handoffs/session-bridge-v1-continuation.md` (commit `f594576`) — still the
authoritative deep contract reference; this document records what happened AFTER it and what remains.
**Repository:** `/Users/shaisnir/Development/immorterm-org/immorterm`
**Integration branch:** `feat/task-grouping`, now at `be0b9a6`
**Verified bridge commit:** `9134b4b` (also tip of `feat/session-bridge-v1`)
**Canonical external blocker:** `task-1786740703871` (outbound connector) — still open
**Tracking:** task `task-1786751434312` (done), plan `plan://immorterm-org/session-bridge-v1-landing`,
memory `session-bridge-v1-landing.md` in the project auto-memory

---

## 1. The handover this agent received (verbatim)

The incoming context was the Codex agent's terminal summary plus the 408-line continuation doc.
The terminal summary, verbatim:

> • Created and committed the full 408-line handoff:
>
> `immorterm/docs/handoffs/session-bridge-v1-continuation.md`
>
> Commit: f594576
>
> I did not merge the feature changes because the shared worktree has 142 modified tracked files and
> 18 untracked paths from multiple agents, interleaved in the same Hub/daemon files. origin/main also
> advanced to 214e68d. Committing everything would capture other agents' work and violate the
> ownership boundary.
>
> The handoff documents:
>
> - Everything implemented and tested
> - Exact Session Bridge contract
> - All affected files
> - Consumer PR baselines
> - Remaining connector, SDK publication, deployment, and E2E work
> - Adjacent unfinished ImmorTerm features
> - Safe commit/merge procedure
> - Full definition of done
>
> No concurrent changes were discarded, stashed, reset, or overwritten.
>
> Token usage: total=4,356,049 input=3,938,793 (+ 159,699,840 cached) output=417,256 (reasoning
> 111,095) To continue this session, run `codex resume 019ff6c0-ada7-7b42-bd28-c4bde4a91689`

Codex session UUID `019ff6c0-ada7-7b42-bd28-c4bde4a91689`, outgoing ImmorTerm ID `57921-8ffb4aa1`.
Read the continuation doc in full before touching anything — its contract description, credential
model, delivery states, and definition of done all still apply verbatim.

## 2. What this agent did (handoff step 1: clean commit + merge — DONE)

### 2.1 Extraction methodology

The shared worktree held ~142 modified + 18 untracked paths from multiple agents interleaved in the
same files. The Session Bridge changes were extracted into an isolated worktree
(`../immorterm-bridge-v1`, branch `feat/session-bridge-v1`, base `f594576`) by hunk-level audit of
every shared file (~9,500 patch lines classified):

- **New files taken wholesale:** `services/hub/src/routes/bridge.rs`, `packages/session-bridge/`
  (dist/ is gitignored and stayed out), `apps/docs/content/docs/session-bridge.mdx`, `CHANGELOG.md`.
- **Shared files trimmed of foreign features** (reverse-applied hunks + within-hunk surgery, with
  the compiler as referee): `routes/mod.rs`, `remote_api.rs`, daemon `daemon.rs`/`ipc.rs`/`mcp.rs`,
  `gpu-terminal-modals.js` + its test, `hub-api.mdx`, `package.json`, `bun.lock` (extension version
  bump reverted), hub `Cargo.toml`, `Cargo.lock` (regenerated minimally for sha2).
- **Prerequisites included** because the bridge contract's persisted presence depends on them and
  they are the same author's persistence cluster: daemon `registry.rs` (heartbeat_at /
  last_activity_at fields, update_tool, build_ai_stats_entry, registry.d history), daemon
  `claude.rs` (stats_dirty Codex stats), hub `routes/registry.rs` (`enriched_registry_snapshot`,
  registry.d union), and the `PersistDelta` background persistence worker in `daemon.rs`.
- **Excluded (still uncommitted in the shared tree, owned by other threads):**
  - Human Inbox: `services/hub/src/routes/inbox.rs` (untracked), inbox routes in `mod.rs`,
    `proxy_remote_inbox*` in `remote_api.rs`, `immorterm_send_to_inbox` MCP tool, Inbox modal UI +
    tests.
  - Shared-session messaging/files: `shared_activity.rs` (untracked), `SendInput` /
    `SendSharedMessage` / `ReceiveShared*` IPC variants + daemon handlers,
    `immorterm_send_message` / `immorterm_send_file` MCP tools, `channel_partner_name` plumbing,
    pairing-direction rework, GetSharedActivity ws command.
  - Vendor-wizard custom command fields (modals h12–h19), Registries modal, digest model-list
    refresh, `registry-d-removal.test.ts`, CLI `sessions.ts`, `immorterm-digest/src/ipc.rs`,
    `libs/conversation-adapters` changes, extension `bin/` dirs.

Test-count deltas vs the predecessor's numbers are exactly the excluded features' tests:
hub 68→67 (inbox.rs's one test), daemon 86→75 (shared/inbox tool tests).

### 2.2 Security fix added on top of the handoff state

Automated review flagged the events-WS bearer credential traveling in the URL. Fixed in the same
commit, both sides:

- `bridge.rs` remote-relay client now builds the WS request via
  `IntoClientRequest` and sends `Authorization: Bearer <relay credential>`; the URL carries only
  `project_id`.
- `EventsQuery.token` and the `query_token` parameter of `authenticate()` were removed — the
  credential can only arrive via the Authorization header. Contract-safe: the SDK was already
  header-based (`ws` with `headers: { Authorization }`), and the relay was the only query-token
  consumer.

Beware: an automated re-scan later flagged the same pattern again — that was the *stale pre-fix
working copy* of `bridge.rs` still sitting in the shared worktree. Its entire diff vs HEAD was the
exact reverse of the fix, so it was synced to HEAD. If a scanner flags token-in-URL in bridge.rs
again, first check whether it is looking at committed content or leftover dirt.

### 2.3 Verification (all green, isolated worktree, own CARGO_TARGET_DIR)

```text
cargo check -p immorterm-hub -p immorterm-daemon   clean, no warnings
cargo test  -p immorterm-hub                       67 + 7 webview contract tests
cargo test  -p immorterm-daemon                    75 tests
packages/session-bridge                            9 SDK tests, typecheck, build,
                                                   bun pm pack --dry-run: 18 files ~82KB
bun run test:extension                             268/268 (later 284/284 after the .mcp.json fix,
                                                   other agents having added tests meanwhile)
bun run build:docs                                 production build passed
```

### 2.4 Landing mechanics (how the merge was done without touching the dirty tree)

`feat/task-grouping` is checked out in the shared dirty worktree, so a normal merge/checkout would
have refused or clobbered. The landing used a **guarded ref update** (explicitly user-approved after
the permission classifier blocked it in auto mode):

```bash
git update-ref -m "land Session Bridge v1" \
  refs/heads/feat/task-grouping 9134b4b f594576   # old-value guard vs concurrent moves
git restore --staged -- <the 26 committed paths>   # refresh index to new HEAD
```

Zero working-tree files were touched; the bridge content simply dropped out of `git status`
(154 → 144 → 140 entries). Other agents' uncommitted work is byte-identical to before.

## 3. Second fix this session: `.mcp.json` reconnect-nonce noise (`be0b9a6`)

A FLAM agent complained that "the ImmorTerm bridge rewrites a reconnect timestamp continuously".
**Attribution correction: it is not the Session Bridge** (all bridge writes live under
`~/.immorterm/`). The writer is the extension's OpenMemory/MCP keepalive:
`refreshMcpConfig` (`apps/extension/src/services/memory/mcp-configurator.ts`) wrote
`_reconnect_ts: Date.now()` into the **tracked** `.mcp.json` and left it resting there, and a
5-minute keepalive (`openmemory-manager.ts`, `MCP_KEEPALIVE_INTERVAL_MS`) rewrote it forever —
perpetual machine dirt in every repo that commits `.mcp.json`.

Fix (commit `be0b9a6`): the nonce write still fires Claude Code's file watcher (the empirically
verified trigger), but the file's exact canonical bytes are restored 2s later; a stale nonce left by
a crash is stripped on the next trigger (self-heal); a concurrent edit inside the window wins over
the restore; and the function no longer creates `.mcp.json` where none existed. Verified with
284/284 extension tests + tsc. The gateway-manager path is different (delegates to the gateway
process) and was not touched.

**NOT yet live:** the fix takes effect only after the extension is rebuilt/deployed
(`/deploy-extension`). Until then the installed extension still rewrites the resting nonce every
5 minutes, and the org root's `.mcp.json` still contains one. After deploy, the first keepalive tick
strips it — consumers (FLAM included) should commit that one-time removal.

## 4. Current state snapshot (2026-08-18)

```text
feat/task-grouping   be0b9a6  (fix .mcp.json nonce)
                     9134b4b  (Session Bridge v1)
                     f594576  (predecessor handoff)
origin/main          30d4da0  (has advanced past the predecessor's 214e68d; bridge NOT on main)
worktrees            immorterm-bridge-v1        9134b4b [feat/session-bridge-v1]  (keep for review)
                     immorterm-remote-files-fix 78c5ee5 [fix/remote-files-legacy] (preserve, per predecessor)
shared tree dirt     ~140 entries — ALL owned by the excluded features listed in §2.1; do not sweep
not deployed         nothing was pushed, published to npm, rebuilt, or restarted this session
```

## 5. What must happen next, in order (unchanged from the predecessor except step 1)

1. ~~Clean commit and merge~~ — **done** (`9134b4b` on `feat/task-grouping`). Decide when/how to get
   it onto `main` (origin/main has moved; a PR from `feat/task-grouping` or a rebase is the user's
   call — check tips first).
2. **Publish and pin the corrected SDK** — `@immorterm/session-bridge` is still pre-publication
   `0.1.0`. Review → version decision → `prepublishOnly` → publish → record immutable
   version/integrity → notify FLAM so PR #85 pins it. Open decision `sdk-publish` on the plan
   (recommendation: wait for human review first). FLAM must not copy wire types.
3. **Build the secure outbound desktop-loopback → FLAM k3s connector** (`task-1786740703871`) —
   the primary remaining ImmorTerm deliverable. Full required-properties list in the predecessor
   doc §3; unchanged.
4. **Prove real end-to-end delivery** — the full checklist in the predecessor doc §4 stands,
   including: receiving-agent-only ack (admin/host denied), correlated reply reaching FLAM,
   duplicate/conflict idempotency, cursor gap → marker + repair snapshot, remote-hub ack/reply,
   latency numbers, revocation, and **post-compaction receipt survival** (receipt lives only in the
   daemon's memory — survives AI compaction, not daemon restart). Do not mark
   `task-1786740703871` complete without this proof.
5. **Deploy/restart only when the user explicitly authorizes** — Hub + daemon binaries from the
   merged commit, extension rebuild (now ALSO required for the `.mcp.json` fix, §3), restart so
   daemons advertise protocol v1, then a live ack/reply after a compaction. A live
   `409 message is not presented to this agent session` from the currently-running old daemon is
   NOT evidence the new source is broken — rebuild first.

## 6. Traps this agent hit (so you don't)

- **The permission classifier blocks `git update-ref` and `git restore --staged --source=`** in
  auto mode. Don't burn retries; ask the user to run the guarded one-liner or approve it.
- **Security scanners see working-tree dirt, not commits.** Check `git diff` before "re-fixing"
  something that is already fixed at HEAD (§2.2).
- **`bun run build:docs` rewrites `apps/docs/next-env.d.ts`** — keep it out of your commits.
- **`git add -A` in the shared worktree is still forbidden** — the ~140 dirty entries are other
  agents' live work. Path-scope everything; check a file's cleanliness before editing it in place.
- **Hub tests are 67+7 and daemon 75 now** — if you see the predecessor's 68/86, you've compiled
  the excluded features back in.
- The org-root repo (`immorterm-org/.git`) intermittently reports "not a git repository" under the
  sandbox; the `immorterm/` subrepo is fine. Work there.

## 7. Adjacent work registry (from the predecessor, updated)

Unchanged and still uncommitted/open: Human Inbox (needs its own scoped review/tests/commit),
Shared Activity + session-to-session files/images, vendor wrappers/wizard, Codex digestion
verification, Codex scroll/selection anchoring (task `task-1786636711313`, in progress — do not
claim solved), fresh-session/resume identity re-test, repository size (`target/debug` tens of GB —
destructive cleanup needs explicit scoping). Remote Files fix worktree preserved at `78c5ee5`.

## 8. Identity and communication state

- Longstory agent: ImmorTerm ID `77802-27028c82`, Codex thread `01a000ed-c105-7070-a448-063aaa410f79`.
- Former FLAM targets `41103-b78ffb92` / `41103-6338d801` may be stale — discover through the
  authenticated bridge directory, never assume.
- FLAM project UUID: `794e3fa2-f27d-41b6-9c67-ec1ef7b06301`.
- Consumer baselines at last check: Longstory PR #70 `2a89b678`, FLAM PR #85 `9ddcb514`; both
  include the five contract corrections; FLAM is waiting on the published SDK to pin.

## 9. Definition of done (inherited, restated)

The effort is done only when: the merged bridge reaches `main`; the SDK is reviewed, published, and
immutably pinned by FLAM; the outbound connector exists and is documented; a real
FLAM k3s → desktop → receiving-agent ack/reply round trip passes locally AND via a configured
remote; cursor repair / offline retry / revocation / idempotency / authorization / latency evidence
is recorded; docs match the deployed contract; and nobody claims completion from PTY write success
or local unit tests alone.
