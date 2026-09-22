import { afterEach, describe, expect, it } from 'vitest'
import { getIdentityIntegratorData, getLocations } from '../src/identityData.js'

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

function mockFetchOnce(body: unknown) {
  globalThis.fetch = (async () => new Response(JSON.stringify(body), { status: 200 })) as typeof fetch
}

describe('getIdentityIntegratorData', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockFetchOnce([{ schema: 'game:ashen-realms:schema:1', integrator_id: 'i1', published_at: 'now', fields: { level: 5 } }])
    const result = await getIdentityIntegratorData('http://127.0.0.1:1', 'id-1')
    expect(result).toEqual([{ schema: 'game:ashen-realms:schema:1', integratorId: 'i1', publishedAt: 'now', fields: { level: 5 } }])
  })

  it('passes through a non-array body instead of crashing on it', async () => {
    mockFetchOnce(null)
    expect(await getIdentityIntegratorData('http://127.0.0.1:1', 'id-1')).toBeNull()
  })
})

describe('getLocations', () => {
  it('returns the locations array from the response', async () => {
    mockFetchOnce({ locations: ['http://node-a', 'http://node-b'] })
    expect(await getLocations('http://127.0.0.1:1', 'id-1')).toEqual(['http://node-a', 'http://node-b'])
  })
})
