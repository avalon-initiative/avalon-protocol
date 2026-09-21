// Live tests against a real, running avalon-server (`make start` from the
// repo root) and real Postgres — mirrors crates/sdk/tests/account_session.rs/
// account_device_login.rs and bindings/csharp/AvalonSdk.Tests/LiveTests.cs's
// own AccountSession_* tests, translated here. Every seeded display name
// carries a fresh random suffix — this Postgres instance is shared and
// long-lived.
//
// Needs AVALON_SERVER_URL and AVALON_LIVE_DATABASE_URL (a `postgres://`
// connection string — this package uses the `pg` npm client directly,
// unlike the C# suite, which needs an Npgsql keyword/value string
// conversion; the Rust `DATABASE_URL` URI form works unmodified here). Run
// with `npm run test:live` from this package's own directory, after
// `make start` from the repo root.
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import pg from 'pg'
import { AvalonClient } from './client.js'
import { generateSigningKey, canonicalMessage, sign, bytesToBase64 } from './crypto/signing.js'
import { DeviceLoginDeniedError } from './errors.js'

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

async function seedSigningKey(identityId: string): Promise<{ keyId: string; secretKey: Uint8Array; publicKey: Uint8Array }> {
  const { secretKey, publicKey } = generateSigningKey()
  const result = await pool.query<{ id: string }>(
    'INSERT INTO identity_signing_keys (identity_id, public_key) VALUES ($1, $2) RETURNING id',
    [identityId, Buffer.from(publicKey)],
  )
  return { keyId: result.rows[0].id, secretKey, publicKey }
}

maybeDescribe('AccountSession live round trips', () => {
  it('resumeAccountSessionWithSigningKey signs automatically and verifies server-side on a signature-required guild action', async () => {
    const { identityId, token } = await seedIdentitySession(`account-session-resume-ts-${crypto.randomUUID()}`)
    const { keyId, secretKey } = await seedSigningKey(identityId)

    const client = new AvalonClient({ serverUrl: serverUrl! })
    const session = await client.resumeAccountSessionWithSigningKey(token, secretKey)

    expect(session.identity().id).toBe(identityId)
    expect(session.signingKeyId()).toBe(keyId)

    const guild = await session.createGuild(`Guild ${crypto.randomUUID().slice(0, 8)}`, `T${crypto.randomUUID().slice(0, 4)}`, 'a test guild')
    expect(guild.owner).toBe(identityId)

    // guild.role.create is signature-required (#697/#698) — this only
    // succeeds if AccountSession.createRole actually attached a valid
    // signature the server verified.
    const role = await session.createRole(guild.id, 'Quartermaster', ['manage_members'], 'trusted role')
    expect(role.name).toBe('Quartermaster')
    expect(role.permissions).toEqual(['manage_members'])
  })

  it('resumeAccountSession without a signing key sends unsigned and is rejected server-side on a signature-required action', async () => {
    const { identityId, token } = await seedIdentitySession(`account-session-nokey-ts-${crypto.randomUUID()}`)
    // The identity does have a registered signing key server-side — just
    // not one this session holds locally.
    await seedSigningKey(identityId)

    const client = new AvalonClient({ serverUrl: serverUrl! })
    const session = await client.resumeAccountSession(token)
    expect(session.signingKeyId()).toBeUndefined()

    const guild = await session.createGuild(
      `Guild ${crypto.randomUUID().slice(0, 8)}`,
      `T${crypto.randomUUID().slice(0, 4)}`,
      'an unsigned-resume test guild',
    )

    await expect(session.createRole(guild.id, 'ShouldFail', [], '')).rejects.toThrow()
  })

  it('startAccountDeviceLogin -> wait resolves to a real AccountSession once approved', async () => {
    const displayName = `account-device-login-ts-${crypto.randomUUID()}`
    const { identityId: approverId, token: approverToken } = await seedIdentitySession(displayName)
    const { keyId: approverKeyId, secretKey: approverSecretKey } = await seedSigningKey(approverId)

    const client = new AvalonClient({ serverUrl: serverUrl! })
    const pairing = await client.startAccountDeviceLogin()
    expect(pairing.userCode.length).toBeGreaterThan(0)
    expect(pairing.verificationUri).toContain(pairing.userCode)
    expect(pairing.expiresIn).toBeGreaterThan(0)

    const approvalTask = (async () => {
      await new Promise((resolve) => setTimeout(resolve, 500))
      const message = canonicalMessage('device_pairing.approve', [approverId, pairing.userCode])
      const signature = bytesToBase64(sign(approverSecretKey, message))
      const response = await fetch(`${serverUrl}/auth/device/approve`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', authorization: `Bearer ${approverToken}` },
        body: JSON.stringify({ user_code: pairing.userCode, signing_key_id: approverKeyId, signature }),
      })
      if (!response.ok) throw new Error(`approve failed: ${response.status}`)
    })()

    const [session] = await Promise.all([pairing.wait(), approvalTask])

    expect(session.profile().displayName).toBe(displayName)
    // The approving device is a different device with its own key — this
    // session never had a WebAuthn ceremony of its own.
    expect(session.signingKeyId()).toBeUndefined()
  })

  it('startAccountDeviceLogin -> wait throws DeviceLoginDeniedError when denied', async () => {
    const { token: approverToken } = await seedIdentitySession(`account-device-login-deny-ts-${crypto.randomUUID()}`)

    const client = new AvalonClient({ serverUrl: serverUrl! })
    const pairing = await client.startAccountDeviceLogin()

    const denialTask = (async () => {
      await new Promise((resolve) => setTimeout(resolve, 500))
      const response = await fetch(`${serverUrl}/auth/device/deny`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', authorization: `Bearer ${approverToken}` },
        body: JSON.stringify({ user_code: pairing.userCode }),
      })
      if (!response.ok) throw new Error(`deny failed: ${response.status}`)
    })()

    await expect(pairing.wait()).rejects.toBeInstanceOf(DeviceLoginDeniedError)
    await denialTask
  })
})
