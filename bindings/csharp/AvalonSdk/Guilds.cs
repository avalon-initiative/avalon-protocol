// Guild membership, rosters, channels, chat, and events — capability-gated
// reads/writes on Session (issue #23), mirroring crates/sdk/src/guilds.rs.
//
// The SDK never lets an integrator act with guild authority — creating
// guilds, inviting, kicking, changing roles, and managing channels all stay
// identity-authority-only actions taken through the Hub, not exposed here.
//
// RosterAsync/ChannelsAsync/MessagesAsync apply no visibility scoping yet
// (issue #87) — this returns exactly what the server returns, matching the
// Rust SDK's own documented gap rather than inventing stricter behavior.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    [JsonConverter(typeof(JsonStringEnumConverter))]
    public enum JoinPolicy
    {
        InviteOnly,
        Open,
    }

    public sealed class GuildLink
    {
        [JsonPropertyName("label")]
        public string Label { get; set; } = "";

        [JsonPropertyName("url")]
        public string Url { get; set; } = "";
    }

    public sealed class Guild
    {
        public Guid Id { get; set; }
        public string Name { get; set; } = "";
        public string Tag { get; set; } = "";
        public string Description { get; set; } = "";
        public Guid Owner { get; set; }
        public DateTimeOffset CreatedAt { get; set; }
        public JoinPolicy JoinPolicy { get; set; }
        public string? Motd { get; set; }
        public string? Banner { get; set; }
        public string? Icon { get; set; }
        public List<GuildLink> Links { get; set; } = new List<GuildLink>();
        public bool Recruiting { get; set; }
    }

    public sealed class GuildRole
    {
        public GuildRole(Guid guildId, uint nameIndex)
        {
            GuildId = guildId;
            NameIndex = nameIndex;
        }

        public Guid GuildId { get; }
        public uint NameIndex { get; }
    }

    public sealed class GuildMember
    {
        public GuildMember(Guid guildId, Guid identityId, GuildRole role, DateTimeOffset joinedAt)
        {
            GuildId = guildId;
            IdentityId = identityId;
            Role = role;
            JoinedAt = joinedAt;
        }

        public Guid GuildId { get; }
        public Guid IdentityId { get; }
        public GuildRole Role { get; }
        public DateTimeOffset JoinedAt { get; }
    }

    public sealed class GuildChannel
    {
        public Guid Id { get; set; }
        public Guid GuildId { get; set; }
        public string Name { get; set; } = "";
        public bool AnnouncementOnly { get; set; }
        public string? Topic { get; set; }
        // Archived is intentionally dropped, not modeled — GuildChannel has nowhere to put
        // it, same posture the Rust SDK's ChannelResponse takes.
    }

    public sealed class GuildMessage
    {
        public Guid Id { get; set; }
        public Guid ChannelId { get; set; }
        public Guid Author { get; set; }
        public string Body { get; set; } = "";
        public DateTimeOffset SentAt { get; set; }
    }

    public sealed class GuildEvent
    {
        public Guid Id { get; set; }
        public Guid GuildId { get; set; }
        public Guid? ChannelId { get; set; }
        public string Title { get; set; } = "";
        public string? Description { get; set; }
        public DateTimeOffset StartsAt { get; set; }
        public DateTimeOffset? EndsAt { get; set; }
        public Guid CreatedBy { get; set; }
        public DateTimeOffset CreatedAt { get; set; }
        // RsvpCounts is intentionally dropped — GuildEvent has nowhere to put it, same
        // posture the Rust SDK's EventResponse takes.
    }

    /// <summary>The calling user's own membership in a guild.</summary>
    public sealed class GuildMembership
    {
        public GuildMembership(Guild guild, GuildRole role, DateTimeOffset joinedAt)
        {
            Guild = guild;
            Role = role;
            JoinedAt = joinedAt;
        }

        public Guild Guild { get; }
        public GuildRole Role { get; }
        public DateTimeOffset JoinedAt { get; }
    }

    /// <summary>A roster entry, from this integrator's point of view — mirrors the Rust SDK's
    /// GuildRosterMember: RosterAsync returns this instead of GuildMember directly since
    /// GuildMember has nowhere to put an embedded Presence.</summary>
    public sealed class GuildRosterMember
    {
        public GuildRosterMember(GuildMember member, Presence? presence)
        {
            Member = member;
            Presence = presence;
        }

        public GuildMember Member { get; }
        public Presence? Presence { get; }
    }

    internal sealed class GuildResponse
    {
        public Guid Id { get; set; }
        public string Name { get; set; } = "";
        public string Tag { get; set; } = "";
        public string Description { get; set; } = "";
        public Guid Owner { get; set; }
        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }
        [JsonPropertyName("join_policy")]
        public string JoinPolicy { get; set; } = "invite_only";
        public string? Motd { get; set; }
        public string? Banner { get; set; }
        public string? Icon { get; set; }
        public List<GuildLink> Links { get; set; } = new List<GuildLink>();
        public bool Recruiting { get; set; }

        public Guild ToGuild() => new Guild()
        {
            Id = Id,
            Name = Name,
            Tag = Tag,
            Description = Description,
            Owner = Owner,
            CreatedAt = CreatedAt,
            // Falls back to InviteOnly on an unparseable string, same posture the server's
            // own read path takes — never a hard failure on a read.
            JoinPolicy = JoinPolicy == "open" ? global::Avalon.Sdk.JoinPolicy.Open : global::Avalon.Sdk.JoinPolicy.InviteOnly,
            Motd = Motd,
            Banner = Banner,
            Icon = Icon,
            Links = Links,
            Recruiting = Recruiting,
        };
    }

    internal sealed class MyGuildMembershipResponse
    {
        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }
        [JsonPropertyName("role_index")]
        public int RoleIndex { get; set; }
        [JsonPropertyName("joined_at")]
        public DateTimeOffset JoinedAt { get; set; }
    }

    internal sealed class GuildMemberResponse
    {
        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }
        [JsonPropertyName("role_index")]
        public int RoleIndex { get; set; }
        [JsonPropertyName("joined_at")]
        public DateTimeOffset JoinedAt { get; set; }

        public GuildMember ToGuildMember() => new GuildMember(
            GuildId,
            IdentityId,
            // Clamped rather than throwing on a negative wire value — a server-side bug
            // should not crash an integrator's read.
            new GuildRole(GuildId, (uint)Math.Max(RoleIndex, 0)),
            JoinedAt);
    }

    internal sealed class ChannelResponse
    {
        public Guid Id { get; set; }
        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }
        public string Name { get; set; } = "";
        public bool Archived { get; set; }
        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }
        [JsonPropertyName("announcement_only")]
        public bool AnnouncementOnly { get; set; }
        public string? Topic { get; set; }

        public GuildChannel ToGuildChannel() => new GuildChannel()
        {
            Id = Id,
            GuildId = GuildId,
            Name = Name,
            AnnouncementOnly = AnnouncementOnly,
            Topic = Topic,
        };
    }

    internal sealed class GuildMessageResponse
    {
        public Guid Id { get; set; }
        [JsonPropertyName("channel_id")]
        public Guid ChannelId { get; set; }
        public Guid Author { get; set; }
        public string Body { get; set; } = "";
        [JsonPropertyName("sent_at")]
        public DateTimeOffset SentAt { get; set; }

        public GuildMessage ToGuildMessage() => new GuildMessage()
        {
            Id = Id,
            ChannelId = ChannelId,
            Author = Author,
            Body = Body,
            SentAt = SentAt,
        };
    }

    internal sealed class SendMessageRequest
    {
        [JsonPropertyName("body")]
        public string Body { get; set; } = "";
    }

    internal sealed class EventResponse
    {
        public Guid Id { get; set; }
        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }
        [JsonPropertyName("channel_id")]
        public Guid? ChannelId { get; set; }
        public string Title { get; set; } = "";
        public string? Description { get; set; }
        [JsonPropertyName("starts_at")]
        public DateTimeOffset StartsAt { get; set; }
        [JsonPropertyName("ends_at")]
        public DateTimeOffset? EndsAt { get; set; }
        [JsonPropertyName("created_by")]
        public Guid CreatedBy { get; set; }
        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }

        public GuildEvent ToGuildEvent() => new GuildEvent()
        {
            Id = Id,
            GuildId = GuildId,
            ChannelId = ChannelId,
            Title = Title,
            Description = Description,
            StartsAt = StartsAt,
            EndsAt = EndsAt,
            CreatedBy = CreatedBy,
            CreatedAt = CreatedAt,
        };
    }

    public sealed partial class Session
    {
        internal static GuildRosterMember MergeRosterMember(GuildMember member, IReadOnlyDictionary<Guid, Presence> presenceById)
        {
            presenceById.TryGetValue(member.IdentityId, out var presence);
            return new GuildRosterMember(member, presence);
        }

        /// <summary>GET /me/guilds — requires guilds.read. Each membership's full Guild is
        /// fetched with one follow-up GET /guilds/{id} per membership.</summary>
        public async Task<IReadOnlyList<GuildMembership>> GuildsAsync(CancellationToken ct = default)
        {
            Require("guilds.read");

            var memberships = await GetJsonAsync<List<MyGuildMembershipResponse>>($"{ServerUrl}/me/guilds", ct).ConfigureAwait(false)
                ?? new List<MyGuildMembershipResponse>();

            var result = new List<GuildMembership>(memberships.Count);
            foreach (var membership in memberships)
            {
                var guild = await FetchGuildAsync(membership.GuildId, ct).ConfigureAwait(false);
                result.Add(new GuildMembership(
                    guild,
                    new GuildRole(guild.Id, (uint)Math.Max(membership.RoleIndex, 0)),
                    membership.JoinedAt));
            }
            return result;
        }

        /// <summary>GET /guilds/{id}. Not capability-gated on its own — only ever called
        /// internally by callers that already checked their own capability.</summary>
        private async Task<Guild> FetchGuildAsync(Guid id, CancellationToken ct)
        {
            var response = await GetJsonAsync<GuildResponse>($"{ServerUrl}/guilds/{id}", ct).ConfigureAwait(false);
            return response.ToGuild();
        }

        /// <summary>A handle scoped to one guild. Not capability-gated itself — the methods
        /// called through it check their own capability.</summary>
        public GuildHandle Guild(Guid id) => new GuildHandle(this, id);

        internal async Task<T> GetJsonAsync<T>(string url, CancellationToken ct) where T : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, url);
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", Token);
            using var response = await Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw ServerError(response.StatusCode);
            }
            return await ReadJsonAsync<T>(response, ct).ConfigureAwait(false);
        }
    }

    /// <summary>See Session.Guild.</summary>
    public sealed class GuildHandle
    {
        private readonly Session _session;
        private readonly Guid _guildId;

        internal GuildHandle(Session session, Guid guildId)
        {
            _session = session;
            _guildId = guildId;
        }

        /// <summary>GET /guilds/{id}/members — requires guilds.read. Full roster, no
        /// visibility scoping (issue #87). Embeds each member's Presence when presence.read
        /// is granted too, via one batched PresenceOfAsync call.</summary>
        public async Task<IReadOnlyList<GuildRosterMember>> RosterAsync(CancellationToken ct = default)
        {
            _session.Require("guilds.read");

            var members = (await _session.GetJsonAsync<List<GuildMemberResponse>>(
                $"{_session.ServerUrl}/guilds/{_guildId}/members", ct).ConfigureAwait(false)
                ?? new List<GuildMemberResponse>())
                .Select(m => m.ToGuildMember())
                .ToList();

            var presenceById = new Dictionary<Guid, Presence>();
            if (_session.HasCapability("presence.read") && members.Count > 0)
            {
                var ids = members.Select(m => m.IdentityId).ToArray();
                foreach (var presence in await _session.PresenceOfAsync(ids, ct).ConfigureAwait(false))
                {
                    presenceById[presence.IdentityId] = presence;
                }
            }

            return members.Select(m => Session.MergeRosterMember(m, presenceById)).ToList();
        }

        /// <summary>GET /guilds/{id}/channels — requires guilds.chat. Lists both active and
        /// archived channels (archived is dropped, not modeled — see GuildChannel).</summary>
        public async Task<IReadOnlyList<GuildChannel>> ChannelsAsync(CancellationToken ct = default)
        {
            _session.Require("guilds.chat");
            var channels = await _session.GetJsonAsync<List<ChannelResponse>>(
                $"{_session.ServerUrl}/guilds/{_guildId}/channels", ct).ConfigureAwait(false)
                ?? new List<ChannelResponse>();
            return channels.Select(c => c.ToGuildChannel()).ToList();
        }

        /// <summary>GET /guilds/{id}/events — requires guilds.read. Lists all scheduled
        /// events, unfiltered (the server also accepts from/to range params, not exposed here
        /// yet, matching the Rust SDK's read-only scope).</summary>
        public async Task<IReadOnlyList<GuildEvent>> EventsAsync(CancellationToken ct = default)
        {
            _session.Require("guilds.read");
            var events = await _session.GetJsonAsync<List<EventResponse>>(
                $"{_session.ServerUrl}/guilds/{_guildId}/events", ct).ConfigureAwait(false)
                ?? new List<EventResponse>();
            return events.Select(e => e.ToGuildEvent()).ToList();
        }

        /// <summary>A handle scoped to one channel within this guild.</summary>
        public ChannelHandle Channel(Guid id) => new ChannelHandle(_session, _guildId, id);
    }

    /// <summary>See GuildHandle.Channel.</summary>
    public sealed class ChannelHandle
    {
        private readonly Session _session;
        private readonly Guid _guildId;
        private readonly Guid _channelId;

        internal ChannelHandle(Session session, Guid guildId, Guid channelId)
        {
            _session = session;
            _guildId = guildId;
            _channelId = channelId;
        }

        /// <summary>GET /guilds/{id}/channels/{cid}/messages?before=&amp;limit= — requires
        /// guilds.chat. Newest first, cursor-paginated exactly as the server paginates it.</summary>
        public async Task<IReadOnlyList<GuildMessage>> MessagesAsync(Guid? before = null, int? limit = null, CancellationToken ct = default)
        {
            _session.Require("guilds.chat");

            var url = $"{_session.ServerUrl}/guilds/{_guildId}/channels/{_channelId}/messages"
                + BuildQuery(before, limit);
            var messages = await _session.GetJsonAsync<List<GuildMessageResponse>>(url, ct).ConfigureAwait(false)
                ?? new List<GuildMessageResponse>();
            return messages.Select(m => m.ToGuildMessage()).ToList();
        }

        /// <summary>POST /guilds/{id}/channels/{cid}/messages — requires guilds.chat. Posts
        /// as the identity under their own session token; there is no path for an integrator
        /// to post as itself.</summary>
        public async Task<GuildMessage> SendAsync(string body, CancellationToken ct = default)
        {
            _session.Require("guilds.chat");

            using var request = new HttpRequestMessage(HttpMethod.Post,
                $"{_session.ServerUrl}/guilds/{_guildId}/channels/{_channelId}/messages");
            request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", _session.Token);
            request.Content = new StringContent(
                JsonSerializer.Serialize(new SendMessageRequest { Body = body }), Encoding.UTF8, "application/json");
            using var response = await _session.Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            var message = await Session.ReadJsonAsync<GuildMessageResponse>(response, ct).ConfigureAwait(false);
            return message!.ToGuildMessage();
        }

        private static string BuildQuery(Guid? before, int? limit)
        {
            var parts = new List<string>();
            if (before.HasValue) parts.Add($"before={before.Value}");
            if (limit.HasValue) parts.Add($"limit={limit.Value}");
            return parts.Count == 0 ? "" : "?" + string.Join("&", parts);
        }
    }
}
