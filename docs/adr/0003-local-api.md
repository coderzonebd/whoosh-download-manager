# ADR 0003 — Local API transport and security

- Status: accepted
- Date: 2026-11-10

## Context
Many clients (desktop window, CLI, extension, web remote, MCP) must control one daemon. A download manager that listens on a local port is a classic attack surface: any web page can try to reach `localhost` and make the app download, or write, files.

## Decision
- **JSON-RPC 2.0 over WebSocket on 127.0.0.1 only**, spec in `docs/api/PROTOCOL.md`, types in `crates/proto`.
- **Token authentication** from a user-only runtime file; the first message must be `session.hello`.
- **Origin allow-list** at the WebSocket upgrade: only native clients (no `Origin`) and the Tauri origins.
- **The extension never talks to the socket.** It uses native messaging to `whoosh-nmhost`, which holds the token. The host manifest restricts which extension IDs may connect.
- A web page can never choose a local path: `directory` in `download.add` is honoured only from authenticated clients, and filenames are sanitised by the daemon in every case.
- The web remote (Phase 3) is served over TLS with device pairing, not with this token.

## Consequences
- The extension needs the native host installed for Pro mode; Lite mode (no app) is a separate code path inside the extension.
- Clients must handle the daemon restarting on a different port by re-reading the runtime file.
