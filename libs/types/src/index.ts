export type LicenseTier = "free" | "pro" | "memory-pro" | (string & {});

/** Human-readable tier label for display surfaces (CLI, TUI, webviews). */
export function tierDisplayLabel(tier?: string | null): string {
	switch (tier) {
		case "memory-pro":
			return "Memory Pro";
		case "pro":
		case undefined:
		case null:
			// A valid paid license with no stored tier is legacy Pro.
			return "Pro";
		case "free":
			return "Free";
		default:
			return tier.charAt(0).toUpperCase() + tier.slice(1);
	}
}

export interface LicenseInfo {
	tier: LicenseTier;
	key?: string;
	email?: string;
	expiresAt?: string;
	machineId?: string;
	instanceId?: string;
	/** Dodo product id (prod_xxx); number kept for legacy LS configs on disk */
	productId?: string | number;
}

export interface UserConfig {
	license: LicenseInfo;
}

/** Tier-specific resource limits, consumed by CLI, MCP server, and extension */
export interface TierLimits {
	maxProjects: number;
	maxTerminals: number;
	maxThemes: number;
	/** Memory retrieval window in hours (Infinity = unlimited) */
	memoryRetentionHours: number;
	/** Max search results per query */
	memorySearchResults: number;
	/** Max sessions for recall (1 = last session only) */
	memorySessionRecall: number;
	knowledgePacks: boolean;
	graphSearch: boolean;
	maxGatewayServers: number;
	dashboardControl: boolean;
}

export const FREE_LIMITS: TierLimits = {
	maxProjects: 1,
	maxTerminals: 3,
	maxThemes: 5,
	memoryRetentionHours: 24,
	memorySearchResults: 3,
	memorySessionRecall: 1,
	knowledgePacks: false,
	graphSearch: false,
	maxGatewayServers: 2,
	dashboardControl: false,
};

export const PRO_LIMITS: TierLimits = {
	maxProjects: Infinity,
	maxTerminals: Infinity,
	maxThemes: Infinity,
	memoryRetentionHours: Infinity,
	memorySearchResults: Infinity,
	memorySessionRecall: Infinity,
	knowledgePacks: true,
	graphSearch: true,
	maxGatewayServers: Infinity,
	dashboardControl: true,
};

/**
 * ImmorTerm Memory Pro ($9/mo) — memory caps lifted ONLY.
 * Themes, terminals, knowledge packs, etc. stay at free-tier levels.
 */
export const MEMORY_PRO_LIMITS: TierLimits = {
	...FREE_LIMITS,
	memoryRetentionHours: Infinity,
	memorySearchResults: Infinity,
	memorySessionRecall: Infinity,
};
