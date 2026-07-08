import type { LicenseInfo, UserConfig } from "@immorterm/types";

const CONFIG_DIR = ".immorterm";
const CONFIG_FILE = "config.json";

/** Dodo Payments API base. Public license endpoints need no API key.
 *  Test mode: DODO_API_BASE=https://test.dodopayments.com */
function apiBase(): string {
	return process.env.DODO_API_BASE ?? "https://live.dodopayments.com";
}

function immortermApiUrl(): string {
	return process.env.IMMORTERM_API_URL ?? "https://api.immorterm.com";
}

/** DODO_MOCK=1 — offline path used only by tests (no Dodo account needed). */
function isMock(): boolean {
	return process.env.DODO_MOCK === "1";
}

/**
 * Cross-product ownership check. Dodo's validate endpoint returns only
 * {valid}, so ownership is verified ONCE at activation time against the
 * product_id in the activate response.
 * DODO_EXPECTED_PRODUCT_IDS: comma-separated prod_xxx ids. Empty = no check
 * (until the Dodo account exists and ids are known).
 */
function expectedProductIds(): string[] {
	return (process.env.DODO_EXPECTED_PRODUCT_IDS ?? "")
		.split(",")
		.map((s) => s.trim())
		.filter(Boolean);
}

/**
 * Resolve tier from the Dodo product id.
 * DODO_PRODUCT_TIER_MAP: JSON map, e.g. {"prod_abc":"pro","prod_def":"memory-pro"}.
 * "pro" = ImmorTerm Pro ($29, everything); "memory-pro" = ImmorTerm Memory Pro
 * ($9, memory caps lifted only). Falls back to "pro" — a valid key means a paid tier.
 */
export function resolveTier(productId?: string | null): string {
	if (productId) {
		try {
			const map = JSON.parse(process.env.DODO_PRODUCT_TIER_MAP ?? "{}");
			if (typeof map[productId] === "string") return map[productId];
		} catch {
			// malformed map — fall through to default
		}
	}
	return "pro";
}

/** Re-validate with the Dodo API every 6 hours */
const REVALIDATION_INTERVAL_MS = 6 * 60 * 60 * 1000;

export interface ActivateResult {
	success: boolean;
	license?: LicenseInfo;
	error?: string;
}

function activationError(status: number, body: string): string {
	switch (status) {
		case 403:
			return "License key is inactive.";
		case 404:
			return "License key not found.";
		case 422:
			return "Activation limit reached. Deactivate another machine first (immorterm license deactivate).";
		default:
			return `Activation failed (${status}): ${body}`;
	}
}

export async function activateLicense(key: string): Promise<ActivateResult> {
	const fingerprint = await getMachineFingerprint();

	if (isMock()) {
		return {
			success: true,
			license: {
				tier: resolveTier("prod_mock"),
				key,
				email: "mock@example.com",
				machineId: fingerprint,
				instanceId: "lki_mock",
				productId: "prod_mock",
			},
		};
	}

	const res = await fetch(`${apiBase()}/licenses/activate`, {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify({
			license_key: key,
			name: fingerprint,
		}),
	});

	if (!res.ok) {
		const body = await res.text().catch(() => "");
		return { success: false, error: activationError(res.status, body) };
	}

	const data = await res.json();

	// Verify the key belongs to one of our products (Dodo has no store_id;
	// validate returns only {valid}, so this check lives here).
	const productId: string | undefined = data.product?.product_id;
	const expected = expectedProductIds();
	if (expected.length > 0 && productId && !expected.includes(productId)) {
		return { success: false, error: "License key does not belong to ImmorTerm." };
	}

	const license: LicenseInfo = {
		tier: resolveTier(productId),
		key,
		email: data.customer?.email,
		// ponytail: Dodo exposes no expiry publicly — subscription-tied keys
		// simply flip valid:false on lapse, which is all the 6h loop needs.
		expiresAt: undefined,
		machineId: fingerprint,
		instanceId: data.id, // lki_* activation instance
		productId,
	};
	await saveLicense(license);

	// Non-blocking ping to our server for tracking
	fetch(`${immortermApiUrl()}/api/licenses/verify`, {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify({ license_key: key, machine_id: fingerprint }),
	}).catch(() => {}); // Silent failure — Dodo is source of truth

	return { success: true, license };
}

export async function validateLicense(
	key: string,
	instanceId?: string,
): Promise<ActivateResult> {
	if (isMock()) {
		return {
			success: true,
			license: {
				tier: resolveTier("prod_mock"),
				key,
				machineId: await getMachineFingerprint(),
				instanceId: instanceId ?? "lki_mock",
				productId: "prod_mock",
			},
		};
	}

	const body: Record<string, string> = { license_key: key };
	if (instanceId) {
		body.license_key_instance_id = instanceId;
	}

	const res = await fetch(`${apiBase()}/licenses/validate`, {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify(body),
	});

	if (!res.ok) {
		return { success: false, error: `Validation failed: ${res.status}` };
	}

	const data = await res.json();

	// Dodo validate returns ONLY {valid} — no product/email/expiry.
	// Ownership was checked at activation; callers only need success here.
	const fingerprint = await getMachineFingerprint();
	return {
		success: data.valid === true,
		license:
			data.valid === true
				? {
						tier: resolveTier(),
						key,
						machineId: fingerprint,
						instanceId,
					}
				: undefined,
	};
}

export async function deactivateLicense(
	key: string,
	instanceId?: string,
): Promise<{ success: boolean; error?: string }> {
	// Dodo requires the activation instance id (lki_*) to deactivate.
	if (!instanceId) {
		return {
			success: false,
			error:
				"No activation instance id stored — cannot deactivate remotely. Manage activations via the customer portal, or re-activate on this machine first.",
		};
	}

	if (isMock()) {
		return { success: true };
	}

	const res = await fetch(`${apiBase()}/licenses/deactivate`, {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify({
			license_key: key,
			license_key_instance_id: instanceId,
		}),
	});

	if (!res.ok) {
		return { success: false, error: `Deactivation failed: ${res.status}` };
	}

	await clearLicense();
	return { success: true };
}

/** Read license from ~/.immorterm/config.json (no API call) */
export async function getLicenseStatus(): Promise<LicenseInfo> {
	const config = await loadConfig();
	return config.license;
}

/**
 * Check if the stored license is valid without calling the API.
 *
 * Requires ALL of:
 * 1. license key present
 * 2. instance ID present (from Dodo activation)
 * 3. status is "active"
 * 4. last validation is within REVALIDATION_INTERVAL_MS
 *
 * If validation is stale, returns false (caller should trigger revalidation).
 */
export function isLicenseValidSync(license: {
	key?: string | null;
	instanceId?: string | null;
	status?: string | null;
	lastValidatedAt?: string | null;
}): boolean {
	if (!license.key || !license.instanceId || license.status !== "active") {
		return false;
	}

	if (!license.lastValidatedAt) {
		return false;
	}

	const lastValidated = new Date(license.lastValidatedAt).getTime();
	if (Number.isNaN(lastValidated)) {
		return false;
	}

	return Date.now() - lastValidated < REVALIDATION_INTERVAL_MS;
}

/**
 * Check if the stored license needs revalidation (stale but has credentials).
 * Returns true if we have key+instanceId but lastValidatedAt is expired/missing.
 */
export function needsRevalidation(license: {
	key?: string | null;
	instanceId?: string | null;
	status?: string | null;
	lastValidatedAt?: string | null;
}): boolean {
	if (!license.key || !license.instanceId) {
		return false;
	}

	if (!license.lastValidatedAt) {
		return true;
	}

	const lastValidated = new Date(license.lastValidatedAt).getTime();
	if (Number.isNaN(lastValidated)) {
		return true;
	}

	return Date.now() - lastValidated >= REVALIDATION_INTERVAL_MS;
}

/** Persist license info to ~/.immorterm/config.json */
export async function saveLicense(license: LicenseInfo): Promise<void> {
	const { homedir } = await import("node:os");
	const { join } = await import("node:path");
	const { mkdir, writeFile } = await import("node:fs/promises");

	const dir = join(homedir(), CONFIG_DIR);
	await mkdir(dir, { recursive: true });

	const config = await loadConfig();
	config.license = license;
	await writeFile(join(dir, CONFIG_FILE), JSON.stringify(config, null, 2));
}

/** Clear stored license (reverts to free tier) */
export async function clearLicense(): Promise<void> {
	await saveLicense({ tier: "free" });
}

/** Load config from disk, returning defaults if missing */
async function loadConfig(): Promise<UserConfig> {
	const { homedir } = await import("node:os");
	const { join } = await import("node:path");
	const { readFile } = await import("node:fs/promises");

	const configPath = join(homedir(), CONFIG_DIR, CONFIG_FILE);
	try {
		const raw = await readFile(configPath, "utf-8");
		return JSON.parse(raw) as UserConfig;
	} catch {
		return { license: { tier: "free" } };
	}
}

/**
 * Machine fingerprint using hardware UUID (more stable than hostname).
 * - macOS: IOPlatformUUID via ioreg
 * - Linux: /etc/machine-id
 * - Fallback: hostname + platform + username hash
 */
export async function getMachineFingerprint(): Promise<string> {
	const { createHash } = await import("node:crypto");
	const { platform } = await import("node:os");
	const { execFile } = await import("node:child_process");
	const { readFile } = await import("node:fs/promises");

	let raw: string | null = null;

	if (platform() === "darwin") {
		raw = await new Promise<string | null>((resolve) => {
			execFile(
				"ioreg",
				["-rd1", "-c", "IOPlatformExpertDevice"],
				(err, stdout) => {
					if (err) return resolve(null);
					const match = stdout.match(/"IOPlatformUUID"\s*=\s*"([^"]+)"/);
					resolve(match?.[1] ?? null);
				},
			);
		});
	} else if (platform() === "linux") {
		try {
			raw = (await readFile("/etc/machine-id", "utf-8")).trim();
		} catch {
			// Fallback below
		}
	}

	if (!raw) {
		// Fallback: hostname + platform + username (original behavior)
		const { hostname, userInfo } = await import("node:os");
		raw = `${hostname()}-${platform()}-${userInfo().username}`;
	}

	return createHash("sha256").update(raw).digest("hex").slice(0, 16);
}
