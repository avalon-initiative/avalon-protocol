// Issue #60's own acceptance criterion: apps/mobile-hub authenticates using
// the shared @avalon/api-client module, with zero API code duplicated from
// apps/hub. Two checks: the shared module is real and actually exercised
// from here (not just re-declared locally), and nothing under this app's
// own src/ reimplements the fetch client @avalon/api-client already owns.
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  createIdentity,
  getServerUrl,
  login,
  registerStart,
  setServerUrl,
  useSessionStore,
} from '@avalon/api-client'

function mockFetchOnce(status: number, body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: status >= 200 && status < 300,
      status,
      json: () => Promise.resolve(body),
      text: () => Promise.resolve(JSON.stringify(body)),
    }),
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

describe('the shared api-client module, exercised from apps/mobile-hub', () => {
  it('registerStart hits the same avalon-server endpoint apps/hub calls', async () => {
    mockFetchOnce(200, { ticket_id: 't1', challenge: { publicKey: {} } })

    await registerStart({ identity_id: 'id-1', display_name: 'name' })

    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/identities/register/start')
  })

  it('getServerUrl/setServerUrl round-trip through the one shared storage key', () => {
    expect(getServerUrl()).toBe('http://127.0.0.1:8080')
    setServerUrl('http://192.168.1.50:8080')
    expect(getServerUrl()).toBe('http://192.168.1.50:8080')
  })

  it('login/createIdentity/useSessionStore are the real functions, not local stand-ins', () => {
    expect(typeof login).toBe('function')
    expect(typeof createIdentity).toBe('function')
    expect(typeof useSessionStore).toBe('function')
  })
})

// Walks this app's own source (not node_modules, not @avalon/api-client
// itself) looking for a second `fetch(` implementation — the actual
// "no duplicated API code" check. A hit here would mean this app grew its
// own copy of client.ts's request() instead of importing the shared one.
function collectSourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules') continue
    const path = join(dir, entry.name)
    if (entry.isDirectory()) {
      collectSourceFiles(path, out)
    } else if (/\.(ts|vue)$/.test(entry.name) && !entry.name.endsWith('.test.ts')) {
      out.push(path)
    }
  }
  return out
}

describe('no duplicated fetch/API client code in apps/mobile-hub/src', () => {
  it('never calls fetch() directly outside the shared @avalon/api-client package', () => {
    const files = collectSourceFiles(join(__dirname))
    const offenders = files.filter((file) => /\bfetch\(/.test(readFileSync(file, 'utf-8')))
    expect(offenders).toEqual([])
  })
})
