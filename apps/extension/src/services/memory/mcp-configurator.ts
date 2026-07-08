/**
 * MCP Configurator
 *
 * Auto-configures Claude Code's MCP settings to connect to OpenMemory.
 * Uses Streamable HTTP transport (single POST endpoint, no persistent connections).
 *
 * Benefits of Streamable HTTP (vs deprecated SSE):
 * - Single endpoint per server (no dual /sse + /messages)
 * - No persistent connections — standard HTTP request/response
 * - Built-in resumability support
 * - Works behind proxies and load balancers
 *
 * MCP config location: {project}/.mcp.json (project-scoped MCP servers)
 * Format: { "mcpServers": { "immorterm-memory": { "type": "http", "url": "..." } } }
 *
 * IMPORTANT: Claude Code reads MCP server definitions from .mcp.json (project scope)
 * or ~/.claude.json (user scope) — NOT from .claude/settings.local.json.
 * settings.local.json is only for permissions, hooks, and settings.
 *
 * Memory isolation: Each project gets its own user_id in the URL path,
 * so memories are scoped per-project with zero extra resource cost.
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { getMemoryPort } from './native-memory-manager';

/**
 * MCP server configuration (Streamable HTTP transport — Claude Code format)
 * Claude Code uses flat format: { "type": "http", "url": "..." }
 */
interface MCPServerConfigHTTP {
  type: 'http';
  url: string;
}

/**
 * Legacy SSE config (kept for migration detection)
 */
interface MCPServerConfigSSE {
  type: 'sse';
  url: string;
}

/**
 * MCP server configuration (stdio transport)
 */
interface MCPServerConfigStdio {
  type: 'stdio';
  command: string;
  args: string[];
  env?: Record<string, string>;
}

type MCPServerConfig = MCPServerConfigHTTP | MCPServerConfigSSE | MCPServerConfigStdio;

/**
 * Claude Code MCP configuration structure (.mcp.json)
 */
interface ClaudeConfig {
  mcpServers?: Record<string, MCPServerConfig>;
  [key: string]: unknown;
}

/** Name of our MCP server in the config */
const MCP_SERVER_NAME = 'immorterm-memory';

/** MCP client name in URL path */
const OPENMEMORY_CLIENT_NAME = 'claude-code';

/**
 * Build the OpenMemory Streamable HTTP URL for a given project ID.
 * The project ID becomes the user_id in the URL path, providing per-project isolation.
 *
 * URL format: /mcp/{client_name}/{user_id}
 * (previously: /mcp/{client_name}/sse/{user_id} with SSE transport)
 */
function buildMCPUrl(projectId: string): string {
  return `http://127.0.0.1:${getMemoryPort()}/mcp/${OPENMEMORY_CLIENT_NAME}/${projectId}`;
}

/**
 * Get the path to the project-scoped MCP config file.
 *
 * Claude Code reads MCP servers from .mcp.json (project scope).
 * NOT from .claude/settings.local.json (that's for permissions/hooks only).
 *
 * @param projectPath Path to the project root
 * @returns Absolute path to {project}/.mcp.json
 */
export function getMCPConfigPath(projectPath?: string): string {
  if (projectPath) {
    return path.join(projectPath, '.mcp.json');
  }
  // Fallback to global for backwards compatibility (reading only)
  return path.join(os.homedir(), '.claude.json');
}

/**
 * Legacy config path (.claude/settings.local.json) — used for cleanup only.
 */
function getLegacyMCPConfigPath(projectPath: string): string {
  return path.join(projectPath, '.claude', 'settings.local.json');
}

/**
 * Read existing project-scoped Claude Code configuration from disk.
 *
 * @param projectPath Path to the project root
 * @returns ClaudeConfig object (empty mcpServers if file doesn't exist)
 */
export function readMCPConfig(projectPath?: string): ClaudeConfig {
  const configPath = getMCPConfigPath(projectPath);

  try {
    if (fs.existsSync(configPath)) {
      const content = fs.readFileSync(configPath, 'utf8');
      return JSON.parse(content) as ClaudeConfig;
    }
  } catch (error) {
    console.error('[mcp] Failed to read Claude config:', error);
  }

  return { mcpServers: {} };
}

/**
 * Write MCP configuration to .mcp.json at project root.
 *
 * @param config MCP configuration object
 * @param projectPath Path to the project root
 */
function writeMCPConfig(config: ClaudeConfig, projectPath: string): void {
  const configPath = getMCPConfigPath(projectPath);

  try {
    fs.writeFileSync(configPath, JSON.stringify(config, null, 2), 'utf8');
    console.log(`[mcp] Config written to ${configPath}`);
  } catch (error) {
    console.error('[mcp] Failed to write MCP config:', error);
    throw error;
  }
}

/**
 * Configure OpenMemory MCP server in the project's .mcp.json.
 * Uses Streamable HTTP transport to connect to the shared OpenMemory server.
 *
 * Memory isolation: projectId becomes the user_id in the URL path.
 * One Docker container serves all projects — zero extra resource cost.
 *
 * Also migrates: removes stale mcpServers from .claude/settings.local.json
 * (legacy location that Claude Code doesn't read for MCP servers).
 *
 * @param projectPath Path to the project root
 * @param projectId The stable project ID (used as user_id in the URL path)
 * @returns true if configuration succeeded
 */
export async function configureOpenMemoryMCP(projectPath: string, projectId: string): Promise<boolean> {
  try {
    const config = readMCPConfig(projectPath);

    if (!config.mcpServers) {
      config.mcpServers = {};
    }

    // Streamable HTTP format: { type: "http", url }
    // Project isolation via URL path segment: /mcp/claude-code/{projectId}
    config.mcpServers[MCP_SERVER_NAME] = {
      type: 'http',
      url: buildMCPUrl(projectId),
    };

    writeMCPConfig(config, projectPath);

    // Migrate: remove stale mcpServers from legacy settings.local.json
    migrateMCPFromSettingsLocal(projectPath);

    console.log(`[mcp] Configured ${MCP_SERVER_NAME} with Streamable HTTP transport for project: ${projectId}`);
    return true;
  } catch (error) {
    console.error('[mcp] Failed to configure OpenMemory MCP:', error);
    return false;
  }
}

/**
 * Remove OpenMemory MCP server from the project's .mcp.json.
 * Called when user disables memory services.
 * Also cleans up legacy locations (settings.local.json, ~/.claude.json).
 *
 * @param projectPath Path to the project root
 * @returns true if removal succeeded
 */
export function removeOpenMemoryMCP(projectPath: string): boolean {
  try {
    // Remove from .mcp.json (correct location)
    const mcpConfig = readMCPConfig(projectPath);

    if (mcpConfig.mcpServers && mcpConfig.mcpServers[MCP_SERVER_NAME]) {
      delete mcpConfig.mcpServers[MCP_SERVER_NAME];

      // Clean up empty mcpServers object
      if (Object.keys(mcpConfig.mcpServers).length === 0) {
        delete mcpConfig.mcpServers;
      }

      writeMCPConfig(mcpConfig, projectPath);
      console.log(`[mcp] Removed ${MCP_SERVER_NAME} from .mcp.json`);
    }

    // Clean up legacy locations
    migrateMCPFromSettingsLocal(projectPath);
    removeFromGlobalConfig();

    return true;
  } catch (error) {
    console.error('[mcp] Failed to remove OpenMemory MCP:', error);
    return false;
  }
}

/** Name of the terminal-control (GPU/AI canvas) MCP server in the config. */
const TERMINAL_MCP_SERVER_NAME = 'immorterm';

/**
 * Resolve the deployed immorterm-ai binary that serves the terminal-control MCP.
 * Always the stable install target — NOT a dev `target/debug` build (the
 * immorterm monorepo keeps its own debug-path entry, which we never clobber).
 */
function terminalMCPCommand(): string {
  return path.join(os.homedir(), '.immorterm', 'bin', 'immorterm-ai');
}

/**
 * Register the `immorterm` terminal-control MCP server in the project's
 * .mcp.json. This is the GPU/AI-canvas tool surface (screenshot, read_screen,
 * tasks, draw_html, …) served by `immorterm-ai mcp serve` over stdio.
 *
 * Gating: callers MUST invoke this ONLY for ImmorTerm-enabled workspaces
 * (master `immorterm.enabled === true`). Because .mcp.json is project-scoped,
 * a project that never enabled ImmorTerm simply never gets this entry, so the
 * tools are never loaded there.
 *
 * Add-if-absent: if an `immorterm` entry already exists (e.g. the monorepo's
 * hand-authored dev `target/debug` entry), it is left untouched.
 *
 * When the MCP Gateway is enabled it transparently wraps this stdio server as
 * a shared singleton; when it's off, Claude Code spawns it directly. Either
 * way the registration is identical.
 *
 * @param projectPath Path to the project root
 * @returns true if the entry is present after the call (added or already there)
 */
export function configureTerminalMCP(projectPath: string): boolean {
  try {
    const config = readMCPConfig(projectPath);

    if (!config.mcpServers) {
      config.mcpServers = {};
    }

    // Add-if-absent — never overwrite an existing (possibly dev-build) entry.
    if (config.mcpServers[TERMINAL_MCP_SERVER_NAME]) {
      return true;
    }

    config.mcpServers[TERMINAL_MCP_SERVER_NAME] = {
      type: 'stdio',
      command: terminalMCPCommand(),
      args: ['mcp', 'serve'],
    };

    writeMCPConfig(config, projectPath);
    console.log(`[mcp] Configured ${TERMINAL_MCP_SERVER_NAME} (stdio terminal MCP) for ${projectPath}`);
    return true;
  } catch (error) {
    console.error('[mcp] Failed to configure terminal MCP:', error);
    return false;
  }
}

/**
 * Remove the `immorterm` terminal-control MCP server from the project's
 * .mcp.json. Called when ImmorTerm is disabled for the workspace, so the
 * tools stop loading there.
 *
 * Only removes our deployed-binary stdio entry — a dev `target/debug` entry
 * (different command path) is preserved so the monorepo keeps dogfooding.
 *
 * @param projectPath Path to the project root
 * @returns true if removal succeeded (or nothing to remove)
 */
export function removeTerminalMCP(projectPath: string): boolean {
  try {
    const config = readMCPConfig(projectPath);
    const entry = config.mcpServers?.[TERMINAL_MCP_SERVER_NAME] as MCPServerConfigStdio | undefined;

    // Only remove the entry we manage (deployed-binary path). Leave a
    // hand-authored dev entry (e.g. target/debug) in place.
    if (entry && entry.command === terminalMCPCommand()) {
      delete config.mcpServers![TERMINAL_MCP_SERVER_NAME];
      if (Object.keys(config.mcpServers!).length === 0) {
        delete config.mcpServers;
      }
      writeMCPConfig(config, projectPath);
      console.log(`[mcp] Removed ${TERMINAL_MCP_SERVER_NAME} from .mcp.json`);
    }

    return true;
  } catch (error) {
    console.error('[mcp] Failed to remove terminal MCP:', error);
    return false;
  }
}

/**
 * Check if OpenMemory MCP server is configured for this project.
 *
 * @param projectPath Path to the project root
 * @returns true if server is in the project's config
 */
export function isOpenMemoryMCPConfigured(projectPath?: string): boolean {
  const config = readMCPConfig(projectPath);
  return Boolean(config.mcpServers?.[MCP_SERVER_NAME]);
}

/**
 * Update the project ID in MCP config.
 * The project ID is embedded in the URL path.
 *
 * @param projectPath Path to the project root
 * @param projectId New project ID
 * @returns true if update succeeded
 */
export function updateMCPProjectId(projectPath: string, projectId: string): boolean {
  try {
    const config = readMCPConfig(projectPath);
    const server = config.mcpServers?.[MCP_SERVER_NAME] as MCPServerConfigHTTP | MCPServerConfigSSE | undefined;

    if (server) {
      if (server.type === 'http' || server.type === 'sse') {
        // Migrate SSE → HTTP while updating project ID
        (server as any).type = 'http';
        server.url = buildMCPUrl(projectId);
        writeMCPConfig(config, projectPath);
        console.log(`[mcp] Updated project ID to: ${projectId}`);
        return true;
      }
    }

    return false;
  } catch (error) {
    console.error('[mcp] Failed to update project ID:', error);
    return false;
  }
}

/**
 * Get the currently configured project ID from MCP config.
 * Extracts from the URL path: /mcp/{client}/{user_id}
 * Also handles legacy SSE format: /mcp/{client}/sse/{user_id}
 *
 * @param projectPath Path to the project root
 * @returns Project ID or null if not configured
 */
export function getMCPProjectId(projectPath?: string): string | null {
  const config = readMCPConfig(projectPath);
  const server = config.mcpServers?.[MCP_SERVER_NAME] as MCPServerConfigHTTP | MCPServerConfigSSE | undefined;

  if (server && (server.type === 'http' || server.type === 'sse') && server.url) {
    // Try new format first: /mcp/{client}/{user_id} (no /sse/ segment)
    const httpMatch = server.url.match(/\/mcp\/[^/]+\/([^/]+)$/);
    if (httpMatch && httpMatch[1] !== 'sse') {
      return httpMatch[1];
    }
    // Legacy SSE format: /mcp/{client}/sse/{user_id}
    const sseMatch = server.url.match(/\/sse\/([^/]+)$/);
    return sseMatch?.[1] ?? null;
  }

  return null;
}

/**
 * Migrate: remove mcpServers from legacy .claude/settings.local.json.
 * Old versions incorrectly wrote MCP config there. Claude Code only reads
 * MCP servers from .mcp.json (project scope) or ~/.claude.json (user scope).
 */
function migrateMCPFromSettingsLocal(projectPath: string): void {
  const legacyPath = getLegacyMCPConfigPath(projectPath);
  try {
    if (fs.existsSync(legacyPath)) {
      const content = JSON.parse(fs.readFileSync(legacyPath, 'utf8'));
      if (content.mcpServers) {
        delete content.mcpServers;
        fs.writeFileSync(legacyPath, JSON.stringify(content, null, 2), 'utf8');
        console.log('[mcp] Migrated: removed mcpServers from legacy settings.local.json');
      }
    }
  } catch {
    // Best-effort migration
  }
}

/**
 * Remove immorterm-memory from the global ~/.claude.json if present.
 * This is a one-time migration cleanup — old versions wrote to global config.
 */
function removeFromGlobalConfig(): void {
  const globalPath = path.join(os.homedir(), '.claude.json');
  try {
    if (fs.existsSync(globalPath)) {
      const content = JSON.parse(fs.readFileSync(globalPath, 'utf8'));
      if (content.mcpServers?.[MCP_SERVER_NAME]) {
        delete content.mcpServers[MCP_SERVER_NAME];
        fs.writeFileSync(globalPath, JSON.stringify(content, null, 2), 'utf8');
        console.log('[mcp] Cleaned up legacy global MCP config');
      }
    }
  } catch {
    // Best-effort cleanup — don't fail if global config can't be updated
  }
}

/**
 * One-time migration: remove immorterm-memory from global ~/.claude.json.
 * Call this during activation to clean up old installations that used global scope.
 */
export function migrateFromGlobalConfig(): void {
  removeFromGlobalConfig();
}

/**
 * Refresh MCP config to trigger Claude Code to reconnect.
 *
 * After an OpenMemory container restart, existing connections may be stale.
 * Claude Code watches .mcp.json for changes — writing a `_reconnect_ts`
 * field forces it to re-read the config and establish a fresh connection.
 *
 * @param projectPath Path to the project root
 */
export function refreshMcpConfig(projectPath: string): void {
  try {
    const config = readMCPConfig(projectPath);
    (config as Record<string, unknown>)._reconnect_ts = Date.now();
    const configPath = getMCPConfigPath(projectPath);
    fs.writeFileSync(configPath, JSON.stringify(config, null, 2), 'utf8');
    console.log('[mcp] Refreshed config to trigger MCP reconnect');
  } catch (error) {
    console.error('[mcp] Failed to refresh MCP config:', error);
  }
}

export default {
  getMCPConfigPath,
  readMCPConfig,
  configureOpenMemoryMCP,
  removeOpenMemoryMCP,
  isOpenMemoryMCPConfigured,
  updateMCPProjectId,
  getMCPProjectId,
  migrateFromGlobalConfig,
  refreshMcpConfig,
};
