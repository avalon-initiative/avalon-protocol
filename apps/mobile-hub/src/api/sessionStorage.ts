// Where session credentials (token, identity id) are persisted. The default is
// localStorage, which is what a plain browser build uses; the Tauri build swaps
// in the OS keychain adapter (storage/secureStorage.ts) once at startup, before
// the session store reads anything.
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
        return null
      }
    },
    async setItem(key, value) {
      try {
        localStorage.setItem(key, value)
      } catch {
        // Nothing to persist to; don't break the caller.
      }
    },
    async removeItem(key) {
      try {
        localStorage.removeItem(key)
      } catch {
        // Same as setItem.
      }
    },
  }
}

let sessionStore: KeyValueStore = createLocalStorageStore()

/** Swaps the store backing session credentials. Call once at startup, before the session store is used. */
export function configureSessionStorage(store: KeyValueStore): void {
  sessionStore = store
}

export function getSessionStorage(): KeyValueStore {
  return sessionStore
}
