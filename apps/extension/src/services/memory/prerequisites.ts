/**
 * Prerequisites Checker
 *
 * Verifies that the native memory binary is available before starting services.
 * Checks:
 * - Binary exists at ~/.immorterm/bin/immorterm-memory
 * - Port 8765 availability (memory service REST + MCP)
 */

import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import * as net from 'net';
import * as os from 'os';

import { getMemoryPort } from './native-memory-manager';

/** Port used by the memory service (dynamic from state.json) */
const MEMORY_PORT = getMemoryPort();

/** Ports for legacy Docker services (Qdrant, Neo4j) used by health checks */
export const PORTS = {
  QDRANT: 16333,
  NEO4J_HTTP: 17474,
} as const;

/** Paths for ImmorTerm binaries and data */
export const PATHS = {
  /** Base directory for ImmorTerm data */
  BASE: path.join(os.homedir(), '.immorterm'),
  /** Binary directory */
  BIN: path.join(os.homedir(), '.immorterm', 'bin'),
  /** Memory binary */
  MEMORY_BINARY: path.join(os.homedir(), '.immorterm', 'bin', 'immorterm-memory'),
  /** Memory data (SQLite + vector index) */
  MEMORY_DATA: path.join(os.homedir(), '.immorterm', 'memory'),
} as const;

/**
 * Result of prerequisite checks
 */
export interface PrerequisiteResult {
  /** Whether the memory binary exists */
  binaryExists: boolean;
  /** Whether the memory port (8765) is available */
  portFree: boolean;
  /** Summary: all prerequisites met for memory service */
  memoryReady: boolean;
}

/**
 * Check if a port is free (no process listening).
 */
async function isPortFree(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const server = net.createServer();

    server.once('error', () => {
      resolve(false);
    });

    server.once('listening', () => {
      server.close();
      resolve(true);
    });

    server.listen(port, '127.0.0.1');
  });
}

/**
 * Check if the memory binary exists and is executable.
 */
function checkBinaryExists(): boolean {
  try {
    const stats = fs.statSync(PATHS.MEMORY_BINARY);
    return stats.isFile() && (stats.mode & fs.constants.X_OK) !== 0;
  } catch {
    return false;
  }
}

/**
 * Run all prerequisite checks.
 */
export async function checkPrerequisites(): Promise<PrerequisiteResult> {
  const portFree = await isPortFree(MEMORY_PORT);
  const binaryExists = checkBinaryExists();

  return {
    binaryExists,
    portFree,
    // Port being in use is OK — the memory service may already be running
    memoryReady: binaryExists || !portFree,
  };
}

/**
 * Ensure the ImmorTerm directories exist.
 */
export function ensureDirectories(): void {
  const dirs = [PATHS.BIN, PATHS.MEMORY_DATA];

  for (const dir of dirs) {
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }
  }
}

/**
 * Check if prerequisites are met and show appropriate prompts.
 * Call this when memory services are enabled.
 *
 * @returns true if minimum prerequisites are met
 */
export async function validateAndPrompt(): Promise<boolean> {
  const result = await checkPrerequisites();

  if (!result.memoryReady) {
    const action = await vscode.window.showWarningMessage(
      'Memory binary not found. Install via: npx immorterm memory install',
      'Copy Command',
      'Continue Anyway'
    );
    if (action === 'Copy Command') {
      await vscode.env.clipboard.writeText('npx immorterm memory install');
    }
  }

  // Always return true — memory hooks can be installed even without the binary
  return true;
}

export default {
  PATHS,
  checkPrerequisites,
  ensureDirectories,
  validateAndPrompt,
};
