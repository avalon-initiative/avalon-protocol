import { afterEach, describe, expect, it } from 'vitest'
import {
  getIntegrator,
  listIntegrators,
  listIssuerKeys,
  listAchievementDefinitions,
  getAttestation,
  listSchemaVersions,
  getSchemaVersion,
  listMappings,
  getMapping,
  listRecognitions,
  listRecognizedBy,
} from '../src/integratorDirectory.js'

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

describe('getAttestation', () => {
  it('converts wire snake_case into camelCase, including nested proof/history', async () => {
    mockFetchOnce({
      id: 'a1',
      issuer: 'game:ashen-realms',
      subject: 's1',
      achievement: 'game:ashen-realms:achievement:dragon_slayer',
      issued_at: 'now',
      proof: { key_id: 'k1', algorithm: 'ed25519' },
      authenticity: { status: 'authentic', key_id: 'k1' },
      validity: { status: 'valid' },
      history: [{ event: 'issued', at: 'now' }, { event: 'revoked', at: 'later', reason_code: 'cheating', reason: 'confirmed' }],
    })
    const result = await getAttestation('http://127.0.0.1:1', 'a1')
    expect(result).toEqual({
      id: 'a1',
      issuer: 'game:ashen-realms',
      subject: 's1',
      achievement: 'game:ashen-realms:achievement:dragon_slayer',
      issuedAt: 'now',
      proof: { keyId: 'k1', algorithm: 'ed25519' },
      authenticity: { status: 'authentic', key_id: 'k1' },
      validity: { status: 'valid' },
      history: [
        { event: 'issued', at: 'now', reasonCode: undefined, reason: undefined },
        { event: 'revoked', at: 'later', reasonCode: 'cheating', reason: 'confirmed' },
      ],
    })
  })
})

describe('listSchemaVersions / getSchemaVersion', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockFetchOnce([
      {
        id: 'game:ashen-realms:schema:1',
        integrator_id: 'i1',
        version: 1,
        proto_source: 'message Foo {}',
        published_at: 'now',
        superseded_by: null,
        default_visibility: 'public',
        field_visibility: {},
      },
    ])
    const versions = await listSchemaVersions('http://127.0.0.1:1', 'ashen-realms')
    expect(versions).toEqual([
      {
        id: 'game:ashen-realms:schema:1',
        integratorId: 'i1',
        version: 1,
        protoSource: 'message Foo {}',
        publishedAt: 'now',
        supersededBy: undefined,
        defaultVisibility: 'public',
        fieldVisibility: {},
      },
    ])
  })

  it('getSchemaVersion converts a single version', async () => {
    mockFetchOnce({
      id: 'game:ashen-realms:schema:1',
      integrator_id: 'i1',
      version: 1,
      proto_source: 'message Foo {}',
      published_at: 'now',
      default_visibility: 'public',
      field_visibility: { level: 'private' },
    })
    const version = await getSchemaVersion('http://127.0.0.1:1', 'ashen-realms', 1)
    expect(version.fieldVisibility).toEqual({ level: 'private' })
  })
})

describe('listMappings / getMapping', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockFetchOnce([
      {
        id: 'game:ashen-realms:schema_mapping:1',
        integrator_id: 'i1',
        from_schema_id: 'game:ashen-realms:schema:1',
        to_schema_id: 'game:ashen-realms:schema:2',
        description: 'renamed field',
        field_correspondence: { old_name: 'new_name' },
        published_at: 'now',
      },
    ])
    const mappings = await listMappings('http://127.0.0.1:1', 'ashen-realms')
    expect(mappings).toEqual([
      {
        id: 'game:ashen-realms:schema_mapping:1',
        integratorId: 'i1',
        fromSchemaId: 'game:ashen-realms:schema:1',
        toSchemaId: 'game:ashen-realms:schema:2',
        description: 'renamed field',
        fieldCorrespondence: { old_name: 'new_name' },
        publishedAt: 'now',
      },
    ])
  })

  it('getMapping converts a single mapping', async () => {
    mockFetchOnce({
      id: 'game:ashen-realms:schema_mapping:1',
      integrator_id: 'i1',
      from_schema_id: 'game:ashen-realms:schema:1',
      to_schema_id: 'game:ashen-realms:schema:2',
      description: '',
      field_correspondence: {},
      published_at: 'now',
    })
    const mapping = await getMapping('http://127.0.0.1:1', 'ashen-realms', 1)
    expect(mapping.fromSchemaId).toBe('game:ashen-realms:schema:1')
  })
})

describe('listRecognitions / listRecognizedBy', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockFetchOnce([
      { recognizer_slug: 'ashen-realms', recognized_slug: 'worldzero', scope: ['achievements'], published_at: 'now' },
    ])
    expect(await listRecognitions('http://127.0.0.1:1', 'ashen-realms')).toEqual([
      { recognizerSlug: 'ashen-realms', recognizedSlug: 'worldzero', scope: ['achievements'], publishedAt: 'now' },
    ])
  })

  it('listRecognizedBy passes through a non-array body instead of crashing on it', async () => {
    mockFetchOnce(null)
    expect(await listRecognizedBy('http://127.0.0.1:1', 'ashen-realms')).toBeNull()
  })
})
