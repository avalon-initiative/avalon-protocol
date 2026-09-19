// Pluggable persistence for exactly one thing: the session credentials
// (`client.ts`'s reconnect lookups, `session.ts`'s store) that issue #60
// requires go through platform secure storage on the Tauri side rather
// than `localStorage`. Everything else this package persists (the chosen
// server URL — not a credential) keeps using `localStorage` directly in
// both apps, unchanged from #55/#232.
//
// A single module-level adapter, defaulting to `localStorage`, keeps every
// existing hub call site working with zero changes — mobile-hub's
// `main.ts` is the only caller expected to ever call
// `configureSessionStorage`, once, at boot, before the session store reads
// anything.
export interface KeyValueStore {
  getItem(key: string): Promise<string | null>
  setItem(key: string, value: string): Promise<void>
  removeItem(key: string): Promise<void>
}

export function createLocalStorageStore(): KeyValueStore {
  return {
    async getItem(key) {
      try {
        return localStorage.getItem(key)
      } catch {
        // Private browsing / storage disabled — same fallback #55's
        // original localStorage call sites already used.
        return null
      }
    },
    async setItem(key, value) {
      try {
        localStorage.setItem(key, value)
      } catch {
        // Nothing to persist to — don't throw and break the caller.
      }
    },
    async removeItem(key) {
      try {
        localStorage.removeItem(key)
      } catch {
        // Same as setItem above.
      }
    },
  }
}

let sessionStore: KeyValueStore = createLocalStorageStore()

/**
 * Swaps the store backing session credentials (session token, and the
 * identityId/signingKeyId reconnect material alongside it). Call once, at
 * app boot, before any api-client function or the session store runs —
 * mobile-hub's `main.ts` calls this with a Tauri secure-storage adapter;
 * the web Hub never calls it and keeps the `localStorage` default.
 */
export function configureSessionStorage(store: KeyValueStore): void {
  sessionStore = store
}

export function getSessionStorage(): KeyValueStore {
  return sessionStore
}
