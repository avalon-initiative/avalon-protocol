// Mirrors docs/trusted-networks.json (repo root, the one canonical
// trust-anchor list — see #232) into src/generated/trusted-networks.json,
// which src/network/trustAnchors.ts imports directly. Gitignored, never
// hand-edited. Run standalone as `npm run prebuild -w apps/hub` (npm runs
// this automatically before `build`, since `vue-tsc -b` type-checks before
// vite.config.ts's own copy of this logic ever loads) and also invoked
// directly from vite.config.ts so `dev`/`test` stay covered too.
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
