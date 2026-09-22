// Direct/small-group conversations (issue #102/#105) on AccountSession. Mirrors
// crates/sdk/src/account/conversations.rs. Named with an Account prefix since Conversation/
// ConversationMessage already exist as the integrator Session's own (differently-shaped)
// types in Conversations.cs.

using System;
using System.Collections.Generic;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>A conversation this identity participates in. Mirrors the Rust SDK's
    /// <c>account::conversations::Conversation</c>.</summary>
    public sealed class AccountConversation
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        /// <summary>Every current participant, including the caller.</summary>
        [JsonPropertyName("participants")]
        public List<Guid> Participants { get; set; } = new List<Guid>();
    }

    /// <summary>One message within a conversation. Mirrors the Rust SDK's
    /// <c>account::conversations::ConversationMessage</c>.</summary>
    public sealed class AccountConversationMessage
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("conversation_id")]
        public Guid ConversationId { get; set; }

        [JsonPropertyName("author")]
        public Guid Author { get; set; }

        [JsonPropertyName("body")]
        public string Body { get; set; } = "";

        [JsonPropertyName("sent_at")]
        public DateTimeOffset SentAt { get; set; }
    }

    public sealed partial class AccountSession
    {
        /// <summary><c>GET /conversations</c>.</summary>
        public async Task<IReadOnlyList<AccountConversation>> ListConversationsAsync(CancellationToken ct = default) =>
            await GetAsync<List<AccountConversation>>("/conversations", ct).ConfigureAwait(false);

        /// <summary><c>POST /conversations</c> — idempotent on the final participant set
        /// (the caller is always added, then deduplicated); returns the existing
        /// conversation rather than creating a duplicate.</summary>
        public async Task<AccountConversation> CreateConversationAsync(IReadOnlyList<Guid> participants, CancellationToken ct = default) =>
            await PostAsync<Avalon.Sdk.Generated.CreateConversationRequest, AccountConversation>(
                "/conversations", new Avalon.Sdk.Generated.CreateConversationRequest { Participants = new List<Guid>(participants) }, ct).ConfigureAwait(false);

        /// <summary><c>GET /conversations/{id}/messages</c>, cursor-paginated with
        /// <paramref name="before"/>.</summary>
        public async Task<IReadOnlyList<AccountConversationMessage>> ConversationMessagesAsync(
            Guid conversationId, string? before = null, uint? limit = null, CancellationToken ct = default)
        {
            var query = new List<(string, string)>();
            if (before != null)
            {
                query.Add(("before", before));
            }
            if (limit != null)
            {
                query.Add(("limit", limit.Value.ToString()));
            }
            return await GetQueryAsync<List<AccountConversationMessage>>($"/conversations/{conversationId}/messages", query, ct).ConfigureAwait(false);
        }

        /// <summary><c>POST /conversations/{id}/messages</c>. Not signature-required (chat
        /// is high-frequency and reversible by deletion, per #697's own invariants) — though
        /// conversations have no moderation-delete endpoint themselves, unlike guild channel
        /// messages.</summary>
        public async Task<AccountConversationMessage> SendConversationMessageAsync(Guid conversationId, string body, CancellationToken ct = default) =>
            await PostAsync<Avalon.Sdk.Generated.SendMessageRequest, AccountConversationMessage>(
                $"/conversations/{conversationId}/messages", new Avalon.Sdk.Generated.SendMessageRequest { Body = body }, ct).ConfigureAwait(false);
    }
}
