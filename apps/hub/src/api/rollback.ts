// Post-compromise rollback: thin wrappers over the AccountSession calls plus
// the pure helpers the Rollback view needs (window formatting, error copy).
import type { AccountSession, RollbackCandidates } from '@avalon-initiative/protocol-sdk'

export function rollbackCandidates(
  session: AccountSession,
  since: string,
): Promise<RollbackCandidates> {
  return session.rollbackCandidates(since)
}

// `since` must be the exact string passed to both calls: the reversal
// signature covers it byte for byte.
export function reverseRollbackEvent(
  session: AccountSession,
  eventId: string,
  since: string,
): Promise<string> {
  return session.reverseRollbackEvent(eventId, since)
}

// Converts a local "YYYY-MM-DDTHH:mm" value to an RFC 3339 UTC string, or
// null when it does not parse.
export function toRfc3339Utc(local: string): string | null {
  if (!local) return null
  const d = new Date(local)
  return Number.isNaN(d.getTime()) ? null : d.toISOString()
}

const ROLLBACK_ERROR_MESSAGES: Record<string, string> = {
  ROLLBACK_NO_COMPLETED_RECOVERY:
    'This identity has no completed recovery, so there is nothing to undo yet.',
  INVALID_ROLLBACK_WINDOW:
    'That start time is not valid. Pick a moment before your recovery completed.',
  ROLLBACK_EVENT_NOT_ELIGIBLE:
    'That action is not in the window you selected, so it cannot be undone from here.',
  ROLLBACK_NOT_REVERSIBLE: 'That action cannot be undone.',
  ROLLBACK_ALREADY_REVERSED: 'That action has already been undone.',
}

const GENERIC_ROLLBACK_ERROR = 'Something went wrong. Try again in a moment.'

export function rollbackErrorMessage(code: string | undefined): string {
  return (code && ROLLBACK_ERROR_MESSAGES[code]) || GENERIC_ROLLBACK_ERROR
}

export function errorCode(e: unknown): string | undefined {
  return e instanceof Error ? (e as { code?: string }).code : undefined
}
