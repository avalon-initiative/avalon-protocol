import { afterEach, describe, expect, it } from 'vitest'
import { getIntegrator, listIntegrators, listIssuerKeys, listAchievementDefinitions } from '../src/integratorDirectory.js'

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

function mockFetchOnce(body: unknown) {
  globalThis.fetch = (async () => new Response(JSON.stringify(body), { status: 200 })) as typeof fetch
}

describe('getIntegrator', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockFetchOnce({
      id: 'i1',
      slug: 'ashen-realms',
      name: 'Ashen Realms',
      owner_name: 'Ashen Studios',
      registered_at: 'now',
      status: 'active',
      category: 'game',
      requested_capabilities: ['profile.read'],
    })
    const integrator = await getIntegrator('http://127.0.0.1:1', 'ashen-realms')
    expect(integrator).toEqual({
      id: 'i1',
      slug: 'ashen-realms',
      name: 'Ashen Realms',
      ownerName: 'Ashen Studios',
      registeredAt: 'now',
      status: 'active',
      category: 'game',
      requestedCapabilities: ['profile.read'],
    })
  })
})

describe('listIntegrators', () => {
  it('converts a page of wire summaries into camelCase', async () => {
    mockFetchOnce({
      integrators: [
        { id: 'i1', slug: 's1', name: 'One', owner_name: 'Owner', registered_at: 'now', status: 'active', category: 'game' },
      ],
      next_cursor: 'c2',
    })
    const page = await listIntegrators('http://127.0.0.1:1')
    expect(page).toEqual({
      integrators: [{ id: 'i1', slug: 's1', name: 'One', ownerName: 'Owner', registeredAt: 'now', status: 'active', category: 'game' }],
      nextCursor: 'c2',
    })
  })

  it('passes through a body whose integrators field is not an array', async () => {
    mockFetchOnce(null)
    expect(await listIntegrators('http://127.0.0.1:1')).toBeNull()
  })
})

describe('listIssuerKeys', () => {
  it('passes through a non-array body instead of crashing on it', async () => {
    mockFetchOnce(null)
    expect(await listIssuerKeys('http://127.0.0.1:1', 's1')).toBeNull()
  })
})

describe('listAchievementDefinitions', () => {
  it('passes through a non-array body instead of crashing on it', async () => {
    mockFetchOnce(null)
    expect(await listAchievementDefinitions('http://127.0.0.1:1', 's1')).toBeNull()
  })
})
