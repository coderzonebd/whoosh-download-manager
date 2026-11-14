// SPDX-License-Identifier: Apache-2.0
// Client wrappers shared by the tests.
import { ErrorCode, type EventMap, type WhooshClient } from "@whoosh/api-client";
import { WhooshRpcError } from "@whoosh/api-client/errors";

/** A daemon from before queues: every queue method answers -32006. */
export function withoutQueues(client: WhooshClient): WhooshClient & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    call: ((method: string, params: unknown) => {
      calls.push(method);
      if (method.startsWith("queue.") || method === "download.setQueue") {
        return Promise.reject(new WhooshRpcError(ErrorCode.Unsupported, "queues are not available in this build", undefined, method));
      }
      return (client.call as (m: string, p: unknown) => Promise<unknown>)(method, params);
    }) as WhooshClient["call"],
    on: client.on.bind(client),
    close: () => client.close(),
  };
}

/** Lets a test send events the mock never sends (e.g. `event.queue.finished`). */
export function withEvents<C extends WhooshClient>(client: C): C & { fire<E extends keyof EventMap>(e: E, p: EventMap[E]): void } {
  const handlers = new Map<string, Set<(p: unknown) => void>>();
  const on = client.on.bind(client);
  return Object.assign(Object.create(client) as C, {
    call: client.call.bind(client),
    close: client.close.bind(client),
    on: ((e: string, h: (p: unknown) => void) => {
      let set = handlers.get(e);
      if (!set) handlers.set(e, (set = new Set()));
      set.add(h);
      const off = on(e as keyof EventMap, h as never);
      return () => {
        set?.delete(h);
        off();
      };
    }) as C["on"],
    fire<E extends keyof EventMap>(e: E, p: EventMap[E]) {
      for (const h of handlers.get(e) ?? []) h(p);
    },
  });
}
