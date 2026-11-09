# ADR 0002 — Licensing

- Status: accepted
- Date: 2026-11-10

## Context
We want two things that pull in different directions: other projects should be able to embed our engine (adoption, reputation, contributors), and nobody should be able to take the finished product, close it and sell it.

## Decision
- **Apache-2.0** for everything under `crates/` (engine, protocol, daemon, CLI, labserver, bench, native messaging host) and under `packages/` (UI library, design tokens, API client). Text: `LICENSE-APACHE`.
- **GPL-3.0-or-later** for the finished applications under `apps/` (desktop, extension, mobile, web remote). Text: `LICENSE-GPL`.
- Each crate states `license` in its `Cargo.toml`; each package states `license` in its `package.json`. `LICENSE.md` at the root explains the split.
- External tools (yt-dlp — Unlicense, ffmpeg — LGPL build, 7-Zip — LGPL with unRAR restriction) run as separate processes and keep their own licenses.
- Contributions are accepted under the license of the directory they touch (inbound = outbound). No CLA.

## Consequences
- GPL applications may depend on Apache crates and packages (one-way compatible); Apache crates must never depend on GPL code.
- A company can ship the engine inside a closed product; it cannot ship a closed fork of our apps.
