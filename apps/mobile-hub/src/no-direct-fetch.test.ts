// All server traffic goes through `@avalon-initiative/protocol-sdk`. This walks
// the app's own source looking for a hand-rolled `fetch(` client, which would
// mean the app grew a second implementation of what the SDK already owns.
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

function collectSourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules') continue
    const path = join(dir, entry.name)
    if (entry.isDirectory()) {
      collectSourceFiles(path, out)
    } else if (/\.(ts|vue)$/.test(entry.name) && !entry.name.endsWith('.test.ts')) {
      out.push(path)
    }
  }
  return out
}

describe('no direct API client code in the app source', () => {
  it('never calls fetch() directly; the SDK owns every server request', () => {
    const files = collectSourceFiles(__dirname)
    const offenders = files.filter((file) => /\bfetch\(/.test(readFileSync(file, 'utf-8')))
    expect(offenders).toEqual([])
  })
})
