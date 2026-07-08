import { describe, it, expect } from "vitest";
import { compareVersions, getCliVersion } from "@immorterm/services";

describe("compareVersions", () => {
	it("detects a newer semver", () => {
		expect(compareVersions("0.1.0", "0.2.0")).toBe(true);
		expect(compareVersions("0.1.0", "0.1.1")).toBe(true);
		expect(compareVersions("1.9.0", "1.10.0")).toBe(true);
	});

	it("returns false for equal or older versions", () => {
		expect(compareVersions("0.1.0", "0.1.0")).toBe(false);
		expect(compareVersions("0.2.0", "0.1.9")).toBe(false);
	});

	it("returns false for null inputs", () => {
		expect(compareVersions(null, "1.0.0")).toBe(false);
		expect(compareVersions("1.0.0", null)).toBe(false);
	});

	it("never claims an update for date-stamped release tags (non-semver)", () => {
		// CI tags like ai-prod-2026-04-09.2 strip to "2026-04-09.2" — not comparable
		expect(compareVersions("0.1.0", "2026-04-09.2")).toBe(false);
		expect(compareVersions("2026-04-09.1", "2026-04-09.2")).toBe(false);
	});
});

describe("getCliVersion", () => {
	it("reads a real semver from the immorterm package.json (not hardcoded)", () => {
		const version = getCliVersion();
		expect(version).toMatch(/^\d+\.\d+\.\d+/);
	});
});
