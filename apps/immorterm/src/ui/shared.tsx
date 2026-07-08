/**
 * Shared UI components for ImmorTerm CLI
 *
 * Extracted from Dashboard.tsx and SetupWizard.tsx to avoid duplication
 * across the interactive app, dashboard, and wizard.
 */

import React, { useState, useEffect } from "react";
import { Box, Text } from "ink";
import type { MemoryState, GatewayState } from "@immorterm/services";

// ── Types ────────────────────────────────────────────────────────

export interface ServiceState {
	memory: MemoryState;
	gateway: GatewayState;
	loading: boolean;
}

export const INITIAL_STATE: ServiceState = {
	memory: { running: false, apiHealthy: false, mcpHealthy: false },
	gateway: { running: false, healthy: false, port: 9100 },
	loading: true,
};

// ── Spinner ──────────────────────────────────────────────────────

const SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

export function Spinner({ label }: { label: string }): React.ReactElement {
	const [frame, setFrame] = useState(0);

	useEffect(() => {
		const timer = setInterval(() => {
			setFrame((f) => (f + 1) % SPINNER_FRAMES.length);
		}, 80);
		return () => clearInterval(timer);
	}, []);

	return (
		<Text>
			<Text color="cyan">{SPINNER_FRAMES[frame]} </Text>
			<Text>{label}</Text>
		</Text>
	);
}

// ── ServiceRow ───────────────────────────────────────────────────

export function ServiceRow({
	name,
	healthy,
	warning,
	detail,
}: {
	name: string;
	healthy: boolean;
	warning?: boolean;
	detail: string;
}): React.ReactElement {
	const icon = healthy ? "●" : warning ? "●" : "○";
	const color = healthy ? "green" : warning ? "yellow" : "red";

	return (
		<Box>
			<Text color={color}>{icon} </Text>
			<Text bold>{name.padEnd(14)}</Text>
			<Text dimColor={!healthy}>{detail}</Text>
		</Box>
	);
}

// ── Service Refresh ──────────────────────────────────────────────

export async function refreshServices(): Promise<Omit<ServiceState, "loading">> {
	const services = await import("@immorterm/services");
	const [memory, gateway] = await Promise.all([
		services.refreshMemoryState(),
		services.checkGatewayHealth(services.GATEWAY_PORT),
	]);
	return { memory, gateway };
}
