// Direct/small-group conversations (issue #102/#105) on AccountSession —
// see crates/server/src/conversations.rs.
import { AccountSession } from './core.js'

export interface Conversation {
  id: string
  participants: string[]
}

export interface ConversationMessage {
  id: string
  conversationId: string
  author: string
  body: string
  sentAt: string
}
interface ConversationMessageWire {
  id: string
  conversation_id: string
  author: string
  body: string
  sent_at: string
}

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

AccountSession.prototype.listConversations = function (this: AccountSession): Promise<Conversation[]> {
  return this.get('/conversations')
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
