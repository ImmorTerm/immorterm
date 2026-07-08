/**
 * MCP Gateway Dashboard — VS Code Webview Panel
 *
 * Opens the ImmorTerm dashboard's MCP Gateway page inside a VS Code
 * webview panel using an iframe. Tries the web app first (localhost:3090),
 * falls back to the gateway's self-served dashboard (localhost:9100).
 */

import * as vscode from 'vscode';
import { GATEWAY_PORT } from './gateway-config';

/** ImmorTerm web app dev server port */
const WEB_APP_PORT = 3090;

let currentPanel: vscode.WebviewPanel | undefined;

/**
 * Open the MCP Gateway Dashboard in a VS Code webview panel.
 * Reuses the existing panel if one is already open.
 */
export function openGatewayDashboard(): void {
  if (currentPanel) {
    currentPanel.reveal(vscode.ViewColumn.One);
    return;
  }

  currentPanel = vscode.window.createWebviewPanel(
    'immorterm.gatewayDashboard',
    'MCP Gateway Dashboard',
    vscode.ViewColumn.One,
    {
      enableScripts: true,
      retainContextWhenHidden: true,
    },
  );

  const primaryUrl = `http://localhost:${WEB_APP_PORT}/dashboard/mcp-gateway`;
  const fallbackUrl = `http://localhost:${GATEWAY_PORT}/dashboard`;

  currentPanel.webview.html = getWebviewHtml(primaryUrl, fallbackUrl);

  currentPanel.onDidDispose(() => {
    currentPanel = undefined;
  });
}

function getWebviewHtml(primaryUrl: string, fallbackUrl: string): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <style>
    body, html { margin: 0; padding: 0; width: 100%; height: 100vh; overflow: hidden; background: #0a0a0a; }
    iframe { width: 100%; height: 100%; border: none; }
    .fallback {
      display: none;
      color: #cccccc;
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, monospace;
      text-align: center;
      padding: 40px 20px;
    }
    .fallback h2 { font-size: 16px; margin-bottom: 12px; }
    .fallback p { font-size: 13px; color: #808080; }
    .fallback a { color: #a855f7; text-decoration: none; }
  </style>
</head>
<body>
  <iframe id="dashboard" src="${primaryUrl}" sandbox="allow-scripts allow-same-origin"></iframe>
  <div class="fallback" id="fallback">
    <h2>MCP Gateway Dashboard</h2>
    <p>Could not connect to the dashboard.</p>
    <p>Run <code>npx immorterm</code> to start the dashboard, or check that the MCP Gateway is running.</p>
  </div>
  <script>
    var iframe = document.getElementById('dashboard');
    var fallbackUrl = ${JSON.stringify(fallbackUrl)};
    var triedFallback = false;

    function showFallback() {
      iframe.style.display = 'none';
      document.getElementById('fallback').style.display = 'block';
    }

    function tryFallback() {
      if (triedFallback) { showFallback(); return; }
      triedFallback = true;
      iframe.src = fallbackUrl;
      // Give the fallback URL time to load
      setTimeout(function() {
        try {
          if (!iframe.contentDocument || !iframe.contentDocument.body.childNodes.length) {
            showFallback();
          }
        } catch(e) { showFallback(); }
      }, 3000);
    }

    iframe.onerror = function() { tryFallback(); };

    // Detect load failure via timeout (onerror doesn't fire for connection refused)
    setTimeout(function() {
      try {
        if (!iframe.contentDocument || !iframe.contentDocument.body.childNodes.length) {
          tryFallback();
        }
      } catch(e) { tryFallback(); }
    }, 3000);
  </script>
</body>
</html>`;
}
