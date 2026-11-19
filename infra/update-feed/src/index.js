// SPDX-License-Identifier: Apache-2.0
// WDM's update feed, served through a Cloudflare Worker so that copies in use
// can be counted without identifying anyone.
//
//   GET /latest.json?target={{target}}&arch={{arch}}&v={{current_version}}
//   GET /stable.json?...   the same file the app used to read from the
//                          `updater` branch, passed through unchanged; the
//                          check is counted as +1 for (day, channel, target,
//                          arch, version)
//   GET /stats             the counts of the last 60 days, as JSON
//
// The desktop app checks once a day, so checks per day are close to the
// number of copies running that day. No IP address, header, ID or time of day
// is read or stored, and request logging is off (wrangler.toml).

const FEEDS = new Set(["latest.json", "stable.json"]);
const TARGETS = new Set(["linux", "darwin", "windows"]);
const ARCHS = new Set(["x86_64", "aarch64", "i686", "armv7", "riscv64"]);
const VERSION = /^\d{1,4}\.\d{1,4}\.\d{1,4}(?:-[0-9A-Za-z.]{1,24})?$/;
const STATS_DAYS = 60;

/** The counted key for a request, with anything unexpected folded into "other". */
export function key(url, now = new Date()) {
  const p = url.searchParams;
  const pick = (v, ok) => (v && ok(v) ? v : "other");
  return {
    day: now.toISOString().slice(0, 10),
    channel: url.pathname.slice(1).replace(".json", ""),
    target: pick(p.get("target"), (v) => TARGETS.has(v)),
    arch: pick(p.get("arch"), (v) => ARCHS.has(v)),
    version: pick(p.get("v"), (v) => VERSION.test(v)),
  };
}

export async function count(db, k) {
  await db
    .prepare(
      "INSERT INTO checks (day, channel, target, arch, version, n) VALUES (?1, ?2, ?3, ?4, ?5, 1) " +
        "ON CONFLICT (day, channel, target, arch, version) DO UPDATE SET n = n + 1",
    )
    .bind(k.day, k.channel, k.target, k.arch, k.version)
    .run();
}

export async function stats(db, now = new Date()) {
  const since = new Date(now.getTime() - STATS_DAYS * 86400e3).toISOString().slice(0, 10);
  const { results } = await db
    .prepare(
      "SELECT day, target, arch, version, SUM(n) AS checks FROM checks WHERE day >= ?1 " +
        "GROUP BY day, target, arch, version ORDER BY day DESC, checks DESC",
    )
    .bind(since)
    .all();
  return { days: results };
}

const json = (body, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", "access-control-allow-origin": "*" },
  });

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (request.method !== "GET") return json({ error: "GET only" }, 405);

    if (url.pathname === "/stats") return json(await stats(env.DB));

    const file = url.pathname.slice(1);
    if (!FEEDS.has(file)) return json({ error: "not found" }, 404);

    const upstream = await fetch(env.UPSTREAM + file, { cf: { cacheTtl: 300, cacheEverything: true } });
    // Count only answers the app can use; if counting fails, the update still works.
    if (upstream.ok) ctx.waitUntil(count(env.DB, key(url)).catch(() => {}));
    return new Response(upstream.body, {
      status: upstream.status,
      headers: { "content-type": "application/json", "cache-control": "no-store" },
    });
  },
};
