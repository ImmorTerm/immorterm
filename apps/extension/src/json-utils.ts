/**
 * @deprecated Phase 2A — replaced by registry-client.ts
 *
 * This file managed per-project restore-terminals.json.
 * All consumers now use registry-client.ts which reads/writes
 * the global ~/.immorterm/registry.json instead.
 *
 * Kept as reference and fallback for environments without the Rust daemon.
 */

import * as fs from 'fs';

let jsonPath: string;
let logFn: (message: string) => void = console.log;

// Cache for parsed JSON to avoid redundant file reads
interface JsonCache {
    config: RestoreConfig | null;
    mtime: number;  // File modification time in ms
}

interface RestoreConfig {
    artificialDelayMilliseconds?: number;
    terminals: Array<{
        splitTerminals: Array<{
            windowId?: string;
            name?: string;
            commands?: string[];
            claudeSessionId?: string;
            claudeTranscriptPath?: string;
            claudeStats?: ClaudeStats;
            theme?: string;
            titleLocked?: boolean;
        }>;
    }>;
}

let jsonCache: JsonCache = { config: null, mtime: 0 };

/**
 * Read and cache the JSON config. Returns cached version if file hasn't changed.
 * This is the single point of file I/O for reads - all other functions use this.
 */
function getCachedConfig(): RestoreConfig | null {
    if (!jsonPath || !fs.existsSync(jsonPath)) {
        jsonCache = { config: null, mtime: 0 };
        return null;
    }

    try {
        const stat = fs.statSync(jsonPath);
        const currentMtime = stat.mtimeMs;

        // Return cached if file hasn't changed
        if (jsonCache.config && jsonCache.mtime === currentMtime) {
            return jsonCache.config;
        }

        // File changed or no cache - read and parse
        const config = JSON.parse(fs.readFileSync(jsonPath, 'utf8')) as RestoreConfig;
        jsonCache = { config, mtime: currentMtime };
        return config;
    } catch {
        jsonCache = { config: null, mtime: 0 };
        return null;
    }
}

/**
 * Write config to file and update cache
 */
function writeCachedConfig(config: RestoreConfig): void {
    fs.writeFileSync(jsonPath, JSON.stringify(config, null, 2) + '\n');
    // Update cache with new config and current mtime
    try {
        const stat = fs.statSync(jsonPath);
        jsonCache = { config, mtime: stat.mtimeMs };
    } catch {
        // If stat fails, invalidate cache
        jsonCache = { config: null, mtime: 0 };
    }
}

/**
 * Invalidate the cache (used when external changes are expected)
 */
export function invalidateJsonCache(): void {
    jsonCache = { config: null, mtime: 0 };
}

export function initJsonUtils(path: string, logger: (message: string) => void) {
    jsonPath = path;
    logFn = logger;
    // Invalidate cache when path changes
    invalidateJsonCache();
}

export function getJsonPath(): string {
    return jsonPath;
}

/**
 * Extract window ID from command array (fallback for old entries without windowId field)
 * Commands look like: "exec $HOME/.immorterm/scripts/screen-auto 12345-abcdef12"
 */
export function extractWindowIdFromCommands(commands: string[] | undefined): string | null {
    if (!commands) return null;
    for (const cmd of commands) {
        const match = cmd.match(/screen-auto\s+(\d+-\w+)/);
        if (match) return match[1];
    }
    return null;
}

/**
 * Get terminal name from JSON for a windowId
 */
export function getTerminalNameFromJson(windowId: string): string | null {
    const config = getCachedConfig();
    if (!config) return null;

    for (const tab of config.terminals || []) {
        for (const split of tab.splitTerminals || []) {
            if (split.windowId === windowId) {
                return split.name || null;
            }
        }
    }
    return null;
}

/**
 * Update the name in restore-terminals.json for a given window ID
 */
export function updateJsonName(windowId: string, newName: string) {
    const config = getCachedConfig();
    if (!config) {
        logFn('restore-terminals.json not found');
        return;
    }

    try {
        let modified = false;

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                const id = split.windowId || extractWindowIdFromCommands(split.commands);
                if (id === windowId && split.name !== newName) {
                    logFn(`JSON update "${split.name}" → "${newName}"`);
                    split.name = newName;
                    modified = true;
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
            logFn('Updated restore-terminals.json');
        }
    } catch (error) {
        logFn(`Error updating JSON: ${error}`);
    }
}

/**
 * Update the theme in restore-terminals.json for a given window ID
 * @param windowId The window ID to update
 * @param theme The theme name, or undefined to clear the per-terminal theme
 */
export function updateJsonTheme(windowId: string, theme: string | undefined) {
    const config = getCachedConfig();
    if (!config) {
        logFn('restore-terminals.json not found');
        return;
    }

    try {
        let modified = false;

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                const id = split.windowId || extractWindowIdFromCommands(split.commands);
                if (id === windowId) {
                    if (theme) {
                        if (split.theme !== theme) {
                            logFn(`JSON theme update for ${windowId}: "${split.theme || 'none'}" → "${theme}"`);
                            split.theme = theme;
                            modified = true;
                        }
                    } else {
                        // Clear the theme
                        if (split.theme) {
                            logFn(`JSON theme cleared for ${windowId}`);
                            delete split.theme;
                            modified = true;
                        }
                    }
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
            logFn('Updated restore-terminals.json (theme)');
        }
    } catch (error) {
        logFn(`Error updating JSON theme: ${error}`);
    }
}

/**
 * Update titleLocked flag in restore-terminals.json for a given window ID.
 * When locked, Claude/OSC cannot change the title (user's custom name is protected).
 */
export function updateJsonTitleLocked(windowId: string, locked: boolean) {
    const config = getCachedConfig();
    if (!config) {
        logFn('restore-terminals.json not found');
        return;
    }

    try {
        let modified = false;

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                const id = split.windowId || extractWindowIdFromCommands(split.commands);
                if (id === windowId) {
                    if (locked) {
                        if (split.titleLocked !== true) {
                            logFn(`JSON titleLocked set for ${windowId}`);
                            split.titleLocked = true;
                            modified = true;
                        }
                    } else {
                        if (split.titleLocked) {
                            logFn(`JSON titleLocked cleared for ${windowId}`);
                            delete split.titleLocked;
                            modified = true;
                        }
                    }
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
            logFn('Updated restore-terminals.json (titleLocked)');
        }
    } catch (error) {
        logFn(`Error updating JSON titleLocked: ${error}`);
    }
}

/**
 * Update both name and command in restore-terminals.json for a given window ID
 * The command is updated to include the display name so screen-auto uses it for the tab title
 */
export function updateJsonNameAndCommand(windowId: string, newName: string) {
    logFn(`updateJsonNameAndCommand called: windowId=${windowId}, newName=${newName}`);

    if (!jsonPath) {
        logFn('ERROR: jsonPath not initialized - call initJsonUtils first');
        return;
    }

    const config = getCachedConfig();
    if (!config) {
        logFn(`restore-terminals.json not found at: ${jsonPath}`);
        return;
    }

    try {
        let modified = false;
        const foundIds: string[] = [];

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                const id = split.windowId || extractWindowIdFromCommands(split.commands);
                if (id) foundIds.push(id);
                if (id === windowId) {
                    if (split.name !== newName) {
                        logFn(`JSON update name "${split.name}" → "${newName}"`);
                        split.name = newName;
                        modified = true;
                    }

                    const newCommand = `exec $HOME/.immorterm/scripts/screen-auto ${windowId} "${newName}"`;
                    if (split.commands && split.commands.length > 0) {
                        if (split.commands[0] !== newCommand) {
                            logFn(`JSON update command → "${newCommand}"`);
                            split.commands[0] = newCommand;
                            modified = true;
                        }
                    }
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
            logFn('Updated restore-terminals.json (name + command)');
        } else {
            logFn(`No match found for windowId=${windowId}. Found IDs in JSON: [${foundIds.join(', ')}]`);
        }
    } catch (error) {
        logFn(`Error updating JSON: ${error}`);
    }
}

/**
 * Get all window IDs from JSON
 */
export function getAllWindowIds(): Set<string> {
    const windowIds = new Set<string>();
    const config = getCachedConfig();
    if (!config) return windowIds;

    for (const tab of config.terminals || []) {
        for (const split of tab.splitTerminals || []) {
            if (split.windowId) {
                windowIds.add(split.windowId);
            }
        }
    }
    return windowIds;
}

/**
 * Get current claudeSessionId from JSON for a window
 */
export function getCurrentClaudeSessionId(windowId: string): string | null {
    const config = getCachedConfig();
    if (!config) return null;

    for (const tab of config.terminals || []) {
        for (const split of tab.splitTerminals || []) {
            if (split.windowId === windowId) {
                return split.claudeSessionId || null;
            }
        }
    }
    return null;
}

/**
 * Update claudeSessionId in JSON for a window
 */
export function updateClaudeSessionId(windowId: string, sessionId: string): boolean {
    const config = getCachedConfig();
    if (!config) return false;

    try {
        let modified = false;

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                if (split.windowId === windowId) {
                    if (split.claudeSessionId !== sessionId) {
                        split.claudeSessionId = sessionId;
                        modified = true;
                        logFn(`[claude-sync] Set claudeSessionId for ${windowId}: ${sessionId.slice(0, 8)}...`);
                    }
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
        }

        return modified;
    } catch (error) {
        logFn(`Error updating claudeSessionId: ${error}`);
        return false;
    }
}

/**
 * Update claudeTranscriptPath in JSON for a window
 */
export function updateClaudeTranscriptPath(windowId: string, transcriptPath: string): boolean {
    const config = getCachedConfig();
    if (!config) return false;

    try {
        let modified = false;

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                if (split.windowId === windowId) {
                    if (split.claudeTranscriptPath !== transcriptPath) {
                        split.claudeTranscriptPath = transcriptPath;
                        modified = true;
                        logFn(`[claude-sync] Set claudeTranscriptPath for ${windowId}`);
                    }
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
        }

        return modified;
    } catch (error) {
        logFn(`Error updating claudeTranscriptPath: ${error}`);
        return false;
    }
}

/**
 * Get claudeTranscriptPath from JSON for a window
 */
export function getClaudeTranscriptPath(windowId: string): string | null {
    const config = getCachedConfig();
    if (!config) return null;

    for (const tab of config.terminals || []) {
        for (const split of tab.splitTerminals || []) {
            if (split.windowId === windowId) {
                return split.claudeTranscriptPath || null;
            }
        }
    }
    return null;
}

/**
 * Remove claudeSessionId from JSON for a window (Claude exited)
 */
export function removeClaudeSessionId(windowId: string): boolean {
    const config = getCachedConfig();
    if (!config) return false;

    try {
        let modified = false;

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                if (split.windowId === windowId && split.claudeSessionId) {
                    delete split.claudeSessionId;
                    delete split.claudeTranscriptPath;
                    modified = true;
                    logFn(`[claude-sync] Removed claudeSessionId for ${windowId} (Claude exited)`);
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
        }

        return modified;
    } catch (error) {
        logFn(`Error removing claudeSessionId: ${error}`);
        return false;
    }
}

/**
 * Claude process stats interface
 */
export interface ClaudeStats {
    pid: number;
    rss: number;        // Memory in KB
    cpu: number;        // CPU percentage
    startTime: number;  // Unix timestamp when Claude first detected
    runtime: number;    // Seconds running
}

/**
 * Update claudeStats in JSON for a window
 */
export function updateClaudeStats(windowId: string, stats: ClaudeStats): boolean {
    const config = getCachedConfig();
    if (!config) return false;

    try {
        let modified = false;

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                if (split.windowId === windowId) {
                    // Only log if stats changed significantly (reduce noise)
                    const oldStats = split.claudeStats;
                    const shouldLog = !oldStats ||
                        Math.abs(oldStats.rss - stats.rss) > 10240 ||  // >10MB change
                        Math.abs(oldStats.cpu - stats.cpu) > 5;        // >5% change

                    split.claudeStats = stats;
                    modified = true;

                    if (shouldLog) {
                        const memMB = Math.round(stats.rss / 1024);
                        logFn(`[claude-stats] ${windowId}: ${memMB}MB, ${stats.cpu.toFixed(1)}% CPU, ${stats.runtime}s`);
                    }
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
        }

        return modified;
    } catch (error) {
        logFn(`Error updating claudeStats: ${error}`);
        return false;
    }
}

/**
 * Remove claudeStats from JSON for a window (Claude exited)
 */
export function removeClaudeStats(windowId: string): boolean {
    const config = getCachedConfig();
    if (!config) return false;

    try {
        let modified = false;

        for (const tab of config.terminals || []) {
            for (const split of tab.splitTerminals || []) {
                if (split.windowId === windowId && split.claudeStats) {
                    delete split.claudeStats;
                    modified = true;
                    logFn(`[claude-stats] Removed stats for ${windowId} (Claude exited)`);
                }
            }
        }

        if (modified) {
            writeCachedConfig(config);
        }

        return modified;
    } catch (error) {
        logFn(`Error removing claudeStats: ${error}`);
        return false;
    }
}

/**
 * Get claudeStats from JSON for a window
 */
export function getClaudeStats(windowId: string): ClaudeStats | null {
    const config = getCachedConfig();
    if (!config) return null;

    for (const tab of config.terminals || []) {
        for (const split of tab.splitTerminals || []) {
            if (split.windowId === windowId) {
                return split.claudeStats || null;
            }
        }
    }
    return null;
}

/**
 * Add a new terminal entry to restore-terminals.json
 * Creates the file if it doesn't exist
 */
export function addTerminalToJson(windowId: string, displayName: string): boolean {
    try {
        // Get existing config or create default
        let config = getCachedConfig();
        if (!config) {
            config = {
                artificialDelayMilliseconds: 0,
                terminals: []
            };
        }
        if (!config.terminals) {
            config.terminals = [];
        }

        // Check if terminal already exists
        for (const tab of config.terminals) {
            for (const split of tab.splitTerminals || []) {
                if (split.windowId === windowId) {
                    logFn(`Terminal ${windowId} already exists in JSON, skipping add`);
                    return false; // Already exists
                }
            }
        }

        // Create the command that screen-auto uses
        const command = `exec $HOME/.immorterm/scripts/screen-auto ${windowId} "${displayName}"`;

        // Add new terminal entry
        config.terminals.push({
            splitTerminals: [
                {
                    windowId,
                    name: displayName,
                    commands: [command]
                }
            ]
        });

        // Write to file (this also updates cache)
        writeCachedConfig(config);
        logFn(`Added terminal ${windowId} ("${displayName}") to restore-terminals.json`);
        return true;
    } catch (error) {
        logFn(`Error adding terminal to JSON: ${error}`);
        return false;
    }
}

/**
 * Remove a specific terminal entry from restore-terminals.json
 * @param windowId The window ID to remove
 * @returns true if terminal was found and removed, false otherwise
 */
export function removeTerminalFromJson(windowId: string): boolean {
    const config = getCachedConfig();
    if (!config) return false;

    try {
        let modified = false;

        // Filter out the terminal with the given windowId
        config.terminals = (config.terminals || []).filter((tab) => {
            if (!tab.splitTerminals) return true;

            const originalLength = tab.splitTerminals.length;
            tab.splitTerminals = tab.splitTerminals.filter(split => split.windowId !== windowId);

            if (tab.splitTerminals.length !== originalLength) {
                modified = true;
            }

            // Keep the tab only if it still has terminals
            return tab.splitTerminals.length > 0;
        });

        if (modified) {
            writeCachedConfig(config);
            logFn(`Removed terminal ${windowId} from restore-terminals.json`);
        }

        return modified;
    } catch (error) {
        logFn(`Error removing terminal from JSON: ${error}`);
        return false;
    }
}

/**
 * Get all terminals from restore-terminals.json
 * Returns array of {windowId, name, claudeSessionId} for restoration
 */
export function getAllTerminalsFromJson(): Array<{ windowId: string; name: string; claudeSessionId?: string; claudeTranscriptPath?: string; theme?: string; titleLocked?: boolean }> {
    if (!jsonPath) {
        logFn('ERROR: getAllTerminalsFromJson called before initJsonUtils - jsonPath not set');
        return [];
    }

    const config = getCachedConfig();
    if (!config) {
        logFn(`getAllTerminalsFromJson: JSON file not found at ${jsonPath}`);
        return [];
    }

    const terminals: Array<{ windowId: string; name: string; claudeSessionId?: string; claudeTranscriptPath?: string; theme?: string; titleLocked?: boolean }> = [];

    for (const tab of config.terminals || []) {
        for (const split of tab.splitTerminals || []) {
            const windowId = split.windowId || extractWindowIdFromCommands(split.commands);
            if (windowId && split.name) {
                terminals.push({
                    windowId,
                    name: split.name,
                    claudeSessionId: split.claudeSessionId,
                    claudeTranscriptPath: split.claudeTranscriptPath,
                    theme: split.theme,
                    titleLocked: split.titleLocked
                });
            }
        }
    }

    return terminals;
}

/**
 * Dedup cleanup: if multiple terminals share the same claudeSessionId,
 * keep only the one with the most recent claudeStats.startTime and clear the rest.
 * Returns count of cleared duplicates. Writes JSON once at end if any changes.
 */
export function deduplicateSessionIds(): number {
    const config = getCachedConfig();
    if (!config) return 0;

    // Group splits by claudeSessionId
    const groups = new Map<string, Array<{ split: any; startTime: number }>>();

    for (const tab of config.terminals || []) {
        for (const split of tab.splitTerminals || []) {
            const sid = split.claudeSessionId;
            if (!sid) continue;
            if (!groups.has(sid)) groups.set(sid, []);
            groups.get(sid)!.push({
                split,
                startTime: split.claudeStats?.startTime ?? 0,
            });
        }
    }

    let cleared = 0;
    for (const [sessionId, entries] of groups) {
        if (entries.length <= 1) continue;

        // Sort descending by startTime — first entry is the keeper
        entries.sort((a, b) => b.startTime - a.startTime);

        for (let i = 1; i < entries.length; i++) {
            const wid = entries[i].split.windowId || '?';
            logFn(`[dedup] Clearing duplicate claudeSessionId ${sessionId.slice(0, 8)}... from ${wid}`);
            delete entries[i].split.claudeSessionId;
            cleared++;
        }
    }

    if (cleared > 0) {
        writeCachedConfig(config);
        logFn(`[dedup] Cleared ${cleared} duplicate session ID(s)`);
    }
    return cleared;
}

/**
 * Clear all terminal entries from restore-terminals.json
 * Resets the file to an empty terminals array
 */
export function clearAllTerminalsFromJson(): boolean {
    if (!jsonPath) {
        logFn('ERROR: clearAllTerminalsFromJson called before initJsonUtils - jsonPath not set');
        return false;
    }

    try {
        // Create empty config structure
        const config: RestoreConfig = {
            artificialDelayMilliseconds: 0,
            terminals: []
        };

        // Write empty config to file (this also updates cache)
        writeCachedConfig(config);
        logFn(`Cleared all terminals from restore-terminals.json at: ${jsonPath}`);
        return true;
    } catch (error) {
        logFn(`Error clearing terminals from JSON (path: ${jsonPath}): ${error}`);
        return false;
    }
}
