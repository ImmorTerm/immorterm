/**
 * ImmorTerm Dashboard — ink TUI
 *
 * Live-polling dashboard showing service status, license, and keyboard shortcuts.
 *
 * Free tier: read-only — [r] refresh, [q] quit only
 * Pro tier: full control — [s] start, [x] stop, [r] refresh, [q] quit
 */

import React, { useState, useEffect } from "react";
import { Box, Text, useApp, useInput } from "ink";
import { ServiceRow, refreshServices, INITIAL_STATE } from "./shared.js";
import type { ServiceState } from "./shared.js";

interface DashboardProps {
	readOnly?: boolean;
}

export function Dashboard({ readOnly = false }: DashboardProps): React.ReactElement {
	const { exit } = useApp();
	const [state, setState] = useState<ServiceState>(INITIAL_STATE);
	const [lastRefresh, setLastRefresh] = useState<Date>(new Date());
	const [action, setAction] = useState<string>("");

	const refresh = async () => {
		try {
			const result = await refreshServices();
			setState({ ...result, loading: false });
			setLastRefresh(new Date());
		} catch {
			setState((prev) => ({ ...prev, loading: false }));
		}
	};

	useEffect(() => {
		refresh();
		const interval = setInterval(refresh, 5000);
		return () => clearInterval(interval);
	}, []);

	useInput(async (input) => {
		if (input === "q") {
			exit();
		} else if (input === "r") {
			setAction("Refreshing...");
			await refresh();
			setAction("");
		} else if (!readOnly && input === "s") {
			setAction("Starting services...");
			const services = await import("@immorterm/services");
			const log = { info: () => {}, warn: () => {}, error: () => {} };
			await services.startMemory(log);
			await services.startGateway(services.GATEWAY_PORT, log);
			setAction("");
			await refresh();
		} else if (!readOnly && input === "x") {
			setAction("Stopping services...");
			const services = await import("@immorterm/services");
			const log = { info: () => {}, warn: () => {}, error: () => {} };
			await services.stopMemory(log);
			await services.stopGateway(services.GATEWAY_PORT, log);
			setAction("");
			await refresh();
		}
	});

	if (state.loading) {
		return (
			<Box flexDirection="column" padding={1}>
				<Text color="cyan" bold>
					IMMORTERM DASHBOARD
				</Text>
				<Text dimColor>Loading...</Text>
			</Box>
		);
	}

	const tierBadge = readOnly ? " (Free — read-only)" : "";

	return (
		<Box flexDirection="column" padding={1}>
			<Text color="magenta" bold>
				IMMORTERM DASHBOARD{tierBadge}
			</Text>
			<Text dimColor>
				Last refresh: {lastRefresh.toLocaleTimeString()}
			</Text>
			<Text> </Text>

			{/* Services */}
			<Box flexDirection="column">
				<Text bold> Services</Text>
				<ServiceRow
					name="Memory"
					healthy={state.memory.apiHealthy}
					warning={state.memory.running && !state.memory.apiHealthy}
					detail={
						state.memory.apiHealthy
							? `healthy (MCP: ${state.memory.mcpHealthy ? "ok" : "off"})`
							: state.memory.running
								? "starting..."
								: "stopped"
					}
				/>
				<ServiceRow
					name="MCP Gateway"
					healthy={state.gateway.healthy}
					detail={
						state.gateway.healthy
							? `${state.gateway.serverCount ?? 0} servers, ${state.gateway.activeChildren ?? 0} active`
							: "stopped"
					}
				/>
			</Box>

			<Text> </Text>

			{/* Action feedback + hotkeys */}
			{action ? (
				<Text color="yellow">{action}</Text>
			) : readOnly ? (
				<Box flexDirection="column">
					<Text dimColor>[r] refresh [q] quit</Text>
					<Text dimColor color="magenta">
						Upgrade to Pro for start/stop controls
					</Text>
				</Box>
			) : (
				<Text dimColor>
					[s] start [x] stop [r] refresh [q] quit
				</Text>
			)}
		</Box>
	);
}
