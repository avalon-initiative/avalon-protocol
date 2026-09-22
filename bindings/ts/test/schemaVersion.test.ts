import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { OPENAPI_SCHEMA_VERSION } from '../src/index.js'

// Issue #735: OPENAPI_SCHEMA_VERSION is generated straight from
// docs/generated/openapi.json's own info.version by generate-types.mjs, so
// this can't drift by construction — this test exists to catch a future
// change to that generation step breaking the link, not routine drift.
describe('OPENAPI_SCHEMA_VERSION', () => {
  it('matches docs/generated/openapi.json info.version', () => {
    const schemaPath = fileURLToPath(new URL('../../../docs/generated/openapi.json', import.meta.url))
    const schema = JSON.parse(readFileSync(schemaPath, 'utf8'))
    expect(OPENAPI_SCHEMA_VERSION).toBe(schema.info.version)
  })
})
