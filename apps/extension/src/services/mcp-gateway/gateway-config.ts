import * as path from 'path';
import * as os from 'os';

/** Default gateway port */
export const GATEWAY_PORT = 9100;

/** State directory for gateway runtime files */
export const GATEWAY_STATE_DIR = path.join(os.homedir(), '.immorterm', 'mcp-gateway');

/** State file path */
export const GATEWAY_STATE_FILE = path.join(GATEWAY_STATE_DIR, 'state.json');

/** Gateway health endpoint */
export function getHealthUrl(port: number = GATEWAY_PORT): string {
  return `http://localhost:${port}/health`;
}

/** VS Code setting key for enabling the gateway */
export const SETTING_KEY = 'immorterm.services.mcpGateway.enabled';
