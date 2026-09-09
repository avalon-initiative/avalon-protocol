import { describe, expect, it } from 'vitest'
import { mergeSuggestion } from './discovery'

const OTHER = 'other-id'

describe('mergeSuggestion', () => {
  it('leaves display name undefined when the profile map has no entry', () => {
    const suggestion = mergeSuggestion(OTHER, new Map())
    expect(suggestion.identityId).toBe(OTHER)
    expect(suggestion.displayName).toBeUndefined()
  })

  it('resolves display name from the profile map when present', () => {
    const suggestion = mergeSuggestion(OTHER, new Map([[OTHER, 'maybe-friend-7']]))
    expect(suggestion.displayName).toBe('maybe-friend-7')
  })
})
