/**
 * MCP Gateway Service
 *
 * Singleton MCP gateway that reduces memory usage by ~90%
 * by sharing MCP server processes across Claude sessions.
 *
 * Architecture:
 * - One gateway process runs all stdio MCP servers as shared children
 * - Claude sessions connect via HTTP instead of spawning processes
 * - Stateless servers (context7, tavily) share one child
 * - Stateful servers (sequential-thinking) get per-session children
 */

export {
  GATEWAY_PORT,
  GATEWAY_STATE_DIR,
  GATEWAY_STATE_FILE,
  getHealthUrl,
  SETTING_KEY,
} from './gateway-config';

export {
  initGatewayManager,
  isGatewayEnabled,
  checkGatewayHealth,
  startGateway,
  stopGateway,
  checkGatewayLifecycle,
  rewriteProjectMcpConfig,
  getMCPGatewayState,
  getGatewayStatusText,
  killOldMcpProcesses,
  cleanupGatewaySessionByPid,
} from './gateway-manager';
export type { GatewayState } from './gateway-manager';

export { openGatewayDashboard } from './dashboard-panel';
