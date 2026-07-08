/**
 * @immorterm/tier-config — Single source of truth for tier limits.
 *
 * This JSON file is consumed by:
 *   - TypeScript services (import from this package)
 *   - Rust memory service (include_str! at compile time)
 *   - API tier-config endpoint (serves these values)
 *
 * To change free tier limits, edit ../config.json.
 */

import config from "../config.json";

export interface TierLimits {
	memorySearchResults: number | null;
	memoryRetentionHours: number | null;
}

export interface TierConfig {
	free: TierLimits;
	pro: TierLimits;
	/** Memory-only SKU — memory caps identical to pro; everything else stays free-tier. */
	"memory-pro": TierLimits;
}

export const TIER_CONFIG: TierConfig = config;

export const FREE_LIMITS = config.free;
export const PRO_LIMITS = config.pro;
export const MEMORY_PRO_LIMITS = config["memory-pro"];
