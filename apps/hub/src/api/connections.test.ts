import { describe, expect, it } from 'vitest'
import { activeCapabilities, capabilityDescription, CAPABILITY_DESCRIPTIONS } from './connections'
import type { IntegratorBindingResponse } from './types'

describe('capabilityDescription', () => {
  it('returns the plain-language description for a known capability', () => {
    expect(capabilityDescription('friends.read')).toBe('See your friends list')
  })

  it('falls back to the raw wire string for an unrecognized capability', () => {
    expect(capabilityDescription('some.unknown.capability')).toBe('some.unknown.capability')
  })

  it('has a description for every known capability wire string', () => {
    // crates/protocol/src/permissions.rs's Capability::KNOWN, kept in sync
    // by hand — this test is the tripwire if the two drift.
    const known = [
      'identity.read',
      'profile.read',
      'friends.read',
      'presence.read',
      'presence.publish',
      'guilds.read',
      'guilds.chat',
      'guilds.issue',
      'achievements.read',
      'achievements.issue',
      'assets.read',
      'assets.issue',
      'wallet.read',
      'wallet.write',
    ]
    for (const capability of known) {
      expect(CAPABILITY_DESCRIPTIONS[capability]).toBeDefined()
    }
  })
})

describe('activeCapabilities', () => {
  const baseBinding: IntegratorBindingResponse = {
    binding_id: 'b1',
    integrator_id: 'g1',
    slug: 'ashen-realms',
    name: 'Ashen Realms',
    established_at: '2026-09-01T00:00:00Z',
    grants: [],
  }

  it('returns an empty list when no requested capabilities were approved', () => {
    expect(activeCapabilities(baseBinding)).toEqual([])
  })

  it('returns every capability with an active grant when several were approved', () => {
    const binding = {
      ...baseBinding,
      grants: [
        { capability: 'friends.read', granted_at: 't1' },
        { capability: 'presence.read', granted_at: 't2' },
      ],
    }
    expect(activeCapabilities(binding)).toEqual(['friends.read', 'presence.read'])
  })

  it('returns an empty list when every requested capability was denied', () => {
    // "All denied" is indistinguishable from "none requested" at the
    // IntegratorBinding level — a binding can exist with zero grants under it
    // (connecting while approving nothing is still a valid connect).
    expect(activeCapabilities({ ...baseBinding, grants: [] })).toEqual([])
  })
})
