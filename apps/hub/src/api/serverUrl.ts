// Which avalon-server this Hub talks to (the network selector) —
// localStorage (checked first) lets a viewer's choice persist across
// reloads; VITE_AVALON_SERVER_URL stays the build-time default for a Hub
// that's never had one explicitly picked. Ported from
// packages/api-client/src/client.ts's own getServerUrl/setServerUrl as
// part of migrating off that package — same behavior, same
// storage key, so an existing viewer's stored choice keeps working
// unchanged across the migration.
const SERVER_URL_STORAGE_KEY = 'avalon.serverUrl'

function readStoredServerUrl(): string | null {
  try {
    return localStorage.getItem(SERVER_URL_STORAGE_KEY)
  } catch {
    return null
  }
}

/** The server URL this Hub is currently configured to talk to. */
export function getServerUrl(): string {
  return readStoredServerUrl() ?? import.meta.env.VITE_AVALON_SERVER_URL ?? 'http://127.0.0.1:8080'
}

/**
 * Switches which server this Hub talks to, persisted across reloads. Does
 * NOT itself reload the page or reset any in-memory session state — a
 * caller (the network selector) is expected to reload immediately after,
 * since an existing session's bearer token/state was established against
 * the *previous* server and has no meaning against a different one.
 */
export function setServerUrl(url: string): void {
  try {
    localStorage.setItem(SERVER_URL_STORAGE_KEY, url)
  } catch {
    // Private browsing / storage disabled — nothing to persist to, but
    // don't throw and break the switch flow.
  }
}
