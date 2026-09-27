# Verifying a release

Each [release](https://github.com/avalon-initiative/avalon-protocol/releases)
carries one tarball per platform (`avalon-<version>-<target>.tar.gz`, holding
`avalon-server` and the `avalon` CLI), a `SHA256SUMS` file, and a GitHub build
provenance attestation for every tarball. Artifacts are built by the release
workflow from a tagged commit on `main`; there are no long-lived signing keys.

Targets: `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`. macOS and
Windows builds are not published yet.

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

## Then

Extract the tarball and continue with [`hosting-quickstart.md`](hosting-quickstart.md)
for configuring the node.
