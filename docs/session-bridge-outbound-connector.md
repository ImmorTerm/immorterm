# Session Bridge outbound connector

The outbound connector lets a served ImmorTerm Hub reach sessions on a desktop Hub that listens only on loopback. The desktop opens the WebSocket. No desktop port is exposed to the internet.

This is transport for Longstory Code. It does not store or search Longstory knowledge itself.

## Provision a connector credential

Use the served Hub deployment administrator credential only to create or revoke a short-lived connector credential:

```http
POST /api/v1/bridge/installations/credentials
Authorization: Bearer <deployment-administrator-token>
Content-Type: application/json

{
  "installation_id": "desktop-shai-1",
  "project_id": "<stable-project-id>",
  "audience": "immorterm:session-bridge:connector:v1",
  "operations": ["connector:connect"],
  "ttl_seconds": 86400
}
```

The returned `imsb_...` token can connect only as that desktop, for that project, to the connector endpoint. It cannot list sessions, send messages, acknowledge messages, or reply as an agent.

## Configure the desktop Hub

Set the connector URL and either a token or a token-file path before starting the desktop Hub:

```bash
IMMORTERM_BRIDGE_CONNECTOR_URL=wss://<served-hub>/api/v1/bridge/connectors
IMMORTERM_BRIDGE_CONNECTOR_TOKEN_FILE=/path/to/mode-0600-token-file
```

`IMMORTERM_BRIDGE_CONNECTOR_TOKEN` is also supported, but the file form is better for rotation. The Hub rejects a token file readable by group or other users and reads the file again on every reconnect. A non-loopback connector must use `wss://`; plain `ws://` is accepted only for `localhost`, `127.0.0.1`, or `::1` development. The URL must be the connector endpoint without user information, a query string, or a fragment, so a token cannot be placed in the URL by mistake.

## Delivery behavior

- The desktop sends only its project-scoped local session directory. Stable `window_id` values remain the only message targets.
- The served Hub saves the last directory snapshot. If the desktop disconnects or the served Hub restarts, known targets remain visible as offline.
- Messages for an offline connector stay `queued` on the served Hub until the connector reconnects or the message expires.
- Reconnect delivery is at least once. The existing message ID and canonical envelope hash make a repeated delivery idempotent.
- The served Hub saves the desktop event cursor. Reconnect replays retained events; an expired cursor sends a bounded repair snapshot, including replies made while the network was down.
- The desktop delivers through the normal local Hub and daemon path. The exact receiving daemon still owns the opaque receipt required for acknowledgement and reply.
- State changes and replies are accepted only when the connector identity, message ID, correlation ID, target window, and envelope hash match the original served message.
- The connector credential is checked during the connection. Expired or revoked credentials close within 30 seconds.

The non-secret connection count is available from `GET /api/v1/bridge/status` as `connected_outbound_connectors`.

## Rotation and revocation

Create the replacement credential first, atomically replace the mode-`0600` token file, then revoke the old token with:

```http
DELETE /api/v1/bridge/installations/{installation_id}/credentials/{token_id}
Authorization: Bearer <deployment-administrator-token>
```

The desktop reconnects with the replacement token after the served Hub closes the old connection. Do not put connector tokens in URLs, command-line arguments, logs, registry records, or Longstory memory.
