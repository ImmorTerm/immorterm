/**
 * Memory Services
 *
 * Persistent memory services for ImmorTerm that give Claude Code
 * semantic memory across sessions.
 *
 * Architecture (v3 - Native Rust):
 * - immorterm-memory: Single native binary (~15MB)
 *   - Unified SQLite (memories, code changes, sessions, entities, git commits)
 *   - ONNX Runtime embeddings (all-MiniLM-L6-v2)
 *   - usearch HNSW vector index (in-process, no separate DB)
 *   - SQLite FTS5 full-text search
 *   - SQLite entity graph (no Neo4j)
 *   - axum REST API + MCP Streamable HTTP
 *
 * Flow:
 * 1. User opts-in via services picker (showServicesPicker)
 * 2. Extension finds & starts native binary (~2s cold start)
 * 3. MCP configured with Streamable HTTP transport
 * 4. Guidance hook installed to remind Claude about memory tools
 * 5. Claude can now actively search/save memories mid-conversation
 */

// Project Identity
export {
  getStableProjectId,
} from './project-identity';

// Services Picker (user opt-in)
export {
  showServicesPicker,
  isMemoryEnabled,
  setMemoryWorkspacePath,
  isGraphEnabled,
  hasUserChosenServices,
  getEnabledServices,
  disableAllServices,
} from './services-picker';

// Native Memory Manager
export {
  findBinary,
  checkNativeHealth,
  startNativeMemory,
  stopNativeMemory,
  getNativeStatus,
  initNativeMemoryManager,
} from './native-memory-manager';

// Memory Manager (lifecycle orchestration)
export {
  initOpenMemoryManager,
  checkOpenMemoryHealth,
  waitForOpenMemory,
  startOpenMemory,
  stopOpenMemory,
  getOpenMemoryState,
  refreshOpenMemoryState,
  checkOpenMemoryLifecycle,
  runDiagnostics,
  tryAutoFix,
} from './openmemory-manager';
export type { OpenMemoryState } from './openmemory-manager';

// Hook Installer
export {
  installMemoryHooks,
  areHooksInstalled,
  removeMemoryHooks,
  updateHooksIfNeeded,
} from './hook-installer';

// Memory digest scheduling is owned entirely by the Rust singleton daemon
// `immorterm-digest` (apps/immorterm-ai/immorterm-digest). The extension-side
// setInterval fallback was removed once the daemon proved stable across
// VS Code / Tauri / CLI hosts.

// MCP Configurator (Streamable HTTP transport — per-project isolation)
export {
  getMCPConfigPath,
  readMCPConfig,
  configureOpenMemoryMCP,
  removeOpenMemoryMCP,
  configureTerminalMCP,
  removeTerminalMCP,
  isOpenMemoryMCPConfigured,
  updateMCPProjectId,
  getMCPProjectId,
  migrateFromGlobalConfig,
} from './mcp-configurator';

// Prerequisites
export {
  validateAndPrompt,
} from './prerequisites';

// Digest LLM Picker (Phase A T11 — wizard + menu + CLI)
export {
  pickDigestLlm,
  detectDefaultProvider,
  hasOnPath,
  resolveShimPath,
  parseOllamaList,
  parseLlmModelsList,
  persistDigestChoice,
  runShimTest,
} from './digest-llm-picker';
export type { DigestProvider, DigestLlmChoice, PickDigestLlmOpts } from './digest-llm-picker';
