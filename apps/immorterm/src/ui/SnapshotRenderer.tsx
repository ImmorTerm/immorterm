import React from "react";
import { Box, Text } from "ink";
import type { GridSnapshot } from "@immorterm/terminal-logs";
import { runsToAnsi } from "@immorterm/terminal-logs";

interface SnapshotRendererProps {
	snapshot: GridSnapshot;
	maxRows?: number; // Limit rows displayed (for preview)
}

export function SnapshotRenderer({ snapshot, maxRows }: SnapshotRendererProps): React.ReactElement {
	const rows = maxRows ? snapshot.grid.slice(0, maxRows) : snapshot.grid;

	return (
		<Box flexDirection="column">
			{/* Header bar */}
			<Box>
				<Text dimColor>
					{snapshot.cols}x{snapshot.rows} | {snapshot.trigger} | {snapshot.cwd}
				</Text>
			</Box>
			{/* Terminal content - uses ANSI from runsToAnsi which Ink renders natively */}
			<Box
				flexDirection="column"
				borderStyle="single"
				borderColor="gray"
				paddingX={1}
			>
				{rows.map((rowRuns, i) => (
					<Text key={i}>{runsToAnsi(rowRuns.runs)}</Text>
				))}
			</Box>
			{maxRows && snapshot.grid.length > maxRows && (
				<Text dimColor>  ... {snapshot.grid.length - maxRows} more rows</Text>
			)}
		</Box>
	);
}
