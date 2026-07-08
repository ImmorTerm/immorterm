#!/usr/bin/env python3
"""
Lightweight HTTP mock server for hook tests.

Usage:
    python3 mock-server.py [--port PORT] [--profile PROFILE] [--log LOG_FILE]

Profiles:
    default   — 200/201 for all known routes
    error-500 — 500 for everything
    timeout   — 30s delay before responding (triggers curl timeout)
    selective — 200 for health, 500 for everything else

Request log:
    Each request is appended as a JSON line to LOG_FILE (default: /tmp/mock-requests.jsonl).
    Body is captured and included for POST/PUT/PATCH.
"""

import argparse
import json
import time
from http.server import HTTPServer, BaseHTTPRequestHandler
from pathlib import Path
from urllib.parse import urlparse

FIXTURE_DIR = Path(__file__).parent / "fixtures"


def load_fixture(name: str) -> tuple[int, dict]:
    """Load a fixture JSON file, return (status_code, body)."""
    path = FIXTURE_DIR / name
    if not path.exists():
        return 200, {}
    with open(path) as f:
        data = json.load(f)
    # Status code encoded in filename: name-NNN.json
    code = 200
    parts = path.stem.rsplit("-", 1)
    if len(parts) == 2 and parts[1].isdigit():
        code = int(parts[1])
    return code, data


# Route table: (method, path_prefix) -> fixture_filename
ROUTES = {
    ("GET", "/health"): "health-200.json",
    ("POST", "/api/v1/code-changes/"): "code-changes-201.json",
    ("POST", "/api/v1/file-checkpoints/"): "file-checkpoints-201.json",
    ("POST", "/api/v1/file-checkpoints/dedup"): "file-checkpoints-201.json",
    ("POST", "/api/v1/wal/enqueue"): "memories-200.json",
    ("POST", "/api/v1/memories/"): "memories-200.json",
    ("POST", "/api/v1/memories/search"): "memories-search-200.json",
    ("POST", "/api/v1/memories/batch"): "memories-200.json",
    ("GET", "/api/v1/memories/lookup-by-meta"): "lookup-by-meta-200.json",
    ("POST", "/api/v1/sessions/"): "sessions-201.json",
    ("PUT", "/api/v1/sessions/"): "sessions-201.json",
    ("POST", "/api/v1/git-commits/"): "git-commits-201.json",
    ("GET", "/api/v1/sessions/tasks"): "sessions-tasks-200.json",
    ("GET", "/api/v1/code-changes/"): "code-changes-window-200.json",
    ("GET", "/api/v1/code-changes/window"): "code-changes-window-200.json",
    ("PUT", "/api/v1/tasks/"): "sessions-201.json",
    ("POST", "/api/v1/tasks/"): "sessions-201.json",
}


class MockHandler(BaseHTTPRequestHandler):
    profile = "default"
    log_file = None

    def log_message(self, fmt, *args):
        """Suppress default HTTP logging."""
        pass

    def _record_request(self, body=None):
        """Append request to JSONL log."""
        if not self.log_file:
            return
        entry = {
            "method": self.command,
            "path": self.path,
            "timestamp": time.time(),
        }
        if body:
            try:
                entry["body"] = json.loads(body)
            except (json.JSONDecodeError, TypeError):
                entry["body_raw"] = body if isinstance(body, str) else body.decode("utf-8", errors="replace")
        with open(self.log_file, "a") as f:
            f.write(json.dumps(entry) + "\n")

    def _read_body(self) -> bytes:
        length = int(self.headers.get("Content-Length", 0))
        return self.rfile.read(length) if length > 0 else b""

    def _respond(self, _body=None):
        if self.profile == "timeout":
            time.sleep(30)

        if self.profile == "error-500":
            self._send(500, {"error": "Mock server error"})
            return

        if self.profile == "selective" and self.path != "/health":
            self._send(500, {"error": "Mock selective error"})
            return

        # Match route by (method, path_prefix)
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/") + "/"  # normalize

        matched_fixture = None
        # Try exact match first, then prefix match (longer prefixes first)
        for (method, route_path), fixture in sorted(ROUTES.items(), key=lambda x: -len(x[0][1])):
            if self.command == method:
                route_normalized = route_path.rstrip("/") + "/"
                if path.startswith(route_normalized) or path == route_normalized:
                    matched_fixture = fixture
                    break

        if matched_fixture:
            code, data = load_fixture(matched_fixture)
            self._send(code, data)
        else:
            self._send(404, {"error": "Not found", "path": self.path})

    def _send(self, code: int, data: dict):
        body = json.dumps(data).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self._record_request()
        self._respond()

    def do_POST(self):
        body = self._read_body()
        self._record_request(body)
        self._respond(body)

    def do_PUT(self):
        body = self._read_body()
        self._record_request(body)
        self._respond(body)

    def do_PATCH(self):
        body = self._read_body()
        self._record_request(body)
        self._respond(body)


def main():
    parser = argparse.ArgumentParser(description="Mock HTTP server for hook tests")
    parser.add_argument("--port", type=int, default=0, help="Port (0 = random)")
    parser.add_argument("--profile", default="default", choices=["default", "error-500", "timeout", "selective"])
    parser.add_argument("--log", default="", help="Request log file path")
    args = parser.parse_args()

    MockHandler.profile = args.profile
    MockHandler.log_file = args.log or None

    server = HTTPServer(("127.0.0.1", args.port), MockHandler)
    actual_port = server.server_address[1]

    # Print port on stdout so the caller can capture it
    print(actual_port, flush=True)

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
