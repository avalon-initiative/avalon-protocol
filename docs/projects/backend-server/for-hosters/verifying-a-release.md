# Verifying a release

Each [release](https://github.com/avalon-initiative/avalon-protocol/releases)
carries one archive per platform (`avalon-<version>-<target>.tar.gz` on Linux
and macOS, `avalon-<version>-<target>.zip` on Windows, each holding
`avalon-server` and the `avalon` CLI), a `SHA256SUMS` file, and a GitHub build
provenance attestation for every archive. Artifacts are built by the release
workflow from a tagged commit on `main`; there are no long-lived signing keys.

Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin`, `aarch64-apple-darwin` (Apple Silicon), and
`x86_64-pc-windows-msvc`.

## Checksum

Download the archive for your platform and `SHA256SUMS` into one directory.

Linux and macOS:

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

Windows (PowerShell — `sha256sum` isn't available by default):

```powershell
$want = (Select-String -Path SHA256SUMS -Pattern 'avalon-<version>-x86_64-pc-windows-msvc\.zip$').Line.Split()[0]
$have = (Get-FileHash avalon-<version>-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()
if ($want -ne $have) { throw "checksum mismatch" }
```

## Provenance

With the [GitHub CLI](https://cli.github.com/):

```bash
gh attestation verify avalon-<version>-<target>.tar.gz --repo avalon-initiative/avalon-protocol
```

(On Windows, verify the `.zip` archive instead — `gh attestation verify`
itself works the same way, `gh` just needs to be on `PATH`.)

This confirms the archive was produced by this repository's release workflow
and names the commit it was built from. The checksum only proves the download
is intact; the attestation proves where it came from, so check both.

## Then

Extract the archive and continue with [`hosting-quickstart.md`](hosting-quickstart.md)
for configuring the node.
