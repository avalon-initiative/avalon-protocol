// Direct/small-group conversations — capability-gated reads/writes on Session,
// mirroring crates/sdk/src/conversations.rs.
//
// messages.read covers discovering and reading conversations an identity is
// already in; messages.send covers starting a conversation and posting into
// one — same split conversations.rs documents. No conversation content is
// cached: every method makes a fresh request.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    public sealed class Conversation
    {
        public Conversation(Guid id, IReadOnlyList<Guid> participants)
        {
            Id = id;
            Participants = participants;
        }

        public Guid Id { get; }
        public IReadOnlyList<Guid> Participants { get; }
    }

    public sealed class ConversationMessage
    {
        public Guid Id { get; set; }
        public Guid ConversationId { get; set; }
        public Guid Author { get; set; }
        public string Body { get; set; } = "";
        public DateTimeOffset SentAt { get; set; }
    }

    /// <summary>Maps the generated <see cref="Avalon.Sdk.Generated.ConversationResponse"/>
    /// wire shape onto the public <see cref="Conversation"/> domain type.</summary>
    internal static class ConversationResponseExtensions
    {
        public static Conversation ToConversation(this Avalon.Sdk.Generated.ConversationResponse response) =>
            new Conversation(response.Id, new List<Guid>(response.Participants));

        public static ConversationMessage ToConversationMessage(this Avalon.Sdk.Generated.ConversationMessageResponse response) =>
            new ConversationMessage
            {
                Id = response.Id,
                ConversationId = response.ConversationId,
                Author = response.Author,
                Body = response.Body,
                SentAt = response.SentAt,
            };
    }

    public sealed partial class Session
    {
        /// <summary>Translates a non-success /conversations response. The server collapses
        /// "never a participant" and "blocked" into the same 403 — mapped here
        /// without inspecting the response body, so the SDK never leaks more than the server
        /// already refused to provide.</summary>
        internal static Exception ConversationError(HttpStatusCode status) =>
            status == HttpStatusCode.Forbidden
                ? new NotConversationParticipantException()
                : ServerError(status);

        /// <summary>GET /conversations — requires messages.read. The caller's own conversation
        /// list; a conversation with a block anywhere in its participant set is already
        /// excluded server-side.</summary>
        public async Task<IReadOnlyList<Conversation>> ConversationsAsync(CancellationToken ct = default)
        {
            Require("messages.read");

            using var request = new HttpRequestMessage(HttpMethod.Get, $"{ServerUrl}/conversations");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", Token);
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ConversationError(response.StatusCode);
            }
            var conversations = await ReadJsonAsync<List<Avalon.Sdk.Generated.ConversationResponse>>(response, ct).ConfigureAwait(false)
                ?? new List<Avalon.Sdk.Generated.ConversationResponse>();
            return conversations.Select(c => c.ToConversation()).ToList();
        }

        /// <summary>A handle scoped to a conversation id already known to the caller — an
        /// ungated local constructor, same as Guild(id): it makes no request of its own.</summary>
        public ConversationHandle Conversation(Guid id) => new ConversationHandle(this, id);

        /// <summary>POST /conversations — requires messages.send. Creates, or returns the
        /// existing, 1:1 conversation between the caller and otherIdentityId (idempotent on
        /// the participant set). Gated on messages.send rather than messages.read: starting a
        /// conversation is the only thing a caller can do with a brand-new one.</summary>
        public async Task<ConversationHandle> DmAsync(Guid otherIdentityId, CancellationToken ct = default)
        {
            Require("messages.send");

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{ServerUrl}/conversations");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", Token);
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.CreateConversationRequest { Participants = new List<Guid> { otherIdentityId } }),
                Encoding.UTF8, "application/json");
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ConversationError(response.StatusCode);
            }
            var body = await ReadJsonAsync<Avalon.Sdk.Generated.ConversationResponse>(response, ct).ConfigureAwait(false);
            return Conversation(body!.Id);
        }
    }

    /// <summary>See Session.Conversation / Session.DmAsync.</summary>
    public sealed class ConversationHandle
    {
        private readonly Session _session;

        internal ConversationHandle(Session session, Guid conversationId)
        {
            _session = session;
            ConversationId = conversationId;
        }

        public Guid ConversationId { get; }

        /// <summary>GET /conversations/{id}/messages?before=&amp;limit= — requires
        /// messages.read. Newest first, cursor-paginated exactly as the server paginates it.
        /// Throws NotConversationParticipantException if the caller isn't (or is no longer,
        /// due to a block) a participant.</summary>
        public async Task<IReadOnlyList<ConversationMessage>> MessagesAsync(Guid? before = null, int? limit = null, CancellationToken ct = default)
        {
            _session.Require("messages.read");

            var parts = new List<string>();
            if (before.HasValue) parts.Add($"before={before.Value}");
            if (limit.HasValue) parts.Add($"limit={limit.Value}");
            var query = parts.Count == 0 ? "" : "?" + string.Join("&", parts);

            using var request = new HttpRequestMessage(HttpMethod.Get,
                $"{_session.ServerUrl}/conversations/{ConversationId}/messages{query}");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", _session.Token);
            using var response = await _session.Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ConversationError(response.StatusCode);
            }
            var messages = await Session.ReadJsonAsync<List<Avalon.Sdk.Generated.ConversationMessageResponse>>(response, ct).ConfigureAwait(false)
                ?? new List<Avalon.Sdk.Generated.ConversationMessageResponse>();
            return messages.Select(m => m.ToConversationMessage()).ToList();
        }

        /// <summary>POST /conversations/{id}/messages — requires messages.send. Posts as the
        /// identity under their own session token. Throws NotConversationParticipantException
        /// if the caller isn't (or is no longer, due to a block) a participant.</summary>
        public Task<ConversationMessage> SendAsync(string body, CancellationToken ct = default) =>
            SendWithClientEntryIdAsync(body, null, ct);

        /// <summary>Same request as SendAsync, with an optional idempotency key — the one
        /// thing a future deferred submission engine needs beyond a direct online send. This
        /// is the single place that builds a POST /conversations/{id}/messages request.</summary>
        internal async Task<ConversationMessage> SendWithClientEntryIdAsync(string body, Guid? clientEntryId, CancellationToken ct = default)
        {
            _session.Require("messages.send");

            using var request = new HttpRequestMessage(HttpMethod.Post, $"{_session.ServerUrl}/conversations/{ConversationId}/messages");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", _session.Token);
            request.Content = new StringContent(
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.ConversationSendMessageRequest { Body = body, ClientEntryId = clientEntryId }),
                Encoding.UTF8, "application/json");
            using var response = await _session.Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ConversationError(response.StatusCode);
            }
            var message = await Session.ReadJsonAsync<Avalon.Sdk.Generated.ConversationMessageResponse>(response, ct).ConfigureAwait(false);
            return message!.ToConversationMessage();
        }
    }
}
