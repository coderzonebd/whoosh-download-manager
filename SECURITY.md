# Security policy

## Reporting a vulnerability

**Please do not open a public issue, discussion or pull request for a
security problem.**

Report it privately through GitHub's security advisories:
**Security → Report a vulnerability** on this repository
(`https://github.com/coderzonebd/vayu/security/advisories/new`). Only the
maintainers can see the report, and we can work on the fix and the advisory
with you there.

Please include:

- the component and version (or commit) affected;
- what an attacker can do, and what they need first (a malicious web page,
  a local account, a malicious server, a crafted file …);
- steps or a proof of concept that reproduces it;
- any fix or mitigation you already have in mind.

## What to expect

- We aim to acknowledge a report within **3 working days** and to tell you
  whether we accept it, with a severity, within **10 working days**.
- We fix accepted issues in private and agree a disclosure date with you,
  normally within **90 days** of the report, sooner for actively exploited
  issues.
- We publish a GitHub security advisory (with a CVE where it applies) when
  the fix is released, and credit you unless you prefer otherwise.
- There is no bug bounty.

## Scope

In scope: everything in this repository, in particular

- the engine and daemon, and its local API (token binding, origin checks);
- the browser extension and the native messaging host `vayu-nmhost`
  (which extensions may start it, what it relays);
- file handling: names from servers and web pages, paths, archive
  extraction, resume data;
- installers, the update channel and release signing.

Out of scope: `vayu-labserver` and the benchmark harness (test tools that are
meant to misbehave; they must still not be exploitable when run on
`127.0.0.1` as documented), vulnerabilities in third-party services, and
reports that need an already-compromised machine or physical access.

## Safe harbour

We will not pursue or support legal action against anyone who researches and
reports in good faith under this policy: test only against your own
installations, avoid harming other users' data or privacy, and give us a
reasonable time to fix before going public.

## Supported versions

The project is pre-1.0 (Phase 0). Only the latest release and the `main`
branch receive security fixes.

## Verifying releases

How to check checksums, build provenance and signatures of a download is
described in [docs/release.md](docs/release.md#verifying-a-download).
