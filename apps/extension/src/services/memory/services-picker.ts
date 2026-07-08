/**
 * Services Picker
 *
 * VS Code QuickPick interface for users to opt-in to memory services.
 * Similar to the theme picker flow - shows available services with
 * descriptions and lets users enable/disable them.
 *
 * Services are stored in VS Code workspace settings:
 * - immorterm.services.memory.enabled (native binary)
 *
 * Note: Graph (entity relationships) is always enabled when memory is on.
 */

import * as vscode from 'vscode';
import { setServiceEnabled, isServiceEnabled, isProTier, readProjectConfig } from '../../utils/immorterm-config';
import { getStableProjectId } from './project-identity';
import { pickDigestLlm, type DigestProvider } from './digest-llm-picker';

/** Cached workspace path, set during activation */
let cachedWorkspacePath: string | undefined;

/**
 * Set the workspace path for config.json-based service checks.
 * Called from activation.ts during workspace initialization.
 */
export function setMemoryWorkspacePath(wsPath: string): void {
  cachedWorkspacePath = wsPath;
}

/**
 * Configuration for a memory service option
 */
interface ServiceOption {
  /** Unique identifier for the service */
  id: string;
  /** Display label with VS Code icon */
  label: string;
  /** Short description shown inline */
  description: string;
  /** Detailed description shown on hover/expand */
  detail: string;
  /** Whether this should be pre-selected */
  defaultEnabled: boolean;
  /** If true, only shown when user has an active Pro license */
  proOnly?: boolean;
}

/**
 * Available memory services.
 *
 * Memory is recommended and pre-selected (~15MB native binary).
 * Graph enables entity relationship tracking for power users.
 */
const AVAILABLE_SERVICES: ServiceOption[] = [
  {
    id: 'memory',
    label: '$(database) Persistent Memory',
    description: 'Semantic search for decisions & context (~15 MB)',
    detail: 'Stores context, decisions, and learnings with vector search and full-text indexing. ' +
            'Claude can recall past decisions, architectural choices, and project context across sessions.',
    defaultEnabled: true, // Pre-selected - recommended for most users
  },
  {
    // MCP Gateway is free forever — never gated behind a tier.
    id: 'mcpGateway',
    label: '$(plug) MCP Gateway',
    description: 'Route AI tools through a single HTTP proxy',
    detail: 'Eliminates per-session MCP process spawning. Routes all AI tools through ' +
            'a single persistent gateway for faster tool responses.',
    defaultEnabled: false,
  },
];

/**
 * QuickPick item that extends VS Code's interface
 */
interface ServiceQuickPickItem extends vscode.QuickPickItem {
  id: string;
  picked: boolean;
}

/**
 * Show the services picker dialog.
 * Called when ImmorTerm is first enabled or via command.
 *
 * @returns Array of enabled service IDs, or empty array if cancelled
 *
 * @example
 * const enabled = await showServicesPicker();
 * // User selected: ['memory']
 * // User cancelled: []
 */
export async function showServicesPicker(): Promise<string[]> {
  // Filter out Pro-only services when user is on free tier
  const isPro = isProTier();
  const visibleServices = AVAILABLE_SERVICES.filter(s => !s.proOnly || isPro);

  // Convert to QuickPick items
  const items: ServiceQuickPickItem[] = visibleServices.map(service => ({
    id: service.id,
    label: service.label,
    description: service.description,
    detail: service.detail,
    picked: service.defaultEnabled,
  }));

  // Show multi-select picker
  const selected = await vscode.window.showQuickPick(items, {
    canPickMany: true,
    title: 'ImmorTerm Memory Services',
    placeHolder: 'Select services to enable (press Enter to confirm)',
    ignoreFocusOut: true, // Don't close on focus loss
  });

  // User cancelled
  if (!selected) {
    return [];
  }

  const enabledServices = selected.map(item => item.id);

  // Save to workspace settings
  const config = vscode.workspace.getConfiguration('immorterm');
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];

  for (const service of AVAILABLE_SERVICES) {
    const isEnabled = enabledServices.includes(service.id);
    await config.update(
      `services.${service.id}.enabled`,
      isEnabled,
      vscode.ConfigurationTarget.Workspace
    );

    // Write-through to config.json
    if (workspaceFolder) {
      const projectId = getStableProjectId(workspaceFolder.uri.fsPath);
      setServiceEnabled(workspaceFolder.uri.fsPath, service.id as 'memory' | 'mcpGateway' | 'graph', isEnabled, projectId);
    }
  }

  // Show confirmation
  if (enabledServices.length > 0) {
    const serviceNames = enabledServices.map(id => {
      const service = AVAILABLE_SERVICES.find(s => s.id === id);
      return service?.id === 'memory' ? 'Persistent Memory' : 'Relationship Tracking';
    });

    vscode.window.showInformationMessage(
      `ImmorTerm Memory: Enabled ${serviceNames.join(' & ')}. ` +
      `Services will start when Claude is detected.`
    );
  }

  // Phase A T11 — when memory is enabled, immediately walk the user
  // through the digest-LLM picker so the digester has a working
  // provider/model out of the box. We honor any pre-existing choice
  // by passing it as the initial selection.
  if (enabledServices.includes('memory') && workspaceFolder) {
    const existing = readProjectConfig(workspaceFolder.uri.fsPath);
    const initialProvider = existing?.services?.digest?.provider as DigestProvider | undefined;
    const initialModel = existing?.services?.digest?.model;
    try {
      await pickDigestLlm({
        workspacePath: workspaceFolder.uri.fsPath,
        initialProvider,
        initialModel,
      });
      // Picker writes its own confirmation; cancel returns undefined and
      // we don't re-prompt here — user can run the menu entry later.
    } catch (err) {
      // Defensive: never let a picker bug block the services flow.
      console.error('[immorterm] digest LLM picker failed:', err);
    }
  }

  return enabledServices;
}

/**
 * Check if memory services are enabled for current workspace.
 * Reads from .immorterm/config.json (canonical source) via isServiceEnabled().
 * Falls back to VS Code settings if cachedWorkspacePath not set.
 *
 * @returns true if the 'memory' service is enabled
 */
export function isMemoryEnabled(): boolean {
  if (cachedWorkspacePath) {
    return isServiceEnabled(cachedWorkspacePath, 'memory');
  }
  // Fallback: try VS Code workspace folders
  try {
    const wsPath = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (wsPath) return isServiceEnabled(wsPath, 'memory');
  } catch {}
  return false;
}

/**
 * Graph is always enabled when memory is on.
 * Kept for backward compatibility with gpu-terminal.ts.
 */
export function isGraphEnabled(): boolean {
  return isMemoryEnabled();
}

/**
 * Check if user has made a services choice.
 * Used to determine if we should show the picker on first run.
 *
 * @returns true if the user has previously made a choice (even if they disabled all services)
 */
export function hasUserChosenServices(): boolean {
  const config = vscode.workspace.getConfiguration('immorterm');

  // Check if the setting has been explicitly set (not just default)
  // VS Code's inspect() returns undefined for defaultValue if never set
  const inspection = config.inspect<boolean>('services.memory.enabled');

  // If workspaceValue is defined, user has made a choice
  return inspection?.workspaceValue !== undefined;
}

/**
 * Get list of enabled services.
 *
 * @returns Array of enabled service IDs
 */
export function getEnabledServices(): string[] {
  const enabled: string[] = [];

  if (isMemoryEnabled()) {
    enabled.push('memory');
    enabled.push('graph'); // Graph is always on when memory is on
  }

  return enabled;
}

/**
 * Disable all memory services.
 * Called when user wants to turn off memory features.
 */
export async function disableAllServices(): Promise<void> {
  const config = vscode.workspace.getConfiguration('immorterm');
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];

  for (const service of AVAILABLE_SERVICES) {
    await config.update(
      `services.${service.id}.enabled`,
      false,
      vscode.ConfigurationTarget.Workspace
    );

    // Write-through to config.json
    if (workspaceFolder) {
      const projectId = getStableProjectId(workspaceFolder.uri.fsPath);
      setServiceEnabled(workspaceFolder.uri.fsPath, service.id as 'memory' | 'mcpGateway' | 'graph', false, projectId);
    }
  }
}

export default {
  showServicesPicker,
  isMemoryEnabled,
  isGraphEnabled,
  hasUserChosenServices,
  getEnabledServices,
  disableAllServices,
};
