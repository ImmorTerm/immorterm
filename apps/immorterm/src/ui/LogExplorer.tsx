import React, { useState, useEffect, useCallback } from "react";
import { Box, Text, useApp, useInput } from "ink";
import { SnapshotRenderer } from "./SnapshotRenderer.js";

// Import types
import type { GridSnapshot } from "@immorterm/terminal-logs";

// Session type from enricher
interface EnrichedSession {
	id: string;
	pid: number;
	displayName: string;
	title: string;
	projectDir: string;
	shell: string;
	createdAt: number;
	sessionType: string;
	status: "alive" | "dead";
	logs: {
		grid: { exists: boolean; size: number; snapshots?: number };
		cast: { exists: boolean; size: number };
		ai: { exists: boolean; size: number; events?: number };
		raw: { exists: boolean; size: number };
	};
}

type LogView = "list" | "snapshot" | "search";

interface LogExplorerProps {
	projectDir: string;
	initialSessionId?: string;
	/** When true, renders inline (used inside InteractiveApp). When false, standalone with exit. */
	embedded?: boolean;
	onBack?: () => void;
}

export function LogExplorer({ projectDir, initialSessionId, embedded, onBack }: LogExplorerProps): React.ReactElement {
	const { exit } = useApp();
	const [view, setView] = useState<LogView>("list");
	const [sessions, setSessions] = useState<EnrichedSession[]>([]);
	const [cursor, setCursor] = useState(0);
	const [loading, setLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);

	// Snapshot view state
	const [snapshots, setSnapshots] = useState<GridSnapshot[]>([]);
	const [snapshotIndex, setSnapshotIndex] = useState(0);
	const [selectedSession, setSelectedSession] = useState<EnrichedSession | null>(null);
	const [loadingSnapshots, setLoadingSnapshots] = useState(false);

	// Search state
	const [searchQuery, setSearchQuery] = useState("");
	const [searchMode, setSearchMode] = useState(false);

	// Load sessions
	const loadSessions = useCallback(async () => {
		setLoading(true);
		try {
			const { enrichAllSessions } = await import("../lib/session-enricher.js");
			const all = enrichAllSessions(projectDir);
			// Sort: alive first, then by createdAt desc
			all.sort((a, b) => {
				if (a.status !== b.status) return a.status === "alive" ? -1 : 1;
				return b.createdAt - a.createdAt;
			});
			setSessions(all);

			// If initial session ID provided, jump to it
			if (initialSessionId) {
				const idx = all.findIndex((s) => s.id === initialSessionId);
				if (idx >= 0) {
					setCursor(idx);
					// Auto-open snapshot view
					await openSession(all[idx]!);
				}
			}
		} catch (err) {
			setError(String(err));
		}
		setLoading(false);
	}, [projectDir, initialSessionId]);

	useEffect(() => {
		loadSessions();
	}, [loadSessions]);

	// Open a session's snapshots
	const openSession = useCallback(async (session: EnrichedSession) => {
		const { resolveSessionLogPath } = await import("../lib/session-enricher.js");
		const gridPath = resolveSessionLogPath(session.id, "grid");
		if (!gridPath) {
			setError("No grid log found for this session");
			return;
		}

		setLoadingSnapshots(true);
		setSelectedSession(session);
		try {
			const { readGridLog } = await import("@immorterm/terminal-logs");
			const snaps = await readGridLog(gridPath);
			setSnapshots(snaps);
			setSnapshotIndex(snaps.length - 1); // Start at latest
			setView("snapshot");
		} catch (err) {
			setError(`Failed to load snapshots: ${err}`);
		}
		setLoadingSnapshots(false);
	}, []);

	// Format time ago
	const timeAgo = useCallback((ts: number) => {
		const now = Date.now() / 1000;
		const diff = now - ts;
		if (diff < 60) return "just now";
		if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
		if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
		return `${Math.floor(diff / 86400)}d ago`;
	}, []);

	// Keyboard handling
	useInput((input, key) => {
		if (key.ctrl && input === "c") {
			if (embedded && onBack) onBack();
			else exit();
			return;
		}

		// Search input mode
		if (searchMode) {
			if (key.escape) {
				setSearchMode(false);
				setSearchQuery("");
				return;
			}
			if (key.return) {
				setSearchMode(false);
				// TODO: execute search
				return;
			}
			if (key.backspace || key.delete) {
				setSearchQuery((q) => q.slice(0, -1));
				return;
			}
			if (input && !key.ctrl && !key.meta && input.length === 1) {
				setSearchQuery((q) => q + input);
			}
			return;
		}

		// List view
		if (view === "list") {
			if (input === "q" || key.escape) {
				if (embedded && onBack) onBack();
				else exit();
				return;
			}
			if (key.upArrow) setCursor((c) => Math.max(0, c - 1));
			if (key.downArrow) setCursor((c) => Math.min(sessions.length - 1, c + 1));
			if (key.return) {
				const session = sessions[cursor];
				if (session && session.logs.grid.exists) {
					openSession(session);
				}
			}
			if (input === "s") {
				setSearchMode(true);
				setSearchQuery("");
			}
			if (input === "r") {
				loadSessions();
			}
		}

		// Snapshot view
		if (view === "snapshot") {
			if (input === "q" || key.escape) {
				setView("list");
				return;
			}
			if (key.leftArrow) {
				setSnapshotIndex((i) => Math.max(0, i - 1));
			}
			if (key.rightArrow) {
				setSnapshotIndex((i) => Math.min(snapshots.length - 1, i + 1));
			}
			// Jump to first/last
			if (key.pageUp || (key.shift && key.upArrow)) {
				setSnapshotIndex(0);
			}
			if (key.pageDown || (key.shift && key.downArrow)) {
				setSnapshotIndex(snapshots.length - 1);
			}
		}
	});

	// ── Render ──

	if (loading) {
		return (
			<Box flexDirection="column" padding={embedded ? 0 : 1}>
				<Text color="cyan">{"\u280B"} Loading sessions...</Text>
			</Box>
		);
	}

	if (error) {
		return (
			<Box flexDirection="column" padding={embedded ? 0 : 1}>
				<Text color="red">Error: {error}</Text>
				<Text dimColor>Press any key to go back</Text>
			</Box>
		);
	}

	// ── Session list view ──
	if (view === "list") {
		return (
			<Box flexDirection="column" padding={embedded ? 0 : 1}>
				<Text bold>Log Explorer</Text>
				<Text dimColor>{projectDir}</Text>
				<Text dimColor>{"\u2500".repeat(50)}</Text>

				{sessions.length === 0 ? (
					<Text dimColor>No sessions found for this project.</Text>
				) : (
					sessions.map((session, i) => {
						const focused = cursor === i;
						const statusIcon = session.status === "alive" ? "\u25CF" : "\u25CB";
						const statusColor = session.status === "alive" ? "green" : "gray";

						// Log type badges
						const badges: string[] = [];
						if (session.logs.grid.exists) badges.push("grid");
						if (session.logs.cast.exists) badges.push("cast");
						if (session.logs.ai.exists) badges.push("ai");

						return (
							<Box key={session.id}>
								<Text color={focused ? "magenta" : "gray"}>
									{focused ? "\u276F" : " "}{" "}
								</Text>
								<Text color={statusColor}>{statusIcon} </Text>
								<Text bold={focused}>
									{session.displayName.padEnd(20)}
								</Text>
								<Text dimColor>
									{session.id.slice(0, 16).padEnd(18)}
								</Text>
								<Text dimColor>
									{timeAgo(session.createdAt).padEnd(10)}
								</Text>
								{badges.map((badge) => (
									<Text key={badge} color="cyan">
										{" "}
										{badge}
									</Text>
								))}
							</Box>
						);
					})
				)}

				{searchMode ? (
					<>
						<Text> </Text>
						<Box>
							<Text color="magenta">search: </Text>
							<Text>{searchQuery || " "}</Text>
							<Text color="magenta">{"\u258C"}</Text>
						</Box>
					</>
				) : (
					<>
						<Text> </Text>
						<Text dimColor>
							{"\u2191\u2193"} navigate {"\u00B7"} enter view {"\u00B7"} s search {"\u00B7"} r refresh {"\u00B7"}{" "}
							{embedded ? "esc back" : "q quit"}
						</Text>
					</>
				)}
			</Box>
		);
	}

	// ── Snapshot view ──
	if (view === "snapshot" && selectedSession) {
		if (loadingSnapshots) {
			return (
				<Box flexDirection="column" padding={embedded ? 0 : 1}>
					<Text color="cyan">{"\u280B"} Loading snapshots...</Text>
				</Box>
			);
		}

		const snapshot = snapshots[snapshotIndex];
		if (!snapshot) {
			return (
				<Box flexDirection="column" padding={embedded ? 0 : 1}>
					<Text color="red">No snapshots available</Text>
					<Text dimColor>Press q to go back</Text>
				</Box>
			);
		}

		return (
			<Box flexDirection="column" padding={embedded ? 0 : 1}>
				<Box>
					<Text bold>
						{selectedSession.status === "alive" ? "\u25CF " : ""}
						{selectedSession.displayName}
					</Text>
					<Text dimColor>
						{" "}{"\u2014"} Snapshot #{snapshotIndex + 1}/{snapshots.length} ({snapshot.trigger},{" "}
						{timeAgo(snapshot.ts)})
					</Text>
				</Box>
				<Text dimColor>{"\u2500".repeat(50)}</Text>

				<SnapshotRenderer snapshot={snapshot} maxRows={24} />

				<Text> </Text>
				<Text dimColor>
					{"\u2190\u2192"} prev/next {"\u00B7"} PgUp/PgDn first/last {"\u00B7"} q back
				</Text>
			</Box>
		);
	}

	return <Text>Unknown view</Text>;
}
