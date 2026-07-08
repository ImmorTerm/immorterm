/**
 * ImmorTerm Setup Wizard — ink TUI
 *
 * Multi-step interactive wizard for `immorterm init`:
 * 1. Theme selection
 * 2. Service selection (checkboxes)
 * 3. License key (optional text input)
 * 4. Summary + write config
 *
 * Uses ink's built-in primitives only (no extra form deps).
 */

import React, { useState, useEffect, useCallback } from "react";
import { Box, Text, useApp, useInput } from "ink";
import { Spinner } from "./shared.js";

// ── Types ────────────────────────────────────────────────────────

interface WizardResult {
	enableMemory: boolean;
	enableGateway: boolean;
	licenseKey: string | null;
}

type Step = "services" | "license" | "theme" | "writing" | "done" | "error";

// ── Progress Bar ─────────────────────────────────────────────────

function StepIndicator({
	current,
	total,
	labels,
}: {
	current: number;
	total: number;
	labels: string[];
}): React.ReactElement {
	return (
		<Box marginBottom={1}>
			{labels.map((label, i) => (
				<Text key={label}>
					<Text color={i < current ? "green" : i === current ? "magenta" : "gray"}>
						{i < current ? "●" : i === current ? "◉" : "○"}
					</Text>
					<Text color={i === current ? "white" : "gray"} bold={i === current}>
						{" "}
						{label}
					</Text>
					{i < total - 1 ? <Text color="gray"> — </Text> : null}
				</Text>
			))}
		</Box>
	);
}

// ── Main Wizard ──────────────────────────────────────────────────

export function SetupWizard({
	onComplete,
}: {
	onComplete: (result: WizardResult) => void;
}): React.ReactElement {
	const { exit } = useApp();

	// State
	const [step, setStep] = useState<Step>("theme");
	const [services, setServices] = useState({ memory: true, gateway: false });
	const [serviceCursor, setServiceCursor] = useState(0);
	const [wantsLicense, setWantsLicense] = useState(false);
	const [licenseKey, setLicenseKey] = useState("");
	const [licenseCursor, setLicenseCursor] = useState(1); // 0=yes, 1=no — default No (most users have no key)
	const [writeProgress, setWriteProgress] = useState("");
	const [writeError, setWriteError] = useState("");

	// Theme state
	const [themeNames, setThemeNames] = useState<string[]>([]);
	const [themeDescriptions, setThemeDescriptions] = useState<Record<string, string>>({});
	const [themeCursor, setThemeCursor] = useState(0);
	const [selectedTheme, setSelectedTheme] = useState("default");
	const [themePreviewFn, setThemePreviewFn] = useState<((name: string) => string) | null>(null);

	const STEP_LABELS = ["Theme", "Services", "License", "Done"];
	const stepIndex: Record<Step, number> = {
		theme: 0, services: 1, license: 2, writing: 3, done: 3, error: 3,
	};

	// Load theme info on mount
	useEffect(() => {
		(async () => {
			const banner = await import("./banner.js");
			setThemeNames(banner.THEME_NAMES);
			setThemeDescriptions(banner.THEME_DESCRIPTIONS);
			setThemePreviewFn(() => banner.renderThemePreview);
		})();
	}, []);

	// ── Write config ──
	const writeConfig = useCallback(
		async (key: string | null) => {
			setStep("writing");
			setWriteProgress("Writing configuration...");

			// Config write is critical — a failure here must NOT render success
			try {
				const {
					ensureGlobalConfig,
					readGlobalConfig,
					writeGlobalConfig,
				} = await import("@immorterm/config");

				ensureGlobalConfig();
				const config = readGlobalConfig();
				config.defaults.services.memory.enabled = services.memory;
				config.defaults.services.mcpGateway.enabled = services.gateway;
				// One terminal now — the Rust engine. Old configs may still say 'regular'/'both'.
				config.defaults.terminalMode = "ai";
				config.theme = selectedTheme;

				if (key) {
					setWriteProgress("Activating license...");
					const { activateLicense } = await import("@immorterm/license");
					const result = await activateLicense(key);
					if (result.success && result.license) {
						config.license.key = result.license.key ?? null;
						config.license.status = "active";
						config.license.customerEmail = result.license.email ?? null;
						config.license.expiresAt = result.license.expiresAt ?? null;
					}
				}

				writeGlobalConfig(config);
			} catch (err) {
				setWriteError(err instanceof Error ? err.message : String(err));
				setStep("error");
				return;
			}

			// Analytics is fire-and-forget — never blocks success
			try {
				setWriteProgress("Tracking analytics...");
				const { identify, track } = await import("@immorterm/analytics");
				await identify({ source: "init" });
				await track("cli_init_completed", {
					memory: services.memory,
					gateway: services.gateway,
					hasLicense: !!key,
					theme: selectedTheme,
				});
			} catch {
				// Non-critical
			}

			setStep("done");
			onComplete({
				enableMemory: services.memory,
				enableGateway: services.gateway,
				licenseKey: key,
			});
		},
		[services, selectedTheme, onComplete],
	);

	// ── Step navigation helpers ──
	const STEP_ORDER: Step[] = ["theme", "services", "license"];
	const goBack = useCallback(() => {
		const idx = STEP_ORDER.indexOf(step);
		if (idx > 0) {
			setStep(STEP_ORDER[idx - 1]!);
		}
	}, [step]);

	// ── Keyboard Input ──
	useInput((input, key) => {
		if (input === "q" || (key.ctrl && input === "c")) {
			exit();
			return;
		}

		// Left arrow = go back on interactive steps
		if (key.leftArrow && !wantsLicense) {
			if (step === "services" || step === "license" || step === "theme") {
				goBack();
				return;
			}
		}

		// Services step: 3 items (memory=0, gateway=1, continue=2)
		// Space toggles the focused checkbox; Enter always advances
		if (step === "services") {
			if (key.upArrow) setServiceCursor((c) => Math.max(0, c - 1));
			if (key.downArrow) setServiceCursor((c) => Math.min(2, c + 1));
			if (input === " ") {
				if (serviceCursor === 0) {
					setServices((s) => ({ ...s, memory: !s.memory }));
				} else if (serviceCursor === 1) {
					setServices((s) => ({ ...s, gateway: !s.gateway }));
				}
			}
			if (key.return || key.rightArrow) setStep("license");
		}

		if (step === "license" && !wantsLicense) {
			if (key.upArrow || key.downArrow) setLicenseCursor((c) => (c === 0 ? 1 : 0));
			if (key.return) {
				if (licenseCursor === 0) {
					setWantsLicense(true);
				} else {
					writeConfig(null);
				}
			}
		}

		if (step === "license" && wantsLicense) {
			if (key.return && licenseKey.length > 0) {
				writeConfig(licenseKey);
			} else if (key.backspace || key.delete) {
				setLicenseKey((k) => k.slice(0, -1));
			} else if (key.escape) {
				setWantsLicense(false);
				setLicenseKey("");
			} else if (input && !key.ctrl && !key.meta && input.length === 1) {
				setLicenseKey((k) => k + input);
			}
		}

		if (step === "theme") {
			if (key.upArrow) setThemeCursor((c) => Math.max(0, c - 1));
			if (key.downArrow) setThemeCursor((c) => Math.min(themeNames.length - 1, c + 1));
			if (key.return) {
				const chosen = themeNames[themeCursor] ?? "default";
				setSelectedTheme(chosen);
				setStep("services");
			}
		}

		// Error step: any key exits (config write failed — nothing more to do)
		if (step === "error") {
			exit();
		}
	});

	// ── Render ──
	return (
		<Box flexDirection="column" padding={1}>
			{/* Header */}
			<Box marginBottom={1}>
				<Text color="magenta" bold>
					{"  "}IMMORTERM SETUP
				</Text>
			</Box>

			<StepIndicator
				current={stepIndex[step]}
				total={STEP_LABELS.length}
				labels={STEP_LABELS}
			/>

			{/* Step 1: Theme */}
			{step === "theme" && (
				<Box flexDirection="column">
					<Text bold>Choose your theme:</Text>
					<Text dimColor>This affects the CLI banner and status bar colors.</Text>
					<Text> </Text>

					{themeNames.map((name, i) => {
						const focused = themeCursor === i;
						const preview = themePreviewFn ? themePreviewFn(name) : "";
						return (
							<Box key={name} flexDirection="column">
								<Box>
									<Text color={focused ? "magenta" : "gray"}>
										{focused ? "❯" : " "}{" "}
									</Text>
									<Text bold={focused}>
										{name.charAt(0).toUpperCase() + name.slice(1)}
									</Text>
									<Text> </Text>
									<Text>{preview}</Text>
								</Box>
								{focused && (
									<Text dimColor>
										{"    "}{themeDescriptions[name] ?? ""}
									</Text>
								)}
							</Box>
						);
					})}

					<Text> </Text>
					<Text dimColor>↑↓ navigate · enter select · ← back</Text>
				</Box>
			)}

			{/* Step 2: Services */}
			{step === "services" && (
				<Box flexDirection="column">
					<Text bold>Choose your services:</Text>
					<Text dimColor>Learn more: https://immorterm.com/features</Text>
					<Text> </Text>
					<ServiceCheckbox
						label="AI Memory"
						description="Your AI remembers every decision, choice, and lesson learned across sessions. No more re-explaining context."
						checked={services.memory}
						focused={serviceCursor === 0}
					/>
					<ServiceCheckbox
						label="MCP Gateway"
						description="Routes all AI tool calls through one persistent proxy — faster tool responses, shared across sessions."
						checked={services.gateway}
						focused={serviceCursor === 1}
					/>
					<Text> </Text>
					<Box>
						<Text color={serviceCursor === 2 ? "magenta" : "gray"}>
							{serviceCursor === 2 ? "❯" : " "}{" "}
						</Text>
						<Text bold={serviceCursor === 2} color={serviceCursor === 2 ? "cyan" : "gray"}>
							Continue →
						</Text>
					</Box>
					<Text> </Text>
					<Text dimColor>
						↑↓ navigate · space toggle · enter continue · ← back
					</Text>
				</Box>
			)}

			{/* Step 3: License */}
			{step === "license" && !wantsLicense && (
				<Box flexDirection="column">
					<Text bold>Do you have a license key?</Text>
					<Text> </Text>
					<Text>
						<Text color={licenseCursor === 0 ? "magenta" : "gray"}>
							{licenseCursor === 0 ? "❯" : " "}
						</Text>
						<Text bold={licenseCursor === 0}> Yes, activate now</Text>
					</Text>
					<Text>
						<Text color={licenseCursor === 1 ? "magenta" : "gray"}>
							{licenseCursor === 1 ? "❯" : " "}
						</Text>
						<Text bold={licenseCursor === 1}> No, continue with Free tier</Text>
					</Text>
					<Text> </Text>
					<Text dimColor>↑↓ select · enter confirm · ← back</Text>
				</Box>
			)}

			{step === "license" && wantsLicense && (
				<Box flexDirection="column">
					<Text bold>Enter your license key:</Text>
					<Text> </Text>
					<Box>
						<Text color="magenta">❯ </Text>
						<Text>{licenseKey || " "}</Text>
						<Text color="magenta">▌</Text>
					</Box>
					<Text> </Text>
					<Text dimColor>type key · enter continue · esc go back</Text>
				</Box>
			)}

			{/* Writing */}
			{step === "writing" && <Spinner label={writeProgress} />}

			{/* Error — config write failed */}
			{step === "error" && (
				<Box flexDirection="column">
					<Text color="red" bold>
						✗ Setup failed — configuration was NOT written
					</Text>
					<Text> </Text>
					<Text color="red">{"  "}{writeError}</Text>
					<Text> </Text>
					<Text>
						{"  "}Check that <Text dimColor>~/.immorterm/</Text> is writable, then re-run{" "}
						<Text color="cyan">immorterm init</Text>
					</Text>
					<Text> </Text>
					<Text dimColor>press any key to exit</Text>
				</Box>
			)}

			{/* Done */}
			{step === "done" && (
				<Box flexDirection="column">
					<Text color="green" bold>
						✓ ImmorTerm initialized!
					</Text>
					<Text> </Text>
					<Text>
						{"  "}Config: <Text dimColor>~/.immorterm/config.json</Text>
					</Text>
					<Text>
						{"  "}Memory:{" "}
						<Text color={services.memory ? "green" : "gray"}>
							{services.memory ? "enabled" : "disabled"}
						</Text>
					</Text>
					<Text>
						{"  "}Gateway:{" "}
						<Text color={services.gateway ? "green" : "gray"}>
							{services.gateway ? "enabled" : "disabled"}
						</Text>
					</Text>
					<Text>
						{"  "}License:{" "}
						<Text color={licenseKey ? "green" : "gray"}>
							{licenseKey ? "Pro" : "Free"}
						</Text>
					</Text>
					<Text>
						{"  "}Theme:{" "}
						<Text color="magenta">{selectedTheme}</Text>
					</Text>
					<Text> </Text>
					<Text>
						Next: <Text color="cyan">immorterm start</Text> to start services
					</Text>
				</Box>
			)}
		</Box>
	);
}

// ── Sub-components ───────────────────────────────────────────────

function ServiceCheckbox({
	label,
	description,
	checked,
	focused,
}: {
	label: string;
	description: string;
	checked: boolean;
	focused: boolean;
}): React.ReactElement {
	return (
		<Box flexDirection="column">
			<Text>
				<Text color={focused ? "magenta" : "gray"}>{focused ? "❯" : " "} </Text>
				<Text color={checked ? "green" : "gray"}>
					{checked ? "◉" : "○"}{" "}
				</Text>
				<Text bold={focused}>{label}</Text>
			</Text>
			{focused && (
				<Text dimColor>{"    "}{description}</Text>
			)}
		</Box>
	);
}
