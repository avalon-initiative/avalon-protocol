// The mobile-hub side of issue #60's platform secure-storage requirement:
// a `KeyValueStore` (see @avalon/api-client's own module) backed by the
// single native command src-tauri/src/lib.rs exposes, rather than
// `localStorage` — the session token this stores never touches the
// webview's own storage on this platform.
import { invoke } from '@tauri-apps/api/core'
import type { KeyValueStore } from '@avalon/api-client'

export function createSecureSessionStorage(): KeyValueStore {
  return {
    async getItem(key) {
      return await invoke<string | null>('secure_storage_get', { key })
    },
    async setItem(key, value) {
      await invoke('secure_storage_set', { key, value })
    },
    async removeItem(key) {
      await invoke('secure_storage_delete', { key })
    },
  }
}
