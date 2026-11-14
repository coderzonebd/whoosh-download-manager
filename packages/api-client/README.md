# @wdm/api-client

TypeScript client for the daemon's local API ([protocol v1](../../docs/api/PROTOCOL.md)).
Every UI codes against the `WdmClient` interface in [`src/types.ts`](src/types.ts);
the transports below implement it. Apache-2.0.

| Import | What |
|---|---|
| `@wdm/api-client` | Types, method/event names, `WdmClient` (the contract; mirrors `crates/proto`) |
| `@wdm/api-client/ws` | `connectWs()` — JSON-RPC 2.0 over WebSocket |
| `@wdm/api-client/mock` | `createMockClient()` — an in-memory daemon for UIs and tests |
| `@wdm/api-client/errors` | `WdmRpcError` (daemon said no), `WdmConnectionError` (transport) |
| `@wdm/api-client/categories` | `categoryForFilename()` and the category ids |

## WebSocket client

```ts
import { connectWs } from "@wdm/api-client/ws";

const client = connectWs({
  url: () => `ws://127.0.0.1:${runtime.port}/rpc`, // functions are re-read on every reconnect
  token: () => runtime.token,
  client: { name: "desktop", version: "0.0.1" },
});
client.onStatus((s) => console.log(s.state)); // connecting | open | reconnecting | closed
const list = await client.call("download.list", {}); // queued until the hello completes
const off = client.on("event.download.progress", (e) => render(e.items));
```

- `session.hello` is always the first frame of every connection.
- Responses are matched by id, in any order; errors become `WdmRpcError`
  with the daemon's `code` (`isNotFound`, `isInvalidState`, …).
- A dropped socket rejects in-flight calls with `connection_lost` (they may
  or may not have run) and reconnects with exponential backoff and jitter;
  calls made meanwhile wait in a queue. Close codes 4401/4426 are final.
- `requestTimeoutMs` (default 30 s) rejects calls that never get an answer.

## Mock daemon

```ts
import { createMockClient } from "@wdm/api-client/mock";
const client = createMockClient();                  // 14 realistic downloads, ticking at 4 Hz
const big = createMockClient({ bulk: 10_000 });     // stress the UI
const test = createMockClient({ autoTick: false }); // tests: drive time with test.advance(ms)
```

It simulates segmented downloads with dynamic splitting, wobbling speeds, the
active-download limit, the global speed limit, a failure (`remote_changed`),
an expiring link (`needs_refresh` → `download.refreshUrl` continues where it
stopped), duplicate file names, probing and verification, and progress events
batched at ≤ 4 Hz. URLs containing `/404` or a `.invalid` host fail to probe.

## Commands

```bash
corepack pnpm --filter @wdm/api-client test       # mock + WebSocket suites (fake daemon on `ws`)
corepack pnpm --filter @wdm/api-client typecheck
```
