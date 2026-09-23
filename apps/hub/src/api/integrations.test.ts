import { describe, expect, it } from 'vitest'
import { buildIntegratorsListQueryString, isActiveIntegratorStatus, listRegistryMetrics } from './integrations'
import type { IntegratorRegistry } from '@avalon-initiative/protocol-sdk'

describe('buildIntegratorsListQueryString', () => {
  it('omits every key when nothing is set', () => {
    expect(buildIntegratorsListQueryString({})).toBe('')
  })

  it('trims and includes q only when non-blank', () => {
    expect(buildIntegratorsListQueryString({ q: '  ashen  ' })).toBe('?q=ashen')
    expect(buildIntegratorsListQueryString({ q: '   ' })).toBe('')
  })

  it('includes sort, limit, and cursor when set', () => {
    const query = buildIntegratorsListQueryString({ sort: 'name', limit: 10, cursor: 'abc-123' })
    expect(query).toContain('sort=name')
    expect(query).toContain('limit=10')
    expect(query).toContain('cursor=abc-123')
  })
})

describe('isActiveIntegratorStatus', () => {
  it('is true only for exactly "active"', () => {
    expect(isActiveIntegratorStatus('active')).toBe(true)
    expect(isActiveIntegratorStatus('suspended')).toBe(false)
    expect(isActiveIntegratorStatus('revoked')).toBe(false)
    expect(isActiveIntegratorStatus('Active')).toBe(false)
  })
})

describe('listRegistryMetrics', () => {
  const registry: IntegratorRegistry = {
    players: { value: 10, definition: 'distinct identities with an active IntegratorBinding', class: 'durable-derived' },
    totalPlayersEver: { value: 12, definition: 'distinct identities that ever had a binding', class: 'durable-derived' },
    achievementsIssued: { value: 5, definition: 'count of achievement.issued events', class: 'durable-derived' },
    achievementsRevoked: { value: 1, definition: 'count of achievement.revoked events', class: 'durable-derived' },
    uniqueAchievementHolders: { value: 4, definition: 'distinct subjects with >=1 valid attestation', class: 'durable-derived' },
  }

  it('returns all five metrics, each with a non-empty label, definition, and class', () => {
    const metrics = listRegistryMetrics(registry)
    expect(metrics).toHaveLength(5)
    for (const metric of metrics) {
      expect(metric.label.length).toBeGreaterThan(0)
      expect(metric.definition.length).toBeGreaterThan(0)
      expect(metric.class.length).toBeGreaterThan(0)
      expect(typeof metric.value).toBe('number')
    }
  })

  it('carries the exact value from the response through, never re-deriving it', () => {
    const metrics = listRegistryMetrics(registry)
    const players = metrics.find((m) => m.key === 'players')
    expect(players?.value).toBe(10)
  })
})
