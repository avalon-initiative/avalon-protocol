// Friends and presence — capability-gated reads/writes on Session (issue #17),
// mirroring crates/sdk/src/social.rs.
//
// Every method here checks its own capability *before* making any request,
// same convention the Rust module established — a Session with no grants
// rejects without ever touching the network.
//
// update_presence/presence_of/subscribe_presence match the Rust SDK's own
// documented gaps rather than inventing stricter behavior: no visibility
// scoping on presence_of (issue #87), and update_presence is an identity
// publishing its own status, not an integrator-authority publish (no
// GameCredential/GameBinding capability-grant system exists yet).

using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Net.WebSockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Channels;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    [JsonConverter(typeof(JsonStringEnumConverter))]
    public enum PresenceStatus
    {
        Online,
        Away,
        DoNotDisturb,
        Offline,
    }

    public sealed class Presence
    {
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        [JsonPropertyName("status")]
        public PresenceStatus Status { get; set; }

        /// <summary>The game the user is currently in, if any and if shared.</summary>
        [JsonPropertyName("playing")]
        public Guid? Playing { get; set; }

        [JsonPropertyName("updated_at")]
        public DateTimeOffset UpdatedAt { get; set; }
    }

    /// <summary>
    /// A friend, from this integrator's point of view. DisplayName is always null today —
    /// GET /friends returns only the two identity ids and the friendship's since timestamp,
    /// no profile-lookup endpoint exists yet to resolve it (matches Friend's Rust doc comment).
    /// </summary>
    public sealed class Friend
    {
        public Friend(Guid identityId, string? displayName, Presence? presence)
        {
            IdentityId = identityId;
            DisplayName = displayName;
            Presence = presence;
        }

        public Guid IdentityId { get; }
        public string? DisplayName { get; }

        /// <summary>Populated only when presence.read is also granted alongside friends.read.</summary>
        public Presence? Presence { get; }
    }

    internal sealed class FriendshipResponse
    {
        [JsonPropertyName("a")]
        public Guid A { get; set; }

        [JsonPropertyName("b")]
        public Guid B { get; set; }

        [JsonPropertyName("since")]
        public DateTimeOffset Since { get; set; }
    }

    internal sealed class UpdatePresenceRequest
    {
        [JsonPropertyName("status")]
        public PresenceStatus Status { get; set; }
    }

    [JsonConverter(typeof(JsonStringEnumConverter))]
    internal enum PresenceSubscribeMessageType
    {
        Subscribe,
    }

    internal sealed class PresenceSubscribeMessage
    {
        [JsonPropertyName("type")]
        public string Type => "subscribe";

        [JsonPropertyName("ids")]
        public List<Guid> Ids { get; set; } = new List<Guid>();
    }

    public sealed partial class Session
    {
        private static readonly JsonSerializerOptions JsonOptions = new JsonSerializerOptions()
        {
            PropertyNameCaseInsensitive = true,
        };

        /// <summary>Builds a Friend view from a raw friendship plus whatever presence data is
        /// available for the other party — testable without any HTTP call.</summary>
        private static Friend MergeFriend(FriendshipResponse friendship, Guid selfId, IReadOnlyDictionary<Guid, Presence> presenceById)
        {
            var other = friendship.A == selfId ? friendship.B : friendship.A;
            presenceById.TryGetValue(other, out var presence);
            return new Friend(other, null, presence);
        }

        /// <summary>server_url is http(s)://…; the websocket endpoint needs ws(s)://….</summary>
        private static string WebSocketUrl(string serverUrl, string path)
        {
            if (serverUrl.StartsWith("https://", StringComparison.Ordinal))
            {
                return "wss://" + serverUrl.Substring("https://".Length) + path;
            }
            if (serverUrl.StartsWith("http://", StringComparison.Ordinal))
            {
                return "ws://" + serverUrl.Substring("http://".Length) + path;
            }
            return serverUrl + path;
        }

        /// <summary>GET /friends — requires friends.read. Also embeds each friend's Presence
        /// when presence.read is granted too, via one batched PresenceOfAsync call.</summary>
        public async Task<IReadOnlyList<Friend>> FriendsAsync(CancellationToken ct = default)
        {
            Require("friends.read");

            using var request = new HttpRequestMessage(HttpMethod.Get, $"{ServerUrl}/friends");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", Token);
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            var friendships = await ReadJsonAsync<List<FriendshipResponse>>(response, ct).ConfigureAwait(false)
                ?? new List<FriendshipResponse>();

            var presenceById = new Dictionary<Guid, Presence>();
            if (HasCapability("presence.read") && friendships.Count > 0)
            {
                var otherIds = friendships.Select(f => f.A == IdentityGuid ? f.B : f.A).ToArray();
                foreach (var presence in await PresenceOfAsync(otherIds, ct).ConfigureAwait(false))
                {
                    presenceById[presence.IdentityId] = presence;
                }
            }

            return friendships.Select(f => MergeFriend(f, IdentityGuid, presenceById)).ToList();
        }

        /// <summary>The calling user's own presence, as the server currently has it.</summary>
        public async Task<Presence> PresenceAsync(CancellationToken ct = default)
        {
            Require("presence.read");
            var mine = await PresenceOfAsync(new[] { IdentityGuid }, ct).ConfigureAwait(false);
            // GET /presence?ids=<one id> always returns exactly one entry — the store always
            // answers for any id (missing/stale reads as Offline).
            return mine[0];
        }

        /// <summary>GET /presence?ids=… for the given identities. No visibility filtering
        /// yet — see the module comment.</summary>
        public async Task<IReadOnlyList<Presence>> PresenceOfAsync(IReadOnlyList<Guid> ids, CancellationToken ct = default)
        {
            Require("presence.read");
            if (ids.Count == 0)
            {
                return Array.Empty<Presence>();
            }

            var idsParam = string.Join(",", ids);
            using var request = new HttpRequestMessage(HttpMethod.Get, $"{ServerUrl}/presence?ids={Uri.EscapeDataString(idsParam)}");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", Token);
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<List<Presence>>(response, ct).ConfigureAwait(false) ?? new List<Presence>();
        }

        /// <summary>PUT /me/presence — an identity publishing their own status. Not
        /// capability-gated: the server requires only a valid identity session for this.</summary>
        public async Task UpdatePresenceAsync(PresenceStatus status, CancellationToken ct = default)
        {
            using var request = new HttpRequestMessage(HttpMethod.Put, $"{ServerUrl}/me/presence");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", Token);
            request.Content = JsonContent(new UpdatePresenceRequest { Status = status });
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
        }

        /// <summary>
        /// Subscribes to live presence updates for the given ids (issue #136), additive to
        /// PresenceOfAsync's point-in-time reads. Connects to GET /ws/presence (auth via a
        /// ?token= query parameter — a websocket handshake can't carry a bearer header), sends
        /// one subscribe message, then forwards every pushed Presence into the returned
        /// channel reader on a background task. Disposing the returned CancellationTokenSource
        /// (or cancelling ct) ends the background task; there is no separate unsubscribe call.
        /// </summary>
        public async Task<ChannelReader<Presence>> SubscribePresenceAsync(IReadOnlyList<Guid> ids, CancellationToken ct = default)
        {
            Require("presence.read");

            var url = WebSocketUrl(ServerUrl, $"/ws/presence?token={Uri.EscapeDataString(Token)}");
            var socket = new ClientWebSocket();
            try
            {
                await socket.ConnectAsync(new Uri(url), ct).ConfigureAwait(false);
            }
            catch (Exception e)
            {
                socket.Dispose();
                throw new AvalonWebSocketException(e.Message);
            }

            var subscribe = JsonSerializer.Serialize(new PresenceSubscribeMessage { Ids = ids.ToList() });
            var subscribeBytes = Encoding.UTF8.GetBytes(subscribe);
            try
            {
                await socket.SendAsync(new ArraySegment<byte>(subscribeBytes), WebSocketMessageType.Text, true, ct)
                    .ConfigureAwait(false);
            }
            catch (Exception e)
            {
                socket.Dispose();
                throw new AvalonWebSocketException(e.Message);
            }

            var channel = Channel.CreateUnbounded<Presence>();
            _ = Task.Run(async () =>
            {
                var buffer = new byte[8192];
                try
                {
                    while (socket.State == WebSocketState.Open)
                    {
                        using var ms = new System.IO.MemoryStream();
                        WebSocketReceiveResult result;
                        do
                        {
                            result = await socket.ReceiveAsync(new ArraySegment<byte>(buffer), ct).ConfigureAwait(false);
                            if (result.MessageType == WebSocketMessageType.Close)
                            {
                                channel.Writer.TryComplete();
                                return;
                            }
                            ms.Write(buffer, 0, result.Count);
                        } while (!result.EndOfMessage);

                        Presence? presence;
                        try
                        {
                            presence = JsonSerializer.Deserialize<Presence>(ms.ToArray(), JsonOptions);
                        }
                        catch (JsonException)
                        {
                            continue;
                        }
                        if (presence == null)
                        {
                            continue;
                        }
                        if (!channel.Writer.TryWrite(presence))
                        {
                            break;
                        }
                    }
                }
                catch (OperationCanceledException)
                {
                    // ct cancelled — end the background task quietly, same as dropping the
                    // Rust receiver ending its forwarding task.
                }
                catch (Exception)
                {
                    // Any other socket failure just ends delivery; the caller observes this as
                    // the channel completing with nothing further.
                }
                finally
                {
                    channel.Writer.TryComplete();
                    socket.Dispose();
                }
            }, ct);

            return channel.Reader;
        }

        internal static async Task<T> ReadJsonAsync<T>(HttpResponseMessage response, CancellationToken ct) where T : class
        {
#if NET5_0_OR_GREATER
            var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
#else
            var stream = await response.Content.ReadAsStreamAsync().ConfigureAwait(false);
#endif
            var value = await JsonSerializer.DeserializeAsync<T>(stream, JsonOptions, ct).ConfigureAwait(false);
            return value ?? throw new System.Text.Json.JsonException("expected a JSON value, got null");
        }

        private static HttpContent JsonContent<T>(T value)
        {
            var json = JsonSerializer.Serialize(value);
            return new StringContent(json, Encoding.UTF8, "application/json");
        }
    }
}
