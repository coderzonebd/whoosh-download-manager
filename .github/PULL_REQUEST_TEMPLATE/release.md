<!-- Release PR: dev → pub. Open with ?template=release.md and merge with a merge commit. -->

## Release vX.Y.Z

### What ships

<!-- Highlights for users, newest first. Link the PRs merged into dev since the last release. -->

### Gate evidence

- [ ] Phase gate / release checklist passed; report: `docs/bench/data/<run-set>/report.md`
- [ ] Crash test (`whoosh-bench hardkill`): 0 corrupt, 0 duplicates
- [ ] Versions bumped in all four places (see `docs/release.md`)
- [ ] `CHANGELOG.md` updated

### After merge

- [ ] Tag the merge commit on `pub`: `git tag -s vX.Y.Z -m "vX.Y.Z"` and push the tag
- [ ] Check the draft GitHub release, then publish it
