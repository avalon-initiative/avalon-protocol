// Issue #232's "not a separately maintained copy" invariant, enforced: the
// README's "Trusted networks" table has to actually match
// docs/trusted-networks.json, the one canonical file. This reads both
// straight off disk (not the Vite-bundled copy — see trustAnchors.ts's own
// note on why that's generated) so a change to the JSON that isn't also
// reflected in the README fails this test rather than silently drifting.
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const repoRoot = resolve(fileURLToPath(import.meta.url), '../../../../..')

interface TrustedNetworksFile {
  networks: Array<{ label: string; network_id: string; verify_key: string }>
}

function readTrustedNetworks(): TrustedNetworksFile {
  const raw = readFileSync(resolve(repoRoot, 'docs/trusted-networks.json'), 'utf-8')
  return JSON.parse(raw) as TrustedNetworksFile
}

function readReadme(): string {
  return readFileSync(resolve(repoRoot, 'README.md'), 'utf-8')
}

describe('README trusted-networks table', () => {
  it('lists every network_id and verify_key from docs/trusted-networks.json', () => {
    const { networks } = readTrustedNetworks()
    expect(networks.length).toBeGreaterThan(0)

    const readme = readReadme()
    for (const entry of networks) {
      expect(readme).toContain(entry.network_id)
      expect(readme).toContain(entry.verify_key)
    }
  })

  it('links to the canonical file and the full trust-anchor model doc', () => {
    const readme = readReadme()
    expect(readme).toContain('docs/trusted-networks.json')
    expect(readme).toContain('docs/projects/backend-server/architecture/network-trust-anchors.md')
  })
})
