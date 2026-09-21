import { describe, expect, it } from 'vitest'
import { mergeIntegratorDataInstance, parseIntegratorSlugFromSchema } from './integratorData'
import type { VisibleIntegratorDataInstance } from '@avalon/sdk'

describe('parseIntegratorSlugFromSchema', () => {
  it('extracts the slug from a game:<slug>:schema:<version> ref', () => {
    expect(parseIntegratorSlugFromSchema('game:ashen-realms:schema:1')).toBe('ashen-realms')
  })

  it('returns null for a malformed schema ref', () => {
    expect(parseIntegratorSlugFromSchema('not-a-ref')).toBeNull()
  })
})

describe('mergeIntegratorDataInstance', () => {
  function makeInstance(
    overrides: Partial<VisibleIntegratorDataInstance> = {},
  ): VisibleIntegratorDataInstance {
    return {
      schema: 'game:ashen-realms:schema:1',
      integratorId: 'int-1',
      publishedAt: '2026-01-01T00:00:00Z',
      fields: { level: 42 },
      ...overrides,
    }
  }

  it('resolves the integrator name when the slug lookup has one', () => {
    const merged = mergeIntegratorDataInstance(
      makeInstance(),
      new Map([['ashen-realms', 'Ashen Realms']]),
    )
    expect(merged.integratorSlug).toBe('ashen-realms')
    expect(merged.integratorName).toBe('Ashen Realms')
    expect(merged.fields).toEqual({ level: 42 })
  })

  it('falls back to the raw integratorId when the schema ref is malformed', () => {
    const merged = mergeIntegratorDataInstance(makeInstance({ schema: 'not-a-ref' }))
    expect(merged.integratorSlug).toBe('int-1')
    expect(merged.integratorName).toBeUndefined()
  })
})
