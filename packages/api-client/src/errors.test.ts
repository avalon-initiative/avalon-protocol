import { describe, expect, it } from 'vitest'
import { messageForStatus } from './errors'

describe('messageForStatus', () => {
  it.each([
    [409, 'That identity id is already taken.'],
    [401, 'Authentication failed.'],
    [400, 'That request has expired or was already used — please try again.'],
    [500, 'Something went wrong on the server. Please try again.'],
    [503, 'Something went wrong on the server. Please try again.'],
  ])('maps %i to a user-facing message, never raw server text', (status, expected) => {
    expect(messageForStatus(status)).toBe(expected)
  })

  it('uses the server-provided message for a 400 that is not the ceremony-expiry case', () => {
    expect(
      messageForStatus(400, 'avatar_url must be an http(s) URL of 2048 characters or fewer'),
    ).toBe('avatar_url must be an http(s) URL of 2048 characters or fewer')
  })

  it('falls back to the generic 400 message when no server message is given', () => {
    expect(messageForStatus(400, undefined)).toBe(
      'That request has expired or was already used — please try again.',
    )
  })
})
