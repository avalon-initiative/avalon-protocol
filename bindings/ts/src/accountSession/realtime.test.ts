// Unit coverage for what's testable without a real avalon-server: message
// queuing/subscribe-before-open behavior and the node_info handshake, all
// against a stubbed WebSocket. The actual live push (server really pushing
// a presence/chat update over the wire) is only honestly covered by
// account.live.test.ts — see this file's own header comment for why.
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { AccountSession } from './core.js'
import { generateSigningKey } from '../crypto/signing.js'
import './realtime.js'

class MockWebSocket {
  static readonly CONNECTING = 0
  static readonly OPEN = 1
  static readonly CLOSED = 3

  readyState = MockWebSocket.CONNECTING
  sent: unknown[] = []
  private listeners: Record<string, ((event: { data?: string }) => void)[]> = {}

  constructor(public url: string) {}

  addEventListener(type: string, cb: (event: { data?: string }) => void): void {
    ;(this.listeners[type] ??= []).push(cb)
  }

  send(data: string): void {
    this.sent.push(JSON.parse(data))
  }

  close(): void {
    this.readyState = MockWebSocket.CLOSED
  }

  triggerOpen(): void {
    this.readyState = MockWebSocket.OPEN
    for (const cb of this.listeners['open'] ?? []) cb({})
  }

  triggerMessage(data: unknown): void {
    for (const cb of this.listeners['message'] ?? []) cb({ data: JSON.stringify(data) })
  }
}

let lastSocket: MockWebSocket | undefined
let originalWebSocket: typeof WebSocket

beforeEach(() => {
  originalWebSocket = globalThis.WebSocket
  lastSocket = undefined
  // A constructor function (not a class extending MockWebSocket) so
  // `new WebSocket(url)` can return the tracked instance without an
  // eslint-flagged `this` alias. Carries MockWebSocket's static OPEN/etc.
  // constants, since realtime.ts compares against `WebSocket.OPEN`.
  function TrackedWebSocket(url: string): MockWebSocket {
    const socket = new MockWebSocket(url)
    lastSocket = socket
    return socket
  }
  Object.assign(TrackedWebSocket, MockWebSocket)
  globalThis.WebSocket = TrackedWebSocket as unknown as typeof WebSocket
})

afterEach(() => {
  globalThis.WebSocket = originalWebSocket
})

function testSession(signing?: { secretKey: Uint8Array; publicKey: Uint8Array; signingKeyId: string }): AccountSession {
  const identity = { id: crypto.randomUUID(), createdAt: new Date().toISOString() }
  return new AccountSession({
    identity,
    profile: {
      identityId: identity.id,
      displayName: 'test',
      avatarUrl: null,
      bio: null,
      favoriteGenres: [],
      pronouns: null,
      bannerUrl: null,
      status: null,
      links: [],
      timezone: null,
      themeColor: null,
      location: null,
      mainGuild: null,
    },
    serverUrl: 'http://127.0.0.1:1',
    token: 'test-token',
    signing,
  })
}

describe('subscribePresence', () => {
  it('connects to /ws/presence with the session token', () => {
    const session = testSession()
    session.subscribePresence(() => {})
    expect(lastSocket!.url).toBe('ws://127.0.0.1:1/ws/presence?token=test-token')
  })

  it('queues subscribe() calls made before open, then flushes them once open fires', () => {
    const session = testSession()
    const sub = session.subscribePresence(() => {})
    sub.subscribe(['id-1', 'id-2'])
    expect(lastSocket!.sent).toEqual([])

    lastSocket!.triggerOpen()
    expect(lastSocket!.sent).toEqual([{ type: 'subscribe', ids: ['id-1', 'id-2'] }])
  })

  it('sends immediately once already open', () => {
    const session = testSession()
    const sub = session.subscribePresence(() => {})
    lastSocket!.triggerOpen()
    sub.subscribe(['id-3'])
    expect(lastSocket!.sent).toEqual([{ type: 'subscribe', ids: ['id-3'] }])
  })

  it('ignores an empty ids array', () => {
    const session = testSession()
    const sub = session.subscribePresence(() => {})
    sub.subscribe([])
    lastSocket!.triggerOpen()
    expect(lastSocket!.sent).toEqual([])
  })

  it('fires onUpdate once per pushed message, camelCased', () => {
    const session = testSession()
    const updates: unknown[] = []
    session.subscribePresence((p) => updates.push(p))
    lastSocket!.triggerMessage({ identity_id: 'id-1', status: 'Online', active_in: null, updated_at: '2026-01-01T00:00:00Z' })
    expect(updates).toEqual([{ identityId: 'id-1', status: 'Online', activeIn: null, updatedAt: '2026-01-01T00:00:00Z' }])
  })

  it('close() closes the underlying socket', () => {
    const session = testSession()
    const sub = session.subscribePresence(() => {})
    sub.close()
    expect(lastSocket!.readyState).toBe(MockWebSocket.CLOSED)
  })
})

describe('subscribeChannelMessages', () => {
  it('connects to /ws/messages and sends subscribe_channel with no claim once node_info arrives (no local signing key)', () => {
    const session = testSession()
    session.subscribeChannelMessages('guild-1', 'chan-1', () => {})
    expect(lastSocket!.url).toBe('ws://127.0.0.1:1/ws/messages?token=test-token')
    expect(lastSocket!.sent).toEqual([])

    lastSocket!.triggerOpen()
    lastSocket!.triggerMessage({ type: 'node_info', data: { base_url: 'https://node.example' } })
    expect(lastSocket!.sent).toEqual([
      { type: 'subscribe_channel', guild_id: 'guild-1', channel_id: 'chan-1', claim: undefined },
    ])
  })

  it('mints and attaches a signed claim when this session holds a local signing key and the server offers a base_url', () => {
    const { secretKey, publicKey } = generateSigningKey()
    const session = testSession({ secretKey, publicKey, signingKeyId: 'key-1' })
    session.subscribeChannelMessages('guild-1', 'chan-1', () => {})

    lastSocket!.triggerOpen()
    lastSocket!.triggerMessage({ type: 'node_info', data: { base_url: 'https://node.example' } })

    expect(lastSocket!.sent).toHaveLength(1)
    const sent = lastSocket!.sent[0] as { claim: string }
    expect(typeof sent.claim).toBe('string')
    const claim = JSON.parse(sent.claim) as { signing_key_id: string; scope: unknown; base_url: string }
    expect(claim.signing_key_id).toBe('key-1')
    expect(claim.base_url).toBe('https://node.example')
    expect(claim.scope).toEqual({ kind: 'channel', channel_id: 'chan-1' })
  })

  it('sends no claim when the server offers no base_url even with a local signing key', () => {
    const { secretKey, publicKey } = generateSigningKey()
    const session = testSession({ secretKey, publicKey, signingKeyId: 'key-1' })
    session.subscribeChannelMessages('guild-1', 'chan-1', () => {})
    lastSocket!.triggerOpen()
    lastSocket!.triggerMessage({ type: 'node_info', data: { base_url: null } })
    expect(lastSocket!.sent).toEqual([{ type: 'subscribe_channel', guild_id: 'guild-1', channel_id: 'chan-1', claim: undefined }])
  })

  it('ignores a repeated node_info (subscribes at most once)', () => {
    const session = testSession()
    session.subscribeChannelMessages('guild-1', 'chan-1', () => {})
    lastSocket!.triggerOpen()
    lastSocket!.triggerMessage({ type: 'node_info', data: { base_url: null } })
    lastSocket!.triggerMessage({ type: 'node_info', data: { base_url: null } })
    expect(lastSocket!.sent).toHaveLength(1)
  })

  it('fires onMessage for a pushed channel_message, camelCased', () => {
    const session = testSession()
    const messages: unknown[] = []
    session.subscribeChannelMessages('guild-1', 'chan-1', (m) => messages.push(m))
    lastSocket!.triggerMessage({
      type: 'channel_message',
      data: { id: 'm1', channel_id: 'chan-1', author: 'a1', body: 'hi', sent_at: '2026-01-01T00:00:00Z' },
    })
    expect(messages).toEqual([{ id: 'm1', channelId: 'chan-1', author: 'a1', body: 'hi', sentAt: '2026-01-01T00:00:00Z' }])
  })

  it('fires onDeleted with the message id for a moderation delete', () => {
    const session = testSession()
    const deletedIds: string[] = []
    session.subscribeChannelMessages(
      'guild-1',
      'chan-1',
      () => {},
      (messageId) => deletedIds.push(messageId),
    )
    lastSocket!.triggerMessage({ type: 'channel_message_deleted', data: { channel_id: 'chan-1', message_id: 'm1' } })
    expect(deletedIds).toEqual(['m1'])
  })

  it('does not throw when a delete arrives with no onDeleted callback given', () => {
    const session = testSession()
    session.subscribeChannelMessages('guild-1', 'chan-1', () => {})
    expect(() =>
      lastSocket!.triggerMessage({ type: 'channel_message_deleted', data: { channel_id: 'chan-1', message_id: 'm1' } }),
    ).not.toThrow()
  })
})

describe('subscribeConversationMessages', () => {
  it('sends subscribe_conversation once node_info arrives and fires onMessage camelCased', () => {
    const session = testSession()
    const messages: unknown[] = []
    session.subscribeConversationMessages('conv-1', (m) => messages.push(m))
    lastSocket!.triggerOpen()
    lastSocket!.triggerMessage({ type: 'node_info', data: { base_url: null } })
    expect(lastSocket!.sent).toEqual([{ type: 'subscribe_conversation', conversation_id: 'conv-1', claim: undefined }])

    lastSocket!.triggerMessage({
      type: 'conversation_message',
      data: { id: 'm1', conversation_id: 'conv-1', author: 'a1', body: 'hi', sent_at: '2026-01-01T00:00:00Z' },
    })
    expect(messages).toEqual([{ id: 'm1', conversationId: 'conv-1', author: 'a1', body: 'hi', sentAt: '2026-01-01T00:00:00Z' }])
  })
})
