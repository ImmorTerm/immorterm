import { describe, it, expect, vi, beforeEach } from "vitest";
import React from "react";
import { render } from "ink-testing-library";
import { ServiceRow, Spinner, refreshServices, INITIAL_STATE } from "../shared.js";

// ── Mocks ────────────────────────────────────────────────────────

vi.mock("@immorterm/services", () => ({
	refreshMemoryState: vi.fn(),
	checkGatewayHealth: vi.fn(),
	GATEWAY_PORT: 9100,
}));

beforeEach(() => {
	vi.clearAllMocks();
});

// ── ServiceRow ───────────────────────────────────────────────────

describe("ServiceRow", () => {
	it("renders healthy state with green dot and detail", () => {
		const { lastFrame } = render(
			<ServiceRow name="Memory" healthy={true} detail="running (API: healthy)" />,
		);
		const frame = lastFrame();
		expect(frame).toContain("●");
		expect(frame).toContain("Memory");
		expect(frame).toContain("running (API: healthy)");
	});

	it("renders unhealthy state with red circle", () => {
		const { lastFrame } = render(
			<ServiceRow name="Memory" healthy={false} detail="not running" />,
		);
		const frame = lastFrame();
		expect(frame).toContain("○");
		expect(frame).toContain("Memory");
		expect(frame).toContain("not running");
	});

	it("renders warning state with yellow dot", () => {
		const { lastFrame } = render(
			<ServiceRow name="Memory" healthy={false} warning={true} detail="starting..." />,
		);
		const frame = lastFrame();
		expect(frame).toContain("●");
		expect(frame).toContain("Memory");
		expect(frame).toContain("starting...");
	});
});

// ── Spinner ──────────────────────────────────────────────────────

describe("Spinner", () => {
	it("renders spinner frame with label", () => {
		const { lastFrame } = render(<Spinner label="Loading..." />);
		const frame = lastFrame();
		expect(frame).toContain("Loading...");
		expect(frame).toMatch(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]/);
	});
});

// ── refreshServices ──────────────────────────────────────────────

describe("refreshServices", () => {
	it("calls all health check functions and returns combined result", async () => {
		const services = await import("@immorterm/services");

		const mockMemory = { running: true, apiHealthy: true, mcpHealthy: true };
		const mockGateway = { running: true, healthy: true, port: 9100 };

		vi.mocked(services.refreshMemoryState).mockResolvedValue(mockMemory);
		vi.mocked(services.checkGatewayHealth).mockResolvedValue(mockGateway);

		const result = await refreshServices();

		expect(services.refreshMemoryState).toHaveBeenCalled();
		expect(services.checkGatewayHealth).toHaveBeenCalledWith(9100);
		expect(result).toEqual({
			memory: mockMemory,
			gateway: mockGateway,
		});
	});
});

// ── INITIAL_STATE ────────────────────────────────────────────────

describe("INITIAL_STATE", () => {
	it("defaults to loading with all services unhealthy", () => {
		expect(INITIAL_STATE.loading).toBe(true);
		expect(INITIAL_STATE.memory.apiHealthy).toBe(false);
		expect(INITIAL_STATE.memory.running).toBe(false);
		expect(INITIAL_STATE.gateway.healthy).toBe(false);
	});
});
