#!/usr/bin/env bash
# One-time GitHub setup for the dev/pub branch model (ADR 0006).
#
# Run once, after the first push of both branches:
#   scripts/github-setup.sh OWNER/REPO
#
# What it does (idempotent, safe to run again):
#   - default branch: dev (new PRs target dev by default)
#   - merge commits + squash allowed, rebase off; delete head branches on merge
#   - applies .github/rulesets/*.json (replacing rulesets of the same name)
# Needs the GitHub CLI (`gh auth login`) with admin rights on the repository.
set -euo pipefail

repo="${1:?usage: scripts/github-setup.sh OWNER/REPO}"
here="$(cd "$(dirname "$0")/.." && pwd)"

for b in dev pub; do
  gh api "repos/$repo/branches/$b" --silent ||
    { echo "branch '$b' is not on GitHub yet; push it first" >&2; exit 1; }
done

echo "• repository settings"
gh api -X PATCH "repos/$repo" --silent \
  -f default_branch=dev \
  -F allow_merge_commit=true \
  -F allow_squash_merge=true \
  -F allow_rebase_merge=false \
  -F delete_branch_on_merge=true \
  -F allow_auto_merge=true \
  -f squash_merge_commit_title=PR_TITLE \
  -f squash_merge_commit_message=PR_BODY \
  -f merge_commit_title=PR_TITLE \
  -f merge_commit_message=PR_BODY

echo "• rulesets"
for f in "$here"/.github/rulesets/*.json; do
  name="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$f")"
  id="$(gh api "repos/$repo/rulesets" --jq ".[] | select(.name == \"$name\") | .id" | head -n1)"
  if [ -n "$id" ]; then
    gh api -X PUT "repos/$repo/rulesets/$id" --input "$f" --silent
    echo "  updated: $name"
  else
    gh api -X POST "repos/$repo/rulesets" --input "$f" --silent
    echo "  created: $name"
  fi
done

echo "done: dev is the default branch; pub takes release PRs from dev only."
