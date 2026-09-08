// Maps avalon-server's `{ "error": ... }` bodies to a message a player can
// see. Never surfaces raw server text — see issue #55's design.
export class AvalonApiError extends Error {
  readonly status: number

  constructor(status: number, message: string) {
    super(message)
    this.name = 'AvalonApiError'
    this.status = status
  }
}

export function messageForStatus(status: number): string {
  if (status === 409) return 'That identity id is already taken.'
  if (status === 401) return 'Authentication failed.'
  if (status === 400) return 'That request has expired or was already used — please try again.'
  // Covers both a friend request that's already been resolved (accept/
  // decline) and removing a friendship that doesn't exist — the server
  // uses 404 for both (crates/server/src/friends.rs), and either way the
  // underlying cause is the same from a player's point of view: whatever
  // this was about to act on isn't there anymore.
  if (status === 404) return "That's no longer there — it may have already been handled."
  if (status >= 500) return 'Something went wrong on the server. Please try again.'
  return 'Something went wrong.'
}
