#!/usr/bin/env node
// The README's "Trusted networks" table must match docs/trusted-networks.json,
// the one canonical file, so a change to the JSON that is not reflected in the
// README fails here instead of silently drifting. No dependencies: plain Node.
//
// Usage: node scripts/check-trust-anchors.mjs [--readme <path>] [--json <path>]
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
const option = (name, fallback) => {
  const i = args.indexOf(name)
  return i >= 0 && args[i + 1] ? resolve(args[i + 1]) : fallback
}
const readmePath = option('--readme', resolve(root, 'README.md'))
const jsonPath = option('--json', resolve(root, 'docs/trusted-networks.json'))

const readme = readFileSync(readmePath, 'utf-8')
const { networks } = JSON.parse(readFileSync(jsonPath, 'utf-8'))

const problems = []
if (!Array.isArray(networks) || networks.length === 0) {
  problems.push(`${jsonPath} lists no networks`)
} else {
  for (const entry of networks) {
    for (const field of ['network_id', 'verify_key']) {
      if (!readme.includes(entry[field])) {
        problems.push(`README is missing ${field} ${entry[field]} (${entry.label})`)
      }
    }
  }
}
for (const needle of ['docs/trusted-networks.json', 'docs/projects/backend-server/architecture/network-trust-anchors.md']) {
  if (!readme.includes(needle)) problems.push(`README does not link ${needle}`)
}

if (problems.length > 0) {
  console.error(`trust anchors: README is out of sync with docs/trusted-networks.json`)
  for (const p of problems) console.error(`  - ${p}`)
  process.exit(1)
}
console.log(`trust anchors: README matches docs/trusted-networks.json (${networks.length} network${networks.length === 1 ? '' : 's'})`)
