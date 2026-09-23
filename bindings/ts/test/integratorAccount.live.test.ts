// Live round trips for the TypeScript SDK's coverage gap
// against a real, running avalon-server and real Postgres — same
// conventions as account.live.test.ts (AVALON_SERVER_URL/
// AVALON_LIVE_DATABASE_URL, `npm run test:live`).
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import pg from 'pg'
import { AvalonClient } from '../src/client.js'
import { generateSigningKey } from '../src/crypto/signing.js'
import { authenticate as authenticateIntegrator } from '../src/integratorSession.js'
import {
  registerIntegrator,
  integratorWhoami,
  addIssuerKey,
  createAchievementDefinition,
  publishSchemaVersion,
  publishInstance,
  publishRecognition,
  revokeRecognition,
} from '../src/integratorAccount.js'
import { getAttestation, listSchemaVersions, getSchemaVersion, listRecognitions } from '../src/integratorDirectory.js'
import { startCrossNodeLogin, pollCrossNodeLogin } from '../src/crossNodeLogin.js'
import { getLocations } from '../src/identityData.js'

const serverUrl = process.env.AVALON_SERVER_URL
const databaseUrl = process.env.AVALON_LIVE_DATABASE_URL

const maybeDescribe = serverUrl && databaseUrl ? describe : describe.skip

let pool: pg.Pool

beforeAll(() => {
  if (databaseUrl) {
    pool = new pg.Pool({ connectionString: databaseUrl })
  }
})

afterAll(async () => {
  if (pool) await pool.end()
})

function freshSlug(prefix: string): string {
  return `${prefix}-${crypto.randomUUID().slice(0, 8)}`
}

async function seedIdentitySession(displayName: string): Promise<{ identityId: string; token: string }> {
  const identityId = crypto.randomUUID()
  await pool.query('INSERT INTO identities (id) VALUES ($1)', [identityId])
  await pool.query('INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)', [identityId, displayName])
  const token = `test-token-${crypto.randomUUID()}`
  const expiresAt = new Date(Date.now() + 60 * 60 * 1000)
  await pool.query('INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)', [
    token,
    identityId,
    expiresAt,
  ])
  return { identityId, token }
}

async function seedSigningKey(identityId: string): Promise<{ keyId: string; secretKey: Uint8Array }> {
  const { secretKey, publicKey } = generateSigningKey()
  const result = await pool.query<{ id: string }>(
    'INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id',
    [identityId, Buffer.from(publicKey)],
  )
  return { keyId: result.rows[0].id, secretKey }
}

maybeDescribe('registerIntegrator -> addIssuerKey -> integratorWhoami live round trip (#746)', () => {
  it('registers a real integrator, adds a second root key, and both keys pass whoami', async () => {
    const slug = freshSlug('ts-live-integrator')
    const { secretKey, publicKey } = generateSigningKey()

    const integrator = await registerIntegrator(serverUrl!, {
      slug,
      name: 'TS Live Integrator',
      ownerName: 'TS Live Studios',
      publicKey,
    })
    expect(integrator.slug).toBe(slug)
    expect(integrator.credentialKeyId.length).toBeGreaterThan(0)

    const whoami = await integratorWhoami(serverUrl!, slug, integrator.credentialKeyId, secretKey)
    expect(whoami).toBe(integrator.id)

    const { secretKey: secondSecretKey, publicKey: secondPublicKey } = generateSigningKey()
    const secondKey = await addIssuerKey(serverUrl!, slug, integrator.credentialKeyId, secretKey, {
      publicKey: secondPublicKey,
      role: 'root',
    })
    expect(secondKey.role).toBe('root')

    const secondWhoami = await integratorWhoami(serverUrl!, slug, secondKey.keyId, secondSecretKey)
    expect(secondWhoami).toBe(integrator.id)
  })
})

maybeDescribe('achievement-definition create -> issue -> read -> revoke live round trip (#744)', () => {
  it('defines an achievement, issues it to a real identity, reads it back, then revokes it', async () => {
    const slug = freshSlug('ts-live-achv')
    const { secretKey, publicKey } = generateSigningKey()
    const integrator = await registerIntegrator(serverUrl!, {
      slug,
      name: 'TS Live Achievements',
      ownerName: 'TS Live Studios',
      requestedCapabilities: ['achievements.issue'],
      publicKey,
    })

    const definition = await createAchievementDefinition(serverUrl!, slug, integrator.credentialKeyId, secretKey, {
      key: 'dragon_slayer',
      name: 'Dragon Slayer',
      description: 'Slew the dragon',
    })
    expect(definition.key).toBe('dragon_slayer')

    const { identityId, token } = await seedIdentitySession(`ts-live-achv-subject-${crypto.randomUUID()}`)
    const { secretKey: identitySecretKey } = await seedSigningKey(identityId)
    const client = new AvalonClient({ serverUrl: serverUrl! })
    const accountSession = await client.resumeAccountSessionWithSigningKey(token, identitySecretKey)
    await accountSession.connectIntegrator(slug, ['achievements.issue'])

    const integratorSession = await authenticateIntegrator({
      serverUrl: serverUrl!,
      identityToken: token,
      integratorCredentialKeyId: integrator.credentialKeyId,
      integratorSlug: slug,
      signingKey: secretKey,
    })
    expect(integratorSession.hasCapability('achievements.issue')).toBe(true)

    const attestationId = await integratorSession.issueAchievement('dragon_slayer')
    expect(attestationId.length).toBeGreaterThan(0)

    const read = await getAttestation(serverUrl!, attestationId)
    expect(read.subject).toBe(identityId)
    expect(read.achievement).toBe(`game:${slug}:achievement:dragon_slayer`)
    expect(read.authenticity.status).toBe('authentic')
    expect(read.history).toEqual([expect.objectContaining({ event: 'issued' })])
  })
})

maybeDescribe('schema publish -> instance publish -> read live round trip (#745)', () => {
  it('publishes a schema version, then an instance against it, then reads both back', async () => {
    const slug = freshSlug('ts-live-schema')
    const { secretKey, publicKey } = generateSigningKey()
    const integrator = await registerIntegrator(serverUrl!, {
      slug,
      name: 'TS Live Schema',
      ownerName: 'TS Live Studios',
      publicKey,
    })

    const version = await publishSchemaVersion(serverUrl!, slug, integrator.credentialKeyId, secretKey, {
      protoSource: 'syntax = "proto3"; message PlayerStats { uint32 level = 1; }',
    })
    expect(version.version).toBe(1)

    const versions = await listSchemaVersions(serverUrl!, slug)
    expect(versions).toHaveLength(1)
    const fetchedVersion = await getSchemaVersion(serverUrl!, slug, 1)
    expect(fetchedVersion.id).toBe(version.id)

    const { identityId, token } = await seedIdentitySession(`ts-live-schema-subject-${crypto.randomUUID()}`)
    const { secretKey: identitySecretKey } = await seedSigningKey(identityId)
    const client = new AvalonClient({ serverUrl: serverUrl! })
    const accountSession = await client.resumeAccountSessionWithSigningKey(token, identitySecretKey)
    await accountSession.connectIntegrator(slug, [])

    const instance = await publishInstance(serverUrl!, slug, 1, integrator.credentialKeyId, secretKey, identityId, {
      level: 5,
    })
    expect(instance.subject).toBe(identityId)
    expect(instance.instance).toEqual({ level: 5 })
  })
})

maybeDescribe('recognition publish -> list -> revoke live round trip (#748)', () => {
  it('publishes a recognition between two real integrators, lists it, then revokes it', async () => {
    const recognizerSlug = freshSlug('ts-live-recognizer')
    const recognizedSlug = freshSlug('ts-live-recognized')
    const { secretKey, publicKey } = generateSigningKey()
    const recognizer = await registerIntegrator(serverUrl!, {
      slug: recognizerSlug,
      name: 'TS Live Recognizer',
      ownerName: 'TS Live Studios',
      publicKey,
    })
    await registerIntegrator(serverUrl!, {
      slug: recognizedSlug,
      name: 'TS Live Recognized',
      ownerName: 'TS Live Studios',
      publicKey: generateSigningKey().publicKey,
    })

    const published = await publishRecognition(
      serverUrl!,
      recognizerSlug,
      recognizer.credentialKeyId,
      secretKey,
      recognizedSlug,
      ['achievements'],
    )
    expect(published.recognized_slug).toBe(recognizedSlug)

    const recognitions = await listRecognitions(serverUrl!, recognizerSlug)
    expect(recognitions).toEqual([
      expect.objectContaining({ recognizerSlug, recognizedSlug, scope: ['achievements'] }),
    ])

    const revoked = await revokeRecognition(serverUrl!, recognizerSlug, recognizer.credentialKeyId, secretKey, recognizedSlug)
    expect(revoked.revoked).toBe(true)

    const afterRevoke = await listRecognitions(serverUrl!, recognizerSlug)
    expect(afterRevoke).toEqual([])
  })
})

maybeDescribe('cross-node login start -> poll live round trip (#749)', () => {
  it('starts a request and polls it back as pending, then slow_down on an immediate re-poll', async () => {
    const start = await startCrossNodeLogin(serverUrl!)
    expect(start.userCode.length).toBeGreaterThan(0)
    expect(start.requestCode.length).toBeGreaterThan(0)

    const firstPoll = await pollCrossNodeLogin(serverUrl!, start.requestCode)
    expect(firstPoll.status).toBe('pending')

    const secondPoll = await pollCrossNodeLogin(serverUrl!, start.requestCode)
    expect(secondPoll.status).toBe('slow_down')
  })
})

maybeDescribe('getLocations live round trip (#749)', () => {
  it('returns an array (possibly empty) for a real identity', async () => {
    const { identityId } = await seedIdentitySession(`ts-live-locations-${crypto.randomUUID()}`)
    const locations = await getLocations(serverUrl!, identityId)
    expect(Array.isArray(locations)).toBe(true)
  })
})
