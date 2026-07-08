#!/usr/bin/env node
/**
 * ImmorTerm CLI — The Control Plane
 *
 * Entry point for `npx immorterm`. Routes subcommands via citty.
 *
 * Behavior:
 * - No args → launch interactive menu (wizard auto-runs on first run)
 * - Subcommand → route to handler (one-shot)
 */

import { defineCommand, runMain } from "citty";
import { getCliVersion } from "@immorterm/services";
import { initCommand } from "./commands/init.js";
import { startCommand } from "./commands/start.js";
import { stopCommand } from "./commands/stop.js";
import { statusCommand } from "./commands/status.js";
import { enableCommand } from "./commands/enable.js";
import { disableCommand } from "./commands/disable.js";
import { proCommand } from "./commands/pro.js";
import { upgradeCommand } from "./commands/upgrade.js";
import { installCommand } from "./commands/install.js";
import { doctorCommand } from "./commands/doctor.js";
import { dashboardCommand } from "./commands/dashboard.js";
import { configCommand } from "./commands/config.js";
import { serveCommand } from "./commands/serve.js";
import { logsCommand } from "./commands/logs.js";
import { memoryCommand } from "./commands/memory.js";
import { hooksCommand } from "./commands/hooks.js";
import { insightsCommand } from "./commands/insights.js";

const main = defineCommand({
	meta: {
		name: "immorterm",
		version: getCliVersion() ?? "unknown",
		description: "ImmorTerm — persistent terminals, AI memory, MCP optimization",
	},
	subCommands: {
		init: initCommand,
		start: startCommand,
		stop: stopCommand,
		status: statusCommand,
		enable: enableCommand,
		disable: disableCommand,
		pro: proCommand,
		license: proCommand, // backward compat alias
		upgrade: upgradeCommand,
		install: installCommand,
		doctor: doctorCommand,
		dashboard: dashboardCommand,
		config: configCommand,
		serve: serveCommand,
		logs: logsCommand,
		memory: memoryCommand,
		hooks: hooksCommand,
		insights: insightsCommand,
	},
	async run({ rawArgs }) {
		// If a subcommand was provided, citty handles it — skip default behavior.
		// citty still calls the parent run() even with subcommands,
		// so we detect subcommands by checking rawArgs.
		const subcommands = [
			"init",
			"start",
			"stop",
			"status",
			"enable",
			"disable",
			"pro",
			"license",
			"upgrade",
			"install",
			"doctor",
			"dashboard",
			"config",
			"serve",
			"logs",
			"memory",
			"hooks",
			"insights",
		];
		if (rawArgs.some((arg: string) => subcommands.includes(arg))) {
			// citty runs subcommands first, so this prints AFTER the command's output.
			// At most one network check per autoUpdate.checkIntervalHours.
			if (!rawArgs.includes("upgrade") && process.stdout.isTTY) {
				const { maybePrintUpgradeHint } = await import("./commands/upgrade.js");
				await maybePrintUpgradeHint();
			}
			return;
		}

		// Interactive mode requires a TTY for Ink's raw-mode keyboard input.
		// Non-TTY environments (Tilt, CI, piped stdin) crash in useInput()
		// with "Raw mode is not supported on the current process.stdin".
		if (!process.stdin.isTTY) {
			const consola = (await import("consola")).default;
			consola.warn(
				"Interactive mode requires a terminal (TTY). " +
				"Use a subcommand instead: immorterm status, immorterm start, etc.",
			);
			consola.info("Available commands: " + subcommands.filter(c => c !== "license").join(", "));
			process.exit(0);
		}

		// No subcommand — launch interactive mode
		const { getGlobalConfigPath } = await import("@immorterm/config");
		const fs = await import("node:fs");
		const firstRun = !fs.existsSync(getGlobalConfigPath());

		// Launch interactive app (banner renders inside InteractiveApp for live theme preview)
		const React = await import("react");
		const { render } = await import("ink");
		const { InteractiveApp } = await import("./ui/InteractiveApp.js");

		const { waitUntilExit } = render(
			React.createElement(InteractiveApp, { firstRun }),
		);
		await waitUntilExit();
	},
});

runMain(main);
