# Docker image (skeleton)

Multi-stage, non-root image for the future headless daemon (MASTER-PLAN §4.5:
Docker Hub/GHCR, Unraid/Synology templates). **The daemon does not exist yet**,
so the image builds and runs [`whoosh-labserver`](../../crates/labserver) to keep
the build path exercised.

```bash
docker build -f packaging/docker/Dockerfile -t whoosh-labserver .
docker run --rm -p 127.0.0.1:18080:18080 -p 127.0.0.1:18443:18443 whoosh-labserver
curl http://127.0.0.1:18080/s/clean/1m.bin -o /dev/null
```

Publish on `127.0.0.1` as above. A bare `-p 18080:18080` exposes the server on
every host interface, and labserver serves gigabytes per second of generated
data with no authentication: keep it off the LAN.

- Build stage: `rust:1.99.1-slim-trixie`; runtime: `debian:trixie-slim` with
  `ca-certificates`, user `whoosh` (uid/gid 10001), no shell login.
- Both bases are pinned by multi-arch index digest, so
  `docker buildx build --platform linux/amd64,linux/arm64` works.
- `Dockerfile.dockerignore` keeps `node_modules`, `target`, `.git` and
  build output out of the build context.
- CI builds it and checks it does not run as root (`docker` job in
  `.github/workflows/ci.yml`, non-blocking for now). Nothing is pushed.

## Placeholders until the daemon and the name are settled

- Package, binary, entrypoint, ports and `LABEL`s name `whoosh-labserver`.
- `org.opencontainers.image.source` points at `github.com/coderzonebd/whoosh-download-manager`.
- No volume, healthcheck or config file yet: the daemon will need a data
  volume (`WHOOSH_DATA_DIR`), a download volume and a token for its API.
- Publishing (GHCR with `GITHUB_TOKEN`, Docker Hub with its own token),
  image signing (cosign) and an SBOM attestation belong in `release.yml`
  once there is a daemon to ship.
- The Rust version in the `FROM` line must follow `rust-toolchain.toml`;
  Dependabot does not watch this file on purpose (it would bump them apart).
