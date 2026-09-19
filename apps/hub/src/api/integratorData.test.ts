import { describe, expect, it } from 'vitest'
import { mergeIntegratorDataInstance, parseIntegratorSlugFromSchema } from './integratorData'
import type { VisibleIntegratorDataInstanceResponse } from '@avalon/api-client'

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
    overrides: Partial<VisibleIntegratorDataInstanceResponse> = {},
  ): VisibleIntegratorDataInstanceResponse {
    return {
      schema: 'game:ashen-realms:schema:1',
      integrator_id: 'int-1',
      published_at: '2026-01-01T00:00:00Z',
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

  it('falls back to the raw integrator_id when the schema ref is malformed', () => {
    const merged = mergeIntegratorDataInstance(makeInstance({ schema: 'not-a-ref' }))
    expect(merged.integratorSlug).toBe('int-1')
    expect(merged.integratorName).toBeUndefined()
  })
})
