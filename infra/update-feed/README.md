# Update feed

A Cloudflare Worker that serves the desktop app's update feed and counts, per
day, how many update checks arrive. The app checks once a day, so this is close
to the number of copies running each day. It is the only usage number WDM
collects, and it identifies nobody:

- the app adds only its platform, processor type and version to the address
  (`?target={{target}}&arch={{arch}}&v={{current_version}}`, filled in by
  `tauri-plugin-updater`);
- the Worker stores one counter per day, channel, platform, processor type and
  version ([`schema.sql`](schema.sql)): no IP address, header, ID or time;
- request logging is off ([`wrangler.toml`](wrangler.toml));
- the counts are public at `/stats`, and `.github/workflows/usage-stats.yml`
  copies them daily to the `stats` branch.

The feed itself is the `updater` branch's `latest.json` / `stable.json`,
passed through unchanged (cached 5 minutes), so its signatures are checked by
the app exactly as before. The app lists this Worker first and
raw.githubusercontent.com second, so if the Worker is down the update still
works and just isn't counted.

## Deploy (once)

```bash
cd infra/update-feed
npx wrangler login                       # opens the browser; sign in to Cloudflare
npx wrangler d1 create wdm-update-feed   # put the printed database_id in wrangler.toml
npx wrangler d1 execute wdm-update-feed --remote --file schema.sql
npx wrangler deploy                      # prints https://wdm-update-feed.<account>.workers.dev
```

Then set the repository variable `WDM_UPDATE_FEED` to that address, and use it
as the first entry of `plugins.updater.endpoints` in
`apps/desktop/src-tauri/tauri.conf.json`. Copies get the new address with the
next release.

## Test

```bash
node --test infra/update-feed/src/index.test.mjs
```

The free Workers and D1 plans cover 100,000 checks a day.
