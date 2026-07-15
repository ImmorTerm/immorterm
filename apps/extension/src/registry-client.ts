/**
 * Registry Client — unified terminal state via ~/.immorterm/registry.json
 *
 * Drop-in replacement for json-utils.ts (Phase 2A: Unified Registry).
 * All terminal metadata is stored in the global registry that the Rust
 * daemon also writes to. This eliminates the per-project restore-terminals.json.
 *
 * Uses mtime-based caching (same pattern as json-utils) to avoid redundant reads.
 * Writes are atomic: write to .tmp → fs.renameSync.
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import * as http from 'node:http';
import { HUB_PORT } from './hub-sidecar';

// ── Types ─────────────────────────────────────────────────────────

export interface TerminalEntry {
    windowId: string;
    name: string;
    screenSession?: string;
    claudeSessionId?: string;
    claudeTranscriptPath?: string;
    theme?: string;
    titleLocked?: boolean;
}

export interface ClaudeStats {
    pid: number;
    rss: number;        // Memory in KB
    cpu: number;        // CPU percentage
    startTime: number;  // Unix timestamp when Claude first detected
    runtime: number;    // Seconds running
}

// ── Registry JSON schema (matches Rust's Registry/RegistryEntry) ──

interface RegistryJson {
    sessions: RegistryEntryJson[];
}

interface ClaudeStatsJson {
    pid?: number;
    rss_kb?: number;
    cpu_percent?: number;
    start_time?: number;
    runtime_secs?: number;
}

interface RegistryEntryJson {
    pid: number;
    name: string;
    window_id: string;
    display_name: string;
    project_dir: string;
    claude_session_id?: string;
    title_locked?: boolean;
    title?: string;
    logfile?: string;
    shell?: string;
    created_at?: number;
    session_type?: string;
    session_status?: string;    // 'active' | 'shelved' | 'dead'
    shelved_at?: number;        // Unix timestamp (seconds) when session was shelved
    claude_resume_id?: string;  // Claude session ID to auto-resume on reattach (set by extension)
    ws_port?: number;
    theme?: string;
    claude_transcript_path?: string;
    claude_stats?: ClaudeStatsJson;
    /** Per-session structured log directory written by the Rust daemon —
     * `{projectDir}/.immorterm/terminals/logs/{windowId}` (or the legacy
     * date-prefixed form). The basename matches the on-disk dir name and
     * is what cleanupLogs uses to skip pruning user-shelved sessions. */
    structured_log_dir?: string;
    /** Stable owner-project root resolved at daemon spawn via
     *  `git rev-parse --git-common-dir`. For worktree-spawned daemons this
     *  is the trunk path, not the worktree path — the restore filter keys
     *  on this so worktree sessions stay visible from the parent project. */
    owner_project_dir?: string;
    /** Stable owner-project UUID from `<owner_project_dir>/.immorterm/project.json`.
     *  Survives project renames and machine moves. Restore filter prefers this
     *  over the path comparison when present on both session and current workspace. */
    owner_project_id?: string;
    /** Human-readable project name (the `name` field of project.json — WHAT
     *  display label in the identity model). Mirrored from the daemon so the
     *  modal/status bar don't re-read the file. */
    owner_project_name?: string;
    /** Worktree path when the daemon is operating inside a git worktree of
     *  its owner project. `undefined` on the trunk. Updated live by the
     *  daemon's OSC 7 cwd watcher. */
    worktree?: string;
}

// ── Module state ──────────────────────────────────────────────────

let projectPath: string = '';
let logFn: (message: string) => void = console.log;

const REGISTRY_PATH = path.join(os.homedir(), '.immorterm', 'registry.json');
const BACKUP_DIR = path.join(os.homedir(), '.immorterm', 'registry-backups');
const MAX_BACKUPS = 200;

// Shelved entries live in a SEPARATE file so that `registry.json` only
// contains sessions the user has not deliberately closed. registry.json is
// the hot, daemon-written file; shelved-registry.json is extension-owned,
// write-once-then-dormant. Keeps the primary file small + fast and
// eliminates shelved entries from every registry.json scan.
const SHELVED_REGISTRY_PATH = path.join(os.homedir(), '.immorterm', 'registry-shelved.json');

// Session status lives in a separate extension-only file that the Rust daemon
// never reads or writes. This eliminates the race condition where the daemon's
// 10-second Claude stats update (full read-modify-write of registry.json)
// overwrites session_status changes made by the extension.
const SESSION_STATUS_PATH = path.join(os.homedir(), '.immorterm', 'session-status.json');

/** Read the project UUID for `projectPath`. Canonical source is
 *  `.immorterm/project.json` (`{"id","name"}`, daemon-owned — see
 *  docs/plans/identity-model.md); legacy bare `.immorterm/project-id` is the
 *  fallback. Order matters: the daemon stamps registry entries from
 *  project.json, so reading project-id first can return a divergent UUID
 *  (minted by an old backfill) and make restore skip every session as
 *  "wrong project". Returns null when neither file exists — the daemon
 *  creates project.json on first session spawn. */
export function readProjectId(projectPath: string): string | null {
    if (!projectPath) return null;
    const dir = path.join(projectPath, '.immorterm');
    try {
        const pj = JSON.parse(fs.readFileSync(path.join(dir, 'project.json'), 'utf-8'));
        if (typeof pj.id === 'string' && pj.id.trim().length > 0) return pj.id.trim();
    } catch {
        // missing or malformed project.json — fall through to legacy file
    }
    try {
        const id = fs.readFileSync(path.join(dir, 'project-id'), 'utf-8').trim();
        return id.length > 0 ? id : null;
    } catch {
        return null;
    }
}

/** Resolve the owner project dir of a spawn dir.
 *
 * Per the user's "each workspace owns its sessions" model: ownerDir is always
 * the spawn dir itself — never walked up to a parent trunk. A worktree-spawned
 * daemon stays attributed to its worktree; the parent project does NOT pull
 * worktree sessions in. To see worktree sessions, open the worktree as its
 * own VS Code workspace.
 */
export function resolveOwnerProjectFromPath(spawnDir: string): { ownerDir: string; worktree: string | undefined } {
    return { ownerDir: spawnDir || '', worktree: undefined };
}

/** Backfill owner_project_dir + owner_project_id + worktree on legacy
 *  registry entries (active and shelved) that predate the migration.
 *  Idempotent — runs every extension activate, no-ops after the first pass.
 *  Generates the project-id file at the resolved owner_project_dir if it
 *  doesn't already exist (atomic tmp+rename).
 *
 *  Returns count of entries that were updated (across both files). */
export function backfillOwnerProjectFields(): number {
    let total = 0;
    const projectIdCache = new Map<string, string>();

    const resolveAndBackfill = (sessions: RegistryEntryJson[]): number => {
        let touched = 0;
        for (const entry of sessions) {
            if (entry.owner_project_dir && entry.owner_project_id) continue;
            if (!entry.project_dir) continue;

            // Resolve owner dir + worktree status
            if (!entry.owner_project_dir) {
                const resolved = resolveOwnerProjectFromPath(entry.project_dir);
                entry.owner_project_dir = resolved.ownerDir;
                if (resolved.worktree && entry.worktree === undefined) {
                    entry.worktree = resolved.worktree;
                }
            }

            // Read or create project-id at owner_project_dir
            if (!entry.owner_project_id) {
                const cached = projectIdCache.get(entry.owner_project_dir);
                if (cached) {
                    entry.owner_project_id = cached;
                } else {
                    let id: string | null = readProjectId(entry.owner_project_dir);
                    if (!id) {
                        // Create new project-id file (mirrors daemon's atomic write).
                        id = generateUuidV4();
                        try {
                            const dir = path.join(entry.owner_project_dir, '.immorterm');
                            fs.mkdirSync(dir, { recursive: true });
                            const file = path.join(dir, 'project-id');
                            const tmp = file + '.tmp';
                            fs.writeFileSync(tmp, id);
                            fs.renameSync(tmp, file);
                            logFn(`[backfill] generated project-id ${id} at ${file}`);
                        } catch (e) {
                            logFn(`[backfill] failed to write project-id for ${entry.owner_project_dir}: ${e}`);
                            id = null;
                        }
                    }
                    if (id) {
                        projectIdCache.set(entry.owner_project_dir, id);
                        entry.owner_project_id = id;
                    }
                }
            }
            touched++;
        }
        return touched;
    };

    // Active registry
    const live = readRegistry();
    if (live) {
        const n = resolveAndBackfill(live.sessions);
        if (n > 0) {
            writeRegistry(live);
            logFn(`[backfill] updated ${n} entries in registry.json`);
            total += n;
        }
    }

    // Shelved registry
    try {
        const shelved = readShelvedRegistry();
        const n = resolveAndBackfill(shelved.sessions);
        if (n > 0) {
            writeShelvedRegistry(shelved);
            logFn(`[backfill] updated ${n} entries in registry-shelved.json`);
            total += n;
        }
    } catch (e) {
        logFn(`[backfill] shelved registry pass failed: ${e}`);
    }

    return total;
}

function generateUuidV4(): string {
    // Minimal UUIDv4 — same shape as the daemon's inline generator.
    const { randomBytes } = require('crypto') as typeof import('crypto');
    const bytes = randomBytes(16);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    const hex = bytes.toString('hex');
    return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

interface SessionStatusEntry {
    status: string;
    shelved_at?: number;
    claude_resume_id?: string;  // Claude session ID to auto-resume on reattach
    /** Set true when shelve detected claude was NOT in the daemon's process
     * tree (user `/exit`'d before shelving). On reattach the extension
     * passes `IMMORTERM_NO_AUTO_RESUME=1` to the daemon so recall skips ALL
     * tiers and lands the user in a bare shell. Without this, the daemon's
     * recall cascade can still find a stale UUID via the claude-env mtime
     * fallback (registry/session.json/env are clean but ~/.immorterm/
     * claude-env/<uuid>.env files persist forever) and false-positive
     * resume a session the user deliberately ended. */
    claude_explicitly_exited?: boolean;
    session_order?: number;     // Sidebar position (0-based, drag-and-drop reordering)
    /** Per-session AI character override (Speak Mode). When absent, the
     * effective character falls through to the project default, then "default".
     * Stored here instead of registry.json because the Rust daemon strips
     * unknown fields from registry entries. */
    speak_mode?: string;
}

interface ActiveTerminals {
    regular?: string;   // window_id of last-focused regular terminal
    ai?: string;        // window_id of last-focused AI terminal
}

interface SessionStatusJson {
    active?: ActiveTerminals;
    sessions: Record<string, SessionStatusEntry>;
}

interface RegistryCache {
    data: RegistryJson | null;
    mtime: number;
}

let cache: RegistryCache = { data: null, mtime: 0 };

// In-memory session status map: windowId → { status, shelved_at }
// Backed by SESSION_STATUS_PATH on disk. Absence = "active" (default).
let sessionStatusMap: Map<string, SessionStatusEntry> = new Map();

// Last-focused terminal per type — persisted across restarts
let activeTerminals: ActiveTerminals = {};

// ── Registry backup infrastructure ────────────────────────────────

/** Copy current registry.json to timestamped backup before overwriting. */
function backupRegistry(): void {
    if (!fs.existsSync(REGISTRY_PATH)) return;

    try {
        if (!fs.existsSync(BACKUP_DIR)) {
            fs.mkdirSync(BACKUP_DIR, { recursive: true });
        }

        const timestamp = Math.floor(Date.now() / 1000);
        const backupPath = path.join(BACKUP_DIR, `registry.${timestamp}.json`);

        // Don't backup if a backup with this timestamp already exists (sub-second writes)
        if (!fs.existsSync(backupPath)) {
            fs.copyFileSync(REGISTRY_PATH, backupPath);
        }

        pruneBackups();
    } catch (err) {
        logFn(`[registry-backup] Failed to backup: ${err}`);
    }
}

/** Keep only the newest MAX_BACKUPS files. */
function pruneBackups(): void {
    try {
        const files = fs.readdirSync(BACKUP_DIR)
            .filter(f => f.startsWith('registry.') && f.endsWith('.json'))
            .sort(); // lexicographic sort = chronological for timestamp names

        if (files.length <= MAX_BACKUPS) return;

        const toDelete = files.slice(0, files.length - MAX_BACKUPS);
        for (const file of toDelete) {
            fs.unlinkSync(path.join(BACKUP_DIR, file));
        }
        logFn(`[registry-backup] Pruned ${toDelete.length} old backups (kept ${MAX_BACKUPS})`);
    } catch {
        // Non-critical — don't fail the write
    }
}

/** Try to load registry from the latest backup file. */
function readLatestBackup(): RegistryJson | null {
    try {
        if (!fs.existsSync(BACKUP_DIR)) return null;

        const files = fs.readdirSync(BACKUP_DIR)
            .filter(f => f.startsWith('registry.') && f.endsWith('.json'))
            .sort();

        // Try backups from newest to oldest
        for (let i = files.length - 1; i >= 0; i--) {
            try {
                const backupPath = path.join(BACKUP_DIR, files[i]);
                const data = JSON.parse(fs.readFileSync(backupPath, 'utf8')) as RegistryJson;
                if (data.sessions && Array.isArray(data.sessions)) {
                    logFn(`[registry-backup] Recovered from backup: ${files[i]} (${data.sessions.length} sessions)`);
                    return data;
                }
            } catch {
                continue; // Try next backup
            }
        }
    } catch {
        // Backup dir unreadable
    }
    return null;
}

// ── Internal helpers ──────────────────────────────────────────────

function readRegistry(): RegistryJson | null {
    // CRITICAL: If we have unflushed pending writes, return them.
    // Without this guard, a daemon write during the 100ms coalescing window
    // changes the disk mtime → cache invalidation re-reads from disk →
    // replaces our in-memory state → next writeRegistry() bases its data on
    // the daemon's version → the pending entries are LOST forever.
    // This was the root cause of "Menu" sessions disappearing on VS Code restart.
    if (pendingWriteData) {
        return pendingWriteData;
    }

    if (!fs.existsSync(REGISTRY_PATH)) {
        cache = { data: null, mtime: 0 };
        return null;
    }

    try {
        const stat = fs.statSync(REGISTRY_PATH);
        const currentMtime = stat.mtimeMs;

        if (cache.data && cache.mtime === currentMtime) {
            return cache.data;
        }

        const raw = fs.readFileSync(REGISTRY_PATH, 'utf8');
        const data = JSON.parse(raw) as RegistryJson;

        // Sanity check: must have sessions array
        if (!data.sessions || !Array.isArray(data.sessions)) {
            throw new Error('registry.json missing sessions array');
        }

        cache = { data, mtime: currentMtime };
        return data;
    } catch (err) {
        // ROOT CAUSE FIX #2: Instead of returning null (which leads to empty overwrites),
        // try to recover from the latest backup. Only return null if no backup exists.
        logFn(`[registry] Failed to read registry.json: ${err}`);
        const recovered = readLatestBackup();
        if (recovered) {
            cache = { data: recovered, mtime: 0 }; // mtime=0 forces re-read next time
            return recovered;
        }
        cache = { data: null, mtime: 0 };
        return null;
    }
}

// Write coalescing: multiple rapid writes (e.g., updating 3 terminals' stats) get
// merged into a single I/O operation. The in-memory cache is updated immediately
// (so reads always see fresh data), but disk I/O is deferred by 100ms.
let writeTimer: ReturnType<typeof setTimeout> | null = null;
let pendingWriteData: RegistryJson | null = null;

function flushRegistryToDisk(): void {
    if (!pendingWriteData) return;
    const data = pendingWriteData;
    pendingWriteData = null;

    const entryCount = data.sessions?.length ?? 0;

    try {
        const dir = path.dirname(REGISTRY_PATH);
        if (!fs.existsSync(dir)) {
            fs.mkdirSync(dir, { recursive: true });
        }

        // LAYER 1: Backup current file before overwriting
        backupRegistry();

        const tmp = REGISTRY_PATH + '.tmp';
        fs.writeFileSync(tmp, JSON.stringify(data, null, 2) + '\n');
        fs.renameSync(tmp, REGISTRY_PATH);

        // Update cache with actual mtime from disk
        try {
            const stat = fs.statSync(REGISTRY_PATH);
            cache = { data, mtime: stat.mtimeMs };
        } catch {
            cache = { data: null, mtime: 0 };
        }

        logFn(`[registry] flushed ${entryCount} entries to disk`);
    } catch (err) {
        cache = { data: null, mtime: 0 };
        logFn(`[registry] flush FAILED (${entryCount} entries): ${err}`);
    }
}

function writeRegistry(data: RegistryJson): void {
    // Update in-memory cache immediately. Note: readRegistry() returns
    // pendingWriteData directly when set, so the mtime here doesn't matter
    // for extension-internal reads — it only matters for the stale check
    // after pendingWriteData is cleared by flushRegistryToDisk().
    cache = { data, mtime: cache.mtime };
    pendingWriteData = data;

    // Coalesce disk writes — flush after 100ms of inactivity
    if (writeTimer) clearTimeout(writeTimer);
    writeTimer = setTimeout(flushRegistryToDisk, 100);
}

// ── Session status (extension-only, daemon-safe) ─────────────────

function loadSessionStatus(): void {
    try {
        if (fs.existsSync(SESSION_STATUS_PATH)) {
            const raw = JSON.parse(fs.readFileSync(SESSION_STATUS_PATH, 'utf8')) as SessionStatusJson;
            sessionStatusMap = new Map(Object.entries(raw.sessions || {}));
            activeTerminals = raw.active || {};
            logFn(`[session-status] Loaded ${sessionStatusMap.size} entries, active: ${JSON.stringify(activeTerminals)}`);
        } else {
            logFn('[session-status] No session-status.json found, starting fresh');
        }
    } catch (e) {
        logFn(`[session-status] Error loading session-status.json: ${e}`);
        sessionStatusMap = new Map();
        activeTerminals = {};
    }
}

// ── Hub-owned session-status writes ───────────────────────────────
//
// SINGLE SOURCE OF TRUTH: the hub owns `~/.immorterm/session-status.json`.
// All mutations go through these HTTP helpers; we never write directly.
// See docs/issues/2026-05-18-session-disappearance.md for context.
//
// The in-memory `sessionStatusMap` here is a READ-only cache, invalidated
// on file change via `fs.watch` (wired in `initRegistryClient`).

function hubPostJson(pathSuffix: string, body: unknown): void {
    // Fire-and-forget POST. Failures land in the hub log; we don't block
    // the UI on the write. This matches the prior debounced-disk behaviour.
    const data = JSON.stringify(body);
    const req = http.request(
        {
            host: '127.0.0.1',
            port: HUB_PORT,
            path: `/api/v1${pathSuffix}`,
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'Content-Length': Buffer.byteLength(data),
            },
            timeout: 3000,
        },
        (res) => { res.resume(); /* drain */ }
    );
    req.on('error', (err) => {
        logFn(`[session-status] hub POST ${pathSuffix} failed: ${err.message}`);
    });
    req.on('timeout', () => { req.destroy(); });
    req.write(data);
    req.end();
}

/**
 * Legacy no-op kept only so the one-time migration path
 * `migrateSessionStatusFromRegistry` can call it without us having to
 * refactor that bootstrap code. Migration writes the in-memory cache
 * directly into a brand-new file (no contention possible) so a direct
 * fs.write there is safe; everywhere else routes through the hub.
 */
function saveSessionStatusBootstrap(): void {
    try {
        const dir = path.dirname(SESSION_STATUS_PATH);
        if (!fs.existsSync(dir)) {
            fs.mkdirSync(dir, { recursive: true });
        }
        const obj: SessionStatusJson = {
            active: activeTerminals,
            sessions: Object.fromEntries(sessionStatusMap),
        };
        const tmp = SESSION_STATUS_PATH + '.tmp';
        fs.writeFileSync(tmp, JSON.stringify(obj, null, 2) + '\n');
        fs.renameSync(tmp, SESSION_STATUS_PATH);
    } catch (e) {
        logFn(`Error bootstrapping session-status.json: ${e}`);
    }
}

/** One-time migration: copy session_status from registry.json entries into session-status.json */
function migrateSessionStatusFromRegistry(): void {
    if (fs.existsSync(SESSION_STATUS_PATH)) return; // Already migrated

    const data = readRegistry();
    if (!data) return;

    let migrated = 0;
    for (const entry of data.sessions) {
        if (entry.session_status && entry.session_status !== 'active') {
            sessionStatusMap.set(entry.window_id, {
                status: entry.session_status,
                shelved_at: entry.shelved_at,
            });
            migrated++;
        }
    }

    if (migrated > 0) {
        // First-run migration only — hub may not be up yet, so write
        // directly here. After this, every other write routes through hub.
        saveSessionStatusBootstrap();
        logFn(`[session-status] Migrated ${migrated} entries from registry.json → session-status.json`);
    } else {
        // Write empty file so migration doesn't re-run
        saveSessionStatusBootstrap();
    }
}

/** Find entry by window_id within current project */
function findEntry(data: RegistryJson, windowId: string): RegistryEntryJson | undefined {
    return data.sessions.find(e =>
        e.window_id === windowId && matchesProject(e)
    );
}

// Cache the project-id file read once per init/projectPath change — filter
// callers iterate hundreds of entries and we don't want fs roundtrips per entry.
let cachedOwnProjectId: string | null | undefined = undefined; // undefined = not-yet-loaded
function ownProjectIdCached(): string | null {
    if (cachedOwnProjectId === undefined) {
        cachedOwnProjectId = readProjectId(projectPath);
    }
    return cachedOwnProjectId;
}

/** Check if a registry entry belongs to the current project.
 *  Three tiers, most-stable first:
 *    1. owner_project_id — UUID from .immorterm/project-id (rename- and machine-portable)
 *    2. owner_project_dir — immutable trunk path written at daemon spawn
 *    3. project_dir       — legacy fallback for unmigrated entries
 *  Worktree-spawned daemons collapse to the trunk via owner_project_dir
 *  and surface here without the brittle name-prefix matching the old
 *  path-only filter relied on. */
function matchesProject(entry: RegistryEntryJson): boolean {
    if (!projectPath) return true; // no project filter
    const ownId = ownProjectIdCached();
    if (ownId && entry.owner_project_id && entry.owner_project_id === ownId) return true;
    if (entry.owner_project_dir && entry.owner_project_dir === projectPath) return true;
    if (entry.project_dir === projectPath) return true;
    // Legacy fallback: endsWith match for symlinked or differently-resolved paths.
    const projectName = path.basename(projectPath);
    if (entry.project_dir && entry.project_dir.endsWith('/' + projectName)) return true;
    return false;
}

/** Map registry entry → TerminalEntry */
function toTerminalEntry(entry: RegistryEntryJson): TerminalEntry {
    return {
        windowId: entry.window_id,
        name: entry.display_name || entry.name,
        screenSession: entry.name,
        claudeSessionId: entry.claude_session_id,
        claudeTranscriptPath: entry.claude_transcript_path,
        theme: entry.theme,
        titleLocked: entry.title_locked,
    };
}

// ── Migration from restore-terminals.json ─────────────────────────

function extractWindowIdFromCommands(commands: string[] | undefined): string | null {
    if (!commands) return null;
    for (const cmd of commands) {
        const match = cmd.match(/screen-auto\s+(\d+-\w+)/);
        if (match) return match[1];
    }
    return null;
}

function migrateRestoreTerminalsJson(): void {
    // Check both legacy locations: activation.ts migrated .vscode/ → .immorterm/
    const vscodeJsonPath = path.join(projectPath, '.vscode', 'restore-terminals.json');
    const immortermJsonPath = path.join(projectPath, '.immorterm', 'restore-terminals.json');
    const jsonPath = fs.existsSync(vscodeJsonPath) ? vscodeJsonPath
        : fs.existsSync(immortermJsonPath) ? immortermJsonPath
        : null;
    if (!jsonPath) return;

    logFn('[registry-client] Migrating restore-terminals.json → registry.json');

    try {
        const oldConfig = JSON.parse(fs.readFileSync(jsonPath, 'utf-8'));
        let data = readRegistry();
        if (!data) {
            data = { sessions: [] };
        }

        // First: deduplicate existing entries for this project (clean up junk)
        const seenWindowIds = new Set<string>();
        const beforeCount = data.sessions.length;
        data.sessions = data.sessions.filter(e => {
            if (!matchesProject(e)) return true; // keep other projects
            if (seenWindowIds.has(e.window_id)) return false; // remove duplicate
            seenWindowIds.add(e.window_id);
            return true;
        });
        const deduped = beforeCount - data.sessions.length;
        if (deduped > 0) {
            logFn(`[registry-client] Removed ${deduped} duplicate entries during migration`);
        }

        let migrated = 0;
        for (const tab of oldConfig.terminals || []) {
            for (const split of (tab.splitTerminals || [])) {
                const windowId = split.windowId || extractWindowIdFromCommands(split.commands);
                if (!windowId || !split.name) continue;

                // Update existing entry or create new one
                const existing = data.sessions.find(e => e.window_id === windowId);
                if (existing) {
                    // Update display_name from restore-terminals.json (better source of truth)
                    if (existing.display_name !== split.name) {
                        logFn(`[registry-client] Updated display_name for ${windowId}: "${existing.display_name}" → "${split.name}"`);
                        existing.display_name = split.name;
                    }
                    if (split.claudeSessionId) existing.claude_session_id = split.claudeSessionId;
                    if (split.titleLocked !== undefined) existing.title_locked = split.titleLocked;
                    if (split.theme) existing.theme = split.theme;
                    if (split.claudeTranscriptPath) existing.claude_transcript_path = split.claudeTranscriptPath;
                    migrated++;
                    continue;
                }

                data.sessions.push({
                    pid: 0, // unknown — daemon will update on next start
                    name: '',
                    window_id: windowId,
                    display_name: split.name,
                    project_dir: projectPath,
                    claude_session_id: split.claudeSessionId,
                    title_locked: split.titleLocked ?? false,
                    title: '',
                    shell: '',
                    created_at: 0,
                    theme: split.theme,
                    claude_transcript_path: split.claudeTranscriptPath,
                });
                migrated++;
            }
        }

        if (migrated > 0 || deduped > 0) {
            writeRegistry(data);
            logFn(`[registry-client] Migrated ${migrated} entries to registry.json`);
        }

        // Rename old file (don't delete — keep as backup)
        fs.renameSync(jsonPath, jsonPath + '.bak');
        logFn('[registry-client] Renamed restore-terminals.json → .bak');
    } catch (error) {
        logFn(`[registry-client] Migration error: ${error}`);
    }
}

// ── Public API ────────────────────────────────────────────────────

let sessionStatusWatcher: fs.FSWatcher | null = null;

export function initRegistryClient(projectDir: string, logger: (message: string) => void): void {
    projectPath = projectDir;
    cachedOwnProjectId = undefined; // invalidate; will lazy-load on next matchesProject call
    logFn = logger;
    cache = { data: null, mtime: 0 };

    // Load session status from extension-only file (daemon-safe)
    loadSessionStatus();
    migrateSessionStatusFromRegistry();

    // Watch session-status.json for hub-driven changes — the hub is the
    // single writer, but the extension keeps a read cache. Without this
    // watcher, hub updates from the Tauri app or another VS Code window
    // would be invisible to this process until restart. See
    // docs/issues/2026-05-18-session-disappearance.md.
    if (sessionStatusWatcher) {
        try { sessionStatusWatcher.close(); } catch { /* ignore */ }
        sessionStatusWatcher = null;
    }
    try {
        if (fs.existsSync(SESSION_STATUS_PATH)) {
            sessionStatusWatcher = fs.watch(SESSION_STATUS_PATH, { persistent: false }, () => {
                // Debounce reloads — atomic tmp+rename writes can fire two events.
                if (sessionStatusReloadTimer) clearTimeout(sessionStatusReloadTimer);
                sessionStatusReloadTimer = setTimeout(() => {
                    loadSessionStatus();
                    sessionStatusReloadTimer = null;
                }, 50);
            });
        }
    } catch (e) {
        logFn(`[session-status] fs.watch failed: ${e}`);
    }

    // Migrate old restore-terminals.json if it exists
    migrateRestoreTerminalsJson();
}

let sessionStatusReloadTimer: ReturnType<typeof setTimeout> | null = null;

export function invalidateRegistryCache(): void {
    cache = { data: null, mtime: 0 };
}

/** Flush any pending coalesced writes to disk immediately. Call during deactivation. */
export function flushRegistryWrites(): void {
    if (writeTimer) { clearTimeout(writeTimer); writeTimer = null; }
    flushRegistryToDisk();
}

// ── Terminal CRUD ─────────────────────────────────────────────────

export function getAllTerminalsFromRegistry(): TerminalEntry[] {
    const data = readRegistry();
    if (!data) return [];

    const allSessions = data.sessions;
    const afterProject = allSessions.filter(matchesProject);
    const afterAi = afterProject.filter(e => e.session_type !== 'ai');
    const afterShelved = afterAi.filter(e => sessionStatusMap.get(e.window_id)?.status !== 'shelved');

    // Diagnostic: log each filter stage so we can see what's being dropped
    const dropped = {
        byProject: allSessions.filter(e => !matchesProject(e)).map(e => `${e.window_id}("${e.display_name}")`),
        byAi: afterProject.filter(e => e.session_type === 'ai').map(e => `${e.window_id}("${e.display_name}")`),
        byShelved: afterAi.filter(e => sessionStatusMap.get(e.window_id)?.status === 'shelved').map(e => `${e.window_id}("${e.display_name}")`),
    };
    logFn(`[getAllTerminals] total=${allSessions.length} → project=${afterProject.length} → notAi=${afterAi.length} → notShelved=${afterShelved.length} | dropped: project=[${dropped.byProject.join(', ')}] ai=[${dropped.byAi.join(', ')}] shelved=[${dropped.byShelved.join(', ')}]`);

    return afterShelved.map(toTerminalEntry);
}

/**
 * Get ALL active window IDs for this project (including AI sessions).
 * Used by cleanup to determine which session directories are still active.
 * Unlike getAllTerminalsFromRegistry(), this does NOT filter out AI sessions
 * because cleanup needs to know about ALL live sessions to avoid archiving them.
 */
/**
 * Returns window IDs for sessions that have an active daemon (pid > 0).
 *
 * Screen sessions register with pid=0 (the C binary doesn't self-register) —
 * those are already covered by screenCommands.listProjectSessions() in cleanup.ts.
 * AI daemon sessions always register with their actual PID.
 *
 * This ensures cleanup can archive directories for dead screen sessions while
 * preserving AI daemon directories that are still being written to.
 */
export function getAllActiveWindowIds(): Set<string> {
    const data = readRegistry();
    if (!data) return new Set();

    return new Set(
        data.sessions
            .filter(matchesProject)
            .filter(e => sessionStatusMap.get(e.window_id)?.status !== 'shelved')
            .filter(e => e.pid > 0)
            .map(e => e.window_id)
            .filter(Boolean)
    );
}

export function addTerminalToRegistry(windowId: string, displayName: string): boolean {
    try {
        let data = readRegistry();
        if (!data) {
            data = { sessions: [] };
        }

        // Check if already exists
        if (data.sessions.some(e => e.window_id === windowId)) {
            logFn(`Terminal ${windowId} already exists in registry, skipping add`);
            return false;
        }

        data.sessions.push({
            pid: 0,
            name: '',
            window_id: windowId,
            display_name: displayName,
            project_dir: projectPath,
            title_locked: false,
            title: '',
            shell: '',
            created_at: Math.floor(Date.now() / 1000),
        });

        writeRegistry(data);
        logFn(`Added terminal ${windowId} ("${displayName}") to registry.json`);
        return true;
    } catch (error) {
        logFn(`Error adding terminal to registry: ${error}`);
        return false;
    }
}

export function removeTerminalFromRegistry(windowId: string): boolean {
    // Only touches registry.json. Session-status (which holds session_order
    // and the shelved/active flag) is owned by `removeSessionStatus()`.
    // Callers that want both — permanent-delete flows like forget.ts — must
    // call both. Callers that only want to drop the registry advertisement
    // — the reconciler — must NOT touch status. Prior to this split, the
    // reconciler was silently nuking the user's sidebar order whenever it
    // archived a stale dir during cold-boot restore races.
    const data = readRegistry();
    if (!data) return false;

    try {
        const before = data.sessions.length;
        data.sessions = data.sessions.filter(e => e.window_id !== windowId);

        if (data.sessions.length !== before) {
            writeRegistry(data);
            logFn(`Removed terminal ${windowId} from registry.json`);
            return true;
        }
        return false;
    } catch (error) {
        logFn(`Error removing terminal from registry: ${error}`);
        return false;
    }
}

/** Drop the session-status entry (status + session_order + speak_mode etc.).
 *  Separate from `removeTerminalFromRegistry` so the reconciler can clean up
 *  registry.json without destroying the user's persisted sidebar order. */
export function removeSessionStatus(windowId: string): boolean {
    if (!sessionStatusMap.has(windowId)) return false;
    sessionStatusMap.delete(windowId);
    hubPostJson('/registry/session-status/remove', { window_id: windowId });
    logFn(`Removed session-status entry for ${windowId} (via hub)`);
    return true;
}

export function clearAllTerminalsFromRegistry(): boolean {
    try {
        let data = readRegistry();
        if (!data) {
            data = { sessions: [] };
        }

        // ROOT CAUSE FIX #4: Explicit backup before destructive clearAll
        const removedCount = data.sessions.filter(matchesProject).length;
        logFn(`[registry] clearAll: removing ${removedCount} entries for project "${projectPath}" (${data.sessions.length} total)`);
        backupRegistry();

        // Only remove entries for the current project — preserve other projects
        data.sessions = data.sessions.filter(e => !matchesProject(e));
        writeRegistry(data);
        logFn(`Cleared all terminals for project from registry.json`);
        return true;
    } catch (error) {
        logFn(`Error clearing terminals from registry: ${error}`);
        return false;
    }
}

// ── Property updates ──────────────────────────────────────────────

export function updateRegistryNameAndCommand(windowId: string, newName: string): void {
    const data = readRegistry();
    if (!data) {
        logFn('registry.json not found');
        return;
    }

    try {
        const entry = findEntry(data, windowId);
        if (entry && entry.display_name !== newName) {
            logFn(`Registry update "${entry.display_name}" → "${newName}"`);
            entry.display_name = newName;
            writeRegistry(data);
        }
    } catch (error) {
        logFn(`Error updating registry name: ${error}`);
    }
}

export function getRegistryTheme(windowId: string): string | undefined {
    const data = readRegistry();
    if (!data) return undefined;
    const entry = findEntry(data, windowId);
    return entry?.theme;
}

export function updateRegistryTheme(windowId: string, theme: string | undefined): void {
    const data = readRegistry();
    if (!data) {
        logFn('registry.json not found');
        return;
    }

    try {
        const entry = findEntry(data, windowId);
        if (!entry) return;

        if (theme) {
            if (entry.theme !== theme) {
                logFn(`Registry theme update for ${windowId}: "${entry.theme || 'none'}" → "${theme}"`);
                entry.theme = theme;
                writeRegistry(data);
            }
        } else {
            if (entry.theme) {
                logFn(`Registry theme cleared for ${windowId}`);
                delete entry.theme;
                writeRegistry(data);
            }
        }
    } catch (error) {
        logFn(`Error updating registry theme: ${error}`);
    }
}

export function updateRegistryTitleLocked(windowId: string, locked: boolean): void {
    const data = readRegistry();
    if (!data) {
        logFn('registry.json not found');
        return;
    }

    try {
        const entry = findEntry(data, windowId);
        if (!entry) return;

        if (entry.title_locked !== locked) {
            logFn(`Registry titleLocked ${locked ? 'set' : 'cleared'} for ${windowId}`);
            entry.title_locked = locked;
            writeRegistry(data);
        }
    } catch (error) {
        logFn(`Error updating registry titleLocked: ${error}`);
    }
}

export function updateRegistrySessionOrder(windowIds: string[]): void {
    // Hub owns session-status.json. Update in-memory cache for fast reads,
    // then ship the canonical write to the hub which atomically persists.
    try {
        for (let i = 0; i < windowIds.length; i++) {
            const existing = sessionStatusMap.get(windowIds[i]);
            if (existing) {
                existing.session_order = i;
            } else {
                sessionStatusMap.set(windowIds[i], { status: 'active', session_order: i });
            }
        }
        hubPostJson('/registry/reorder', { window_ids: windowIds });
        logFn(`Session order updated for ${windowIds.length} entries (via hub)`);
    } catch (error) {
        logFn(`Error updating session order: ${error}`);
    }
}

/** Get session order from the extension-only status file (daemon-safe) */
export function getSessionOrder(windowId: string): number | undefined {
    return sessionStatusMap.get(windowId)?.session_order;
}

/** Set (or clear) the per-session Speak Mode override.
 * Stored in session-status.json because the Rust daemon strips unknown
 * fields from registry.json entries.
 *
 * Semantics:
 * - `undefined` / `""` / `"default"` → clear the override (cascade falls
 *   through to project default, then the silent "default" character).
 * - any other character ID → store as the active override.
 *
 * LLMs tend to keep a persona (like caveman) for a few turns after the
 * override is removed, because prior turns anchor the style. To yank the
 * model out of that pattern, the caller should ALSO write a one-shot reset
 * marker via `markSpeakModeReset(windowId)`; the hook consumes it on the
 * next prompt and emits a single "drop any persona" instruction, then
 * deletes the marker so future prompts don't pay the token cost. */
export function updateSessionSpeakMode(windowId: string, speakMode: string | undefined): void {
    try {
        const existing = sessionStatusMap.get(windowId);
        const cleared = speakMode === undefined || speakMode === '' || speakMode === 'default';
        if (existing) {
            if (cleared) {
                if (existing.speak_mode === undefined) return;
                delete existing.speak_mode;
            } else {
                if (existing.speak_mode === speakMode) return;
                existing.speak_mode = speakMode;
            }
        } else {
            if (cleared) return;
            sessionStatusMap.set(windowId, { status: 'active', speak_mode: speakMode });
        }
        hubPostJson('/registry/speak-mode', {
            window_id: windowId,
            mode: cleared ? null : speakMode,
        });
        logFn(`Session speak_mode ${cleared ? 'cleared' : `set to '${speakMode}'`} for ${windowId} (via hub)`);
    } catch (error) {
        logFn(`Error updating session speak_mode: ${error}`);
    }
}

/** Get per-session Speak Mode override, or undefined if no override set */
export function getSessionSpeakMode(windowId: string): string | undefined {
    return sessionStatusMap.get(windowId)?.speak_mode;
}

/** Drop a one-shot "reset persona" marker for this session. The next
 * UserPromptSubmit hook invocation reads the marker, injects a single
 * "drop any persona" instruction, and deletes the marker — so the cost
 * is paid exactly once per transition, not every prompt. Used when a
 * user clears caveman override mid-conversation to yank the model out
 * of the style pattern accumulated in prior turns.
 *
 * Marker files live in ~/.immorterm/pending-resets/<windowId> (empty;
 * file presence is the signal). The hook atomically deletes on consume. */
export function markSpeakModeReset(windowId: string): void {
    try {
        const resetDir = path.join(os.homedir(), '.immorterm', 'pending-resets');
        fs.mkdirSync(resetDir, { recursive: true });
        const markerPath = path.join(resetDir, windowId);
        fs.writeFileSync(markerPath, '', 'utf-8');
        logFn(`Speak mode reset marker written for ${windowId}`);
    } catch (error) {
        logFn(`Error writing speak_mode reset marker: ${error}`);
    }
}

// ── Active terminal tracking (focus persistence) ────────────────

let activeWriteTimer: NodeJS.Timeout | null = null;

/** Update which terminal was last focused, by type. Debounced to disk. */
export function setActiveTerminal(type: 'regular' | 'ai', windowId: string): void {
    activeTerminals[type] = windowId;

    // Debounce hub writes — focus changes rapidly during tab switching.
    // The hub is the single writer to session-status.json; we just POST.
    if (activeWriteTimer) clearTimeout(activeWriteTimer);
    activeWriteTimer = setTimeout(() => {
        hubPostJson('/registry/active-terminal', {
            terminal_type: type,
            window_id: windowId,
        });
        activeWriteTimer = null;
    }, 500);
}

/** Get the last-focused terminal windowId for a given type */
export function getActiveTerminal(type: 'regular' | 'ai'): string | undefined {
    return activeTerminals[type];
}

// ── Claude tracking ───────────────────────────────────────────────

export function updateClaudeSessionId(windowId: string, sessionId: string): boolean {
    const data = readRegistry();
    if (!data) return false;

    try {
        const entry = findEntry(data, windowId);
        if (entry && entry.claude_session_id !== sessionId) {
            entry.claude_session_id = sessionId;
            writeRegistry(data);
            logFn(`[claude-sync] Set claudeSessionId for ${windowId}: ${sessionId.slice(0, 8)}...`);
            return true;
        }
        return false;
    } catch (error) {
        logFn(`Error updating claudeSessionId: ${error}`);
        return false;
    }
}

export function removeClaudeSessionId(windowId: string): boolean {
    const data = readRegistry();
    if (!data) return false;

    try {
        const entry = findEntry(data, windowId);
        if (entry && entry.claude_session_id) {
            delete entry.claude_session_id;
            delete entry.claude_transcript_path;
            writeRegistry(data);
            logFn(`[claude-sync] Removed claudeSessionId for ${windowId} (Claude exited)`);
            return true;
        }
        return false;
    } catch (error) {
        logFn(`Error removing claudeSessionId: ${error}`);
        return false;
    }
}

export function getCurrentClaudeSessionId(windowId: string): string | null {
    const data = readRegistry();
    if (!data) return null;

    const entry = findEntry(data, windowId);
    return entry?.claude_session_id || null;
}

export function updateClaudeTranscriptPath(windowId: string, transcriptPath: string): boolean {
    const data = readRegistry();
    if (!data) return false;

    try {
        const entry = findEntry(data, windowId);
        if (entry && entry.claude_transcript_path !== transcriptPath) {
            entry.claude_transcript_path = transcriptPath;
            writeRegistry(data);
            logFn(`[claude-sync] Set claudeTranscriptPath for ${windowId}`);
            return true;
        }
        return false;
    } catch (error) {
        logFn(`Error updating claudeTranscriptPath: ${error}`);
        return false;
    }
}

export function getClaudeTranscriptPath(windowId: string): string | null {
    const data = readRegistry();
    if (!data) return null;

    const entry = findEntry(data, windowId);
    return entry?.claude_transcript_path || null;
}

export function updateClaudeStats(windowId: string, stats: ClaudeStats): boolean {
    const data = readRegistry();
    if (!data) return false;

    try {
        const entry = findEntry(data, windowId);
        if (!entry) return false;

        const oldStats = entry.claude_stats;
        const shouldLog = !oldStats ||
            Math.abs((oldStats.rss_kb || 0) - stats.rss) > 10240 ||
            Math.abs((oldStats.cpu_percent || 0) - stats.cpu) > 5;

        entry.claude_stats = {
            pid: stats.pid,
            rss_kb: stats.rss,
            cpu_percent: stats.cpu,
            start_time: stats.startTime,
            runtime_secs: stats.runtime,
        };
        writeRegistry(data);

        if (shouldLog) {
            const memMB = Math.round(stats.rss / 1024);
            logFn(`[claude-stats] ${windowId}: ${memMB}MB, ${stats.cpu.toFixed(1)}% CPU, ${stats.runtime}s`);
        }
        return true;
    } catch (error) {
        logFn(`Error updating claudeStats: ${error}`);
        return false;
    }
}

export function removeClaudeStats(windowId: string): boolean {
    const data = readRegistry();
    if (!data) return false;

    try {
        const entry = findEntry(data, windowId);
        if (entry && entry.claude_stats) {
            delete entry.claude_stats;
            writeRegistry(data);
            logFn(`[claude-stats] Removed stats for ${windowId} (Claude exited)`);
            return true;
        }
        return false;
    } catch (error) {
        logFn(`Error removing claudeStats: ${error}`);
        return false;
    }
}

export function getClaudeStats(windowId: string): ClaudeStats | null {
    const data = readRegistry();
    if (!data) return null;

    const entry = findEntry(data, windowId);
    if (!entry?.claude_stats) return null;

    const s = entry.claude_stats;
    return {
        pid: s.pid || 0,
        rss: s.rss_kb || 0,
        cpu: s.cpu_percent || 0,
        startTime: s.start_time || 0,
        runtime: s.runtime_secs || 0,
    };
}

// ── Session shelving ─────────────────────────────────────────────

export function updateSessionStatus(windowId: string, status: string, shelvedAt?: number, claudeResumeId?: string, claudeExplicitlyExited?: boolean): boolean {
    try {
        if (status === 'shelved') {
            sessionStatusMap.set(windowId, {
                status: 'shelved',
                shelved_at: shelvedAt,
                claude_resume_id: claudeResumeId,
                ...(claudeExplicitlyExited ? { claude_explicitly_exited: true } : {}),
            });
        } else {
            // 'active' or 'dead' — remove from map (absence = active/default)
            sessionStatusMap.delete(windowId);
        }
        hubPostJson('/registry/session-status', {
            window_id: windowId,
            status,
            shelved_at: shelvedAt,
            claude_resume_id: claudeResumeId,
            claude_explicitly_exited: claudeExplicitlyExited,
        });
        logFn(`Session status updated for ${windowId}: ${status}${claudeExplicitlyExited ? ' (claude_explicitly_exited)' : ''} (via hub)`);
        return true;
    } catch (error) {
        logFn(`Error updating session status: ${error}`);
        return false;
    }
}

/** Read the `claude_explicitly_exited` flag set at shelve time when claude
 * wasn't in the daemon's process tree. The reattach path uses this to
 * pass `IMMORTERM_NO_AUTO_RESUME=1` so the daemon's recall skips all tiers. */
export function getClaudeExplicitlyExited(windowId: string): boolean {
    return !!sessionStatusMap.get(windowId)?.claude_explicitly_exited;
}

// ── Shelved registry (separate file) ──────────────────────────────

function readShelvedRegistry(): RegistryJson {
    if (!fs.existsSync(SHELVED_REGISTRY_PATH)) {
        return { sessions: [] };
    }
    try {
        const raw = fs.readFileSync(SHELVED_REGISTRY_PATH, 'utf8');
        const data = JSON.parse(raw) as RegistryJson;
        if (!data.sessions || !Array.isArray(data.sessions)) {
            return { sessions: [] };
        }
        return data;
    } catch (err) {
        logFn(`[registry-shelved] Failed to read: ${err}`);
        return { sessions: [] };
    }
}

function writeShelvedRegistry(data: RegistryJson): void {
    try {
        const dir = path.dirname(SHELVED_REGISTRY_PATH);
        if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
        const tmp = SHELVED_REGISTRY_PATH + '.tmp';
        fs.writeFileSync(tmp, JSON.stringify(data, null, 2) + '\n');
        fs.renameSync(tmp, SHELVED_REGISTRY_PATH);
    } catch (err) {
        logFn(`[registry-shelved] Failed to write: ${err}`);
    }
}

/**
 * Move a registry entry from registry.json to registry-shelved.json.
 * Called when the user deliberately closes (shelves) a session. After this
 * call, registry.json no longer lists the entry; only readShelvedRegistry()
 * or getShelvedSessions() will surface it.
 *
 * Idempotent. Returns true if the entry moved (or was already shelved).
 */
export function moveToShelvedRegistry(windowId: string): boolean {
    const live = readRegistry();
    if (!live) return false;

    const idx = live.sessions.findIndex(e => e.window_id === windowId);
    if (idx < 0) {
        // Already not in live; treat as success if shelved file has it.
        const shelved = readShelvedRegistry();
        return shelved.sessions.some(e => e.window_id === windowId);
    }

    const entry = live.sessions[idx];
    live.sessions.splice(idx, 1);

    // Upsert into shelved file (replace any stale entry with same window_id).
    const shelved = readShelvedRegistry();
    shelved.sessions = shelved.sessions.filter(e => e.window_id !== windowId);
    shelved.sessions.push({
        ...entry,
        session_status: 'shelved',
        shelved_at: Math.floor(Date.now() / 1000),
    });

    writeRegistry(live);
    writeShelvedRegistry(shelved);
    return true;
}

/**
 * Reverse of moveToShelvedRegistry: restore a shelved entry back into
 * registry.json as an active session. Called on reattach/unshelve.
 * Returns the restored entry (with shelved metadata stripped), or null
 * if not found.
 */
export function moveFromShelvedRegistry(windowId: string): RegistryEntryJson | null {
    const shelved = readShelvedRegistry();
    const idx = shelved.sessions.findIndex(e => e.window_id === windowId);
    if (idx < 0) return null;

    const entry = { ...shelved.sessions[idx] };
    shelved.sessions.splice(idx, 1);
    delete entry.session_status;
    delete entry.shelved_at;

    const live = readRegistry() ?? { sessions: [] };
    live.sessions = live.sessions.filter(e => e.window_id !== windowId);
    live.sessions.push(entry);

    writeShelvedRegistry(shelved);
    writeRegistry(live);
    return entry;
}

export function getShelvedSessions(): RegistryEntryJson[] {
    // Primary source: registry-shelved.json (entries deliberately shelved).
    const shelvedData = readShelvedRegistry();
    const fromShelvedFile = shelvedData.sessions
        .filter(matchesProject)
        .map(e => {
            const statusEntry = sessionStatusMap.get(e.window_id);
            return statusEntry
                ? {
                    ...e,
                    ...(statusEntry.shelved_at && { shelved_at: statusEntry.shelved_at }),
                    ...(statusEntry.claude_resume_id && { claude_resume_id: statusEntry.claude_resume_id }),
                }
                : e;
        });

    // Fallback: any entry still in registry.json that session-status.json
    // marks as shelved (pre-migration). Included so the UI doesn't lose
    // shelved sessions during the transition; a subsequent write path
    // (e.g. reattach) will move them into the shelved file naturally.
    const live = readRegistry();
    const seen = new Set(fromShelvedFile.map(e => e.window_id));
    const fromRegistryLegacy = !live ? [] : live.sessions
        .filter(e => !seen.has(e.window_id))
        .filter(e => sessionStatusMap.get(e.window_id)?.status === 'shelved')
        .filter(matchesProject)
        .map(e => {
            const statusEntry = sessionStatusMap.get(e.window_id);
            if (!statusEntry) return e;
            return {
                ...e,
                ...(statusEntry.shelved_at && { shelved_at: statusEntry.shelved_at }),
                ...(statusEntry.claude_resume_id && { claude_resume_id: statusEntry.claude_resume_id }),
            };
        });

    return [...fromShelvedFile, ...fromRegistryLegacy];
}

/** Remove an entry from registry-shelved.json. Used when a shelved session
 * is permanently deleted. Idempotent. */
export function removeShelvedRegistryEntry(windowId: string): boolean {
    const data = readShelvedRegistry();
    const idx = data.sessions.findIndex(e => e.window_id === windowId);
    if (idx < 0) return false;
    data.sessions.splice(idx, 1);
    writeShelvedRegistry(data);
    return true;
}

/**
 * Sweep registry of stub-zombie entries — `pid=0, name=''` rows left behind by
 * `reconcileTerminal()` when a regular VS Code terminal was tracked but no
 * daemon ever updated the stub. These accumulate forever because nothing
 * triggers their removal (regular terminals have no daemon writer).
 *
 * Drops entries matching ALL of:
 *   - pid === 0
 *   - name is empty
 *   - age > 7 days (safety: don't nuke a stub that just got created)
 *
 * Project-dir agnostic — sweeps everything (global stub cleanup).
 * Returns the number of entries removed. Idempotent (no stubs → 0).
 */
export function sweepStubZombies(maxAgeDays: number = 7): number {
    const data = readRegistry();
    if (!data) return 0;
    const cutoffSec = Math.floor(Date.now() / 1000) - (maxAgeDays * 86400);
    const before = data.sessions.length;
    data.sessions = data.sessions.filter(entry => {
        const isStub = entry.pid === 0
            && (!entry.name || entry.name.length === 0);
        if (!isStub) return true;
        const age = (entry.created_at ?? 0);
        if (age === 0 || age > cutoffSec) return true;  // recent stub, let it ripen
        return false;
    });
    const removed = before - data.sessions.length;
    if (removed > 0) {
        writeRegistry(data);
        logFn(`[registry] Swept ${removed} stub-zombie entries (pid=0, empty name, age>${maxAgeDays}d)`);
    }
    return removed;
}

/**
 * One-time migration: move any legacy shelved entries still in registry.json
 * (session-status.json says "shelved") into registry-shelved.json. Idempotent.
 * Call once at extension activation. Returns the number of entries migrated.
 */
export function migrateShelvedOutOfRegistry(): number {
    const live = readRegistry();
    if (!live) return 0;
    const toMove: string[] = [];
    for (const entry of live.sessions) {
        if (sessionStatusMap.get(entry.window_id)?.status === 'shelved') {
            toMove.push(entry.window_id);
        }
    }
    if (toMove.length === 0) return 0;
    for (const wid of toMove) {
        moveToShelvedRegistry(wid);
    }
    logFn(`[registry-shelved] Migrated ${toMove.length} shelved entries out of registry.json`);
    return toMove.length;
}

/**
 * Drop shelved registry entries whose backing session dir no longer exists
 * on disk (neither in `logs/` nor in `logs/archive/`). These orphans are
 * leftovers from the period when `cleanupLogs()` pruned archive subdirs to
 * stay under `maxLogSizeMb` without informing `registry-shelved.json`.
 *
 * Idempotent. Call from activate() AFTER migrateShelvedOutOfRegistry() so
 * legacy entries land in the shelved file before this sweep checks them.
 *
 * Returns the number of orphan entries removed.
 */
export function sweepOrphanShelvedEntries(): number {
    const shelved = readShelvedRegistry();
    if (shelved.sessions.length === 0) return 0;

    const orphanWids: string[] = [];
    for (const entry of shelved.sessions) {
        const dir = entry.structured_log_dir;
        if (!dir) continue; // No path to check — leave alone.
        // Check the registered path AND the archive equivalent. The dir name
        // basename is preserved across archive (logs/{name} ↔ archive/{name}).
        const archived = path.join(path.dirname(dir), 'archive', path.basename(dir));
        try {
            if (fs.existsSync(dir) || fs.existsSync(archived)) continue;
        } catch {
            continue; // FS error — don't risk removing.
        }
        orphanWids.push(entry.window_id);
    }
    if (orphanWids.length === 0) return 0;

    shelved.sessions = shelved.sessions.filter(e => !orphanWids.includes(e.window_id));
    writeShelvedRegistry(shelved);
    // Also strip session-status entries so the UI doesn't keep showing them.
    for (const wid of orphanWids) {
        sessionStatusMap.delete(wid);
        hubPostJson('/registry/session-status/remove', { window_id: wid });
    }
    logFn(`[registry-shelved] Swept ${orphanWids.length} orphan shelved entries (dirs missing on disk): ${orphanWids.join(', ')}`);
    return orphanWids.length;
}

/** Get session status from the extension-only status file (daemon-safe) */
export function getSessionStatus(windowId: string): string | undefined {
    return sessionStatusMap.get(windowId)?.status;
}

/** Get Claude resume ID from session-status (set during shelve, consumed during reattach) */
export function getClaudeResumeId(windowId: string): string | undefined {
    return sessionStatusMap.get(windowId)?.claude_resume_id;
}

/** Get a raw registry entry by window ID (for reattach flow) */
export function getRegistryEntryByWindowId(windowId: string): RegistryEntryJson | undefined {
    const data = readRegistry();
    if (!data) return undefined;
    return data.sessions.find(e => e.window_id === windowId);
}

export function deduplicateSessionIds(): number {
    const data = readRegistry();
    if (!data) return 0;

    // Group entries by claude_session_id
    const groups = new Map<string, Array<{ entry: RegistryEntryJson; startTime: number }>>();

    for (const entry of data.sessions) {
        const sid = entry.claude_session_id;
        if (!sid) continue;
        if (!groups.has(sid)) groups.set(sid, []);
        groups.get(sid)!.push({
            entry,
            startTime: entry.claude_stats?.start_time ?? 0,
        });
    }

    let cleared = 0;
    for (const [sessionId, entries] of groups) {
        if (entries.length <= 1) continue;

        // Sort descending by startTime — first entry is the keeper
        entries.sort((a, b) => b.startTime - a.startTime);

        for (let i = 1; i < entries.length; i++) {
            const wid = entries[i].entry.window_id || '?';
            logFn(`[dedup] Clearing duplicate claudeSessionId ${sessionId.slice(0, 8)}... from ${wid}`);
            delete entries[i].entry.claude_session_id;
            cleared++;
        }
    }

    if (cleared > 0) {
        writeRegistry(data);
        logFn(`[dedup] Cleared ${cleared} duplicate session ID(s)`);
    }
    return cleared;
}

/**
 * Batch-update all Claude session state in a single read-modify-write cycle.
 * Replaces N individual read/write cycles per sync (was 3N+1 with N sessions).
 */
export interface ClaudeSyncUpdate {
    windowId: string;
    active: boolean;
    sessionId?: string;
    transcriptPath?: string;
    stats?: ClaudeStats;
}

export function batchSyncClaudeState(updates: ClaudeSyncUpdate[]): void {
    const data = readRegistry();
    if (!data) return;

    let dirty = false;

    for (const update of updates) {
        const entry = findEntry(data, update.windowId);
        if (!entry) continue;

        if (update.active && update.sessionId) {
            // Active Claude session — set all fields
            if (entry.claude_session_id !== update.sessionId) {
                entry.claude_session_id = update.sessionId;
                dirty = true;
            }
            if (update.transcriptPath && entry.claude_transcript_path !== update.transcriptPath) {
                entry.claude_transcript_path = update.transcriptPath;
                dirty = true;
            }
            if (update.stats) {
                entry.claude_stats = {
                    pid: update.stats.pid,
                    rss_kb: update.stats.rss,
                    cpu_percent: update.stats.cpu,
                    start_time: update.stats.startTime,
                    runtime_secs: update.stats.runtime,
                };
                dirty = true;
            }
        } else {
            // Claude not active — clean up stale data
            if (entry.claude_session_id) {
                delete entry.claude_session_id;
                delete entry.claude_transcript_path;
                dirty = true;
            }
            if (entry.claude_stats) {
                delete entry.claude_stats;
                dirty = true;
            }
        }
    }

    // Inline deduplication (same logic, avoids second read/write cycle)
    const groups = new Map<string, Array<{ entry: RegistryEntryJson; startTime: number }>>();
    for (const entry of data.sessions) {
        const sid = entry.claude_session_id;
        if (!sid) continue;
        if (!groups.has(sid)) groups.set(sid, []);
        groups.get(sid)!.push({ entry, startTime: entry.claude_stats?.start_time ?? 0 });
    }
    for (const [, entries] of groups) {
        if (entries.length <= 1) continue;
        entries.sort((a, b) => b.startTime - a.startTime);
        for (let i = 1; i < entries.length; i++) {
            delete entries[i].entry.claude_session_id;
            dirty = true;
        }
    }

    if (dirty) {
        writeRegistry(data);
    }
}
