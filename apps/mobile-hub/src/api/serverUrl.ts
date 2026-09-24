// Which avalon-server this app talks to. localStorage (checked first) lets the
// user's choice persist across launches; VITE_AVALON_SERVER_URL is the
// build-time default for an install that has never picked one. The server URL
// is not a credential, so it stays in localStorage rather than secure storage.
const SERVER_URL_STORAGE_KEY = 'avalon.serverUrl'

function readStoredServerUrl(): string | null {
  try {
    return localStorage.getItem(SERVER_URL_STORAGE_KEY)
  } catch {
    return null
  }
}

/** The server URL this app is currently configured to talk to. */
export function getServerUrl(): string {
  return readStoredServerUrl() ?? import.meta.env.VITE_AVALON_SERVER_URL ?? 'http://127.0.0.1:8080'
}

/**
 * Switches which server this app talks to, persisted across launches. Does
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
