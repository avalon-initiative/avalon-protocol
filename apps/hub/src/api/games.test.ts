import { describe, expect, it } from 'vitest'
import { buildGamesListQueryString, isActiveGameStatus, listRegistryMetrics } from './games'
import type { GameRegistryResponse } from './types'

describe('buildGamesListQueryString', () => {
  it('omits every key when nothing is set', () => {
    expect(buildGamesListQueryString({})).toBe('')
  })

  it('trims and includes q only when non-blank', () => {
    expect(buildGamesListQueryString({ q: '  ashen  ' })).toBe('?q=ashen')
    expect(buildGamesListQueryString({ q: '   ' })).toBe('')
  })

  it('includes sort, limit, and cursor when set', () => {
    const query = buildGamesListQueryString({ sort: 'name', limit: 10, cursor: 'abc-123' })
    expect(query).toContain('sort=name')
    expect(query).toContain('limit=10')
    expect(query).toContain('cursor=abc-123')
  })
})

describe('isActiveGameStatus', () => {
  it('is true only for exactly "active"', () => {
    expect(isActiveGameStatus('active')).toBe(true)
    expect(isActiveGameStatus('suspended')).toBe(false)
    expect(isActiveGameStatus('revoked')).toBe(false)
    expect(isActiveGameStatus('Active')).toBe(false)
  })
})

describe('listRegistryMetrics', () => {
  const registry: GameRegistryResponse = {
    players: { value: 10, definition: 'distinct identities with an active GameBinding', class: 'durable-derived' },
    total_players_ever: { value: 12, definition: 'distinct identities that ever had a binding', class: 'durable-derived' },
    achievements_issued: { value: 5, definition: 'count of achievement.issued events', class: 'durable-derived' },
    achievements_revoked: { value: 1, definition: 'count of achievement.revoked events', class: 'durable-derived' },
    unique_achievement_holders: { value: 4, definition: 'distinct subjects with >=1 valid attestation', class: 'durable-derived' },
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
