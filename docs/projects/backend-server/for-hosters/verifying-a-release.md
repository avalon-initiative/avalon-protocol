# Verifying a release

Each [release](https://github.com/avalon-initiative/avalon-protocol/releases)
carries one tarball per platform (`avalon-<version>-<target>.tar.gz`, holding
`avalon-server` and the `avalon` CLI), a `SHA256SUMS` file, and a GitHub build
provenance attestation for every tarball, plus a multi-arch container image at
`ghcr.io/avalon-initiative/avalon-protocol`, also attested. Artifacts are
built by the release workflow from a tagged commit on `main`; there are no
long-lived signing keys.

Tarball targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin`, and `aarch64-apple-darwin` (Apple Silicon). Windows
builds are not available yet — the server is Linux-first for now. The
container image covers the two Linux targets only, as `linux/amd64` and
`linux/arm64`.

## Checksum

Download the tarball for your platform and `SHA256SUMS` into one directory:

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

## Provenance

With the [GitHub CLI](https://cli.github.com/):

```bash
gh attestation verify avalon-<version>-<target>.tar.gz --repo avalon-initiative/avalon-protocol
```

This confirms the tarball was produced by this repository's release workflow
and names the commit it was built from. The checksum only proves the download
is intact; the attestation proves where it came from, so check both.

## Verifying the container image

The same command works against the image reference, by digest or by tag:

```bash
gh attestation verify oci://ghcr.io/avalon-initiative/avalon-protocol:v<version> \
  --repo avalon-initiative/avalon-protocol
```

This confirms the published `linux/amd64`/`linux/arm64` manifest list was
built by the release workflow's `image`/`image-publish` jobs from the exact
binaries in that same release's tarballs — not a separate rebuild from
source. `docker pull`ing the image resolves the same reference; no separate
download is required to verify it beforehand, unlike the tarball.

## Then

Extract the tarball and continue with [`hosting-quickstart.md`](hosting-quickstart.md)
for configuring the node.
