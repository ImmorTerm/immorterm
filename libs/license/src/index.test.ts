import { describe, test, expect, beforeAll } from "bun:test";
import {
	activateLicense,
	validateLicense,
	deactivateLicense,
	resolveTier,
	isLicenseValidSync,
	needsRevalidation,
} from "./index.js";

// DODO_MOCK=1 — offline path; env is read at call time so this works
beforeAll(() => {
	process.env.DODO_MOCK = "1";
});

describe("Dodo mock path (DODO_MOCK=1)", () => {
	test("activate returns license with instance id + tier", async () => {
		const result = await activateLicense("LK-TEST-KEY");
		expect(result.success).toBe(true);
		expect(result.license?.instanceId).toBe("lki_mock");
		expect(result.license?.tier).toBe("pro");
		expect(result.license?.key).toBe("LK-TEST-KEY");
		expect(result.license?.productId).toBe("prod_mock");
	});

	test("validate succeeds and echoes instance id", async () => {
		const result = await validateLicense("LK-TEST-KEY", "lki_abc");
		expect(result.success).toBe(true);
		expect(result.license?.instanceId).toBe("lki_abc");
	});

	test("deactivate requires instance id (Dodo hard requirement)", async () => {
		const missing = await deactivateLicense("LK-TEST-KEY");
		expect(missing.success).toBe(false);
		expect(missing.error).toContain("instance id");

		const ok = await deactivateLicense("LK-TEST-KEY", "lki_abc");
		expect(ok.success).toBe(true);
	});
});

describe("resolveTier", () => {
	test("uses DODO_PRODUCT_TIER_MAP when set", () => {
		process.env.DODO_PRODUCT_TIER_MAP = JSON.stringify({ prod_team: "team" });
		expect(resolveTier("prod_team")).toBe("team");
		expect(resolveTier("prod_unknown")).toBe("pro");
		delete process.env.DODO_PRODUCT_TIER_MAP;
	});

	test("defaults to pro (malformed map, missing id)", () => {
		process.env.DODO_PRODUCT_TIER_MAP = "not json";
		expect(resolveTier("prod_x")).toBe("pro");
		delete process.env.DODO_PRODUCT_TIER_MAP;
		expect(resolveTier()).toBe("pro");
	});
});

describe("local validity logic (provider-agnostic)", () => {
	const base = { key: "k", instanceId: "lki_1", status: "active" };

	test("valid when recently validated", () => {
		expect(
			isLicenseValidSync({ ...base, lastValidatedAt: new Date().toISOString() }),
		).toBe(true);
	});

	test("stale validation → invalid + needs revalidation", () => {
		const stale = new Date(Date.now() - 7 * 60 * 60 * 1000).toISOString();
		expect(isLicenseValidSync({ ...base, lastValidatedAt: stale })).toBe(false);
		expect(needsRevalidation({ ...base, lastValidatedAt: stale })).toBe(true);
	});

	test("no instance id → never valid, never revalidates", () => {
		expect(isLicenseValidSync({ key: "k", status: "active" })).toBe(false);
		expect(needsRevalidation({ key: "k" })).toBe(false);
	});
});
