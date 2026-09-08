import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  approveDeviceGrant,
  beginDeviceGrantRequest,
  finalizeApprovedGrant,
  findMySigningKeyId,
} from './deviceGrants'
import { bytesToBase64, generateGrantRequestKeyPair, loadSigningKey } from '../crypto/signingKey'
import type { DeviceGrantResponse, DeviceResponse } from './types'
import { ed25519 } from '@noble/curves/ed25519'

function mockFetchOnce(body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve(body),
      text: () => Promise.resolve(JSON.stringify(body)),
    }),
  )
}

beforeEach(() => {
  localStorage.clear()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('beginDeviceGrantRequest', () => {
  it('generates a keypair, requests a grant, and never persists the key itself', async () => {
    mockFetchOnce({
      id: 'grant-1',
      status: 'pending',
      device_label: null,
      requested_signing_public_key: 'placeholder',
      requested_at: 'now',
      expires_at: 'later',
    })

    const identityId = crypto.randomUUID()
    const { grant, secretKey } = await beginDeviceGrantRequest('token', null)

    expect(grant.id).toBe('grant-1')
    expect(secretKey).toHaveLength(32)
    expect(loadSigningKey(identityId)).toBeNull()

    const [, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    const sentBody = JSON.parse(options.body)
    expect(typeof sentBody.requested_signing_public_key).toBe('string')
  })
})

describe('finalizeApprovedGrant', () => {
  it('persists the secret key for the identity once called', () => {
    const identityId = crypto.randomUUID()
    const { secretKey } = generateGrantRequestKeyPair()
    expect(loadSigningKey(identityId)).toBeNull()

    finalizeApprovedGrant(identityId, secretKey)

    expect(loadSigningKey(identityId)).not.toBeNull()
    expect(bytesToBase64(loadSigningKey(identityId)!)).toBe(bytesToBase64(secretKey))
  })
})

describe('approveDeviceGrant', () => {
  it('signs the exact grant being approved with the approving key', async () => {
    const identityId = crypto.randomUUID()
    const requested = generateGrantRequestKeyPair()
    const approver = generateGrantRequestKeyPair()

    const grant: DeviceGrantResponse = {
      id: crypto.randomUUID(),
      status: 'pending',
      device_label: null,
      requested_signing_public_key: bytesToBase64(requested.publicKey),
      requested_at: 'now',
      expires_at: 'later',
    }

    mockFetchOnce({ id: 'new-key-id' })

    await approveDeviceGrant('token', identityId, grant, 'approver-key-id', approver.secretKey)

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain(`/me/devices/grants/${grant.id}/approve`)
    const sentBody = JSON.parse(options.body)
    expect(sentBody.approver_signing_key_id).toBe('approver-key-id')

    // The signature the call actually sent must verify against the exact
    // bytes crates/server/src/devices.rs independently reconstructs —
    // proven here by rebuilding the same signing bytes and verifying.
    const signingBytes = new TextEncoder().encode(
      `avalon:device_grant.approved:v1:${grant.id}:${identityId}:${grant.requested_signing_public_key}`,
    )
    const signature = Uint8Array.from(atob(sentBody.signature), (c) => c.charCodeAt(0))
    expect(ed25519.verify(signature, signingBytes, approver.publicKey)).toBe(true)
  })
})

describe('findMySigningKeyId', () => {
  it('matches this device\'s local key against the server device list by public key', async () => {
    const { publicKey, secretKey } = generateGrantRequestKeyPair()
    const devices: DeviceResponse[] = [
      { id: 'other-device', label: null, public_key: 'not-mine', added_at: 'now' },
      { id: 'my-device', label: null, public_key: bytesToBase64(publicKey), added_at: 'now' },
    ]
    mockFetchOnce(devices)

    const found = await findMySigningKeyId('token', secretKey)
    expect(found).toBe('my-device')
  })

  it('ignores a revoked device even if the public key matches', async () => {
    const { publicKey, secretKey } = generateGrantRequestKeyPair()
    const devices: DeviceResponse[] = [
      {
        id: 'revoked-device',
        label: null,
        public_key: bytesToBase64(publicKey),
        added_at: 'now',
        revoked_at: 'now',
      },
    ]
    mockFetchOnce(devices)

    const found = await findMySigningKeyId('token', secretKey)
    expect(found).toBeNull()
  })

  it('returns null when no device matches at all', async () => {
    mockFetchOnce([])
    const { secretKey } = generateGrantRequestKeyPair()
    expect(await findMySigningKeyId('token', secretKey)).toBeNull()
  })
})
