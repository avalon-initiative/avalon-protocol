// Mirrors docs/trusted-networks.json into src/generated/ for trustAnchors.ts (#232). Gitignored, never hand-edited.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const dirName = dirname(fileURLToPath(import.meta.url))

export function generateTrustedNetworks() {
  const canonicalPath = resolve(dirName, '../../../docs/trusted-networks.json')
  const generatedPath = resolve(dirName, '../src/generated/trusted-networks.json')
  mkdirSync(dirname(generatedPath), { recursive: true })
  writeFileSync(generatedPath, readFileSync(canonicalPath))
}

if (import.meta.url === `file://${process.argv[1]}`) {
  generateTrustedNetworks()
}
