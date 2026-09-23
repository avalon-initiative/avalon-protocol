// Friends, blocks, presence, and discovery on AccountSession
// — the first-party counterpart to Social.cs's capability-gated integrator methods. Mirrors
// crates/sdk/src/account/social.rs. Presence/PresenceStatus are reused directly from
// Social.cs — same wire shape on both the integrator and account surfaces.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>A confirmed friendship — <see cref="A"/>/<see cref="B"/> are the two
    /// identities, in no particular order. Mirrors the Rust SDK's
    /// <c>account::social::Friendship</c>.</summary>
    public sealed class AccountFriendship
    {
        [JsonPropertyName("a")]
        public Guid A { get; set; }

        [JsonPropertyName("b")]
        public Guid B { get; set; }

        [JsonPropertyName("since")]
        public DateTimeOffset Since { get; set; }
    }

    /// <summary>A pending friend request. Mirrors the Rust SDK's
    /// <c>account::social::FriendRequest</c>.</summary>
    public sealed class AccountFriendRequest
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("from")]
        public Guid From { get; set; }

        [JsonPropertyName("to")]
        public Guid To { get; set; }

        [JsonPropertyName("requested_at")]
        public DateTimeOffset RequestedAt { get; set; }
    }

    /// <summary>One outgoing block — there is no endpoint anywhere that reveals who has
    /// blocked *you*. Mirrors the Rust SDK's <c>account::social::Block</c>.</summary>
    public sealed class AccountBlock
    {
        [JsonPropertyName("blocked")]
        public Guid Blocked { get; set; }

        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }
    }

    /// <summary>A public-face profile — the least-sensitive batch-resolvable fields only
    /// (<c>GET /identities/profiles</c>). Mirrors the Rust SDK's
    /// <c>account::social::PublicProfile</c>.</summary>
    public sealed class PublicProfile
    {
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        [JsonPropertyName("display_name")]
        public string DisplayName { get; set; } = "";

        [JsonPropertyName("avatar_url")]
        public string? AvatarUrl { get; set; }
    }

    /// <summary>A "people you may know" candidate (<c>GET /people/discover</c>). Mirrors the
    /// Rust SDK's <c>account::social::DiscoveryCandidate</c>.
    ///
    /// Issue #725: this previously also declared <c>display_name</c>/<c>avatar_url</c>/
    /// <c>mutual_friends</c>/<c>mutual_guilds</c> fields the server has never actually sent —
    /// <c>crates/server/src/discovery.rs</c>'s own <c>DiscoveryCandidate</c> has only ever had
    /// <c>identity_id</c>. Found migrating onto <c>Avalon.Sdk.Generated.DiscoveryCandidate</c>,
    /// which is exactly the real server response shape (the Rust SDK found and fixed the same
    /// bug the same way).</summary>
    public sealed class DiscoveryCandidate
    {
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }
    }

    /// <summary>A single global search result (<c>GET /identities/search</c>). Mirrors the
    /// Rust SDK's <c>account::social::SearchResultIdentity</c>.</summary>
    public sealed class SearchResultIdentity
    {
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        [JsonPropertyName("display_name")]
        public string DisplayName { get; set; } = "";

        [JsonPropertyName("avatar_url")]
        public string? AvatarUrl { get; set; }
    }

    /// <summary>One entry of the caller's own recent protocol history (<c>GET
    /// /me/history</c>). Mirrors the Rust SDK's <c>account::social::HistoryEntry</c>.</summary>
    public sealed class HistoryEntry
    {
        [JsonPropertyName("event_id")]
        public Guid EventId { get; set; }

        [JsonPropertyName("kind")]
        public string Kind { get; set; } = "";

        [JsonPropertyName("subject")]
        public string Subject { get; set; } = "";

        [JsonPropertyName("payload")]
        public JsonElement? Payload { get; set; }

        [JsonPropertyName("timestamp")]
        public DateTimeOffset Timestamp { get; set; }
    }

    /// <summary>Another identity's full self-description profile (<c>GET
    /// /identities/{id}/profile</c>) — same fields <c>GET /me</c> exposes for the
    /// caller's own profile. Mirrors the Rust SDK's
    /// <c>account::social::PublicIdentityProfile</c>.</summary>
    public sealed class PublicIdentityProfile
    {
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        [JsonPropertyName("identity_created_at")]
        public DateTimeOffset IdentityCreatedAt { get; set; }

        [JsonPropertyName("display_name")]
        public string DisplayName { get; set; } = "";

        [JsonPropertyName("avatar_url")]
        public string? AvatarUrl { get; set; }

        [JsonPropertyName("bio")]
        public string? Bio { get; set; }

        [JsonPropertyName("favorite_genres")]
        public List<string> FavoriteGenres { get; set; } = new List<string>();

        [JsonPropertyName("pronouns")]
        public string? Pronouns { get; set; }

        [JsonPropertyName("banner_url")]
        public string? BannerUrl { get; set; }

        [JsonPropertyName("status")]
        public string? Status { get; set; }

        [JsonPropertyName("links")]
        public List<string> Links { get; set; } = new List<string>();

        [JsonPropertyName("timezone")]
        public string? Timezone { get; set; }

        [JsonPropertyName("theme_color")]
        public string? ThemeColor { get; set; }

        [JsonPropertyName("location")]
        public string? Location { get; set; }
    }

    /// <summary>A recent announcement-only channel post (<c>GET
    /// /me/guild-announcements</c>). Mirrors the Rust SDK's
    /// <c>account::social::GuildAnnouncementAlert</c>.</summary>
    public sealed class GuildAnnouncementAlert
    {
        [JsonPropertyName("message_id")]
        public Guid MessageId { get; set; }

        [JsonPropertyName("channel_id")]
        public Guid ChannelId { get; set; }

        [JsonPropertyName("channel_name")]
        public string ChannelName { get; set; } = "";

        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("author")]
        public Guid Author { get; set; }

        [JsonPropertyName("body")]
        public string Body { get; set; } = "";

        [JsonPropertyName("sent_at")]
        public DateTimeOffset SentAt { get; set; }
    }

    public sealed partial class AccountSession
    {
        /// <summary>Generated.cs's own <see cref="Avalon.Sdk.Generated.PresenceStatus"/> and
        /// this SDK's public <see cref="PresenceStatus"/> are deliberately two separate enum
        /// types with identical member names, mirroring the <c>ToDomainGenre</c> pattern in
        /// this file's own <c>AccountSession.cs</c> — a name round-trip via
        /// <see cref="Enum.Parse"/> rather than a fragile numeric cast.</summary>
        private static PresenceStatus ToDomainPresenceStatus(Avalon.Sdk.Generated.PresenceStatus generated) =>
            (PresenceStatus)Enum.Parse(typeof(PresenceStatus), generated.ToString());

        private static Avalon.Sdk.Generated.PresenceStatus ToGeneratedPresenceStatus(PresenceStatus status) =>
            (Avalon.Sdk.Generated.PresenceStatus)Enum.Parse(typeof(Avalon.Sdk.Generated.PresenceStatus), status.ToString());

        private static Presence ToDomainPresence(Avalon.Sdk.Generated.PresenceResponse p) => new Presence
        {
            IdentityId = p.IdentityId,
            Status = ToDomainPresenceStatus(p.Status),
            ActiveIn = p.ActiveIn,
            UpdatedAt = p.UpdatedAt,
        };

        private static DiscoveryCandidate ToDomainDiscoveryCandidate(Avalon.Sdk.Generated.DiscoveryCandidate c) => new DiscoveryCandidate
        {
            IdentityId = c.IdentityId,
        };

        private static SearchResultIdentity ToDomainSearchResultIdentity(Avalon.Sdk.Generated.SearchResultIdentity r) => new SearchResultIdentity
        {
            IdentityId = r.IdentityId,
            DisplayName = r.DisplayName,
            AvatarUrl = r.AvatarUrl,
        };

        /// <summary><c>GET /friends</c>.</summary>
        public async Task<IReadOnlyList<AccountFriendship>> FriendsAsync(CancellationToken ct = default) =>
            await GetAsync<List<AccountFriendship>>("/friends", ct).ConfigureAwait(false);

        /// <summary><c>GET /friends/requests</c> — both incoming and outgoing.</summary>
        public async Task<IReadOnlyList<AccountFriendRequest>> FriendRequestsAsync(CancellationToken ct = default) =>
            await GetAsync<List<AccountFriendRequest>>("/friends/requests", ct).ConfigureAwait(false);

        /// <summary><c>POST /friends/requests</c>.</summary>
        public async Task<AccountFriendRequest> CreateFriendRequestAsync(Guid to, CancellationToken ct = default) =>
            await PostAsync<Avalon.Sdk.Generated.CreateFriendRequestRequest, AccountFriendRequest>(
                "/friends/requests", new Avalon.Sdk.Generated.CreateFriendRequestRequest { To = to }, ct).ConfigureAwait(false);

        /// <summary><c>POST /friends/requests/{id}/accept</c>.</summary>
        public async Task<AccountFriendship> AcceptFriendRequestAsync(Guid requestId, CancellationToken ct = default) =>
            await PostEmptyAsync<AccountFriendship>($"/friends/requests/{requestId}/accept", ct).ConfigureAwait(false);

        /// <summary><c>DELETE /friends/requests/{id}</c> — declines an incoming request or
        /// withdraws an outgoing one (the server infers which).</summary>
        public async Task DeclineOrWithdrawFriendRequestAsync(Guid requestId, CancellationToken ct = default) =>
            await DeleteAsync($"/friends/requests/{requestId}", ct).ConfigureAwait(false);

        /// <summary><c>DELETE /friends/{identity_id}</c>.</summary>
        public async Task RemoveFriendAsync(Guid identityId, CancellationToken ct = default) =>
            await DeleteAsync($"/friends/{identityId}", ct).ConfigureAwait(false);

        /// <summary><c>GET /friends/handle/{handle}</c> — exact-match handle resolution for
        /// the "add friend" flow.</summary>
        public async Task<Guid> ResolveHandleAsync(string handle, CancellationToken ct = default)
        {
            var response = await GetAsync<Avalon.Sdk.Generated.ResolveHandleResponse>($"/friends/handle/{Uri.EscapeDataString(handle)}", ct).ConfigureAwait(false);
            return response.IdentityId;
        }

        /// <summary><c>GET /blocks</c> — the caller's own outgoing blocks only.</summary>
        public async Task<IReadOnlyList<AccountBlock>> BlocksAsync(CancellationToken ct = default) =>
            await GetAsync<List<AccountBlock>>("/blocks", ct).ConfigureAwait(false);

        /// <summary><c>POST /blocks</c>.</summary>
        public async Task<AccountBlock> BlockAsync(Guid identityId, CancellationToken ct = default) =>
            await PostAsync<Avalon.Sdk.Generated.CreateBlockRequest, AccountBlock>(
                "/blocks", new Avalon.Sdk.Generated.CreateBlockRequest { IdentityId = identityId }, ct).ConfigureAwait(false);

        /// <summary><c>DELETE /blocks/{identity_id}</c>.</summary>
        public async Task UnblockAsync(Guid identityId, CancellationToken ct = default) =>
            await DeleteAsync($"/blocks/{identityId}", ct).ConfigureAwait(false);

        /// <summary><c>GET /people/discover</c> — no query parameters; the caller's own
        /// session is the only input.</summary>
        public async Task<IReadOnlyList<DiscoveryCandidate>> DiscoverPeopleAsync(CancellationToken ct = default)
        {
            var response = await GetAsync<Avalon.Sdk.Generated.DiscoverPeopleResponse>("/people/discover", ct).ConfigureAwait(false);
            return response.Candidates.Select(ToDomainDiscoveryCandidate).ToList();
        }

        /// <summary><c>GET /identities/search?q=</c> — only matches identities that opted
        /// into <c>discoverable</c>. An empty/blank <paramref name="q"/> returns no results
        /// without a round trip.</summary>
        public async Task<IReadOnlyList<SearchResultIdentity>> SearchIdentitiesAsync(string q, CancellationToken ct = default)
        {
            if (string.IsNullOrWhiteSpace(q))
            {
                return Array.Empty<SearchResultIdentity>();
            }
            var response = await GetQueryAsync<Avalon.Sdk.Generated.SearchIdentitiesResponse>("/identities/search", new[] { ("q", q) }, ct).ConfigureAwait(false);
            return response.Results.Select(ToDomainSearchResultIdentity).ToList();
        }

        /// <summary><c>GET /identities/profiles?ids=</c> — batched, public-fields-only.</summary>
        public async Task<IReadOnlyList<PublicProfile>> ProfilesAsync(IReadOnlyList<Guid> ids, CancellationToken ct = default)
        {
            if (ids.Count == 0)
            {
                return Array.Empty<PublicProfile>();
            }
            var joined = string.Join(",", ids);
            return await GetQueryAsync<List<PublicProfile>>("/identities/profiles", new[] { ("ids", joined) }, ct).ConfigureAwait(false);
        }

        /// <summary><c>GET /me/history</c>.</summary>
        public async Task<IReadOnlyList<HistoryEntry>> HistoryAsync(CancellationToken ct = default) =>
            await GetAsync<List<HistoryEntry>>("/me/history", ct).ConfigureAwait(false);

        /// <summary><c>GET /identities/{id}/profile</c> — another identity's full
        /// self-description profile, same exposure level as <c>GET /me</c>.</summary>
        public async Task<PublicIdentityProfile> IdentityProfileAsync(Guid identityId, CancellationToken ct = default) =>
            await GetAsync<PublicIdentityProfile>($"/identities/{identityId}/profile", ct).ConfigureAwait(false);

        /// <summary><c>GET /me/guild-announcements</c> — recent
        /// announcement-only channel posts across every guild the caller currently belongs
        /// to.</summary>
        public async Task<IReadOnlyList<GuildAnnouncementAlert>> GuildAnnouncementsAsync(CancellationToken ct = default) =>
            await GetAsync<List<GuildAnnouncementAlert>>("/me/guild-announcements", ct).ConfigureAwait(false);

        /// <summary><c>PUT /me/presence</c> — publishes the caller's own status. Not
        /// signature-required (high-frequency, self-correcting).</summary>
        public async Task<Presence> UpdatePresenceAsync(PresenceStatus status, bool? hideActiveIn = null, CancellationToken ct = default)
        {
            var response = await PutAsync<Avalon.Sdk.Generated.UpdatePresenceRequest, Avalon.Sdk.Generated.PresenceResponse>(
                "/me/presence", new Avalon.Sdk.Generated.UpdatePresenceRequest { Status = ToGeneratedPresenceStatus(status), HideActiveIn = hideActiveIn }, ct).ConfigureAwait(false);
            return ToDomainPresence(response);
        }

        /// <summary><c>GET /presence?ids=</c> — no visibility filtering server-side (#87
        /// tracks adding it); returns exactly what the server returns.</summary>
        public async Task<IReadOnlyList<Presence>> PresenceOfAsync(IReadOnlyList<Guid> ids, CancellationToken ct = default)
        {
            if (ids.Count == 0)
            {
                return Array.Empty<Presence>();
            }
            var joined = string.Join(",", ids);
            var response = await GetQueryAsync<List<Avalon.Sdk.Generated.PresenceResponse>>("/presence", new[] { ("ids", joined) }, ct).ConfigureAwait(false);
            return response.Select(ToDomainPresence).ToList();
        }
    }
}
