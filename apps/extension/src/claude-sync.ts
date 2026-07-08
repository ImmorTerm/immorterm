/**
 * Claude Session Sync -- Push-based architecture
 *
 * Zero child processes. All state comes from SessionManager adapters:
 * - AI sessions: WebSocket push from Rust daemon
 * - C sessions: fs.watchFile on project-scoped context files
 */

import * as fs from 'fs';
import * as path from 'path';
import * as http from 'http';
import { getMemoryPort } from './services/memory/native-memory-manager';
import {
    updateClaudeSessionId,
    removeClaudeSessionId,
    updateClaudeTranscriptPath,
    updateClaudeStats,
    removeClaudeStats,
    deduplicateSessionIds,
    batchSyncClaudeState,
    ClaudeStats,
    ClaudeSyncUpdate,
} from './registry-client';
import {
    isMemoryEnabled,
    checkOpenMemoryLifecycle,
    getOpenMemoryState,
} from './services/memory';
import {
    isGatewayEnabled,
    checkGatewayLifecycle,
} from './services/mcp-gateway';
import { SessionManager, ClaudeState } from './session-manager';

let sessionManager: SessionManager | null = null;
let logFn: (message: string) => void = console.log;

/**
 * Initialize the session manager.
 * Called once during extension activation.
 */
export function initClaudeSync(
    projectName: string,
    workspacePath: string,
    logger: (msg: string) => void,
    screenBin: string,
): SessionManager {
    logFn = logger;
    sessionManager = new SessionManager(projectName, workspacePath, logger, screenBin);
    return sessionManager;
}

/** Get the current session manager instance (if initialized). */
export function getSessionManager(): SessionManager | null {
    return sessionManager;
}

/**
 * Dispose the session manager and clean up all watchers/connections.
 * Called during extension deactivation.
 */
export function disposeClaudeSync(): void {
    if (sessionManager) {
        sessionManager.dispose();
        sessionManager = null;
    }
}

/**
 * Sync Claude sessions -- called every 30s by the extension timer.
 * Reads cached state from SessionManager (zero I/O for state reads).
 * Lifecycle checks (OpenMemory, Gateway) are non-blocking HTTP calls.
 */
export async function syncClaudeSessions(
    onMemoryStateChange?: () => void,
): Promise<void> {
    if (!sessionManager) return;

    // Poll all C-binary context files in one batch pass.
    // Replaces N per-adapter setInterval(15s) + fs.watch() instances.
    sessionManager.pollAllContextFiles();

    const states = sessionManager.getAllClaudeStates();

    // Build session info for lifecycle checks
    const sessionInfo = Array.from(states.entries()).map(([wid, s]) => ({
        windowId: wid,
        hasClaudeProcess: s.active,
    }));

    // PERF: Batch all registry updates into a single read-modify-write cycle.
    // Previously did 3N+1 separate read/write cycles per sync (N = active sessions),
    // each JSON.parse + JSON.stringify of the full registry.
    const updates: ClaudeSyncUpdate[] = [];
    for (const [windowId, state] of states) {
        updates.push({
            windowId,
            active: state.active && !!state.sessionId,
            sessionId: state.sessionId,
            transcriptPath: state.transcriptPath,
            stats: state.active && state.sessionId ? {
                pid: state.pid ?? 0,
                rss: state.rssKb,
                cpu: state.cpuPercent,
                startTime: Math.floor(Date.now() / 1000) - state.runtimeSecs,
                runtime: state.runtimeSecs,
            } : undefined,
        });
    }
    batchSyncClaudeState(updates);

    // OpenMemory lifecycle check (non-blocking HTTP)
    if (isMemoryEnabled()) {
        try {
            await checkOpenMemoryLifecycle(sessionInfo);
        } catch { /* ignore */ }
    }

    // MCP Gateway lifecycle check (non-blocking HTTP)
    if (isGatewayEnabled()) {
        try {
            await checkGatewayLifecycle(sessionInfo);
        } catch { /* ignore */ }
    }

    // Fire heartbeat to OpenMemory (non-blocking HTTP POST)
    fireSessionHeartbeat(states);

    // Notify memory state change callback
    if (onMemoryStateChange && isMemoryEnabled()) {
        onMemoryStateChange();
    }
}

/**
 * Install the statusline script into the PROJECT's .claude/ directory and configure
 * the project's .claude/settings.local.json to use it. This is project-scoped --
 * only affects ImmorTerm-enabled projects, not all Claude Code sessions globally.
 * @param extensionResourcesPath Path to the extension's resources directory
 * @param projectPath The workspace folder path for the project
 */
export function installStatuslineScript(extensionResourcesPath: string, projectPath: string): void {
    const projectClaudeDir = path.join(projectPath, '.claude');
    const targetScript = path.join(projectClaudeDir, 'statusline.sh');
    const sourceScript = path.join(extensionResourcesPath, 'immorterm-statusline.sh');
    const settingsPath = path.join(projectClaudeDir, 'settings.local.json');

    try {
        // Ensure <project>/.claude exists
        if (!fs.existsSync(projectClaudeDir)) {
            fs.mkdirSync(projectClaudeDir, { recursive: true });
        }

        // Copy script if source exists and target is missing or outdated
        if (fs.existsSync(sourceScript)) {
            let needsCopy = !fs.existsSync(targetScript);
            if (!needsCopy) {
                const sourceContent = fs.readFileSync(sourceScript, 'utf8');
                const targetContent = fs.readFileSync(targetScript, 'utf8');
                needsCopy = sourceContent !== targetContent;
            }
            if (needsCopy) {
                fs.copyFileSync(sourceScript, targetScript);
                fs.chmodSync(targetScript, 0o755);
                logFn(`[statusline] Installed statusline.sh to ${projectClaudeDir}/`);
            }
        }

        // Configure project-scoped settings.local.json with statusLine entry
        let settings: Record<string, unknown> = {};
        if (fs.existsSync(settingsPath)) {
            try {
                settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
            } catch {
                // Corrupted settings -- don't overwrite, just skip
                return;
            }
        }

        const desired = {
            type: 'command',
            command: path.join(projectClaudeDir, 'statusline.sh')
        };

        const current = settings['statusLine'] as Record<string, unknown> | undefined;
        if (!current || current.command !== desired.command || current.type !== desired.type) {
            settings['statusLine'] = desired;
            fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2) + '\n', 'utf8');
            logFn(`[statusline] Configured statusLine in ${settingsPath}`);
        }
    } catch (error) {
        logFn(`[statusline] Error installing statusline: ${error}`);
    }
}

/** Fire a non-blocking HTTP heartbeat to OpenMemory with session data. */
function fireSessionHeartbeat(states: Map<string, ClaudeState>): void {
    if (!isMemoryEnabled() || states.size === 0) return;

    const sessions = Array.from(states.entries())
        .filter(([, s]) => s.active && s.sessionId)
        .map(([wid, s]) => ({
            session_id: s.sessionId,
            terminal_name: wid,
        }));

    if (sessions.length === 0) return;

    const body = JSON.stringify({ sessions });
    const req = http.request({
        hostname: '127.0.0.1',
        port: getMemoryPort(),
        path: '/api/v1/sessions/heartbeat',
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            'Content-Length': Buffer.byteLength(body),
        },
        timeout: 3000,
    }, (res) => {
        res.resume(); // drain to free socket
    });
    req.on('error', () => {}); // ignore -- OpenMemory may not be running
    req.write(body);
    req.end();
}
