// Direct/small-group conversations (issue #102/#105) on AccountSession —
// see crates/server/src/conversations.rs.
import { AccountSession } from './core.js'
import type { components } from '../generated.js'

export type Conversation = components['schemas']['ConversationResponse']

export interface ConversationMessage {
  id: string
  conversationId: string
  author: string
  body: string
  sentAt: string
}
// `ConversationMessageResponse`, not `MessageResponse` — the two used to
// collide in the published schema (both registered as bare
// `MessageResponse` in `crates/server/src/openapi.rs`'s aggregator; utoipa
// silently let the second-registered `guild_messages::MessageResponse`
// win, so `docs/generated/openapi.json`'s `MessageResponse` component
// described the *guild channel* shape (`channel_id`) even for this
// endpoint, which actually sends `conversation_id` — found and fixed at
// the schema source, issue #726, see `crates/server/src/conversations.rs`'s
// own `#[schema(as = ConversationMessageResponse)]` comment).
type ConversationMessageWire = components['schemas']['ConversationMessageResponse']

declare module './core.js' {
  interface AccountSession {
    listConversations(): Promise<Conversation[]>
    /** `POST /conversations` — idempotent on the final participant set. */
    createConversation(participants: string[]): Promise<Conversation>
    conversationMessages(
      conversationId: string,
      before?: string,
      limit?: number,
    ): Promise<ConversationMessage[]>
    /** Not signature-required (chat is high-frequency and reversible). */
    sendConversationMessage(conversationId: string, body: string): Promise<ConversationMessage>
  }
}

AccountSession.prototype.listConversations = async function (this: AccountSession): Promise<Conversation[]> {
  const w = await this.get<Conversation[]>('/conversations')
  // Passed through as-is on a non-array body rather than trusting it —
  // same reasoning as every other list-returning method on this session
  // (see e.g. passkeys.ts's own comment): callers that want to treat a
  // malformed response as a real failure check Array.isArray themselves.
  return Array.isArray(w) ? w : (w as unknown as Conversation[])
}

AccountSession.prototype.createConversation = function (
  this: AccountSession,
  participants: string[],
): Promise<Conversation> {
  return this.post('/conversations', { participants })
}

AccountSession.prototype.conversationMessages = async function (
  this: AccountSession,
  conversationId: string,
  before?: string,
  limit?: number,
): Promise<ConversationMessage[]> {
  const query: Record<string, string> = {}
  if (before) query.before = before
  if (limit !== undefined) query.limit = String(limit)
  const w = await this.getQuery<ConversationMessageWire[]>(`/conversations/${conversationId}/messages`, query)
  if (!Array.isArray(w)) return w as unknown as ConversationMessage[]
  return w.map((m) => ({ id: m.id, conversationId: m.conversation_id, author: m.author, body: m.body, sentAt: m.sent_at }))
}

AccountSession.prototype.sendConversationMessage = async function (
  this: AccountSession,
  conversationId: string,
  body: string,
): Promise<ConversationMessage> {
  const m = await this.post<ConversationMessageWire>(`/conversations/${conversationId}/messages`, { body })
  return { id: m.id, conversationId: m.conversation_id, author: m.author, body: m.body, sentAt: m.sent_at }
}
