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

// `serverMessage`, when present, is avalon-server's own `{ "error": ... }`
// text — used as-is for the statuses below that don't already have a
// hand-picked friendlier override. `400` in particular covers several
// unrelated AppError variants (a WebAuthn ceremony expiring, an invalid
// avatar_url, a self-friend request, ...) with no single friendly string
// that fits all of them, so the server's own message is the right default
// there rather than a fallback that only made sense for one of those cases.
export function messageForStatus(status: number, serverMessage?: string): string {
  if (status === 409) return 'That identity id is already taken.'
  if (status === 401) return 'Authentication failed.'
  if (status === 400) {
    return serverMessage ?? 'That request has expired or was already used — please try again.'
  }
  // Covers both a friend request that's already been resolved (accept/
  // decline) and removing a friendship that doesn't exist — the server
  // uses 404 for both (crates/server/src/friends.rs), and either way the
  // underlying cause is the same from a player's point of view: whatever
  // this was about to act on isn't there anymore.
  if (status === 404) return "That's no longer there — it may have already been handled."
  if (status >= 500) return 'Something went wrong on the server. Please try again.'
  return 'Something went wrong.'
}
