/**
 * ImmorTerm Analytics — PostHog Integration
 *
 * Provides event tracking, feature flags, and A/B testing.
 * Uses PostHog's server-side SDK (posthog-node).
 *
 * - Public client key (not a secret) — safe to embed
 * - Respects `telemetry: false` in global config (opt-out)
 * - User ID = machine fingerprint from @immorterm/license
 */

import { readGlobalConfig } from "@immorterm/config";
import { getMachineFingerprint } from "@immorterm/license";
import { PostHog } from "posthog-node";

// ── Constants ──────────────────────────────────────────────────────

/** PostHog public client key — injected via env */
const POSTHOG_API_KEY = process.env["POSTHOG_API_KEY"] ?? "";

/** PostHog host */
const POSTHOG_HOST = process.env["POSTHOG_HOST"] || "https://us.i.posthog.com";

// ── Client Management ──────────────────────────────────────────────

let client: PostHog | null = null;
let distinctId: string | null = null;

/** Check if telemetry is opted out */
function isTelemetryDisabled(): boolean {
	const config = readGlobalConfig();
	return config.telemetry === false;
}

/** Get or create the PostHog client */
function getClient(): PostHog | null {
	if (isTelemetryDisabled()) return null;
	if (!POSTHOG_API_KEY) return null;

	if (!client) {
		client = new PostHog(POSTHOG_API_KEY, {
			host: POSTHOG_HOST,
			flushAt: 10,
			flushInterval: 30000,
		});
	}
	return client;
}

/** Get the distinct user ID (machine fingerprint) */
async function getDistinctId(): Promise<string> {
	if (!distinctId) {
		distinctId = await getMachineFingerprint();
	}
	return distinctId;
}

// ── Event Tracking ─────────────────────────────────────────────────

/** Identify the user with their properties */
export async function identify(properties?: Record<string, any>): Promise<void> {
	const ph = getClient();
	if (!ph) return;

	const id = await getDistinctId();
	const config = readGlobalConfig();

	ph.identify({
		distinctId: id,
		properties: {
			os: process.platform,
			arch: process.arch,
			nodeVersion: process.version,
			tier: config.license.status ?? "free",
			...properties,
		},
	});
}

/** Track an event */
export async function track(
	event: string,
	properties?: Record<string, any>,
): Promise<void> {
	const ph = getClient();
	if (!ph) return;

	const id = await getDistinctId();
	ph.capture({
		distinctId: id,
		event,
		properties: {
			os: process.platform,
			version: process.env["IMMORTERM_VERSION"] ?? "unknown",
			...properties,
		},
	});
}

// ── Feature Flags ──────────────────────────────────────────────────

/**
 * Check if a feature flag is enabled for this user.
 * Used for gradual rollouts and kill switches.
 */
export async function isFeatureEnabled(flagKey: string): Promise<boolean> {
	const ph = getClient();
	if (!ph) return false;

	const id = await getDistinctId();
	return (await ph.isFeatureEnabled(flagKey, id)) ?? false;
}

/**
 * Get a feature flag value (string/boolean/number).
 * Used for multivariate flags and A/B test variants.
 */
export async function getFeatureFlag(flagKey: string): Promise<string | boolean | undefined> {
	const ph = getClient();
	if (!ph) return undefined;

	const id = await getDistinctId();
	return ph.getFeatureFlag(flagKey, id);
}

/**
 * Get the payload attached to a feature flag.
 * Used for A/B testing — payloads carry variant-specific config
 * (e.g., different UI copy, pricing tiers, behavior toggles).
 */
export async function getPayload(flagKey: string): Promise<any> {
	const ph = getClient();
	if (!ph) return undefined;

	const id = await getDistinctId();
	return ph.getFeatureFlagPayload(flagKey, id);
}

/**
 * Get all feature flags for this user in one call.
 * More efficient than checking flags individually.
 */
export async function getAllFlags(): Promise<Record<string, string | boolean>> {
	const ph = getClient();
	if (!ph) return {};

	const id = await getDistinctId();
	return ph.getAllFlags(id);
}

// ── Lifecycle ──────────────────────────────────────────────────────

/** Flush pending events and shut down the client */
export async function shutdown(): Promise<void> {
	if (client) {
		await client.shutdown();
		client = null;
	}
}
