/** Logger interface — CLI passes consola, extension passes output channel */
export interface Logger {
	info(message: string): void;
	warn(message: string): void;
	error(message: string): void;
}

/** Native memory service state */
export interface MemoryState {
	/** Memory service process is running */
	running: boolean;
	/** REST API is healthy */
	apiHealthy: boolean;
	/** MCP endpoint is healthy */
	mcpHealthy: boolean;
	/** When service was started */
	startedAt?: number;
	/** Last error message */
	lastError?: string;
}

/** MCP Gateway state */
export interface GatewayState {
	/** Gateway process is running */
	running: boolean;
	/** Gateway API is healthy */
	healthy: boolean;
	/** PID of the gateway process */
	pid?: number;
	/** Port the gateway is listening on */
	port: number;
	/** Number of managed servers */
	serverCount?: number;
	/** Number of active child processes */
	activeChildren?: number;
	/** Memory usage in MB */
	memoryMB?: number;
	/** Last error */
	lastError?: string;
}

/** Health check result for a single service */
export interface HealthResult {
	/** Whether the service is healthy/responsive */
	healthy: boolean;
	/** Response time in milliseconds */
	responseTimeMs?: number;
	/** Error message if unhealthy */
	error?: string;
	/** Version info if available */
	version?: string;
}
