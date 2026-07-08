/**
 * ImmorTerm Interactive App — Persistent CLI Menu
 *
 * Like Claude Code: run `immorterm` and get a persistent menu with live
 * service status, keyboard navigation, and action execution.
 *
 * State machine: menu → wizard | services | action | result | theme | pro
 *                services → service-detail → action → result → service-detail
 * After each action completes → result → any key → returnView (smart back-navigation)
 */

import React, { useState, useEffect, useCallback, useRef } from "react";
import { Box, Text, useApp, useInput } from "ink";
import { ServiceRow, Spinner, refreshServices, INITIAL_STATE } from "./shared.js";
import type { ServiceState } from "./shared.js";
import { SetupWizard } from "./SetupWizard.js";
import { LogExplorer } from "./LogExplorer.js";
import {
	MENU_ITEMS as SHARED_MENU_ITEMS,
	SERVICE_DEFS,
	PRO_ITEMS_ACTIVE as SHARED_PRO_ACTIVE,
	PRO_ITEMS_FREE as SHARED_PRO_FREE,
	getDetailItems as getSharedDetailItems,
} from "@immorterm/menu-data";
import type { MenuItem, ServiceDef, DetailItem } from "@immorterm/menu-data";

// ── Isolated banner component — owns its own tick state ──────────
// Only this component re-renders at 60fps. The rest of the app stays
// stable. This is the key to high-fps animation without jank.

interface AnimatedBannerProps {
	themeName: string;
	renderFn: ((name: string, tick: number) => string[]) | null;
	fallbackLines: string[];
}

function AnimatedBanner({ themeName, renderFn, fallbackLines }: AnimatedBannerProps): React.ReactElement {
	const [tick, setTick] = useState(0);
	const rafRef = useRef<ReturnType<typeof setInterval> | null>(null);

	useEffect(() => {
		if (!renderFn) return;
		// 15fps animation — smooth shimmer without terminal flicker
		rafRef.current = setInterval(() => setTick((t) => t + 1), 66);
		return () => {
			if (rafRef.current) clearInterval(rafRef.current);
		};
	}, [renderFn]);

	const lines = renderFn ? renderFn(themeName, tick) : fallbackLines;
	return (
		<>
			{lines.map((line, i) => (
				<Text key={`b${i}`}>{line}</Text>
			))}
		</>
	);
}

// ── Types ────────────────────────────────────────────────────────

type View = "menu" | "wizard" | "services" | "service-detail" | "action" | "result" | "theme" | "pro" | "logs";

interface InteractiveAppProps {
	firstRun: boolean;
}

// ── Menu / License / Detail items (shared + TUI-specific nav) ───

const MENU_ITEMS: MenuItem[] = [
	...SHARED_MENU_ITEMS,
	{ id: "quit", label: "Quit", desc: "Exit ImmorTerm" },
];

function getDetailItems(def: ServiceDef, enabled: boolean): DetailItem[] {
	const items = getSharedDetailItems(def, enabled);
	items.push({ id: "back", label: "Back", desc: "Return to services" });
	return items;
}

const PRO_ITEMS_ACTIVE = [
	...SHARED_PRO_ACTIVE,
	{ id: "back", label: "Back", desc: "Return to main menu" },
];

const PRO_ITEMS_FREE = [
	...SHARED_PRO_FREE,
	{ id: "back", label: "Back", desc: "Return to main menu" },
];

// ── Recovery suggestion helper ───────────────────────────────────

function getRecoverySuggestions(serviceName: string, error: string | undefined): string[] {
	if (!error) return ["Try stopping and restarting:", `  immorterm stop ${serviceName} && immorterm start ${serviceName}`];

	if (error.includes("not found") || error.includes("not installed")) {
		if (serviceName === "gateway") {
			return ["The gateway binary was not found.", "Fix: cd services/mcp-gateway && npx tsc"];
		}
		return ["Required binary or container not found.", "Fix: immorterm init"];
	}
	if (error.includes("EADDRINUSE") || error.includes("address already in use")) {
		const port = serviceName === "gateway" ? "9100" : "8765";
		return [`Port ${port} is already in use.`, `Fix: lsof -ti:${port} | xargs kill, then retry`];
	}
	if (error.includes("Timed out") || error.includes("timed out")) {
		return ["The service started but didn't respond in time.", `Try: immorterm stop ${serviceName}, then retry`];
	}
	return ["Try stopping and restarting:", `  immorterm stop ${serviceName} && immorterm start ${serviceName}`];
}

// ── Main Component ───────────────────────────────────────────────

export function InteractiveApp({ firstRun }: InteractiveAppProps): React.ReactElement {
	const { exit } = useApp();

	// State
	const [view, setView] = useState<View>(firstRun ? "wizard" : "menu");
	const [cursor, setCursor] = useState(0);
	const [state, setState] = useState<ServiceState>(INITIAL_STATE);
	const [actionLabel, setActionLabel] = useState("");
	const [resultLines, setResultLines] = useState<string[]>([]);

	// Theme state — banner data is loaded once, banner switching is instant via BANNER_CACHE
	const [bannerThemeNames, setBannerThemeNames] = useState<string[]>([]);
	const [bannerThemeLabels, setBannerThemeLabels] = useState<Record<string, string>>({});
	const [bannerThemeDescs, setBannerThemeDescs] = useState<Record<string, string>>({});
	const [bannerFreeThemes, setBannerFreeThemes] = useState<Set<string>>(new Set());
	const [bannerCache, setBannerCache] = useState<Record<string, string[]>>({});
	const [currentTheme, setCurrentTheme] = useState("Purple Haze");
	const [previewTheme, setPreviewTheme] = useState<string | null>(null);
	const [themeCursor, setThemeCursor] = useState(0);
	const [themePreviewFn, setThemePreviewFn] = useState<((name: string) => string) | null>(null);
	const [menuAccentFn, setMenuAccentFn] = useState<((s: string) => string) | null>(null);

	// Animation state — renderFn is passed to AnimatedBanner component
	const [animBannerFn, setAnimBannerFn] = useState<((name: string, tick: number) => string[]) | null>(null);

	// License sub-menu state
	const [licenseCursor, setLicenseCursor] = useState(0);
	const [licenseInput, setLicenseInput] = useState("");
	const [licenseInputMode, setLicenseInputMode] = useState(false);

	// Services sub-view state
	const [serviceCursor, setServiceCursor] = useState(0);
	const [serviceEnabled, setServiceEnabled] = useState<Record<string, boolean>>({
		memory: false, mcpGateway: false,
	});
	const [selectedService, setSelectedService] = useState<ServiceDef | null>(null);
	const [detailCursor, setDetailCursor] = useState(0);
	const [returnView, setReturnView] = useState<View>("menu");

	// Tier info
	const [tierLabel, setTierLabel] = useState("");
	const [tierEmail, setTierEmail] = useState("");
	const [isPro, setIsPro] = useState(false);
	// memory-pro is a valid paid license but does NOT unlock themes
	const [themesUnlocked, setThemesUnlocked] = useState(false);
	const [isMemoryPro, setIsMemoryPro] = useState(false);

	// ── Load tier info + service config ──
	const loadConfig = useCallback(async () => {
		try {
			const { readGlobalConfig } = await import("@immorterm/config");
			const { tierDisplayLabel } = await import("@immorterm/types");
			const config = readGlobalConfig();
			const pro = config.license.status === "active";
			const memoryPro = pro && config.license.tier === "memory-pro";
			setIsPro(pro);
			setIsMemoryPro(memoryPro);
			setThemesUnlocked(pro && !memoryPro);
			setTierLabel(pro ? tierDisplayLabel(config.license.tier) : "Free");
			setTierEmail(config.license.customerEmail ?? "");
			setCurrentTheme(config.theme ?? "Purple Haze");
			setServiceEnabled({
				memory: config.defaults.services.memory.enabled,
				mcpGateway: config.defaults.services.mcpGateway.enabled,
			});
		} catch {
			setTierLabel("Free");
		}
	}, []);

	// ── Load theme data once (including pre-computed banner cache) ──
	useEffect(() => {
		(async () => {
			const banner = await import("./banner.js");
			setBannerThemeNames(banner.THEME_NAMES);
			setBannerThemeLabels(banner.THEME_LABELS);
			setBannerThemeDescs(banner.THEME_DESCRIPTIONS);
			setBannerFreeThemes(banner.FREE_THEMES);
			setBannerCache(banner.BANNER_CACHE);
			setThemePreviewFn(() => banner.renderThemePreview);
			setMenuAccentFn(() => banner.getMenuAccent(banner.resolveTheme()));
			setAnimBannerFn(() => banner.renderAnimatedBanner);
			const resolved = banner.resolveTheme();
			setCurrentTheme(resolved);
		})();
	}, []);

	// Animation timer removed — AnimatedBanner component owns its own tick state

	// ── Refresh services ──
	const refresh = useCallback(async () => {
		try {
			const result = await refreshServices();
			setState({ ...result, loading: false });
		} catch {
			setState((prev) => ({ ...prev, loading: false }));
		}
	}, []);

	// Initial load + periodic refresh
	useEffect(() => {
		refresh();
		loadConfig();
		const interval = setInterval(refresh, 10000);
		return () => clearInterval(interval);
	}, [refresh, loadConfig]);

	// ── Service actions ──

	const toggleService = useCallback(async (def: ServiceDef) => {
		try {
			const { readGlobalConfig, writeGlobalConfig } = await import("@immorterm/config");
			const config = readGlobalConfig();
			const current = config.defaults.services[def.configKey]?.enabled ?? false;
			config.defaults.services[def.configKey].enabled = !current;
			writeGlobalConfig(config);
			await loadConfig();
		} catch { /* ignore */ }
	}, [loadConfig]);

	const startService = useCallback(async (def: ServiceDef) => {
		setReturnView("service-detail");
		setView("action");
		setActionLabel(`Starting ${def.name}...`);
		const lines: string[] = [];
		try {
			const services = await import("@immorterm/services");
			const log = {
				info: (msg: string) => lines.push(msg),
				warn: (msg: string) => lines.push(`! ${msg}`),
				error: (msg: string) => lines.push(`\u2717 ${msg}`),
			};
			if (def.id === "memory") {
				const s = await services.startMemory(log);
				if (s.apiHealthy) {
					lines.push(`\u2713 Memory running (MCP: ${s.mcpHealthy ? "ok" : "starting"})`);
				} else {
					lines.push(`\u2717 Memory failed: ${s.lastError ?? "unknown"}`);
					lines.push("");
					for (const suggestion of getRecoverySuggestions("memory", s.lastError)) {
						lines.push(suggestion);
					}
				}
			} else if (def.id === "gateway") {
				const s = await services.startGateway(services.GATEWAY_PORT, log);
				if (s.healthy) {
					lines.push(`\u2713 MCP Gateway running (port ${s.port})`);
				} else {
					lines.push(`\u2717 MCP Gateway failed: ${s.lastError ?? "unknown"}`);
					lines.push("");
					for (const suggestion of getRecoverySuggestions("gateway", s.lastError)) {
						lines.push(suggestion);
					}
				}
			}
		} catch (err) {
			lines.push(`\u2717 Error: ${err}`);
		}
		await refresh();
		setResultLines(lines);
		setView("result");
	}, [refresh]);

	const stopService = useCallback(async (def: ServiceDef) => {
		setReturnView("service-detail");
		setView("action");
		setActionLabel(`Stopping ${def.name}...`);
		const lines: string[] = [];
		try {
			const services = await import("@immorterm/services");
			const log = {
				info: (msg: string) => lines.push(msg),
				warn: (msg: string) => lines.push(`! ${msg}`),
				error: (msg: string) => lines.push(`\u2717 ${msg}`),
			};
			if (def.id === "memory") {
				await services.stopMemory(log);
				lines.push("\u2713 Memory stopped");
			} else if (def.id === "gateway") {
				await services.stopGateway(services.GATEWAY_PORT, log);
				lines.push("\u2713 MCP Gateway stopped");
			}
		} catch (err) {
			lines.push(`\u2717 Error: ${err}`);
		}
		await refresh();
		setResultLines(lines);
		setView("result");
	}, [refresh]);

	const restartService = useCallback(async (def: ServiceDef) => {
		setReturnView("service-detail");
		setView("action");
		setActionLabel(`Restarting ${def.name}...`);
		const lines: string[] = [];
		try {
			const services = await import("@immorterm/services");
			const log = {
				info: (msg: string) => lines.push(msg),
				warn: (msg: string) => lines.push(`! ${msg}`),
				error: (msg: string) => lines.push(`\u2717 ${msg}`),
			};
			if (def.id === "memory") {
				await services.stopMemory(log);
				lines.push("\u2713 Memory stopped");
				const s = await services.startMemory(log);
				if (s.apiHealthy) {
					lines.push(`\u2713 Memory running (MCP: ${s.mcpHealthy ? "ok" : "starting"})`);
				} else {
					lines.push(`\u2717 Memory failed to restart: ${s.lastError ?? "unknown"}`);
				}
			} else if (def.id === "gateway") {
				await services.stopGateway(services.GATEWAY_PORT, log);
				lines.push("\u2713 MCP Gateway stopped");
				const s = await services.startGateway(services.GATEWAY_PORT, log);
				if (s.healthy) {
					lines.push(`\u2713 MCP Gateway running (port ${s.port})`);
				} else {
					lines.push(`\u2717 MCP Gateway failed to restart: ${s.lastError ?? "unknown"}`);
				}
			}
		} catch (err) {
			lines.push(`\u2717 Error: ${err}`);
		}
		await refresh();
		setResultLines(lines);
		setView("result");
	}, [refresh]);

	// ── Menu action handlers ──

	const runAction = useCallback(async (actionId: string) => {
		if (actionId === "wizard") {
			setView("wizard");
			return;
		}
		if (actionId === "services") {
			setServiceCursor(0);
			setView("services");
			return;
		}
		if (actionId === "theme") {
			const banner = await import("./banner.js");
			const idx = banner.THEME_NAMES.indexOf(currentTheme);
			setThemeCursor(idx >= 0 ? idx : 0);
			setPreviewTheme(null);
			setView("theme");
			return;
		}
		if (actionId === "pro") {
			setLicenseCursor(0);
			setLicenseInputMode(false);
			setLicenseInput("");
			setView("pro");
			return;
		}
		if (actionId === "logs") {
			setView("logs");
			return;
		}
		if (actionId === "quit") {
			exit();
			return;
		}

		// Insights
		if (actionId === "insights") {
			setView("action");
			setActionLabel("Loading insights...");
			const lines: string[] = [];
			try {
				const { fetchInsights } = await import("../commands/insights.js");
				const data = await fetchInsights();
				const eng = data.engagement ?? {};
				const rate = eng.overall_rate != null ? eng.overall_rate.toFixed(1) : "0.0";

				lines.push("ImmorTerm Memory \u2014 Proactive Insights");
				lines.push("\u2500".repeat(42));
				lines.push(
					`Suggestions: ${eng.total_shown ?? 0}   Acted On: ${eng.total_acted_on ?? 0}   Rate: ${rate}%   Guardrails: ${data.guardrails_active ?? 0}`,
				);
				lines.push("");

				// Signal effectiveness
				const signals = eng.by_signal ?? {};
				const signalKeys = Object.keys(signals);
				if (signalKeys.length > 0) {
					lines.push("Signal Effectiveness");
					for (const key of signalKeys) {
						const s = signals[key];
						const sRate = s.shown > 0 ? (s.acted_on / s.shown) * 100 : 0;
						const filled = Math.round(sRate / 6.25);
						const bar = "\u2588".repeat(filled) + "\u2591".repeat(16 - filled);
						const name = key.replace(/_/g, " ").padEnd(20);
						lines.push(`  ${name}${bar}  ${sRate.toFixed(1)}%  (${s.acted_on}/${s.shown})`);
					}
					lines.push("");
				}

				// Failure patterns
				const patterns = data.failure_patterns ?? [];
				if (patterns.length > 0) {
					lines.push("Top Failure Patterns");
					for (const p of patterns) {
						lines.push(`  [x${p.frequency}] ${p.description}`);
					}
					lines.push("");
				}

				// Summary
				const ss = data.session_summary ?? {};
				lines.push(`Sessions (7d): ${ss.total_7d ?? 0}   Today: ${ss.active_today ?? 0}   Lessons: ${data.lessons_count ?? 0}`);
			} catch (err) {
				lines.push(`\u2717 Error: ${err}`);
			}
			setResultLines(lines);
			setView("result");
			return;
		}

		// Doctor
		if (actionId === "doctor") {
			setView("action");
			setActionLabel("Running diagnostics...");
			const lines: string[] = [];
			try {
				const { runDoctorChecks } = await import("../commands/doctor.js");
				const checks = await runDoctorChecks();
				for (const check of checks) {
					const icon = check.status === "pass" ? "\u2713" : check.status === "warn" ? "!" : "\u2717";
					lines.push(`${icon} ${check.name}: ${check.detail}`);
				}
				const fails = checks.filter((c) => c.status === "fail").length;
				const warns = checks.filter((c) => c.status === "warn").length;
				lines.push("");
				lines.push(fails === 0 && warns === 0 ? "All checks passed!" : `${fails} failure(s), ${warns} warning(s)`);
			} catch (err) {
				lines.push(`\u2717 Error: ${err}`);
			}
			setResultLines(lines);
			setView("result");
			await refresh();
		}
	}, [exit, refresh, currentTheme]);

	// ── Pro sub-actions ──
	const runProAction = useCallback(async (actionId: string) => {
		if (actionId === "back") {
			setView("menu");
			return;
		}

		setView("action");
		const lines: string[] = [];

		if (actionId === "status") {
			setActionLabel("Checking license...");
			try {
				const { readGlobalConfig } = await import("@immorterm/config");
				const config = readGlobalConfig();
				const lic = config.license;
				lines.push(`Tier:    ${lic.status === "active" ? "Pro" : "Free"}`);
				lines.push(`Email:   ${lic.customerEmail ?? "\u2014"}`);
				lines.push(`Expires: ${lic.expiresAt ?? "\u2014"}`);
				lines.push(`Key:     ${lic.key ? lic.key.slice(0, 8) + "..." : "\u2014"}`);

				if (lic.key) {
					const { validateLicense } = await import("@immorterm/license");
					const result = await validateLicense(lic.key);
					lines.push("");
					lines.push(result.success ? "\u2713 License is valid" : "! License validation failed");
				}
			} catch (err) {
				lines.push(`\u2717 Error: ${err}`);
			}
		} else if (actionId === "deactivate") {
			setActionLabel("Deactivating license...");
			try {
				const { readGlobalConfig, writeGlobalConfig } = await import("@immorterm/config");
				const config = readGlobalConfig();
				if (!config.license.key) {
					lines.push("No license is currently active.");
				} else {
					const { deactivateLicense } = await import("@immorterm/license");
					const result = await deactivateLicense(config.license.key, config.license.instanceId ?? undefined);
					if (result.success) {
						config.license.key = null;
						config.license.status = null;
						config.license.tier = null;
						config.license.customerEmail = null;
						config.license.expiresAt = null;
						config.license.instanceId = null;
						config.license.productId = null;
						config.license.lastValidatedAt = null;
						writeGlobalConfig(config);
						lines.push("\u2713 License deactivated. Reverted to Free tier.");
					} else {
						lines.push(`\u2717 Deactivation failed: ${result.error}`);
					}
				}
			} catch (err) {
				lines.push(`\u2717 Error: ${err}`);
			}
			await loadConfig();
		} else if (actionId === "upgrade") {
			setActionLabel("Opening upgrade...");
			lines.push("Upgrade to Pro for the full CLI upgrade experience:");
			lines.push("  immorterm pro");
			lines.push("");
			lines.push("Or visit: https://immorterm.com/pro");
		} else if (actionId === "pro-page") {
			setActionLabel("Opening Pro page...");
			try {
				const { execFile } = await import("node:child_process");
				execFile("open", ["https://immorterm.com/pro"]);
				lines.push("\u2713 Opened immorterm.com/pro in your browser.");
			} catch {
				lines.push("Visit: https://immorterm.com/pro");
			}
		}

		setResultLines(lines);
		setView("result");
	}, [loadConfig]);

	const activateLicenseKey = useCallback(async (key: string) => {
		setView("action");
		setActionLabel("Activating license...");
		const lines: string[] = [];
		try {
			const { activateLicense } = await import("@immorterm/license");
			const { readGlobalConfig, writeGlobalConfig } = await import("@immorterm/config");
			const result = await activateLicense(key);
			if (result.success) {
				const config = readGlobalConfig();
				config.license.key = key;
				config.license.status = "active";
				config.license.tier = result.license?.tier ?? "pro";
				config.license.customerEmail = result.license?.email ?? null;
				config.license.expiresAt = result.license?.expiresAt ?? null;
				config.license.instanceId = result.license?.instanceId ?? null;
				config.license.productId = result.license?.productId?.toString() ?? null;
				config.license.lastValidatedAt = new Date().toISOString();
				writeGlobalConfig(config);
				lines.push(`\u2713 License activated! Pro (${result.license?.email ?? ""})`);
				lines.push("");
				lines.push("Pro unlocked! You now have access to:");
				lines.push("  \u2713 Unlimited memory search results");
				lines.push("  \u2713 Full session recall");
				lines.push("  \u2713 Knowledge Packs");
				lines.push("  \u2713 Graph search");
				lines.push("  \u2713 MCP Gateway optimization");
				lines.push("  \u2713 All 21 themes");
			} else {
				lines.push(`\u2717 Activation failed: ${result.error}`);
			}
		} catch (err) {
			lines.push(`\u2717 Error: ${err}`);
		}
		await loadConfig();
		setResultLines(lines);
		setView("result");
	}, [loadConfig]);

	// ── Keyboard Input ──

	useInput((input, key) => {
		if (key.ctrl && input === "c") {
			exit();
			return;
		}

		// ── Menu view ──
		if (view === "menu") {
			if (input === "q") { exit(); return; }
			if (key.upArrow) setCursor((c) => Math.max(0, c - 1));
			if (key.downArrow) setCursor((c) => Math.min(MENU_ITEMS.length - 1, c + 1));
			if (key.return) {
				const item = MENU_ITEMS[cursor];
				if (item) runAction(item.id);
			}
		}

		// ── Result view — any key returns to returnView ──
		if (view === "result") {
			setView(returnView);
			setReturnView("menu");
			return;
		}

		// ── Services sub-view ──
		if (view === "services") {
			if (input === "q" || key.escape) { setView("menu"); return; }
			if (key.upArrow) setServiceCursor((c) => Math.max(0, c - 1));
			if (key.downArrow) setServiceCursor((c) => Math.min(SERVICE_DEFS.length - 1, c + 1));
			if (key.return) {
				const def = SERVICE_DEFS[serviceCursor];
				if (def) {
					setSelectedService(def);
					setDetailCursor(0);
					setView("service-detail");
				}
			}
		}

		// ── Service detail view ──
		if (view === "service-detail" && selectedService) {
			const enabled = serviceEnabled[selectedService.configKey] ?? false;
			const items = getDetailItems(selectedService, enabled);

			if (key.escape || input === "q") { setView("services"); return; }
			if (key.upArrow) setDetailCursor((c) => Math.max(0, c - 1));
			if (key.downArrow) setDetailCursor((c) => Math.min(items.length - 1, c + 1));
			if (key.return) {
				const item = items[detailCursor];
				if (!item) return;
				switch (item.id) {
					case "toggle":  toggleService(selectedService); break;
					case "start":   startService(selectedService); break;
					case "stop":    stopService(selectedService); break;
					case "restart": restartService(selectedService); break;
					case "back":    setView("services"); break;
				}
			}
		}

		// ── Theme picker ──
		if (view === "theme") {
			if (input === "q" || key.escape) {
				setPreviewTheme(null);
				setView("menu");
				return;
			}
			if (key.upArrow) {
				setThemeCursor((c) => {
					const next = Math.max(0, c - 1);
					setPreviewTheme(bannerThemeNames[next] ?? null);
					return next;
				});
			}
			if (key.downArrow) {
				setThemeCursor((c) => {
					const next = Math.min(bannerThemeNames.length - 1, c + 1);
					setPreviewTheme(bannerThemeNames[next] ?? null);
					return next;
				});
			}
			if (key.return) {
				const selected = bannerThemeNames[themeCursor];
				if (selected) {
					if (!themesUnlocked && !bannerFreeThemes.has(selected)) {
						setResultLines([
							"This theme requires Pro.",
							"",
							"Upgrade: https://immorterm.com/pro",
						]);
						setReturnView("theme");
						setView("result");
					} else {
						(async () => {
							const { readGlobalConfig, writeGlobalConfig } = await import("@immorterm/config");
							const config = readGlobalConfig();
							config.theme = selected;
							writeGlobalConfig(config);
							setCurrentTheme(selected);
							setPreviewTheme(null);
							setResultLines([`\u2713 Theme set to "${selected}"`, "", "The banner and status bar now use this theme."]);
							setReturnView("menu");
							setView("result");
						})();
					}
				}
			}
		}

		// ── Pro sub-menu ──
		if (view === "pro") {
			const items = isPro ? PRO_ITEMS_ACTIVE : PRO_ITEMS_FREE;
			if (licenseInputMode) {
				if (key.return && licenseInput.length > 0) { activateLicenseKey(licenseInput); return; }
				if (key.escape) { setLicenseInputMode(false); setLicenseInput(""); return; }
				if (key.backspace || key.delete) { setLicenseInput((k) => k.slice(0, -1)); return; }
				if (input && !key.ctrl && !key.meta && input.length === 1) {
					setLicenseInput((k) => k + input);
				}
				return;
			}

			if (input === "q" || key.escape) { setView("menu"); return; }
			if (key.upArrow) setLicenseCursor((c) => Math.max(0, c - 1));
			if (key.downArrow) setLicenseCursor((c) => Math.min(items.length - 1, c + 1));
			if (key.return) {
				const item = items[licenseCursor];
				if (item) {
					if (item.id === "activate") {
						setLicenseInputMode(true);
						setLicenseInput("");
					} else {
						runProAction(item.id);
					}
				}
			}
		}
	});

	// ── Render ──

	const activeTheme = previewTheme ?? currentTheme;
	const staticFallback = bannerCache[activeTheme] ?? bannerCache["Purple Haze"] ?? [];

	// Wizard view — full-screen, no banner
	if (view === "wizard") {
		return (
			<SetupWizard
				onComplete={() => {
					loadConfig();
					refresh();
					setResultLines(["\u2713 Setup complete! Returning to menu..."]);
					setView("result");
				}}
			/>
		);
	}

	// Action spinner — full-screen, no banner
	if (view === "action") {
		return (
			<Box flexDirection="column" padding={1}>
				<Spinner label={actionLabel} />
			</Box>
		);
	}

	// Result view — full-screen, no banner
	if (view === "result") {
		return (
			<Box flexDirection="column" padding={1}>
				{resultLines.map((line, i) => (
					<Text key={i}>{line}</Text>
				))}
				<Text> </Text>
				<Text dimColor>Press any key to continue...</Text>
			</Box>
		);
	}

	// ── All other views: persistent banner + view content ──
	return (
		<Box flexDirection="column" padding={1}>
			{/* Persistent banner — AnimatedBanner owns its own 60fps timer */}
			<AnimatedBanner
				themeName={activeTheme}
				renderFn={animBannerFn}
				fallbackLines={staticFallback}
			/>

			{/* ── Services sub-view ── */}
			{view === "services" && (
				<>
					<Text bold>Services</Text>
					<Text dimColor>{"\u2500".repeat(37)}</Text>

					{SERVICE_DEFS.map((def, i) => {
						const focused = serviceCursor === i;
						const enabled = serviceEnabled[def.configKey] ?? false;

						let statusText = "";
						let statusColor: string = "red";
						if (def.id === "memory") {
							if (state.memory.apiHealthy) { statusText = "healthy"; statusColor = "green"; }
							else if (state.memory.running) { statusText = "starting..."; statusColor = "yellow"; }
							else { statusText = "stopped"; }
						} else if (def.id === "gateway") {
							if (state.gateway.healthy) { statusText = "healthy"; statusColor = "green"; }
							else { statusText = "stopped"; }
						}

						return (
							<Box key={def.id}>
								<Text color={focused ? "magenta" : "gray"}>
									{focused ? "\u276F" : " "}{" "}
								</Text>
								<Text bold={focused}>{def.name.padEnd(20)}</Text>
								<Text color={statusColor === "green" ? "green" : statusColor === "yellow" ? "yellow" : "red"}>
									{statusColor === "green" ? "\u25CF" : "\u25CB"} {statusText.padEnd(12)}
								</Text>
								<Text color={enabled ? "green" : "gray"}>
									{enabled ? "[enabled]" : "[disabled]"}
								</Text>
							</Box>
						);
					})}

					<Text> </Text>
					<Text dimColor>{"\u2191\u2193"} navigate {"\u00B7"} enter open {"\u00B7"} esc back</Text>
				</>
			)}

			{/* ── Service detail view ── */}
			{view === "service-detail" && selectedService && (() => {
				const enabled = serviceEnabled[selectedService.configKey] ?? false;
				const items = getDetailItems(selectedService, enabled);

				let statusText = "";
				let statusColor: string = "red";
				if (selectedService.id === "memory") {
					if (state.memory.apiHealthy) {
						statusText = `healthy (MCP: ${state.memory.mcpHealthy ? "ok" : "off"})`;
						statusColor = "green";
					} else if (state.memory.running) {
						statusText = "starting..."; statusColor = "yellow";
					} else { statusText = "stopped"; }
				} else if (selectedService.id === "gateway") {
					if (state.gateway.healthy) {
						statusText = `healthy (${state.gateway.serverCount ?? 0} servers)`;
						statusColor = "green";
					} else { statusText = "stopped"; }
				}

				return (
					<>
						<Box>
							<Text bold>{selectedService.name}</Text>
							<Text>{"  "}</Text>
							<Text color={statusColor === "green" ? "green" : statusColor === "yellow" ? "yellow" : "red"}>
								{statusColor === "green" ? "\u25CF" : "\u25CB"} {statusText}
							</Text>
						</Box>
						<Text dimColor>{selectedService.desc}</Text>
						<Text dimColor>{"\u2500".repeat(37)}</Text>

						{items.map((item, i) => {
							const focused = detailCursor === i;
							return (
								<Box key={item.id}>
									<Text color={focused ? "magenta" : "gray"}>
										{focused ? "\u276F" : " "}{" "}
									</Text>
									<Text bold={focused}>{item.label.padEnd(22)}</Text>
									<Text dimColor>{item.desc}</Text>
								</Box>
							);
						})}

						<Text> </Text>
						<Text dimColor>{"\u2191\u2193"} navigate {"\u00B7"} enter select {"\u00B7"} esc back</Text>
					</>
				);
			})()}

			{/* ── Theme picker ── */}
			{view === "theme" && (
				<>
					<Text bold>Choose a theme:</Text>
					<Text dimColor>Current: {currentTheme}</Text>
					<Text dimColor>{"\u2500".repeat(37)}</Text>

					{bannerThemeNames.map((name, i) => {
						const focused = themeCursor === i;
						const isCurrent = name === currentTheme;
						const isFree = bannerFreeThemes.has(name);
						const label = bannerThemeLabels[name] ?? name;

						return (
							<Box key={name}>
								<Text color={focused ? "magenta" : "gray"}>
									{focused ? "\u276F" : " "}{" "}
								</Text>
								<Text bold={focused}>
									{label.padEnd(24)}
								</Text>
								<Text dimColor>
									{(bannerThemeDescs[name] ?? "").padEnd(28)}
								</Text>
								{isCurrent && <Text color="green"> (current)</Text>}
								{!isFree && !themesUnlocked && <Text color="yellow"> (Pro)</Text>}
								{focused && themePreviewFn && <Text> {themePreviewFn(name)}</Text>}
							</Box>
						);
					})}

					<Text> </Text>
					<Text dimColor>{"\u2191\u2193"} navigate {"\u00B7"} enter select {"\u00B7"} esc back</Text>
				</>
			)}

			{/* ── Pro sub-menu ── */}
			{view === "pro" && (
				licenseInputMode ? (
					<>
						<Text bold>Enter your license key:</Text>
						<Text> </Text>
						<Box>
							<Text color="magenta">{"\u276F"} </Text>
							<Text>{licenseInput || " "}</Text>
							<Text color="magenta">{"\u258C"}</Text>
						</Box>
						<Text> </Text>
						<Text dimColor>type key {"\u00B7"} enter activate {"\u00B7"} esc cancel</Text>
					</>
				) : (
					<>
						<Text bold>Pro</Text>
						<Text dimColor>
							Current: <Text color={isPro ? "green" : "gray"}>{tierLabel}</Text>
							{tierEmail ? ` (${tierEmail})` : ""}
						</Text>
						<Text> </Text>

						{!isPro && (
							<Box flexDirection="column" marginBottom={1}>
								<Text bold color="magenta">Upgrade to Pro</Text>
								<Text>  Unlimited memory, Knowledge Packs, Graph search, all themes</Text>
								<Text>  <Text color="cyan">https://immorterm.com/pro</Text></Text>
								<Text> </Text>
							</Box>
						)}

						{(isPro ? PRO_ITEMS_ACTIVE : PRO_ITEMS_FREE).map((item, i) => {
							const focused = licenseCursor === i;
							return (
								<Box key={item.id}>
									<Text color={focused ? "magenta" : "gray"}>
										{focused ? "\u276F" : " "}{" "}
									</Text>
									<Text bold={focused}>{item.label.padEnd(16)}</Text>
									<Text dimColor>{item.desc}</Text>
								</Box>
							);
						})}

						<Text> </Text>
						<Text dimColor>{"\u2191\u2193"} navigate {"\u00B7"} enter select {"\u00B7"} esc back</Text>
					</>
				)
			)}

			{/* ── Log Explorer view ── */}
			{view === "logs" && (
				<LogExplorer
					projectDir={process.cwd()}
					embedded
					onBack={() => setView("menu")}
				/>
			)}

			{/* ── Main menu view ── */}
			{view === "menu" && (
				<>
					{/* Status panel */}
					<Box>
						{/* Left: Services */}
						<Box flexDirection="column" width="50%">
							<Text bold>Services</Text>
							{state.loading ? (
								<Spinner label="Loading..." />
							) : (
								<>
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
								</>
							)}
						</Box>

						{/* Right: License + theme info */}
						<Box flexDirection="column" width="50%">
							<Text bold>
								Pro: <Text color={isPro ? "green" : "gray"}>{tierLabel}</Text>
								{tierEmail ? <Text dimColor> ({tierEmail})</Text> : null}
							</Text>
							<Text dimColor>{"\u2500".repeat(29)}</Text>
							<Text dimColor>Theme: {currentTheme}</Text>
							{isPro ? (
								<>
									<Text><Text color="green">{"\u2713"} </Text>Unlimited memory</Text>
									{!isMemoryPro && (
										<>
											<Text><Text color="green">{"\u2713"} </Text>Knowledge Packs</Text>
											<Text><Text color="green">{"\u2713"} </Text>Graph search</Text>
										</>
									)}
								</>
							) : (
								<>
									<Text><Text color="gray">{"\u00B7"} </Text>5 memory results</Text>
									<Text><Text color="gray">{"\u00B7"} </Text>Basic search</Text>
									<Text dimColor color="magenta">Upgrade: immorterm.com/pro</Text>
								</>
							)}
						</Box>
					</Box>

					<Text> </Text>

					{/* Menu */}
					<Box flexDirection="column">
						{MENU_ITEMS.map((item, i) => {
							const focused = cursor === i;
							return (
								<Box key={item.id}>
									<Text color={focused ? "magenta" : "gray"}>
										{focused ? "\u276F" : " "}{" "}
									</Text>
									<Text bold={focused}>{item.label.padEnd(20)}</Text>
									<Text dimColor>{item.desc}</Text>
								</Box>
							);
						})}
					</Box>

					<Text> </Text>
					<Text dimColor>{"\u2191\u2193"} navigate {"\u00B7"} enter select {"\u00B7"} q quit</Text>
				</>
			)}
		</Box>
	);
}
