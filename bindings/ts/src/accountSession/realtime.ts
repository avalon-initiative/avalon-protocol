// Live push over WebSocket on AccountSession — additive
// to the point-in-time reads `social.ts`/`guildAdmin.ts`/`conversations.ts`
// already cover, not a replacement for them. Mirrors
// packages/api-client/src/client.ts's `openPresenceSocket`/
// `openChannelMessageSocket`/`openConversationMessageSocket`; see
// crates/server/src/presence.rs/chat.rs for the server side.
import { AccountSession, type AccountSigningKey } from './core.js'
import { mintInterestClaim, type ClaimedScope } from '../crypto/interestClaim.js'

// serverUrl is http(s)://…; the websocket endpoint needs ws(s)://… — same
// scheme swap packages/api-client/src/client.ts's own websocketUrl does.
function websocketUrl(serverUrl: string, path: string): string {
  if (serverUrl.startsWith('https://')) {
    return `wss://${serverUrl.slice('https://'.length)}${path}`
  }
  if (serverUrl.startsWith('http://')) {
    return `ws://${serverUrl.slice('http://'.length)}${path}`
  }
  return `${serverUrl}${path}`
}

export type PresenceStatusWire = 'Online' | 'Away' | 'DoNotDisturb' | 'Offline'

export interface PresenceUpdate {
  identityId: string
  status: PresenceStatusWire
  activeIn: string | null
  updatedAt: string
}
interface PresenceUpdateWire {
  identity_id: string
  status: PresenceStatusWire
  active_in: string | null
  updated_at: string
}
function presenceUpdateFromWire(w: PresenceUpdateWire): PresenceUpdate {
  return { identityId: w.identity_id, status: w.status, activeIn: w.active_in, updatedAt: w.updated_at }
}

export interface PresenceSubscription {
  // Additive — calling this again with more ids grows the subscription
  // rather than replacing it, mirroring the server's own `ClientMessage`
  // semantics (crates/server/src/presence.rs). Queued until the socket
  // finishes connecting if called before `open`.
  subscribe(ids: string[]): void
  close(): void
}

export interface ChannelMessageUpdate {
  id: string
  channelId: string
  author: string
  body: string
  sentAt: string
}
interface ChannelMessageUpdateWire {
  id: string
  channel_id: string
  author: string
  body: string
  sent_at: string
}
function channelMessageUpdateFromWire(w: ChannelMessageUpdateWire): ChannelMessageUpdate {
  return { id: w.id, channelId: w.channel_id, author: w.author, body: w.body, sentAt: w.sent_at }
}

export interface ConversationMessageUpdate {
  id: string
  conversationId: string
  author: string
  body: string
  sentAt: string
}
interface ConversationMessageUpdateWire {
  id: string
  conversation_id: string
  author: string
  body: string
  sent_at: string
}
function conversationMessageUpdateFromWire(w: ConversationMessageUpdateWire): ConversationMessageUpdate {
  return { id: w.id, conversationId: w.conversation_id, author: w.author, body: w.body, sentAt: w.sent_at }
}

export interface RealtimeSubscription {
  close(): void
}

type ChatUpdate =
  | { type: 'channel_message'; data: ChannelMessageUpdateWire }
  | { type: 'channel_message_deleted'; data: { channel_id: string; message_id: string } }
  | { type: 'conversation_message'; data: ConversationMessageUpdateWire }

// Issue #610: what the server sends unprompted, right after upgrade, ahead
// of anything in `ChatUpdate` above — the `base_url` this connection's own
// signed interest claim (if any) must bind to. See
// `crates/server/src/chat.rs::ChatServerMessage::NodeInfo`'s own doc
// comment for why a browser/client can't otherwise know this.
type ChatServerHello = { type: 'node_info'; data: { base_url: string | null } }

// Mints and sends the `subscribe_*` message for `scope` once this
// connection's `node_info` hello has told it which `base_url` to sign a
// claim against — mirrors `subscribeAfterNodeInfo` in
// packages/api-client/src/client.ts. Sends with no `claim` at all when
// this session holds no local signing key or the server offered no
// `base_url` — same "local-only delivery, no DHT registration" degenerate
// case `crates/server/src/chat.rs`'s own module doc comment documents.
function subscribeAfterNodeInfo(
  identityId: string,
  signing: AccountSigningKey | undefined,
  socket: WebSocket,
  baseUrl: string | null,
  scope: ClaimedScope,
  message: Record<string, unknown>,
): void {
  let claim: string | undefined
  if (baseUrl && signing) {
    claim = mintInterestClaim(identityId, signing.signingKeyId, signing.secretKey, scope, baseUrl)
  }
  if (socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ ...message, claim }))
  }
}

declare module './core.js' {
  interface AccountSession {
    /** `GET /ws/presence` — live presence push, additive to
     * `presenceOf`'s point-in-time reads. `onUpdate` fires once per pushed
     * update, including the immediate catch-up snapshot the server sends
     * for each newly-subscribed id. */
    subscribePresence(onUpdate: (presence: PresenceUpdate) => void): PresenceSubscription
    /** `GET /ws/messages` — live push for one guild channel's
     * messages, additive to `channelMessages`'s cursor-paginated reads. One
     * connection per subscription; a caller viewing a different channel
     * closes this one and opens a fresh one. `onDeleted` (optional) fires
     * with a message id on a moderation delete — conversations have no
     * equivalent, since they have no moderation-delete endpoint. */
    subscribeChannelMessages(
      guildId: string,
      channelId: string,
      onMessage: (message: ChannelMessageUpdate) => void,
      onDeleted?: (messageId: string) => void,
    ): RealtimeSubscription
    /** Same as `subscribeChannelMessages`, for one conversation's messages
     * (additive to `conversationMessages`). */
    subscribeConversationMessages(
      conversationId: string,
      onMessage: (message: ConversationMessageUpdate) => void,
    ): RealtimeSubscription
  }
}

AccountSession.prototype.subscribePresence = function (
  this: AccountSession,
  onUpdate: (presence: PresenceUpdate) => void,
): PresenceSubscription {
  const socket = new WebSocket(websocketUrl(this._serverUrl, `/ws/presence?token=${encodeURIComponent(this._token)}`))
  const pendingSubscriptions: string[][] = []

  function send(ids: string[]) {
    socket.send(JSON.stringify({ type: 'subscribe', ids }))
  }

  socket.addEventListener('open', () => {
    for (const ids of pendingSubscriptions.splice(0)) {
      send(ids)
    }
  })
  socket.addEventListener('message', (event) => {
    onUpdate(presenceUpdateFromWire(JSON.parse(event.data as string) as PresenceUpdateWire))
  })

  return {
    subscribe(ids: string[]) {
      if (ids.length === 0) return
      if (socket.readyState === WebSocket.OPEN) {
        send(ids)
      } else {
        pendingSubscriptions.push(ids)
      }
    },
    close() {
      socket.close()
    },
  }
}

AccountSession.prototype.subscribeChannelMessages = function (
  this: AccountSession,
  guildId: string,
  channelId: string,
  onMessage: (message: ChannelMessageUpdate) => void,
  onDeleted?: (messageId: string) => void,
): RealtimeSubscription {
  const identityId = this._identity.id
  const signing = this._signing
  const socket = new WebSocket(websocketUrl(this._serverUrl, `/ws/messages?token=${encodeURIComponent(this._token)}`))
  let subscribed = false
  socket.addEventListener('message', (event) => {
    const update = JSON.parse(event.data as string) as ChatUpdate | ChatServerHello
    if (update.type === 'node_info') {
      if (subscribed) return
      subscribed = true
      subscribeAfterNodeInfo(identityId, signing, socket, update.data.base_url, { kind: 'channel', channelId }, {
        type: 'subscribe_channel',
        guild_id: guildId,
        channel_id: channelId,
      })
    } else if (update.type === 'channel_message') {
      onMessage(channelMessageUpdateFromWire(update.data))
    } else if (update.type === 'channel_message_deleted') {
      onDeleted?.(update.data.message_id)
    }
  })
  return {
    close() {
      socket.close()
    },
  }
}

AccountSession.prototype.subscribeConversationMessages = function (
  this: AccountSession,
  conversationId: string,
  onMessage: (message: ConversationMessageUpdate) => void,
): RealtimeSubscription {
  const identityId = this._identity.id
  const signing = this._signing
  const socket = new WebSocket(websocketUrl(this._serverUrl, `/ws/messages?token=${encodeURIComponent(this._token)}`))
  let subscribed = false
  socket.addEventListener('message', (event) => {
    const update = JSON.parse(event.data as string) as ChatUpdate | ChatServerHello
    if (update.type === 'node_info') {
      if (subscribed) return
      subscribed = true
      subscribeAfterNodeInfo(
        identityId,
        signing,
        socket,
        update.data.base_url,
        { kind: 'conversation', conversationId },
        { type: 'subscribe_conversation', conversation_id: conversationId },
      )
    } else if (update.type === 'conversation_message') {
      onMessage(conversationMessageUpdateFromWire(update.data))
    }
  })
  return {
    close() {
      socket.close()
    },
  }
}
