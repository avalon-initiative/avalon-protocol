import { afterEach, describe, expect, it } from 'vitest'
import { getNodeStatus } from '../src/nodeStatus.js'

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

function mockFetchOnce(body: unknown) {
  globalThis.fetch = (async () => new Response(JSON.stringify(body), { status: 200 })) as typeof fetch
}

describe('getNodeStatus', () => {
  it('returns the parsed status response, including roles', async () => {
    mockFetchOnce({
      protocol_version: '0.1.0',
      network_id: 'avalon-dev-local',
      roles: ['combined'],
      stale: false,
      newest_known_peer_version: '0.1.0',
    })
    const result = await getNodeStatus('http://127.0.0.1:1')
    expect(result.roles).toEqual(['combined'])
    expect(result.protocol_version).toBe('0.1.0')
  })
})
