/**
 * immorterm dashboard — ink TUI with live service polling
 *
 * Free tier: read-only (view status, no start/stop hotkeys)
 * Pro tier: full control (s=start, x=stop, r=refresh, q=quit)
 */

import { defineCommand } from "citty";
import consola from "consola";
import { isPro } from "../feature-gate.js";

export const dashboardCommand = defineCommand({
	meta: {
		name: "dashboard",
		description: "Launch interactive TUI dashboard",
	},
	async run() {
		const readOnly = !isPro();

		if (readOnly) {
			consola.info("Dashboard running in read-only mode (Free tier).");
			consola.info("Upgrade to Pro for full control: https://immorterm.dev/pricing\n");
		}

		// Auto-start the local API server so the dashboard can fetch session data
		try {
			const { ensureServeRunning } = await import("./serve.js");
			await ensureServeRunning();
		} catch {
			// Non-fatal — dashboard still works without the API server
		}

		try {
			const React = await import("react");
			const { render } = await import("ink");
			const { Dashboard } = await import("../ui/Dashboard.js");

			const { waitUntilExit } = render(React.createElement(Dashboard, { readOnly }));
			await waitUntilExit();
		} catch (err) {
			consola.error("Failed to launch dashboard. Ensure 'ink' and 'react' are installed.");
			consola.error(String(err));
			process.exit(1);
		}
	},
});
