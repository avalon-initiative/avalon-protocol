import { describe, expect, it } from 'vitest'
import { messageForStatus } from './errors'

describe('messageForStatus', () => {
  it.each([
    [409, 'That identity id is already taken.'],
    [401, 'Authentication failed.'],
    [400, 'That request has expired or was already used — please try again.'],
    [500, 'Something went wrong on the server. Please try again.'],
    [503, 'Something went wrong on the server. Please try again.'],
  ])('maps %i to a player-facing message, never raw server text', (status, expected) => {
    expect(messageForStatus(status)).toBe(expected)
  })
})
