/**
 * Feature Gate — Free vs Pro tier enforcement
 *
 * Nothing is fully locked out on Free — everything is usable but capped.
 * When the user hits a limit and data exists beyond it, show a nudge:
 * "N more results available with Pro" / "Upgrade to access full history"
 *
 * Tier definitions live in @immorterm/types (shared with MCP server + extension).
 * This module provides CLI-specific gate functions and UI.
 */

import { readGlobalConfig, writeGlobalConfig, IMMORTERM_GLOBAL_DIR } from "@immorterm/config";
import { isLicenseValidSync, needsRevalidation, validateLicense } from "@immorterm/license";
import { FREE_LIMITS, MEMORY_PRO_LIMITS, PRO_LIMITS, tierDisplayLabel } from "@immorterm/types";
import type { TierLimits } from "@immorterm/types";
import { existsSync } from "node:fs";
import { join } from "node:path";
import consola from "consola";
import pc from "picocolors";

/** Dev mode is enabled when ~/.immorterm/.dev sentinel file exists */
const isDevMode = existsSync(join(IMMORTERM_GLOBAL_DIR, ".dev"));

export type { TierLimits };

// ── Core Gate Functions ───────────────────────────────────────────

/** Check if the current license is Pro tier (cached LS validation) */
export function isPro(): boolean {
	const config = readGlobalConfig();
	const lic = config.license;

	// Dev override from config — only when ~/.immorterm/.dev exists
	if (isDevMode && lic.devTierOverride) {
		return lic.devTierOverride !== "free";
	}

	// Primary: verify key + instanceId + recent LS validation
	if (isLicenseValidSync(lic)) {
		return true;
	}

	// Trigger background revalidation if stale but has credentials
	if (needsRevalidation(lic)) {
		revalidateInBackground(lic.key!, lic.instanceId!);
		// Optimistic: treat as pro while revalidating
		return lic.status === "active";
	}

	return false;
}

/**
 * Fire-and-forget revalidation with LS API.
 * Updates config.json on success/failure.
 */
let _revalidating = false;
function revalidateInBackground(key: string, instanceId: string): void {
	if (_revalidating) return;
	_revalidating = true;

	validateLicense(key, instanceId)
		.then((result) => {
			const config = readGlobalConfig();
			if (result.success) {
				config.license.lastValidatedAt = new Date().toISOString();
				config.license.status = "active";
			} else {
				// License revoked or expired — downgrade
				config.license.status = "expired";
				config.license.lastValidatedAt = new Date().toISOString();
			}
			writeGlobalConfig(config);
		})
		.catch(() => {
			// Network error — don't downgrade, just leave stale
		})
		.finally(() => {
			_revalidating = false;
		});
}

/**
 * The active paid tier string ("pro" | "memory-pro" | ...), or "free".
 * Legacy licenses have no stored tier — they are full Pro.
 */
function currentTier(): string {
	if (!isPro()) return "free";
	const config = readGlobalConfig();
	if (isDevMode && config.license.devTierOverride) return config.license.devTierOverride;
	return config.license.tier ?? "pro";
}

/** Get the tier limits for the current user */
export function getLimits(): TierLimits {
	const tier = currentTier();
	if (tier === "free") return FREE_LIMITS;
	// memory-pro lifts memory caps only — themes/packs/etc. stay free-tier.
	if (tier === "memory-pro") return MEMORY_PRO_LIMITS;
	return PRO_LIMITS;
}

/** Get the tier label for display */
export function getTierLabel(): string {
	const tier = currentTier();
	return tier === "free" ? pc.dim("Free") : pc.green(tierDisplayLabel(tier));
}

// ── Gate Checks ─────────────────────────────────────────────────

export type GatedFeature = "knowledge_packs" | "dashboard_control";

const FEATURE_LABELS: Record<GatedFeature, string> = {
	knowledge_packs: "Knowledge Packs",
	dashboard_control: "Dashboard Controls",
};

/**
 * Hard gate — blocks the action entirely.
 * Used for features that don't have a free-tier equivalent (graph, knowledge packs).
 */
export function requirePro(feature: GatedFeature): boolean {
	// Gate on limits, not raw license validity — memory-pro is a valid paid
	// license but does NOT include these features.
	const limits = getLimits();
	const unlocked = feature === "knowledge_packs" ? limits.knowledgePacks : limits.dashboardControl;
	if (unlocked) return true;

	const label = FEATURE_LABELS[feature];
	consola.warn(`${pc.bold(label)} is a ${pc.magenta("Pro")} feature.`);
	showUpgradePrompt();
	return false;
}

/**
 * Soft nudge — the action proceeds but with a hint about hidden data.
 * Used when free-tier gets partial results and more exist.
 */
export function nudgeIfLimited(opts: {
	totalAvailable: number;
	returned: number;
	feature: string;
}): void {
	if (isPro()) return;
	if (opts.totalAvailable <= opts.returned) return;

	const hidden = opts.totalAvailable - opts.returned;
	consola.info("");
	consola.info(
		pc.dim(`  ${pc.magenta("+")}${hidden} more ${opts.feature} available with ${pc.magenta("Pro")}`),
	);
	consola.info(pc.dim(`  Upgrade: ${pc.cyan("https://immorterm.dev/pricing")}`));
}

/**
 * Nudge when a time-gated feature hits the retention wall.
 * Shows when memories exist beyond the free-tier time window.
 */
export function nudgeRetentionLimit(oldestAvailableHoursAgo: number): void {
	if (isPro()) return;

	const limits = getLimits();
	if (oldestAvailableHoursAgo <= limits.memoryRetentionHours) return;

	const days = Math.floor(oldestAvailableHoursAgo / 24);
	consola.info("");
	consola.info(
		pc.dim(
			`  ${pc.magenta("*")} Memories from ${days}+ days ago exist — upgrade to ${pc.magenta("Pro")} to access`,
		),
	);
	consola.info(pc.dim(`  Upgrade: ${pc.cyan("https://immorterm.dev/pricing")}`));
}

// ── Shared UI ───────────────────────────────────────────────────

export function showUpgradePrompt(): void {
	consola.info("");
	consola.info(`  Upgrade to Pro: ${pc.cyan("https://immorterm.dev/pricing")}`);
	consola.info(`  Activate key:   ${pc.dim("immorterm license activate <key>")}`);
	consola.info("");
}
