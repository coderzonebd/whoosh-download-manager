# ADR 0006: Two long-lived branches, `dev` and `pub`

- Status: accepted
- Date: 2026-11-13
- Supersedes: the branch part of [ADR 0004](0004-parallel-development.md) (phase branches and `main`)

## Context
Phases 0 and 1 used one integration branch per phase plus `main`. That served a single maintainer working locally. Once the repository is public, contributors need one obvious place to send work. Users need one branch that holds only released, tested code.

## Decision
- **`dev`** is the default branch and the only integration branch. Every change reaches it through a pull request with green CI (the `CI OK` check) and one approving review. Squash and merge commits are both allowed.
- **`pub`** holds releases and production code only. It changes in exactly one way: a **release PR from `dev`**, merged with a merge commit so `pub` stays an ancestor-compatible snapshot of `dev`. Release tags (`vX.Y.Z`) are cut on `pub`; the release workflow refuses a tag that is not on `pub`.
- **Nobody opens a PR into `pub` from anything but `dev`.** GitHub cannot forbid opening such a PR, so the repository enforces it in three layers:
  1. [`pub-redirect.yml`](../../.github/workflows/pub-redirect.yml) moves any PR into `pub` that is not from this repository's `dev` to `dev` at once, with a comment. It uses the API only and never runs PR code.
  2. [`pub-guard.yml`](../../.github/workflows/pub-guard.yml) fails the required check `PR source is dev` on such a PR.
  3. The [`pub` ruleset](../../.github/rulesets/pub.json) requires that check, `CI OK` and a review. It also forbids force pushes and deletion.
- **Work branches** (`feat/…`, `fix/…`, `perf/…`, `docs/…`) are cut from `dev` and deleted after merge. Phase work and parallel sessions keep ADR 0004's ownership and build-budget rules, but target `dev`.
- **Phase gates** still decide when `dev` may be released. A phase closes with a release PR `dev → pub` and a tag.
- [`scripts/github-setup.sh`](../../scripts/github-setup.sh) applies the settings and rulesets once both branches are on GitHub.

## Consequences
- Contributors never need to know the release process: the default branch is `dev`, and a mistaken PR into `pub` fixes itself.
- `pub` is always releasable, so its README, tags and releases are what users see.
- Repository admins can merge a release PR without a second reviewer (`bypass_mode: pull_request`), but still only through a PR, never by pushing.
