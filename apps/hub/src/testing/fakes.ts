// Test doubles shared by the view tests. Not shipped — only imported from
// *.test.ts files.
import { vi } from 'vitest'

/**
 * A minimal stand-in for the browser's `WebSocket` — records what was sent
 * and lets a test drive `open`/`message` events by hand rather than
 * depending on a real connection.
 */
export class FakeWebSocket {
  static readonly OPEN = 1
  static instances: FakeWebSocket[] = []

  readyState = 0
  sent: string[] = []
  url: string
  private listeners: Record<string, ((event: unknown) => void)[]> = {}

  constructor(url: string) {
    this.url = url
    FakeWebSocket.instances.push(this)
  }

  addEventListener(type: string, listener: (event: unknown) => void) {
    ;(this.listeners[type] ??= []).push(listener)
  }

  send(data: string) {
    this.sent.push(data)
  }

  close() {}

  emitOpen() {
    this.readyState = FakeWebSocket.OPEN
    for (const listener of this.listeners.open ?? []) listener({})
  }

  emitMessage(data: unknown) {
    for (const listener of this.listeners.message ?? []) {
      listener({ data: JSON.stringify(data) })
    }
  }
}

/**
 * Stubs `fetch` with a per-path response table: the request URL's pathname
 * (query string stripped) is looked up in `responses`; anything not listed
 * gets an empty 200 body. Lets a view that calls several endpoints on
 * mount be tested without one mock body having to satisfy all of them.
 */
export function mockFetchByPath(responses: Record<string, unknown>) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockImplementation((url: string) => {
      const path = new URL(url, 'http://test').pathname
      const body = path in responses ? responses[path] : undefined
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve(body),
        text: () => Promise.resolve(body === undefined ? '' : JSON.stringify(body)),
      })
    }),
  )
}
