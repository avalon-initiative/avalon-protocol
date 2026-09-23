// Guild membership, rosters, channels, chat, and events — capability-gated
// reads/writes on Session, mirroring crates/sdk/src/guilds.rs.
//
// The SDK never lets an integrator act with guild authority — creating
// guilds, inviting, kicking, changing roles, and managing channels all stay
// identity-authority-only actions taken through the Hub, not exposed here.
//
// RosterAsync/ChannelsAsync/MessagesAsync apply no visibility scoping yet
// this returns exactly what the server returns, matching the
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

    public sealed partial class Session
    {
        internal static Guild GuildResponseToGuild(Avalon.Sdk.Generated.GuildResponse response) => new Guild()
        {
            Id = response.Id,
            Name = response.Name,
            Tag = response.Tag,
            Description = response.Description,
            Owner = response.Owner,
            CreatedAt = response.CreatedAt,
            // Falls back to InviteOnly on an unparseable string, same posture the server's
            // own read path takes — never a hard failure on a read.
            JoinPolicy = response.JoinPolicy == "open" ? global::Avalon.Sdk.JoinPolicy.Open : global::Avalon.Sdk.JoinPolicy.InviteOnly,
            Motd = response.Motd,
            Banner = response.Banner,
            Icon = response.Icon,
            Links = response.Links.Select(l => new GuildLink { Label = l.Label, Url = l.Url }).ToList(),
            Recruiting = response.Recruiting,
        };

        internal static GuildMember GuildMemberResponseToGuildMember(Avalon.Sdk.Generated.GuildMemberResponse response) => new GuildMember(
            response.GuildId,
            response.IdentityId,
            // Clamped rather than throwing on a negative wire value — a server-side bug
            // should not crash an integrator's read.
            new GuildRole(response.GuildId, (uint)Math.Max(response.RoleIndex, 0)),
            response.JoinedAt);

        internal static GuildChannel ChannelResponseToGuildChannel(Avalon.Sdk.Generated.ChannelResponse response) => new GuildChannel()
        {
            Id = response.Id,
            GuildId = response.GuildId,
            Name = response.Name,
            AnnouncementOnly = response.AnnouncementOnly,
            Topic = response.Topic,
        };

        internal static GuildMessage MessageResponseToGuildMessage(Avalon.Sdk.Generated.MessageResponse response) => new GuildMessage()
        {
            Id = response.Id,
            ChannelId = response.ChannelId,
            Author = response.Author,
            Body = response.Body,
            SentAt = response.SentAt,
        };

        internal static GuildEvent EventResponseToGuildEvent(Avalon.Sdk.Generated.EventResponse response) => new GuildEvent()
        {
            Id = response.Id,
            GuildId = response.GuildId,
            ChannelId = response.ChannelId,
            Title = response.Title,
            Description = response.Description,
            StartsAt = response.StartsAt,
            EndsAt = response.EndsAt,
            CreatedBy = response.CreatedBy,
            CreatedAt = response.CreatedAt,
        };

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

            var memberships = await GetJsonAsync<List<Avalon.Sdk.Generated.MyGuildMembershipResponse>>($"{ServerUrl}/me/guilds", ct).ConfigureAwait(false)
                ?? new List<Avalon.Sdk.Generated.MyGuildMembershipResponse>();

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
            var response = await GetJsonAsync<Avalon.Sdk.Generated.GuildResponse>($"{ServerUrl}/guilds/{id}", ct).ConfigureAwait(false);
            return GuildResponseToGuild(response);
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
        /// visibility scoping. Embeds each member's Presence when presence.read
        /// is granted too, via one batched PresenceOfAsync call.</summary>
        public async Task<IReadOnlyList<GuildRosterMember>> RosterAsync(CancellationToken ct = default)
        {
            _session.Require("guilds.read");

            var members = (await _session.GetJsonAsync<List<Avalon.Sdk.Generated.GuildMemberResponse>>(
                $"{_session.ServerUrl}/guilds/{_guildId}/members", ct).ConfigureAwait(false)
                ?? new List<Avalon.Sdk.Generated.GuildMemberResponse>())
                .Select(Session.GuildMemberResponseToGuildMember)
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
            var channels = await _session.GetJsonAsync<List<Avalon.Sdk.Generated.ChannelResponse>>(
                $"{_session.ServerUrl}/guilds/{_guildId}/channels", ct).ConfigureAwait(false)
                ?? new List<Avalon.Sdk.Generated.ChannelResponse>();
            return channels.Select(Session.ChannelResponseToGuildChannel).ToList();
        }

        /// <summary>GET /guilds/{id}/events — requires guilds.read. Lists all scheduled
        /// events, unfiltered (the server also accepts from/to range params, not exposed here
        /// yet, matching the Rust SDK's read-only scope).</summary>
        public async Task<IReadOnlyList<GuildEvent>> EventsAsync(CancellationToken ct = default)
        {
            _session.Require("guilds.read");
            var events = await _session.GetJsonAsync<List<Avalon.Sdk.Generated.EventResponse>>(
                $"{_session.ServerUrl}/guilds/{_guildId}/events", ct).ConfigureAwait(false)
                ?? new List<Avalon.Sdk.Generated.EventResponse>();
            return events.Select(Session.EventResponseToGuildEvent).ToList();
        }

        /// <summary>GET /guilds/{id}/integrator-breakdown — requires guilds.read. How many
        /// current members are actively bound to each integrator the guild plays, most-played
        /// first (ties broken alphabetically). A display of real counts, never a
        /// system verdict — no minimum-member threshold, no fixed cap.</summary>
        public async Task<Avalon.Sdk.Generated.GameBreakdownResponse> GameBreakdownAsync(CancellationToken ct = default)
        {
            _session.Require("guilds.read");
            return await _session.GetJsonAsync<Avalon.Sdk.Generated.GameBreakdownResponse>(
                $"{_session.ServerUrl}/guilds/{_guildId}/integrator-breakdown", ct).ConfigureAwait(false);
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
            var messages = await _session.GetJsonAsync<List<Avalon.Sdk.Generated.MessageResponse>>(url, ct).ConfigureAwait(false)
                ?? new List<Avalon.Sdk.Generated.MessageResponse>();
            return messages.Select(Session.MessageResponseToGuildMessage).ToList();
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
                JsonSerializer.Serialize(new Avalon.Sdk.Generated.SendMessageRequest { Body = body }), Encoding.UTF8, "application/json");
            using var response = await _session.Http.SendAsync(request, ct).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw Session.ServerError(response.StatusCode);
            }
            var message = await Session.ReadJsonAsync<Avalon.Sdk.Generated.MessageResponse>(response, ct).ConfigureAwait(false);
            return Session.MessageResponseToGuildMessage(message!);
        }

        /// <summary>GET /guilds/{id}/channels/{cid}/messages/archive — requires guilds.chat.
        /// Same cursor-paginated, newest-first shape as <see cref="MessagesAsync"/>, but reads
        /// from the separate archive table channel archival moves messages into — this does
        /// not try to reconstruct membership as of when each archived message was originally
        /// sent.</summary>
        public async Task<IReadOnlyList<Avalon.Sdk.Generated.ArchivedMessageResponse>> ArchiveAsync(Guid? before = null, int? limit = null, CancellationToken ct = default)
        {
            _session.Require("guilds.chat");

            return await _session.GetJsonAsync<List<Avalon.Sdk.Generated.ArchivedMessageResponse>>(
                $"{_session.ServerUrl}/guilds/{_guildId}/channels/{_channelId}/messages/archive{BuildQuery(before, limit)}", ct).ConfigureAwait(false)
                ?? new List<Avalon.Sdk.Generated.ArchivedMessageResponse>();
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
