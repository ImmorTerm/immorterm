/**
 * immorterm serve — Local API server for the web dashboard and TUI
 *
 * Starts a Hono HTTP server on localhost that exposes session data from
 * registry.json and structured log files. No data leaves the machine.
 *
 * Auto-start: writes a PID file to ~/.immorterm/serve.pid so other entry
 * points (dashboard, TUI log explorer) can check/start the server.
 */

import { defineCommand } from "citty";
import { Hono } from "hono";
import { cors } from "hono/cors";
import { serve as honoServe } from "@hono/node-server";
import * as fs from "node:fs";
import * as path from "node:path";
import { IMMORTERM_GLOBAL_DIR } from "@immorterm/config";
import {
	enrichAllSessions,
	enrichArchivedSessions,
	resolveSessionLogPath,
	resolveArchivedSessionLogPath,
	findRegistryEntry,
	enrichSession,
} from "../lib/session-enricher.js";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_PORT = 3847;
const PID_FILE = path.join(IMMORTERM_GLOBAL_DIR, "serve.pid");
const VERSION = "1.0.0";

// ---------------------------------------------------------------------------
// PID file management
// ---------------------------------------------------------------------------

export function isServeRunning(): { running: boolean; pid?: number } {
	try {
		if (!fs.existsSync(PID_FILE)) return { running: false };
		const pid = parseInt(fs.readFileSync(PID_FILE, "utf-8").trim(), 10);
		if (isNaN(pid)) return { running: false };
		// Check if process is alive
		process.kill(pid, 0);
		return { running: true, pid };
	} catch {
		// Process not running — clean up stale PID file
		try { fs.unlinkSync(PID_FILE); } catch { /* ignore */ }
		return { running: false };
	}
}

function writePidFile(): void {
	fs.mkdirSync(IMMORTERM_GLOBAL_DIR, { recursive: true });
	fs.writeFileSync(PID_FILE, String(process.pid), "utf-8");
}

function removePidFile(): void {
	try { fs.unlinkSync(PID_FILE); } catch { /* ignore */ }
}

// ---------------------------------------------------------------------------
// API Router
// ---------------------------------------------------------------------------

function createApp(): Hono {
	const app = new Hono();

	// CORS: allow immorterm.com + localhost origins
	app.use(
		"*",
		cors({
			origin: (origin) => {
				if (!origin) return origin; // allow non-browser requests
				if (
					origin.includes("immorterm.com") ||
					origin.includes("localhost") ||
					origin.includes("127.0.0.1")
				) {
					return origin;
				}
				return undefined as unknown as string; // reject
			},
			allowMethods: ["GET", "POST", "OPTIONS"],
			allowHeaders: ["Content-Type"],
		}),
	);

	// ── Health check ────────────────────────────────────────────
	app.get("/api/health", (c) =>
		c.json({ status: "ok", version: VERSION, pid: process.pid }),
	);

	// ── List sessions ───────────────────────────────────────────
	app.get("/api/sessions", (c) => {
		const project = c.req.query("project");
		const includeArchived = c.req.query("include_archived") === "true";
		const sessions = enrichAllSessions(project || undefined);

		if (includeArchived) {
			// Collect unique project dirs from active sessions to scan archives
			const projectDirs = new Set<string>();
			for (const s of sessions) {
				if (s.projectDir) projectDirs.add(s.projectDir);
			}
			// If filtering by project, only scan that one
			if (project) {
				projectDirs.clear();
				projectDirs.add(project);
			}
			const archived = [...projectDirs].flatMap((dir) =>
				enrichArchivedSessions(dir),
			);
			return c.json({ sessions: [...sessions, ...archived] });
		}

		return c.json({ sessions });
	});

	// ── Helper: resolve log path for active or archived sessions ──
	function resolveLogPath(
		sessionId: string,
		logType: "grid" | "cast" | "ai" | "raw",
	): string | null {
		if (sessionId.startsWith("archived:")) {
			const archiveDirName = sessionId.slice("archived:".length);
			// Need to find the project dir — scan registry for a hint, or check common locations
			const registry = enrichAllSessions();
			const projectDirs = new Set<string>();
			for (const s of registry) {
				if (s.projectDir) projectDirs.add(s.projectDir);
			}
			for (const dir of projectDirs) {
				const resolved = resolveArchivedSessionLogPath(
					dir,
					archiveDirName,
					logType,
				);
				if (resolved) return resolved;
			}
			return null;
		}
		return resolveSessionLogPath(sessionId, logType);
	}

	// ── Session snapshot (latest grid rendered as HTML) ─────────
	app.get("/api/sessions/:id/snapshot", async (c) => {
		const sessionId = c.req.param("id");
		const gridPath = resolveLogPath(sessionId, "grid");
		if (!gridPath || !fs.existsSync(gridPath)) {
			return c.json({ error: "Grid log not found" }, 404);
		}

		try {
			const { readLastSnapshot } = await import("@immorterm/terminal-logs");
			const { snapshotToHtml } = await import("@immorterm/terminal-logs");
			const snapshot = await readLastSnapshot(gridPath);
			if (!snapshot) {
				return c.json({ error: "No snapshots found" }, 404);
			}
			return c.json({
				html: snapshotToHtml(snapshot),
				snapshot: {
					ts: snapshot.ts,
					trigger: snapshot.trigger,
					cols: snapshot.cols,
					rows: snapshot.rows,
					cwd: snapshot.cwd,
					cursor: snapshot.cursor,
					sb_lines: snapshot.sb_lines,
				},
			});
		} catch (err) {
			return c.json({ error: String(err) }, 500);
		}
	});

	// ── Grid log (paginated snapshots with scrollback) ──────────
	app.get("/api/sessions/:id/grid", async (c) => {
		const sessionId = c.req.param("id");
		const offset = parseInt(c.req.query("offset") ?? "0", 10);
		const limit = parseInt(c.req.query("limit") ?? "50", 10);
		const gridPath = resolveLogPath(sessionId, "grid");
		if (!gridPath || !fs.existsSync(gridPath)) {
			return c.json({ error: "Grid log not found" }, 404);
		}

		try {
			const { readNdjson } = await import("@immorterm/terminal-logs");
			// Read ALL entries (snapshots + scrollback dumps) from the .grid.jsonl
			type RawEntry = { type: string; [k: string]: unknown };
			const snapshots: RawEntry[] = [];
			const scrollbackByHash = new Map<string, { runs: unknown[]; wrapped?: boolean }[]>();
			// Keep scrollback dumps in order for fallback matching
			const scrollbackDumps: { hash: string; lines: { runs: unknown[]; wrapped?: boolean }[] }[] = [];

			for await (const entry of readNdjson<RawEntry>(gridPath)) {
				if (entry.type === "snapshot") {
					snapshots.push(entry);
				} else if (entry.type === "scrollback") {
					const hash = (entry as { hash?: string }).hash ?? "";
					const lines = (entry as { lines?: unknown[] }).lines as { runs: unknown[]; wrapped?: boolean }[] ?? [];
					if (hash) scrollbackByHash.set(hash, lines);
					scrollbackDumps.push({ hash, lines });
				}
			}

			const total = snapshots.length;
			const page = snapshots.slice(offset, offset + limit);

			return c.json({
				total,
				offset,
				limit,
				snapshots: page.map((s, i) => {
					const sbHash = (s as { sb_hash?: string }).sb_hash ?? "";
					const sbLines = (s as { sb_lines?: number }).sb_lines ?? 0;
					// Match scrollback: by hash first, then fall back to latest dump
					let scrollback: { runs: unknown[]; wrapped?: boolean }[] | undefined;
					if (sbLines > 0) {
						if (sbHash && scrollbackByHash.has(sbHash)) {
							const full = scrollbackByHash.get(sbHash)!;
							scrollback = full.slice(0, sbLines);
						} else if (scrollbackDumps.length > 0) {
							// No hash match — use latest dump, take first sb_lines rows
							const full = scrollbackDumps[scrollbackDumps.length - 1]!.lines;
							scrollback = full.slice(0, sbLines);
						}
					}

					return {
						index: offset + i,
						ts: s.ts,
						trigger: s.trigger,
						cols: s.cols,
						rows: s.rows,
						cwd: s.cwd,
						cursor: s.cursor,
						grid: s.grid,
						sb_lines: s.sb_lines,
						sb_hash: sbHash,
						scrollback: scrollback ?? [],
					};
				}),
			});
		} catch (err) {
			return c.json({ error: String(err) }, 500);
		}
	});

	// ── AI conversation events ──────────────────────────────────
	app.get("/api/sessions/:id/ai", async (c) => {
		const sessionId = c.req.param("id");
		const aiPath = resolveLogPath(sessionId, "ai");
		if (!aiPath || !fs.existsSync(aiPath)) {
			return c.json({ error: "AI log not found" }, 404);
		}

		try {
			const { readNdjson, AiConversationEventSchema } = await import(
				"@immorterm/terminal-logs"
			);
			const events: unknown[] = [];
			for await (const raw of readNdjson(aiPath, AiConversationEventSchema)) {
				// Transform backend schema (event/duration_s) → frontend interface (type/duration_secs)
				const { event, duration_s, ...rest } = raw as Record<string, unknown>;
				events.push({
					...rest,
					type: event,
					...(duration_s != null ? { duration_secs: duration_s } : {}),
				});
			}
			return c.json({ events });
		} catch (err) {
			return c.json({ error: String(err) }, 500);
		}
	});

	// ── Asciicast data ──────────────────────────────────────────
	app.get("/api/sessions/:id/cast", async (c) => {
		const sessionId = c.req.param("id");
		const castPath = resolveLogPath(sessionId, "cast");
		if (!castPath || !fs.existsSync(castPath)) {
			return c.json({ error: "Cast file not found" }, 404);
		}

		try {
			const content = fs.readFileSync(castPath, "utf-8");
			return c.text(content, 200, {
				"Content-Type": "application/x-asciicast",
			});
		} catch (err) {
			return c.json({ error: String(err) }, 500);
		}
	});

	// ── Search across grid logs ─────────────────────────────────
	app.get("/api/search", async (c) => {
		const query = c.req.query("q");
		const project = c.req.query("project");
		const includeArchived = c.req.query("include_archived") !== "false"; // default: true

		if (!query || query.length < 2) {
			return c.json({ error: "Query must be at least 2 characters" }, 400);
		}

		const sessions = enrichAllSessions(project || undefined);

		// Optionally include archived sessions in search
		if (includeArchived) {
			const projectDirs = new Set<string>();
			for (const s of sessions) {
				if (s.projectDir) projectDirs.add(s.projectDir);
			}
			if (project) {
				projectDirs.clear();
				projectDirs.add(project);
			}
			const archived = [...projectDirs].flatMap((dir) =>
				enrichArchivedSessions(dir),
			);
			sessions.push(...archived);
		}

		const gridPaths: { sessionId: string; path: string }[] = [];

		for (const session of sessions) {
			const gridPath = resolveLogPath(session.id, "grid");
			if (gridPath && fs.existsSync(gridPath)) {
				gridPaths.push({ sessionId: session.id, path: gridPath });
			}
		}

		try {
			const { searchGridFile } = await import("@immorterm/terminal-logs");
			const results = await Promise.all(
				gridPaths.map(async ({ sessionId, path: fp }) => {
					const result = await searchGridFile(fp, query, 20);
					if (result.matches.length === 0) return null;
					const session = sessions.find((s) => s.id === sessionId);
					return {
						session: session
							? {
									id: session.id,
									displayName: session.displayName,
									projectDir: session.projectDir,
									status: session.status,
									isArchived: session.isArchived ?? false,
								}
							: null,
						matches: result.matches.map((m) => ({
							snapshotIndex: m.snapshotIndex,
							snapshotTs: m.snapshot.ts,
							row: m.row,
							plainText: m.plainText,
							matchOffset: m.matchOffset,
							matchLength: m.matchLength,
						})),
					};
				}),
			);

			return c.json({
				query,
				results: results.filter(Boolean),
			});
		} catch (err) {
			return c.json({ error: String(err) }, 500);
		}
	});

	// ── Daemon health ──────────────────────────────────────────
	app.get("/api/health/daemons", (c) => {
		const registryPath = path.join(IMMORTERM_GLOBAL_DIR, "registry.json");
		if (!fs.existsSync(registryPath)) {
			return c.json({ daemons: [] });
		}

		const registry = JSON.parse(fs.readFileSync(registryPath, "utf-8"));
		const now = Math.floor(Date.now() / 1000);

		const daemons = (registry.sessions || []).map((entry: any) => {
			let alive = false;
			try {
				process.kill(entry.pid, 0);
				alive = true;
			} catch {}

			// Read death.json if daemon is dead and has structured_log_dir
			let death_reason: string | undefined;
			let death_timestamp: string | undefined;
			if (!alive && entry.structured_log_dir) {
				const deathPath = path.join(entry.structured_log_dir, "death.json");
				try {
					if (fs.existsSync(deathPath)) {
						const death = JSON.parse(fs.readFileSync(deathPath, "utf-8"));
						death_reason = death.reason;
						death_timestamp = death.timestamp;
					}
				} catch {}
			}

			// Derive last_active_at from structured log file mtimes
			let last_active_at: number | undefined;
			if (entry.structured_log_dir) {
				const candidates = ["cast", "grid.jsonl", "daemon.log"];
				let latestMtime = 0;
				for (const candidate of candidates) {
					try {
						const fp = path.join(entry.structured_log_dir, candidate);
						const st = fs.statSync(fp);
						const mt = Math.floor(st.mtimeMs / 1000);
						if (mt > latestMtime) latestMtime = mt;
					} catch {}
				}
				if (latestMtime > 0) last_active_at = latestMtime;
			}

			return {
				session_id: entry.name,
				name: entry.name,
				display_name: entry.display_name || entry.name,
				pid: entry.pid,
				alive,
				ws_port: entry.ws_port || null,
				uptime_secs: alive ? now - entry.created_at : 0,
				created_at: entry.created_at,
				project_dir: entry.project_dir || "",
				session_type: entry.session_type || "regular",
				shell: entry.shell || "",
				title: entry.title || "",
				...(last_active_at && { last_active_at }),
				...(death_reason && { death_reason }),
				...(death_timestamp && { death_timestamp }),
			};
		});

		return c.json({ daemons });
	});

	// ── Service health ─────────────────────────────────────────
	app.get("/api/health/services", async (c) => {
		const services: Array<{ name: string; status: string; port?: number; pid?: number; error?: string }> = [];

		// Check memory service
		try {
			const resp = await fetch("http://localhost:8765/health", { signal: AbortSignal.timeout(2000) });
			if (resp.ok) {
				const data = await resp.json() as any;
				services.push({ name: "memory", status: "ok", port: 8765, pid: data.pid });
			} else {
				services.push({ name: "memory", status: "error", port: 8765, error: `HTTP ${resp.status}` });
			}
		} catch (e) {
			services.push({ name: "memory", status: "stopped", port: 8765, error: String(e) });
		}

		// Check MCP gateway
		try {
			const gatewayStatePath = path.join(IMMORTERM_GLOBAL_DIR, "mcp-gateway", "state.json");
			if (fs.existsSync(gatewayStatePath)) {
				const state = JSON.parse(fs.readFileSync(gatewayStatePath, "utf-8"));
				const port = state.port || 9100;
				const resp = await fetch(`http://localhost:${port}/health`, { signal: AbortSignal.timeout(2000) });
				if (resp.ok) {
					services.push({ name: "mcp-gateway", status: "ok", port, pid: state.pid });
				} else {
					services.push({ name: "mcp-gateway", status: "error", port, error: `HTTP ${resp.status}` });
				}
			} else {
				services.push({ name: "mcp-gateway", status: "stopped" });
			}
		} catch (e) {
			services.push({ name: "mcp-gateway", status: "error", error: String(e) });
		}

		// CLI serve is always ok (we're serving this request)
		services.push({ name: "cli-serve", status: "ok", port: parseInt(c.req.query("_port") || "3847"), pid: process.pid });

		return c.json({ services });
	});

	// ── Daemon log ──────────────────────────────────────────────
	app.get("/api/sessions/:id/daemon-log", (c) => {
		const sessionId = c.req.param("id");
		const maxLines = parseInt(c.req.query("lines") ?? "100", 10);

		// Find session in registry
		const registryPath = path.join(IMMORTERM_GLOBAL_DIR, "registry.json");
		if (!fs.existsSync(registryPath)) {
			return c.json({ error: "Registry not found" }, 404);
		}

		const registry = JSON.parse(fs.readFileSync(registryPath, "utf-8"));
		const entry = (registry.sessions || []).find((e: any) => e.name === sessionId);
		if (!entry?.structured_log_dir) {
			return c.json({ error: "Session not found or no log directory" }, 404);
		}

		const logPath = path.join(entry.structured_log_dir, "daemon.log");
		if (!fs.existsSync(logPath)) {
			return c.json({ error: "Daemon log not found" }, 404);
		}

		const content = fs.readFileSync(logPath, "utf-8");
		const allLines = content.split("\n");
		const lines = allLines.slice(-maxLines);

		return c.json({ lines, total_lines: allLines.length });
	});

	// ── Restart dead daemon ────────────────────────────────────
	app.post("/api/sessions/:id/restart", async (c) => {
		const sessionId = c.req.param("id");

		const registryPath = path.join(IMMORTERM_GLOBAL_DIR, "registry.json");
		if (!fs.existsSync(registryPath)) {
			return c.json({ error: "Registry not found" }, 404);
		}

		const registry = JSON.parse(fs.readFileSync(registryPath, "utf-8"));
		const entry = (registry.sessions || []).find((e: any) => e.name === sessionId);
		if (!entry) {
			return c.json({ error: "Session not found" }, 404);
		}

		// Verify daemon is actually dead
		try {
			process.kill(entry.pid, 0);
			return c.json({ error: "Daemon is still alive" }, 400);
		} catch {}

		// Spawn new daemon
		try {
			const binPath = path.join(IMMORTERM_GLOBAL_DIR, "bin", "immorterm-ai");

			const child = require("node:child_process").spawn(
				binPath,
				["daemon", "-S", entry.name, "-s", entry.shell],
				{
					detached: true,
					stdio: "ignore",
					env: {
						...process.env,
						IMMORTERM_WINDOW_ID: entry.window_id || "",
						SCREEN_PROJECT_DIR: entry.project_dir || "",
					},
				},
			);
			child.unref();

			// Wait briefly for daemon to register
			await new Promise((resolve) => setTimeout(resolve, 1000));

			return c.json({ success: true, new_pid: child.pid });
		} catch (e) {
			return c.json({ success: false, error: String(e) }, 500);
		}
	});

	// ── Remove dead daemon from registry ─────────────────────
	app.post("/api/sessions/:id/remove", (c) => {
		const sessionId = c.req.param("id");

		const registryPath = path.join(IMMORTERM_GLOBAL_DIR, "registry.json");
		if (!fs.existsSync(registryPath)) {
			return c.json({ error: "Registry not found" }, 404);
		}

		const registry = JSON.parse(fs.readFileSync(registryPath, "utf-8"));
		const idx = (registry.sessions || []).findIndex((e: any) => e.name === sessionId);
		if (idx === -1) {
			return c.json({ error: "Session not found" }, 404);
		}

		const entry = registry.sessions[idx];

		// Refuse to remove alive daemons
		try {
			process.kill(entry.pid, 0);
			return c.json({ error: "Daemon is still alive — stop it first" }, 400);
		} catch {}

		registry.sessions.splice(idx, 1);
		fs.writeFileSync(registryPath, JSON.stringify(registry, null, 2), "utf-8");

		return c.json({ success: true });
	});

	// ── Prune all dead daemons from registry ─────────────────
	app.post("/api/health/daemons/prune", (c) => {
		const registryPath = path.join(IMMORTERM_GLOBAL_DIR, "registry.json");
		if (!fs.existsSync(registryPath)) {
			return c.json({ success: true, removed: 0 });
		}

		const registry = JSON.parse(fs.readFileSync(registryPath, "utf-8"));
		const before = (registry.sessions || []).length;
		registry.sessions = (registry.sessions || []).filter((e: any) => {
			try {
				process.kill(e.pid, 0);
				return true; // alive — keep
			} catch {
				return false; // dead — remove
			}
		});
		const removed = before - registry.sessions.length;

		if (removed > 0) {
			fs.writeFileSync(registryPath, JSON.stringify(registry, null, 2), "utf-8");
		}

		return c.json({ success: true, removed });
	});

	// ── 404 ─────────────────────────────────────────────────────
	app.notFound((c) => c.json({ error: "Not found" }, 404));

	return app;
}

// ---------------------------------------------------------------------------
// Start server
// ---------------------------------------------------------------------------

export async function startServe(port: number, quiet = false): Promise<void> {
	const consola = (await import("consola")).default;

	// Check if already running
	const { running, pid: existingPid } = isServeRunning();
	if (running) {
		if (!quiet) {
			consola.info(`Server already running (PID ${existingPid}) on port ${port}`);
		}
		return;
	}

	const app = createApp();

	return new Promise((_resolve, reject) => {
		try {
			const server = honoServe({ fetch: app.fetch, port }, (info) => {
				writePidFile();
				if (!quiet) {
					consola.success(`ImmorTerm API server running on http://localhost:${info.port}`);
					consola.info("Endpoints:");
					consola.info("  GET  /api/health");
					consola.info("  GET  /api/health/daemons");
					consola.info("  GET  /api/health/services");
					consola.info("  GET  /api/sessions[?include_archived=true]");
					consola.info("  GET  /api/sessions/:id/snapshot");
					consola.info("  GET  /api/sessions/:id/grid");
					consola.info("  GET  /api/sessions/:id/ai");
					consola.info("  GET  /api/sessions/:id/cast");
					consola.info("  GET  /api/sessions/:id/daemon-log[?lines=100]");
					consola.info("  POST /api/sessions/:id/restart");
					consola.info("  POST /api/sessions/:id/remove");
					consola.info("  POST /api/health/daemons/prune");
					consola.info("  GET  /api/search?q=...[&include_archived=false]");
					consola.info("");
					consola.info("Press Ctrl+C to stop");
				}
			});

			// Cleanup on exit
			const cleanup = () => {
				removePidFile();
				server.close();
			};
			process.on("SIGINT", () => { cleanup(); process.exit(0); });
			process.on("SIGTERM", () => { cleanup(); process.exit(0); });
		} catch (err) {
			reject(err);
		}
	});
}

// ---------------------------------------------------------------------------
// Auto-start helper (for dashboard/TUI to use)
// ---------------------------------------------------------------------------

export async function ensureServeRunning(port = DEFAULT_PORT): Promise<void> {
	const { running } = isServeRunning();
	if (running) return;

	// Fork a detached serve process
	const { fork } = await import("node:child_process");
	const child = fork(
		process.argv[1]!,
		["serve", "--port", String(port)],
		{
			detached: true,
			stdio: "ignore",
		},
	);
	child.unref();

	// Wait briefly for server to start
	await new Promise((resolve) => setTimeout(resolve, 500));
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

export const serveCommand = defineCommand({
	meta: {
		name: "serve",
		description: "Start local API server for the web dashboard",
	},
	args: {
		port: {
			type: "string",
			description: `Port to listen on (default: ${DEFAULT_PORT})`,
			default: String(DEFAULT_PORT),
		},
	},
	async run({ args }) {
		const port = parseInt(args.port, 10) || DEFAULT_PORT;
		await startServe(port);
	},
});
