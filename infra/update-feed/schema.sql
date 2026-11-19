-- SPDX-License-Identifier: Apache-2.0
-- One row per day, channel, platform and app version: how many update checks
-- arrived. Nothing else is stored: no IP address, no ID, no time of day.
CREATE TABLE IF NOT EXISTS checks (
  day     TEXT    NOT NULL, -- UTC, YYYY-MM-DD
  channel TEXT    NOT NULL, -- latest | stable
  target  TEXT    NOT NULL, -- linux | darwin | windows | other
  arch    TEXT    NOT NULL, -- x86_64 | aarch64 | i686 | armv7 | other
  version TEXT    NOT NULL, -- the checking copy's version, or "other"
  n       INTEGER NOT NULL,
  PRIMARY KEY (day, channel, target, arch, version)
);
