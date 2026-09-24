// A `KeyValueStore` (see ../api/sessionStorage) backed by the native commands
// src-tauri/src/lib.rs exposes, rather than `localStorage`: the session token
// this stores never touches the webview's own storage.
import { invoke } from '@tauri-apps/api/core'
import type { KeyValueStore } from '../api/sessionStorage'

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
