import { afterEach, describe, expect, it, vi } from 'vitest'
import { createSecureSessionStorage } from './secureStorage'

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@tauri-apps/api/core'

afterEach(() => {
  vi.mocked(invoke).mockReset()
})

// The session token goes through these native commands, never localStorage.
// These confirm the adapter calls the right command with the right arguments,
// matching the KeyValueStore shape the session store expects.
describe('createSecureSessionStorage', () => {
  it('getItem invokes secure_storage_get with the key', async () => {
    vi.mocked(invoke).mockResolvedValue('a-token')
    const store = createSecureSessionStorage()

    const result = await store.getItem('avalon:session:token')

    expect(result).toBe('a-token')
    expect(invoke).toHaveBeenCalledWith('secure_storage_get', { key: 'avalon:session:token' })
  })

  it('getItem returns null when the native command finds nothing', async () => {
    vi.mocked(invoke).mockResolvedValue(null)
    const store = createSecureSessionStorage()

    expect(await store.getItem('avalon:session:token')).toBeNull()
  })

  it('setItem invokes secure_storage_set with the key and value', async () => {
    vi.mocked(invoke).mockResolvedValue(undefined)
    const store = createSecureSessionStorage()

    await store.setItem('avalon:session:token', 'a-token')

    expect(invoke).toHaveBeenCalledWith('secure_storage_set', {
      key: 'avalon:session:token',
      value: 'a-token',
    })
  })

  it('removeItem invokes secure_storage_delete with the key', async () => {
    vi.mocked(invoke).mockResolvedValue(undefined)
    const store = createSecureSessionStorage()

    await store.removeItem('avalon:session:token')

    expect(invoke).toHaveBeenCalledWith('secure_storage_delete', { key: 'avalon:session:token' })
  })
})
