import { describe, expect, it } from 'vitest'
import {
  filterAchievementsByIntegrator,
  mergeAchievement,
  parseAchievementRef,
  parseIssuerSlug,
  sortAchievements,
} from './achievements'
import type { Achievement } from './achievements'
import type { AttestationResponse } from './types'

describe('parseIssuerSlug', () => {
  it('extracts the slug from a two-part issuer ref', () => {
    expect(parseIssuerSlug('game:ashen-realms')).toBe('ashen-realms')
  })

  it('returns null for a malformed issuer ref', () => {
    expect(parseIssuerSlug('not-a-ref')).toBeNull()
  })
})

describe('parseAchievementRef', () => {
  it('extracts namespace, slug, and key from a four-part achievement ref', () => {
    expect(parseAchievementRef('game:ashen-realms:achievement:dragon_slayer')).toEqual({
      namespace: 'game',
      slug: 'ashen-realms',
      key: 'dragon_slayer',
    })
  })

  it('works identically for a milestone ref', () => {
    expect(parseAchievementRef('app:my-app:milestone:first_login')).toEqual({
      namespace: 'app',
      slug: 'my-app',
      key: 'first_login',
    })
  })

  it('returns null for a malformed ref', () => {
    expect(parseAchievementRef('game:ashen-realms')).toBeNull()
  })
})

function attestation(overrides: Partial<AttestationResponse> = {}): AttestationResponse {
  return {
    id: 'a1',
    issuer: 'game:ashen-realms',
    subject: 'user-1',
    achievement: 'game:ashen-realms:achievement:dragon_slayer',
    issued_at: '2027-03-14T00:00:00Z',
    proof: { key_id: 'k1', algorithm: 'ed25519' },
    authenticity: { status: 'authentic', key_id: 'k1' },
    validity: { status: 'valid' },
    history: [{ event: 'issued', at: '2027-03-14T00:00:00Z' }],
    ...overrides,
  }
}

describe('mergeAchievement', () => {
  it('resolves issuer and achievement names from the given maps', () => {
    const merged = mergeAchievement(
      attestation(),
      new Map([['ashen-realms', 'Ashen Realms']]),
      new Map([
        [
          'game:ashen-realms:achievement:dragon_slayer',
          { name: 'Dragon Slayer', icon: 'sword', iconUrl: undefined },
        ],
      ]),
    )
    expect(merged.issuerName).toBe('Ashen Realms')
    expect(merged.achievementName).toBe('Dragon Slayer')
    expect(merged.achievementIcon).toBe('sword')
  })

  it('leaves names and icon undefined when the maps have no entry', () => {
    const merged = mergeAchievement(attestation())
    expect(merged.issuerName).toBeUndefined()
    expect(merged.achievementName).toBeUndefined()
    expect(merged.achievementIcon).toBeUndefined()
    expect(merged.achievementIconUrl).toBeUndefined()
  })

  it('prefers iconUrl over icon and drops an icon key outside the built-in set', () => {
    const withUrl = mergeAchievement(
      attestation(),
      new Map(),
      new Map([
        [
          'game:ashen-realms:achievement:dragon_slayer',
          { name: 'Dragon Slayer', icon: 'trophy', iconUrl: 'https://cdn.example.com/icon.png' },
        ],
      ]),
    )
    expect(withUrl.achievementIconUrl).toBe('https://cdn.example.com/icon.png')

    const bogusIcon = mergeAchievement(
      attestation(),
      new Map(),
      new Map([
        ['game:ashen-realms:achievement:dragon_slayer', { name: 'Dragon Slayer', icon: 'not_real' }],
      ]),
    )
    expect(bogusIcon.achievementIcon).toBeUndefined()
  })

  it('carries the full history and validity/invalid reason through unchanged', () => {
    const merged = mergeAchievement(
      attestation({
        validity: { status: 'invalid', reason: 'attestation has been revoked' },
        history: [
          { event: 'issued', at: '2027-03-14T00:00:00Z' },
          { event: 'revoked', at: '2027-05-02T00:00:00Z', reason_code: 'cheating_detected', reason: 'cheated' },
        ],
      }),
    )
    expect(merged.status).toBe('invalid')
    expect(merged.invalidReason).toBe('attestation has been revoked')
    expect(merged.history).toHaveLength(2)
    expect(merged.history[1]).toEqual({
      event: 'revoked',
      at: '2027-05-02T00:00:00Z',
      reasonCode: 'cheating_detected',
      reason: 'cheated',
    })
  })
})

function achievement(overrides: Partial<Achievement> = {}): Achievement {
  return {
    id: 'a1',
    achievementRef: 'game:g:achievement:x',
    achievementName: 'X',
    issuerSlug: 'g',
    issuerName: 'G',
    issuedAt: '2027-01-01T00:00:00Z',
    status: 'valid',
    history: [{ event: 'issued', at: '2027-01-01T00:00:00Z' }],
    ...overrides,
  }
}

describe('sortAchievements', () => {
  it('sorts by date, most recent first', () => {
    const older = achievement({ id: '1', issuedAt: '2027-01-01T00:00:00Z' })
    const newer = achievement({ id: '2', issuedAt: '2027-06-01T00:00:00Z' })
    expect(sortAchievements([older, newer], 'date').map((a) => a.id)).toEqual(['2', '1'])
  })

  it('sorts by name, falling back to the ref when unresolved', () => {
    const zebra = achievement({ id: '1', achievementName: 'Zebra' })
    const apple = achievement({ id: '2', achievementName: 'Apple' })
    expect(sortAchievements([zebra, apple], 'name').map((a) => a.id)).toEqual(['2', '1'])
  })

  it('sorts by integrator name, then achievement name as a tiebreaker', () => {
    const bIntegrator = achievement({ id: '1', issuerName: 'B Studio', achievementName: 'A' })
    const aIntegrator = achievement({ id: '2', issuerName: 'A Studio', achievementName: 'Z' })
    expect(sortAchievements([bIntegrator, aIntegrator], 'game').map((a) => a.id)).toEqual(['2', '1'])
  })

  it('does not mutate the input array', () => {
    const list = [achievement({ id: '1' }), achievement({ id: '2' })]
    const copy = [...list]
    sortAchievements(list, 'date')
    expect(list).toEqual(copy)
  })
})

describe('filterAchievementsByIntegrator', () => {
  it('returns every achievement when no integrator is selected', () => {
    const list = [achievement({ issuerSlug: 'a' }), achievement({ issuerSlug: 'b' })]
    expect(filterAchievementsByIntegrator(list, null)).toHaveLength(2)
  })

  it('returns only achievements from the selected issuer slug', () => {
    const list = [achievement({ id: '1', issuerSlug: 'a' }), achievement({ id: '2', issuerSlug: 'b' })]
    expect(filterAchievementsByIntegrator(list, 'b').map((a) => a.id)).toEqual(['2'])
  })
})
