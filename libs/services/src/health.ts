/**
 * Health Check Helpers — IDE-Independent
 *
 * Generic health check utilities. Service-specific health checks
 * live in their respective modules (memory.ts, gateway.ts).
 */

import type { HealthResult } from "./types.js";

/** Wait for a service to become healthy */
export async function waitForHealthy(
	checkFn: () => Promise<HealthResult>,
	timeoutMs: number = 30000,
	intervalMs: number = 1000,
): Promise<boolean> {
	const startTime = Date.now();
	while (Date.now() - startTime < timeoutMs) {
		const result = await checkFn();
		if (result.healthy) return true;
		await new Promise((resolve) => setTimeout(resolve, intervalMs));
	}
	return false;
}
