import { afterEach, describe, expect, it } from 'vitest'
import {
  registerIntegrator,
  integratorWhoami,
  addIssuerKey,
  createAchievementDefinition,
  updateAchievementDefinition,
  publishSchemaVersion,
  publishRecognition,
  revokeRecognition,
  deleteInstance,
  createRegistrationChallenge,
  registerIssuer,
} from '../src/integratorAccount.js'
import { generateSigningKey } from '../src/crypto/signing.js'

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

/** Every write in this module first calls `POST /integrations/{slug}/challenge`,
 * then the actual write — this stubs both in order. */
function mockChallengeThen(body: unknown, status = 200) {
  let call = 0
  globalThis.fetch = (async (_url, _init) => {
    call += 1
    if (call === 1) {
      return new Response(JSON.stringify({ challenge_id: 'c1', nonce: 'bm9uY2U=', expires_at: 'later' }), { status: 200 })
    }
    return new Response(JSON.stringify(body), { status })
  }) as typeof fetch
}

function mockFetchOnce(body: unknown, status = 200) {
  globalThis.fetch = (async () => new Response(JSON.stringify(body), { status })) as typeof fetch
}

describe('registerIntegrator', () => {
  it('sends initial_key as base64 and converts the response', async () => {
    let capturedBody: Record<string, unknown> | undefined
    globalThis.fetch = (async (_url, init) => {
      capturedBody = JSON.parse(init?.body as string)
      return new Response(
        JSON.stringify({
          id: 'i1',
          slug: 'ashen-realms',
          name: 'Ashen Realms',
          owner_name: 'Ashen Studios',
          registered_at: 'now',
          status: 'active',
          category: 'game',
          requested_capabilities: [],
          credential: { integrator_id: 'i1', key_id: 'k1' },
        }),
        { status: 200 },
      )
    }) as typeof fetch

    const { publicKey } = generateSigningKey()
    const result = await registerIntegrator('http://127.0.0.1:1', {
      slug: 'ashen-realms',
      name: 'Ashen Realms',
      ownerName: 'Ashen Studios',
      publicKey,
    })
    expect(result.credentialKeyId).toBe('k1')
    expect(result.slug).toBe('ashen-realms')
    expect(capturedBody?.initial_key).toEqual({ algorithm: 'ed25519', public_key: expect.any(String) })
    expect(capturedBody?.category).toBeNull()
  })
})

describe('integratorWhoami', () => {
  it('sends the challenge-response headers and returns integrator_id', async () => {
    const headersByCall: Record<string, string>[] = []
    let call = 0
    globalThis.fetch = (async (_url, init) => {
      call += 1
      headersByCall.push((init?.headers as Record<string, string>) ?? {})
      if (call === 1) {
        return new Response(JSON.stringify({ challenge_id: 'c1', nonce: 'bm9uY2U=', expires_at: 'later' }), { status: 200 })
      }
      return new Response(JSON.stringify({ integrator_id: 'i1' }), { status: 200 })
    }) as typeof fetch

    const { secretKey } = generateSigningKey()
    const integratorId = await integratorWhoami('http://127.0.0.1:1', 'ashen-realms', 'k1', secretKey)
    expect(integratorId).toBe('i1')
    expect(headersByCall[1]['x-avalon-integrator-key-id']).toBe('k1')
    expect(headersByCall[1]['x-avalon-integrator-challenge-id']).toBe('c1')
    expect(headersByCall[1]['x-avalon-integrator-signature']).toEqual(expect.any(String))
  })
})

describe('addIssuerKey', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockChallengeThen({
      key_id: 'k2',
      algorithm: 'ed25519',
      role: 'operational',
      purpose: 'attestation',
      valid_from: 'now',
      valid_until: null,
      revoked_at: null,
    })
    const { secretKey, publicKey } = generateSigningKey()
    const result = await addIssuerKey('http://127.0.0.1:1', 'ashen-realms', 'root-key', secretKey, {
      publicKey,
      role: 'operational',
    })
    expect(result).toEqual({
      keyId: 'k2',
      algorithm: 'ed25519',
      role: 'operational',
      purpose: 'attestation',
      validFrom: 'now',
      validUntil: undefined,
      revokedAt: undefined,
    })
  })
})

describe('createAchievementDefinition / updateAchievementDefinition', () => {
  it('createAchievementDefinition converts wire snake_case into camelCase', async () => {
    mockChallengeThen({
      id: 'game:ashen-realms:achievement:dragon_slayer',
      integrator_id: 'i1',
      key: 'dragon_slayer',
      name: 'Dragon Slayer',
      description: 'Slew the dragon',
      schema: null,
      icon: 'default',
      icon_url: null,
      version: 1,
      created_at: 'now',
      updated_at: 'now',
      retired: false,
      retired_at: null,
    })
    const { secretKey } = generateSigningKey()
    const result = await createAchievementDefinition('http://127.0.0.1:1', 'ashen-realms', 'k1', secretKey, {
      key: 'dragon_slayer',
      name: 'Dragon Slayer',
      description: 'Slew the dragon',
    })
    expect(result.id).toBe('game:ashen-realms:achievement:dragon_slayer')
    expect(result.retired).toBe(false)
  })

  it('updateAchievementDefinition sends absent fields as null', async () => {
    let capturedBody: Record<string, unknown> | undefined
    let call = 0
    globalThis.fetch = (async (_url, init) => {
      call += 1
      if (call === 1) {
        return new Response(JSON.stringify({ challenge_id: 'c1', nonce: 'bm9uY2U=', expires_at: 'later' }), { status: 200 })
      }
      capturedBody = JSON.parse(init?.body as string)
      return new Response(
        JSON.stringify({
          id: 'game:ashen-realms:achievement:dragon_slayer',
          integrator_id: 'i1',
          key: 'dragon_slayer',
          name: 'Dragon Slayer II',
          description: 'Slew the dragon',
          schema: null,
          icon: 'default',
          icon_url: null,
          version: 2,
          created_at: 'now',
          updated_at: 'later',
          retired: false,
          retired_at: null,
        }),
        { status: 200 },
      )
    }) as typeof fetch

    const { secretKey } = generateSigningKey()
    const result = await updateAchievementDefinition('http://127.0.0.1:1', 'ashen-realms', 'dragon_slayer', 'k1', secretKey, {
      name: 'Dragon Slayer II',
    })
    expect(result.name).toBe('Dragon Slayer II')
    expect(capturedBody).toEqual({
      name: 'Dragon Slayer II',
      description: null,
      schema: null,
      icon: null,
      icon_url: null,
      retired: null,
    })
  })
})

describe('publishSchemaVersion', () => {
  it('defaults default_visibility to public and field_visibility to {}', async () => {
    let capturedBody: Record<string, unknown> | undefined
    let call = 0
    globalThis.fetch = (async (_url, init) => {
      call += 1
      if (call === 1) {
        return new Response(JSON.stringify({ challenge_id: 'c1', nonce: 'bm9uY2U=', expires_at: 'later' }), { status: 200 })
      }
      capturedBody = JSON.parse(init?.body as string)
      return new Response(
        JSON.stringify({
          id: 'game:ashen-realms:schema:1',
          integrator_id: 'i1',
          version: 1,
          proto_source: 'message Foo {}',
          published_at: 'now',
          superseded_by: null,
          default_visibility: 'public',
          field_visibility: {},
        }),
        { status: 200 },
      )
    }) as typeof fetch

    const { secretKey } = generateSigningKey()
    await publishSchemaVersion('http://127.0.0.1:1', 'ashen-realms', 'k1', secretKey, { protoSource: 'message Foo {}' })
    expect(capturedBody).toEqual({ proto_source: 'message Foo {}', default_visibility: 'public', field_visibility: {} })
  })
})

describe('publishRecognition / revokeRecognition', () => {
  it('publishRecognition sends recognized_slug/scope', async () => {
    mockChallengeThen({
      recognizer_slug: 'ashen-realms',
      recognized_slug: 'worldzero',
      scope: ['achievements'],
      published_at: 'now',
    })
    const { secretKey } = generateSigningKey()
    const result = await publishRecognition('http://127.0.0.1:1', 'ashen-realms', 'k1', secretKey, 'worldzero', ['achievements'])
    expect(result.recognized_slug).toBe('worldzero')
  })

  it('revokeRecognition returns the revoked flag as-is', async () => {
    mockChallengeThen({ revoked: true })
    const { secretKey } = generateSigningKey()
    const result = await revokeRecognition('http://127.0.0.1:1', 'ashen-realms', 'k1', secretKey, 'worldzero')
    expect(result).toEqual({ revoked: true })
  })
})

describe('deleteInstance', () => {
  it('defaults reason_code to "deleted" and returns void on an empty body', async () => {
    let capturedBody: Record<string, unknown> | undefined
    let call = 0
    globalThis.fetch = (async (_url, init) => {
      call += 1
      if (call === 1) {
        return new Response(JSON.stringify({ challenge_id: 'c1', nonce: 'bm9uY2U=', expires_at: 'later' }), { status: 200 })
      }
      capturedBody = JSON.parse(init?.body as string)
      return new Response('', { status: 200 })
    }) as typeof fetch

    const { secretKey } = generateSigningKey()
    const result = await deleteInstance('http://127.0.0.1:1', 'ashen-realms', 1, 'subject-1', 'k1', secretKey)
    expect(result).toBeUndefined()
    expect(capturedBody).toEqual({ reason_code: 'deleted', reason: null })
  })
})

describe('createRegistrationChallenge / registerIssuer', () => {
  it('createRegistrationChallenge converts wire snake_case into camelCase', async () => {
    mockFetchOnce({ challenge_id: 'c1', nonce: 'bm9uY2U=', expires_at: 'later' })
    const { publicKey } = generateSigningKey()
    const result = await createRegistrationChallenge('http://127.0.0.1:1', publicKey)
    expect(result).toEqual({ challengeId: 'c1', nonce: 'bm9uY2U=', expiresAt: 'later' })
  })

  it('registerIssuer drives challenge -> sign -> register and converts the response', async () => {
    let call = 0
    let registerBody: Record<string, unknown> | undefined
    globalThis.fetch = (async (_url, init) => {
      call += 1
      if (call === 1) {
        return new Response(JSON.stringify({ challenge_id: 'c1', nonce: 'bm9uY2U=', expires_at: 'later' }), { status: 200 })
      }
      registerBody = JSON.parse(init?.body as string)
      return new Response(JSON.stringify({ issuer_ref: 'game:ashen-realms', registered_at: 'now' }), { status: 200 })
    }) as typeof fetch

    const { secretKey } = generateSigningKey()
    const result = await registerIssuer('http://127.0.0.1:1', 'game:ashen-realms', 'avalon-dev-local', secretKey)
    expect(result).toEqual({ issuerRef: 'game:ashen-realms', registeredAt: 'now' })
    expect(registerBody?.challenge_id).toBe('c1')
    expect(registerBody?.declared_network_id).toBe('avalon-dev-local')
    expect(registerBody?.issuer_ref).toBe('game:ashen-realms')
  })
})
