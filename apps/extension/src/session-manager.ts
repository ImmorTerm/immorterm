/**
 * SessionManager -- Push-based terminal session management.
 *
 * Zero child processes. All state comes from:
 * - AI sessions: WebSocket push from Rust daemon
 * - C sessions: fs.watchFile on project-scoped context files
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import * as http from 'http';
import * as crypto from 'crypto';
import * as net from 'net';
import { execFile } from 'child_process';
import { promisify } from 'util';
import { EventEmitter } from 'events';

const execFileAsync = promisify(execFile);

// -- Types ----------------------------------------------------------------

export interface ClaudeState {
    active: boolean;
    pid?: number;
    sessionId?: string;
    rssKb: number;
    cpuPercent: number;
    runtimeSecs: number;
    model?: string;
    costUsd?: number;
    contextPct?: number;
    transcriptPath?: string;
}

export interface SessionInfo {
    windowId: string;
    displayName: string;
    type: 'regular' | 'ai';
    pid: number;
}

// -- Minimal WebSocket Client (Node.js built-ins only) --------------------

class MinimalWsClient {
    private socket: net.Socket | null = null;
    private buffer = Buffer.alloc(0);
    private onMessage: (data: string) => void;
    private onClose: () => void;
    private connected = false;

    constructor(
        private url: string,
        handlers: { onMessage: (data: string) => void; onClose: () => void }
    ) {
        this.onMessage = handlers.onMessage;
        this.onClose = handlers.onClose;
        this.connect();
    }

    private connect() {
        const key = crypto.randomBytes(16).toString('base64');
        const parsed = new URL(this.url);

        const req = http.request({
            hostname: parsed.hostname,
            port: parseInt(parsed.port),
            path: parsed.pathname || '/',
            method: 'GET',
            headers: {
                'Upgrade': 'websocket',
                'Connection': 'Upgrade',
                'Sec-WebSocket-Key': key,
                'Sec-WebSocket-Version': '13',
            }
        });

        req.on('upgrade', (_res: http.IncomingMessage, socket: net.Socket, head: Buffer) => {
            this.socket = socket;
            this.connected = true;
            this.buffer = Buffer.from(head);

            socket.on('data', (chunk: Buffer) => {
                this.buffer = Buffer.concat([this.buffer, chunk]);
                this.processFrames();
            });
            socket.on('close', () => { this.connected = false; this.onClose(); });
            socket.on('error', () => { this.connected = false; this.onClose(); });
        });

        req.on('error', () => this.onClose());
        req.end();
    }

    private processFrames() {
        while (this.buffer.length >= 2) {
            const firstByte = this.buffer[0];
            const opcode = firstByte & 0x0f;
            const secondByte = this.buffer[1];
            const masked = (secondByte & 0x80) !== 0;
            let payloadLen = secondByte & 0x7f;
            let headerLen = 2;

            if (payloadLen === 126) {
                if (this.buffer.length < 4) return;
                payloadLen = this.buffer.readUInt16BE(2);
                headerLen = 4;
            } else if (payloadLen === 127) {
                if (this.buffer.length < 10) return;
                payloadLen = Number(this.buffer.readBigUInt64BE(2));
                headerLen = 10;
            }

            // Mask key (4 bytes) sits between header and payload
            const maskKeyOffset = headerLen;
            const payloadOffset = masked ? headerLen + 4 : headerLen;
            const totalFrameLen = payloadOffset + payloadLen;

            if (this.buffer.length < totalFrameLen) return;

            let payload = this.buffer.subarray(payloadOffset, payloadOffset + payloadLen);

            // If server masked the frame, unmask it
            if (masked) {
                const maskKey = this.buffer.subarray(maskKeyOffset, maskKeyOffset + 4);
                const unmasked = Buffer.alloc(payloadLen);
                for (let i = 0; i < payloadLen; i++) {
                    unmasked[i] = payload[i] ^ maskKey[i % 4];
                }
                payload = unmasked;
            }

            // MEMORY FIX: Buffer.from() creates a NEW buffer, releasing the old parent.
            // subarray() creates a view into the same underlying ArrayBuffer, keeping
            // the entire concatenated buffer alive until the next concat.
            this.buffer = Buffer.from(this.buffer.subarray(totalFrameLen));

            if (opcode === 0x01) { // text frame
                this.onMessage(payload.toString('utf-8'));
            } else if (opcode === 0x08) { // close
                this.close();
                return;
            } else if (opcode === 0x09) { // ping -> send pong
                this.sendFrame(0x0A, payload);
            }
        }
    }

    send(text: string) {
        if (!this.connected || !this.socket) return;
        const payload = Buffer.from(text, 'utf-8');
        this.sendFrame(0x01, payload);
    }

    private sendFrame(opcode: number, payload: Buffer) {
        if (!this.socket) return;
        const mask = crypto.randomBytes(4);
        let header: Buffer;

        if (payload.length < 126) {
            header = Buffer.alloc(6);
            header[0] = 0x80 | opcode; // FIN + opcode
            header[1] = 0x80 | payload.length; // masked
            mask.copy(header, 2);
        } else if (payload.length < 65536) {
            header = Buffer.alloc(8);
            header[0] = 0x80 | opcode;
            header[1] = 0x80 | 126;
            header.writeUInt16BE(payload.length, 2);
            mask.copy(header, 4);
        } else {
            header = Buffer.alloc(14);
            header[0] = 0x80 | opcode;
            header[1] = 0x80 | 127;
            header.writeBigUInt64BE(BigInt(payload.length), 2);
            mask.copy(header, 10);
        }

        const masked = Buffer.alloc(payload.length);
        for (let i = 0; i < payload.length; i++) {
            masked[i] = payload[i] ^ mask[i % 4];
        }
        this.socket.write(Buffer.concat([header, masked]));
    }

    close() {
        this.connected = false;
        if (this.socket) {
            this.socket.destroy();
            this.socket = null;
        }
    }
}

// -- ImmorTermAiAdapter (Rust daemon, WebSocket push) ---------------------

class ImmorTermAiAdapter extends EventEmitter {
    readonly type = 'ai' as const;
    readonly windowId: string;
    private ws: MinimalWsClient | null = null;
    private claudeState: ClaudeState | null = null;
    private sessionInfo: SessionInfo;
    private disposed = false;
    private reconnectDelay = 1000;
    private reconnectTimer: NodeJS.Timeout | null = null;

    constructor(
        windowId: string,
        private wsPort: number,
        private displayName: string,
        private pid: number,
    ) {
        super();
        this.windowId = windowId;
        this.sessionInfo = { windowId, displayName, type: 'ai', pid };
        this.connectWs();
    }

    private connectWs() {
        if (this.disposed) return;
        try {
            this.ws = new MinimalWsClient(
                `ws://127.0.0.1:${this.wsPort}`,
                {
                    onMessage: (data: string) => this.handleMessage(data),
                    onClose: () => this.scheduleReconnect(),
                }
            );
            // Send subscribe_control after a short delay to let WS handshake complete
            setTimeout(() => {
                if (this.ws && !this.disposed) {
                    this.reconnectDelay = 1000; // reset backoff on successful connect
                    this.ws.send(JSON.stringify({ type: 'subscribe_control' }));
                }
            }, 100);
        } catch {
            this.scheduleReconnect();
        }
    }

    private handleMessage(data: string) {
        try {
            const msg = JSON.parse(data);
            if (msg.type === 'control_hello') {
                this.sessionInfo.displayName = msg.display_name || this.displayName;
                if (msg.claude) {
                    this.claudeState = this.parseClaudeState(msg.claude);
                    this.emit('claude-update', this.claudeState);
                }
            } else if (msg.type === 'control_event') {
                if (msg.event === 'claude_update' && msg.claude) {
                    this.claudeState = this.parseClaudeState(msg.claude);
                    this.emit('claude-update', this.claudeState);
                } else if (msg.event === 'claude_exited') {
                    this.claudeState = null;
                    this.emit('claude-exited');
                } else if (msg.event === 'session_closing') {
                    this.emit('session-closing');
                }
            }
            // Ignore other message types (hello, viewport_diff, etc.)
        } catch { /* ignore parse errors */ }
    }

    private scheduleReconnect() {
        if (this.disposed) return;
        this.ws = null;
        this.reconnectTimer = setTimeout(() => {
            this.reconnectDelay = Math.min(this.reconnectDelay * 2, 30000);
            this.connectWs();
        }, this.reconnectDelay);
    }

    private parseClaudeState(raw: Record<string, unknown>): ClaudeState {
        return {
            active: (raw.active as boolean) ?? false,
            pid: (raw.pid as number) ?? undefined,
            sessionId: (raw.session_id as string) ?? undefined,
            rssKb: (raw.rss_kb as number) ?? 0,
            cpuPercent: (raw.cpu_percent as number) ?? 0,
            runtimeSecs: (raw.runtime_secs as number) ?? 0,
            model: (raw.model as string) ?? undefined,
            costUsd: (raw.cost_usd as number) ?? undefined,
            contextPct: (raw.context_pct as number) ?? undefined,
            transcriptPath: (raw.transcript_path as string) ?? undefined,
        };
    }

    getClaudeState(): ClaudeState | null { return this.claudeState; }
    getSessionInfo(): SessionInfo { return this.sessionInfo; }

    isAlive(): boolean {
        try { process.kill(this.pid, 0); return true; }
        catch { return false; }
    }

    dispose() {
        this.disposed = true;
        if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
        if (this.ws) { this.ws.close(); this.ws = null; }
        this.removeAllListeners();
    }
}

// -- ImmorTermAdapter (C binary, file-based) ------------------------------

class ImmorTermAdapter extends EventEmitter {
    readonly type = 'regular' as const;
    readonly windowId: string;
    private claudeState: ClaudeState | null = null;
    private sessionInfo: SessionInfo;
    private disposed = false;
    private contextFilePath: string;

    constructor(
        windowId: string,
        private displayName: string,
        private pid: number,
        projectDir: string,
    ) {
        super();
        this.windowId = windowId;
        this.sessionInfo = { windowId, displayName, type: 'regular', pid };
        this.contextFilePath = path.join(projectDir, '.immorterm', 'claude-ctx', windowId);
        this.readContextFile(); // initial read
    }

    /** Poll context file — called by SessionManager's consolidated loop. */
    readContextFile() {
        try {
            const content = fs.readFileSync(this.contextFilePath, 'utf-8');
            const vars = this.parseKeyValue(content);
            const now = Math.floor(Date.now() / 1000);
            const ts = parseInt(vars['TIMESTAMP'] || '0', 10);

            if (now - ts > 300) { // stale (>5 min)
                if (this.claudeState?.active) {
                    this.claudeState = null;
                    this.emit('claude-exited');
                }
                return;
            }

            const newState: ClaudeState = {
                active: true,
                sessionId: vars['SESSION_ID'],
                rssKb: parseInt(vars['RSS_KB'] || '0', 10),
                cpuPercent: parseFloat(vars['CPU_PCT'] || '0'),
                runtimeSecs: parseInt(vars['RUNTIME_SECS'] || '0', 10),
                model: vars['MODEL'],
                costUsd: vars['COST'] ? parseFloat(vars['COST']) : undefined,
                contextPct: vars['CTX_PCT'] ? parseFloat(vars['CTX_PCT']) : undefined,
                transcriptPath: vars['TRANSCRIPT_PATH'],
            };
            this.claudeState = newState;
            this.emit('claude-update', newState);
        } catch {
            if (this.claudeState?.active) {
                this.claudeState = null;
                this.emit('claude-exited');
            }
        }
    }

    private parseKeyValue(content: string): Record<string, string> {
        const result: Record<string, string> = {};
        for (const line of content.split('\n')) {
            const idx = line.indexOf('=');
            if (idx > 0) {
                result[line.substring(0, idx)] = line.substring(idx + 1);
            }
        }
        return result;
    }

    getClaudeState(): ClaudeState | null { return this.claudeState; }
    getSessionInfo(): SessionInfo { return this.sessionInfo; }

    isAlive(): boolean {
        try { process.kill(this.pid, 0); return true; }
        catch { return false; }
    }

    dispose() {
        this.disposed = true;
        this.removeAllListeners();
    }
}

// -- AI Stats Formatting (mirrors gpu-terminal.html for C binary sessions) --

function formatMemory(kb: number): string {
    if (kb >= 1048576) return (kb / 1048576).toFixed(1) + 'G';
    return Math.round(kb / 1024) + 'M';
}

function formatRuntime(secs: number): string {
    if (secs >= 3600) {
        const h = Math.floor(secs / 3600);
        const m = Math.floor((secs % 3600) / 60);
        return h + 'h' + (m > 0 ? m + 'm' : '');
    }
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return m + 'm' + (s > 0 ? s + 's' : '');
}

/** Threshold color for context bar fill: green→yellow→orange→red */
function getCtxBarColor(pct: number): string {
    if (pct >= 95) return '#FF0000';
    if (pct >= 85) return '#FF3333';
    if (pct >= 70) return '#FF6B00';
    if (pct >= 50) return '#FFB800';
    return '#00CC44';
}

/** Inline color escape: \x03#RRGGBB = set fg truecolor (parsed by winmsg.c) */
const CLR = (hex: string) => `\x03${hex}`;
/** Inline color escape: \x03- = pop rendition */
const CLR_POP = '\x03-';

/** Mode 0 (default): 🔋 RAM:240M CPU:5% 1h23m */
function formatProcessStats(c: ClaudeState): string {
    return `\uD83D\uDD0B RAM:${formatMemory(c.rssKb)} CPU:${Math.round(c.cpuPercent)}% ${formatRuntime(c.runtimeSecs)}`;
}

/** Mode 1 (F5 toggle): 🤖 CTX: ▰▰▰▰▱▱▱▱▱▱ 42%
 *  Colored progress bar using inline \x03#RRGGBB protocol. */
function formatApiStats(c: ClaudeState): string {
    if (c.contextPct == null) return '';
    const barWidth = 10;
    const filled = Math.round((c.contextPct / 100) * barWidth);
    const empty = barWidth - filled;
    const fillColor = getCtxBarColor(c.contextPct);
    const emptyColor = '#444444';

    let bar = '';
    if (filled > 0) bar += CLR(fillColor) + '\u25B0'.repeat(filled);
    if (empty > 0) bar += CLR(emptyColor) + '\u25B1'.repeat(empty);
    bar += CLR_POP;

    return `\uD83E\uDD16 CTX: ${bar} ${Math.round(c.contextPct)}%`;
}

// -- SessionManager -------------------------------------------------------

export class SessionManager {
    private adapters = new Map<string, ImmorTermAiAdapter | ImmorTermAdapter>();
    private disposed = false;
    private lastPushedStats = new Map<string, string>();
    private statsToggleTimer: NodeJS.Timeout | null = null;
    /** Cache: windowId → PID-prefixed screen session name (e.g. "68857.immorterm-67140-tYVOd326") */
    private screenSessionCache = new Map<string, string>();
    /** The currently visible terminal's windowId — only this one gets stats toggled */
    private activeWindowId: string | null = null;

    constructor(
        private projectName: string,
        private projectPath: string,
        private logFn: (msg: string) => void,
        private screenBin: string = 'immorterm',
    ) {
        this.watchRegistry();
        this.loadRegistry();
        this.startStatsAutoToggle();
    }

    private get registryPath(): string {
        return path.join(os.homedir(), '.immorterm', 'registry.json');
    }

    private registryWatcher: fs.FSWatcher | null = null;
    private registryDebounce: NodeJS.Timeout | null = null;

    private watchRegistry() {
        // Use OS-native fs.watch (FSEvents on macOS) instead of polling fs.watchFile.
        // Debounce because FSEvents may fire multiple events for atomic writes.
        const startWatch = () => {
            try {
                this.registryWatcher = fs.watch(this.registryPath, () => {
                    if (this.registryDebounce) clearTimeout(this.registryDebounce);
                    this.registryDebounce = setTimeout(() => this.loadRegistry(), 200);
                });
                this.registryWatcher.on('error', () => {
                    // File may have been deleted/renamed — retry watching after a delay
                    this.registryWatcher?.close();
                    this.registryWatcher = null;
                    setTimeout(() => { if (!this.disposed) startWatch(); }, 2000);
                });
            } catch {
                // File doesn't exist yet — retry after a delay
                setTimeout(() => { if (!this.disposed) startWatch(); }, 2000);
            }
        };
        startWatch();
    }

    private loadRegistry() {
        try {
            const content = fs.readFileSync(this.registryPath, 'utf-8');
            const registry = JSON.parse(content);
            const sessions: Array<Record<string, unknown>> = registry.sessions || [];

            const relevant = sessions.filter((s) =>
                s.project_dir === this.projectPath ||
                (typeof s.project_dir === 'string' && s.project_dir.endsWith('/' + this.projectName))
            );

            const activeIds = new Set<string>();

            for (const entry of relevant) {
                const wid = entry.window_id as string | undefined;
                if (!wid) continue;
                activeIds.add(wid);
                if (this.adapters.has(wid)) continue;

                if ((entry.session_type === 'ai' || entry.type === 'ai') && entry.ws_port) {
                    const adapter = new ImmorTermAiAdapter(
                        wid,
                        entry.ws_port as number,
                        (entry.display_name as string) || '',
                        (entry.pid as number) || 0,
                    );
                    this.adapters.set(wid, adapter);
                    this.logFn(`[SessionManager] Tracking AI session: ${wid}`);
                } else if (entry.session_type === 'regular' || entry.type === 'regular' || (!entry.session_type && !entry.type)) {
                    const adapter = new ImmorTermAdapter(
                        wid,
                        (entry.display_name as string) || '',
                        (entry.pid as number) || 0,
                        this.projectPath,
                    );
                    // Push Claude stats to C binary's screen hardstatus (%Z)
                    adapter.on('claude-update', (state: ClaudeState) => {
                        this.pushAiStatsToScreen(wid, state);
                    });
                    adapter.on('claude-exited', () => {
                        this.clearAiStatsFromScreen(wid);
                    });
                    this.adapters.set(wid, adapter);
                    this.logFn(`[SessionManager] Tracking C session: ${wid}`);
                }
            }

            for (const [wid, adapter] of this.adapters) {
                if (!activeIds.has(wid)) {
                    adapter.dispose();
                    this.adapters.delete(wid);
                    this.lastPushedStats.delete(wid);
                    this.logFn(`[SessionManager] Removed session: ${wid}`);
                }
            }
        } catch { /* registry doesn't exist yet */ }
    }

    /**
     * Poll all regular (C binary) adapters' context files in a single pass.
     * Called by the consolidated sync loop (every 30s) instead of per-adapter timers.
     * This replaces N individual setInterval(15s) + N fs.watch() instances.
     */
    pollAllContextFiles(): void {
        for (const adapter of this.adapters.values()) {
            if (adapter.type === 'regular') {
                (adapter as ImmorTermAdapter).readContextFile();
            }
        }
    }

    getAllClaudeStates(): Map<string, ClaudeState> {
        const result = new Map<string, ClaudeState>();
        for (const [wid, adapter] of this.adapters) {
            const state = adapter.getClaudeState();
            if (state) result.set(wid, state);
        }
        return result;
    }

    getClaudeState(windowId: string): ClaudeState | null {
        return this.adapters.get(windowId)?.getClaudeState() ?? null;
    }

    getAllSessions(): SessionInfo[] {
        return Array.from(this.adapters.values()).map(a => a.getSessionInfo());
    }

    isAlive(windowId: string): boolean {
        return this.adapters.get(windowId)?.isAlive() ?? false;
    }

    /**
     * Push formatted AI stats to a C binary screen session's hardstatus (%Z).
     * Uses the `aistats` screen command (process.c:4068) with both modes.
     * Deduplicated: only pushes when the formatted string actually changes.
     */
    private async pushAiStatsToScreen(windowId: string, state: ClaudeState): Promise<void> {
        if (!state.active) return;
        // Mode 0 (default view) = Process stats (RAM/CPU/runtime) — always available
        // Mode 1 (F5 toggle)   = API stats (colored CTX bar) — requires statusline data
        const mode0 = formatProcessStats(state);
        const mode1 = formatApiStats(state);
        const key = `${mode0}|${mode1}`;
        if (this.lastPushedStats.get(windowId) === key) return;
        this.lastPushedStats.set(windowId, key);

        const sessionName = `${this.projectName}-${windowId}`;
        try {
            const cmd = `aistats "${mode0.replace(/"/g, '')}" "${mode1.replace(/"/g, '')}"`;
            await execFileAsync(this.screenBin, ['-S', sessionName, '-X', 'eval', cmd], { timeout: 3000 });
        } catch { /* session may not exist or screen binary unavailable */ }
    }

    private async clearAiStatsFromScreen(windowId: string): Promise<void> {
        this.lastPushedStats.delete(windowId);
        const sessionName = `${this.projectName}-${windowId}`;
        try {
            await execFileAsync(this.screenBin, ['-S', sessionName, '-X', 'eval', 'aistats "" ""'], { timeout: 3000 });
        } catch { /* ignore */ }
    }

    /** Set which terminal is currently visible. Only this one gets stats toggled. */
    setActiveWindowId(windowId: string | null): void {
        this.activeWindowId = windowId;
    }

    /** Get the type ('regular' | 'ai') of a tracked terminal, or undefined if unknown. */
    getSessionType(windowId: string): 'regular' | 'ai' | undefined {
        return this.adapters.get(windowId)?.type;
    }

    /**
     * Auto-toggle AI stats display mode every 30 seconds for the active (visible) C session.
     * Cycles between mode 0 (process stats: RAM/CPU/runtime) and mode 1 (colored CTX bar).
     */
    private startStatsAutoToggle(): void {
        this.statsToggleTimer = setInterval(() => {
            const wid = this.activeWindowId;
            if (!wid) return;
            const adapter = this.adapters.get(wid);
            if (!adapter || adapter.type !== 'regular') return;
            const state = adapter.getClaudeState();
            if (!state?.active) return;
            const sessionName = `${this.projectName}-${wid}`;
            execFileAsync(this.screenBin, ['-S', sessionName, '-X', 'eval', 'aistatstoggle'], { timeout: 3000 })
                .catch(() => { /* ignore */ });
        }, 30_000);
    }

    dispose() {
        this.disposed = true;
        if (this.statsToggleTimer) clearInterval(this.statsToggleTimer);
        if (this.registryDebounce) clearTimeout(this.registryDebounce);
        if (this.registryWatcher) { this.registryWatcher.close(); this.registryWatcher = null; }
        for (const adapter of this.adapters.values()) {
            adapter.dispose();
        }
        this.adapters.clear();
        this.lastPushedStats.clear();
    }
}
