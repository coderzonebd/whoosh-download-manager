#!/usr/bin/env bash
# One-time GitHub setup for the dev/pub branch model (ADR 0006).
#
# Run once, after the first push of both branches:
#   scripts/github-setup.sh OWNER/REPO
#
# What it does (idempotent, safe to run again):
#   - default branch: pub, so visitors see the latest release (PRs into pub
#     are moved to dev by .github/workflows/pub-redirect.yml)
#   - merge commits + squash allowed, rebase off; delete head branches on merge
#   - description, topics, Issues + Discussions on, Wiki + Projects off
#   - Dependabot alerts, private vulnerability reporting, secret scanning
#     with push protection
#   - the `release` environment, deployable from v* tags only
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
  -f default_branch=pub \
  -F allow_merge_commit=true \
  -F allow_squash_merge=true \
  -F allow_rebase_merge=false \
  -F delete_branch_on_merge=true \
  -F allow_auto_merge=true \
  -f squash_merge_commit_title=PR_TITLE \
  -f squash_merge_commit_message=PR_BODY \
  -f merge_commit_title=PR_TITLE \
  -f merge_commit_message=PR_BODY

echo "• about, topics and features"
gh api -X PATCH "repos/$repo" --silent \
  -f description="Whoosh Download Manager (WDM): an open-source, crash-proof download manager that proves it is fast. Rust engine, daemon and CLI; desktop app and browser extension next." \
  -F has_issues=true \
  -F has_discussions=true \
  -F has_projects=false \
  -F has_wiki=false
gh api -X PUT "repos/$repo/topics" --silent \
  -f 'names[]=download-manager' -f 'names[]=downloader' -f 'names[]=download-accelerator' \
  -f 'names[]=rust' -f 'names[]=http3' -f 'names[]=quic' -f 'names[]=cli' -f 'names[]=tauri' \
  -f 'names[]=browser-extension' -f 'names[]=cross-platform' -f 'names[]=crash-safe' \
  -f 'names[]=benchmark' -f 'names[]=bangladesh' -f 'names[]=idm-alternative'

echo "• security"
gh api -X PUT "repos/$repo/vulnerability-alerts" --silent
gh api -X PUT "repos/$repo/private-vulnerability-reporting" --silent
gh api -X PATCH "repos/$repo" --silent --input - <<'JSON'
{"security_and_analysis": {
  "secret_scanning": {"status": "enabled"},
  "secret_scanning_push_protection": {"status": "enabled"}
}}
JSON

echo "• release environment (deployments from v* tags only)"
gh api -X PUT "repos/$repo/environments/release" --silent --input - <<'JSON'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
JSON
if ! gh api "repos/$repo/environments/release/deployment-branch-policies" \
  --jq '.branch_policies[] | select(.type == "tag" and .name == "v*") | .id' | grep -q .; then
  gh api -X POST "repos/$repo/environments/release/deployment-branch-policies" \
    -f name='v*' -f type=tag --silent
fi

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

echo "done: pub is the default branch and takes release PRs from dev only; all work goes to dev."
