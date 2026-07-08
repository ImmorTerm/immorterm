/**
 * immorterm logs [session-id] — Standalone log explorer
 *
 * Shows sessions for the current project directory and lets you view snapshots.
 * Launches the Ink TUI Log Explorer component.
 */

import { defineCommand } from "citty";

export const logsCommand = defineCommand({
	meta: {
		name: "logs",
		description: "Browse terminal session logs for the current project",
	},
	args: {
		session: {
			type: "positional",
			description: "Session ID to view directly (optional)",
			required: false,
		},
	},
	async run({ args }) {
		try {
			const React = await import("react");
			const { render } = await import("ink");
			const { LogExplorer } = await import("../ui/LogExplorer.js");

			const { waitUntilExit } = render(
				React.createElement(LogExplorer, {
					projectDir: process.cwd(),
					initialSessionId: args.session as string | undefined,
				}),
			);
			await waitUntilExit();
		} catch (err) {
			const consola = (await import("consola")).default;
			consola.error("Failed to launch log explorer.");
			consola.error(String(err));
			process.exit(1);
		}
	},
});
