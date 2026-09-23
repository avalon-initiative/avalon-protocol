// Cross-SDK conformance suite — loads the shared
// test vectors under conformance/vectors/ (repo root) and asserts this
// SDK's real implementation produces byte-for-byte identical output.
// Offline, no server needed — runs in the default `npm test` (vitest) job
// alongside every other unit test here.
//
// See conformance/vectors/SCHEMA.md for the vector format and the
// standing "add a vector when you add smart-client behavior" requirement.
import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

import { signingBytes as crossNodeLoginSigningBytes } from '../src/crypto/crossNodeLogin.js'
import { signingBytes as continuationSigningBytes } from '../src/crypto/continuation.js'
import { signingBytes as interestClaimSigningBytes, type ClaimedScope } from '../src/crypto/interestClaim.js'
import { deriveSigningKeyFromMnemonic, isValidMnemonic } from '../src/crypto/mnemonic.js'
import {
  attestationSigningBytes,
  bulkAttestationSigningBytes,
} from '../src/integratorSession.js'
import { revocationSigningBytes } from '../src/integratorAccount.js'
import { sign, verify } from '../src/crypto/signing.js'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const VECTORS_DIR = path.resolve(__dirname, '../../../conformance/vectors')

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function loadVector(name: string): any {
  return JSON.parse(readFileSync(path.join(VECTORS_DIR, name), 'utf-8'))
}

function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2)
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  }
  return bytes
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

function toUtf8(bytes: Uint8Array): string {
  return new TextDecoder().decode(bytes)
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function requireSupported(doc: any, lang: string): void {
  expect(doc.supportedIn, `${lang} must be listed in supportedIn for this vector file`).toContain(lang)
}

describe('conformance: cross-node-login grant signing (#623/#707)', () => {
  const doc = loadVector('cross-node-login.json')

  it('lists typescript as supported', () => {
    requireSupported(doc, 'typescript')
  })

  it('derives the shared test keypair public key correctly', () => {
    const secretKey = hexToBytes(doc.signingKeySeedHex)
    const publicKey = hexToBytes(doc.signingPublicKeyHex)
    // Round-trip: sign something with secretKey and verify with publicKey.
    const probe = new TextEncoder().encode('probe')
    expect(verify(publicKey, probe, sign(secretKey, probe))).toBe(true)
  })

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  for (const vector of doc.vectors as any[]) {
    it(`matches the shared vector: ${vector.name}`, () => {
      const secretKey = hexToBytes(doc.signingKeySeedHex)
      const { input, expected } = vector

      const bytes = crossNodeLoginSigningBytes(
        input.identityId,
        input.signingKeyId,
        input.destinationBaseUrl,
        input.requestingContext,
        input.nonce,
        new Date(input.issuedAtUnixSeconds * 1000),
        new Date(input.expiresAtUnixSeconds * 1000),
      )

      expect(toUtf8(bytes)).toBe(expected.signingBytesUtf8)

      const signature = sign(secretKey, bytes)
      expect(bytesToHex(signature)).toBe(expected.signatureHex)
    })
  }
})

describe('conformance: session-continuation token minting (#525)', () => {
  const doc = loadVector('session-continuation.json')

  it('lists typescript as supported', () => {
    requireSupported(doc, 'typescript')
  })

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  for (const vector of doc.vectors as any[]) {
    it(`matches the shared vector: ${vector.name}`, () => {
      const secretKey = hexToBytes(doc.signingKeySeedHex)
      const { input, expected } = vector

      const bytes = continuationSigningBytes(
        input.identityId,
        input.signingKeyId,
        input.nonce,
        new Date(input.issuedAtUnixSeconds * 1000),
        new Date(input.expiresAtUnixSeconds * 1000),
      )
      expect(toUtf8(bytes)).toBe(expected.signingBytesUtf8)

      const signature = sign(secretKey, bytes)
      expect(bytesToHex(signature)).toBe(expected.signatureHex)
      expect(expected.wireToken.startsWith(expected.wireTokenPrefix)).toBe(true)
    })
  }
})

describe('conformance: websocket interest-claim minting (#610/#712)', () => {
  const doc = loadVector('websocket-interest-claim.json')

  it('lists typescript as supported', () => {
    requireSupported(doc, 'typescript')
  })

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  for (const vector of doc.vectors as any[]) {
    it(`matches the shared vector: ${vector.name}`, () => {
      const secretKey = hexToBytes(doc.signingKeySeedHex)
      const { input, expected } = vector
      const scope: ClaimedScope = { kind: 'channel', channelId: input.scope.channelId }

      const bytes = interestClaimSigningBytes(
        input.identityId,
        input.signingKeyId,
        scope,
        input.baseUrl,
        input.nonce,
        new Date(input.issuedAtUnixSeconds * 1000),
        new Date(input.expiresAtUnixSeconds * 1000),
      )
      expect(toUtf8(bytes)).toBe(expected.signingBytesUtf8)

      const signature = sign(secretKey, bytes)
      expect(bytesToHex(signature)).toBe(expected.signatureHex)
    })
  }
})

describe('conformance: BIP39 mnemonic-derived signing keys (#134/#712)', () => {
  const doc = loadVector('bip39-mnemonic.json')

  it('lists typescript as supported', () => {
    requireSupported(doc, 'typescript')
  })

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  for (const vector of doc.vectors as any[]) {
    it(`matches the shared vector: ${vector.name}`, () => {
      const { input, expected } = vector
      if (expected.isValid === false) {
        expect(isValidMnemonic(input.mnemonic)).toBe(false)
        return
      }
      const derived = deriveSigningKeyFromMnemonic(input.mnemonic)
      expect(bytesToHex(derived.secretKey)).toBe(expected.derivedSecretKeyHex)
      expect(bytesToHex(derived.publicKey)).toBe(expected.derivedPublicKeyHex)
    })
  }
})

describe('conformance: attestation issuance/bulk-issuance/revocation signing (#774)', () => {
  const doc = loadVector('attestation-signing.json')

  it('lists typescript as supported', () => {
    requireSupported(doc, 'typescript')
  })

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  for (const vector of doc.vectors as any[]) {
    it(`matches the shared vector: ${vector.name}`, () => {
      const secretKey = hexToBytes(doc.signingKeySeedHex)
      const { input, expected } = vector
      const claimKind = input.claimKind as 'achievement' | 'milestone'

      let bytes: Uint8Array
      switch (input.operation) {
        case 'issue':
          bytes = attestationSigningBytes(claimKind, input.issuerRef, input.subject, input.achievement)
          break
        case 'bulk_issue':
          bytes = bulkAttestationSigningBytes(claimKind, input.issuerRef, input.subject, input.achievements)
          break
        case 'revoke':
          bytes = revocationSigningBytes(claimKind, input.issuerRef, input.attestationId, input.reasonCode)
          break
        default:
          throw new Error(`unknown operation ${input.operation}`)
      }

      expect(bytesToHex(bytes)).toBe(expected.signingBytesHex)
      expect(bytesToHex(sign(secretKey, bytes))).toBe(expected.signatureHex)
    })
  }
})

describe('conformance: Signed Tree Head signing (#39/#210)', () => {
  const doc = loadVector('signed-tree-head.json')

  it('is a known typescript SDK gap', () => {
    expect(doc.supportedIn).not.toContain('typescript')
    expect(doc.notSupported?.typescript, 'notSupported.typescript must explain the gap').toBeTruthy()
  })
})
