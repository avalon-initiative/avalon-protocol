// Full guild administration (issues #20/#21/#22/#152/#153/#169/#242/#250/#442, on top of
// #23's read/roster surface Guilds.cs already covers for the integrator Session) on
// AccountSession — creation, roles, per-resource permission overrides, ownership transfer,
// membership, invites, join requests, channels, chat, and events, as an identity acting with
// its own authority rather than through any integrator grant. Mirrors
// crates/sdk/src/account/guild_admin.rs. Every type here is prefixed Account (or otherwise
// renamed) to avoid colliding with Guilds.cs's differently-shaped integrator-facing types of
// the same base name (Guild, GuildMember, GuildChannel, GuildMessage, GuildEvent).

using System;
using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    /// <summary>A guild, as returned by every guild-admin endpoint that hands back the full
    /// record. Mirrors the Rust SDK's <c>account::guild_admin::Guild</c>.</summary>
    public sealed class AccountGuild
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("name")]
        public string Name { get; set; } = "";

        [JsonPropertyName("tag")]
        public string Tag { get; set; } = "";

        [JsonPropertyName("description")]
        public string Description { get; set; } = "";

        [JsonPropertyName("owner")]
        public Guid Owner { get; set; }

        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }

        /// <summary>"open", "invite_only", or "application".</summary>
        [JsonPropertyName("join_policy")]
        public string JoinPolicy { get; set; } = "";

        [JsonPropertyName("motd")]
        public string? Motd { get; set; }

        [JsonPropertyName("banner")]
        public string? Banner { get; set; }

        [JsonPropertyName("icon")]
        public string? Icon { get; set; }

        [JsonPropertyName("links")]
        public List<GuildLink> Links { get; set; } = new List<GuildLink>();

        [JsonPropertyName("recruiting")]
        public bool Recruiting { get; set; }

        [JsonPropertyName("public")]
        public bool Public { get; set; }
    }

    /// <summary>One guild in a discovery-board listing (<c>GET /guilds/discover</c>, issue
    /// #154) — a narrower public summary than <see cref="AccountGuild"/>. Mirrors the Rust
    /// SDK's <c>account::guild_admin::DiscoverGuildSummary</c>.</summary>
    public sealed class DiscoverGuildSummary
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("name")]
        public string Name { get; set; } = "";

        [JsonPropertyName("tag")]
        public string Tag { get; set; } = "";

        [JsonPropertyName("description")]
        public string Description { get; set; } = "";

        [JsonPropertyName("recruiting")]
        public bool Recruiting { get; set; }

        [JsonPropertyName("member_count")]
        public long MemberCount { get; set; }

        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }

        [JsonPropertyName("banner")]
        public string? Banner { get; set; }

        [JsonPropertyName("icon")]
        public string? Icon { get; set; }
    }

    /// <summary>One page of a discovery-board listing. Mirrors the Rust SDK's
    /// <c>account::guild_admin::DiscoverGuildsPage</c>.</summary>
    public sealed class DiscoverGuildsPage
    {
        [JsonPropertyName("guilds")]
        public List<DiscoverGuildSummary> Guilds { get; set; } = new List<DiscoverGuildSummary>();

        /// <summary>Non-null when another page exists — pass it back as <c>cursor=</c> in
        /// the next call's own query string.</summary>
        [JsonPropertyName("next_cursor")]
        public Guid? NextCursor { get; set; }
    }

    /// <summary>One curated favorite-integrator pin (issue #207, decision #160). Mirrors the
    /// Rust SDK's <c>account::guild_admin::FavoriteGameEntry</c>.</summary>
    public sealed class FavoriteGameEntry
    {
        [JsonPropertyName("integrator_id")]
        public Guid IntegratorId { get; set; }

        [JsonPropertyName("integrator_slug")]
        public string IntegratorSlug { get; set; } = "";

        [JsonPropertyName("integrator_name")]
        public string IntegratorName { get; set; } = "";

        /// <summary>0-indexed curated display order.</summary>
        [JsonPropertyName("position")]
        public short Position { get; set; }

        /// <summary><c>true</c> when this integrator no longer has any actively-bound guild
        /// member — a stale pin <c>manage_guild</c> may choose to unpin.</summary>
        [JsonPropertyName("stale")]
        public bool Stale { get; set; }
    }

    /// <summary>A guild's curated favorite-integrators list. Mirrors the Rust SDK's
    /// <c>account::guild_admin::FavoriteGames</c>.</summary>
    public sealed class FavoriteGames
    {
        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("favorites")]
        public List<FavoriteGameEntry> Favorites { get; set; } = new List<FavoriteGameEntry>();
    }

    /// <summary>A guild role definition. Mirrors the Rust SDK's
    /// <c>account::guild_admin::Role</c>.</summary>
    public sealed class AccountGuildRole
    {
        /// <summary>The role's index within its guild (stable identifier, not a
        /// <see cref="Guid"/>).</summary>
        [JsonPropertyName("name_index")]
        public int NameIndex { get; set; }

        [JsonPropertyName("name")]
        public string Name { get; set; } = "";

        /// <summary>Guild-wide permission strings this role grants.</summary>
        [JsonPropertyName("permissions")]
        public List<string> Permissions { get; set; } = new List<string>();

        [JsonPropertyName("description")]
        public string Description { get; set; } = "";

        [JsonPropertyName("badge")]
        public JsonElement Badge { get; set; }
    }

    /// <summary>A per-resource permission override. Mirrors the Rust SDK's
    /// <c>account::guild_admin::PermissionOverride</c>.</summary>
    public sealed class PermissionOverride
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("role_index")]
        public int RoleIndex { get; set; }

        /// <summary>The kind of resource overridden (e.g. "channel").</summary>
        [JsonPropertyName("resource_kind")]
        public string ResourceKind { get; set; } = "";

        [JsonPropertyName("resource_id")]
        public Guid ResourceId { get; set; }

        [JsonPropertyName("permission")]
        public string Permission { get; set; } = "";

        /// <summary>Whether this override grants (<c>true</c>) or denies (<c>false</c>)
        /// it.</summary>
        [JsonPropertyName("allow")]
        public bool Allow { get; set; }
    }

    /// <summary>One member of a guild's roster. Mirrors the Rust SDK's
    /// <c>account::guild_admin::GuildMember</c>.</summary>
    public sealed class AccountGuildMember
    {
        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        [JsonPropertyName("role_index")]
        public int RoleIndex { get; set; }

        [JsonPropertyName("joined_at")]
        public DateTimeOffset JoinedAt { get; set; }
    }

    /// <summary>The caller's own membership summary (<c>GET /me/guilds</c>). Mirrors the
    /// Rust SDK's <c>account::guild_admin::MyGuildMembership</c>.</summary>
    public sealed class MyGuildMembership
    {
        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("role_index")]
        public int RoleIndex { get; set; }

        [JsonPropertyName("joined_at")]
        public DateTimeOffset JoinedAt { get; set; }
    }

    /// <summary>A sent (or received) guild invite. Mirrors the Rust SDK's
    /// <c>account::guild_admin::GuildInvite</c>.</summary>
    public sealed class GuildInvite
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("to")]
        public Guid To { get; set; }

        [JsonPropertyName("from")]
        public Guid From { get; set; }

        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }
    }

    /// <summary>One of the caller's own pending invites, across every guild (<c>GET
    /// /me/guild-invites</c>, issue #442). Mirrors the Rust SDK's
    /// <c>account::guild_admin::MyGuildInvite</c>.</summary>
    public sealed class MyGuildInvite
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("guild_name")]
        public string GuildName { get; set; } = "";

        [JsonPropertyName("from")]
        public Guid From { get; set; }

        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }
    }

    /// <summary>An applicant-initiated join request (issue #242). Mirrors the Rust SDK's
    /// <c>account::guild_admin::GuildJoinRequest</c>.</summary>
    public sealed class GuildJoinRequest
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("applicant")]
        public Guid Applicant { get; set; }

        [JsonPropertyName("message")]
        public string? Message { get; set; }

        /// <summary>"pending", "approved", or "rejected".</summary>
        [JsonPropertyName("status")]
        public string Status { get; set; } = "";

        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }

        [JsonPropertyName("decided_at")]
        public DateTimeOffset? DecidedAt { get; set; }

        [JsonPropertyName("decided_by")]
        public Guid? DecidedBy { get; set; }
    }

    /// <summary>A guild text channel. Mirrors the Rust SDK's
    /// <c>account::guild_admin::GuildChannel</c>.</summary>
    public sealed class AccountGuildChannel
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("name")]
        public string Name { get; set; } = "";

        [JsonPropertyName("archived")]
        public bool Archived { get; set; }

        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }

        /// <summary>Whether posting requires the <c>channel_post</c> permission via override
        /// (issue #250) rather than any current member being able to post.</summary>
        [JsonPropertyName("announcement_only")]
        public bool AnnouncementOnly { get; set; }

        [JsonPropertyName("topic")]
        public string? Topic { get; set; }

        /// <summary>Non-member visibility baseline for a public guild (issue #458).</summary>
        [JsonPropertyName("public")]
        public bool Public { get; set; }
    }

    /// <summary>A channel message. Mirrors the Rust SDK's
    /// <c>account::guild_admin::GuildMessage</c>.</summary>
    public sealed class AccountGuildMessage
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("channel_id")]
        public Guid ChannelId { get; set; }

        [JsonPropertyName("author")]
        public Guid Author { get; set; }

        [JsonPropertyName("body")]
        public string Body { get; set; } = "";

        [JsonPropertyName("sent_at")]
        public DateTimeOffset SentAt { get; set; }
    }

    /// <summary>RSVP tallies embedded in a <see cref="AccountGuildEvent"/>. Mirrors the Rust
    /// SDK's <c>account::guild_admin::RsvpCounts</c>.</summary>
    public sealed class RsvpCounts
    {
        [JsonPropertyName("going")]
        public long Going { get; set; }

        [JsonPropertyName("maybe")]
        public long Maybe { get; set; }

        [JsonPropertyName("not_going")]
        public long NotGoing { get; set; }
    }

    /// <summary>A scheduled guild event. Mirrors the Rust SDK's
    /// <c>account::guild_admin::GuildEvent</c>.</summary>
    public sealed class AccountGuildEvent
    {
        [JsonPropertyName("id")]
        public Guid Id { get; set; }

        [JsonPropertyName("guild_id")]
        public Guid GuildId { get; set; }

        [JsonPropertyName("channel_id")]
        public Guid? ChannelId { get; set; }

        [JsonPropertyName("title")]
        public string Title { get; set; } = "";

        [JsonPropertyName("description")]
        public string? Description { get; set; }

        [JsonPropertyName("starts_at")]
        public DateTimeOffset StartsAt { get; set; }

        [JsonPropertyName("ends_at")]
        public DateTimeOffset? EndsAt { get; set; }

        [JsonPropertyName("created_by")]
        public Guid CreatedBy { get; set; }

        [JsonPropertyName("created_at")]
        public DateTimeOffset CreatedAt { get; set; }

        [JsonPropertyName("rsvp_counts")]
        public RsvpCounts RsvpCounts { get; set; } = new RsvpCounts();

        /// <summary>Whether a non-member of a public guild may see this event (issue
        /// #448).</summary>
        [JsonPropertyName("public")]
        public bool Public { get; set; }
    }

    /// <summary>The caller's own RSVP, as set by <see cref="AccountSession.RsvpToEventAsync"/>.
    /// Mirrors the Rust SDK's <c>account::guild_admin::Rsvp</c>.</summary>
    public sealed class Rsvp
    {
        [JsonPropertyName("event_id")]
        public Guid EventId { get; set; }

        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        /// <summary>"going", "maybe", or "not_going".</summary>
        [JsonPropertyName("status")]
        public string Status { get; set; } = "";

        [JsonPropertyName("responded_at")]
        public DateTimeOffset RespondedAt { get; set; }
    }

    /// <summary>One entry of an event's per-member RSVP roster (<c>GET
    /// /guilds/{id}/events/{eid}/rsvps</c>, issue #248). Mirrors the Rust SDK's
    /// <c>account::guild_admin::RsvpRosterEntry</c>.</summary>
    public sealed class RsvpRosterEntry
    {
        [JsonPropertyName("identity_id")]
        public Guid IdentityId { get; set; }

        [JsonPropertyName("status")]
        public string Status { get; set; } = "";

        [JsonPropertyName("responded_at")]
        public DateTimeOffset RespondedAt { get; set; }
    }

    /// <summary>A partial update to a guild's own metadata — every field <c>null</c> means
    /// "leave untouched," matching <c>PATCH /guilds/{id}</c>'s own convention. Mirrors the
    /// Rust SDK's <c>account::guild_admin::GuildUpdate</c>.</summary>
    public sealed class AccountGuildUpdate
    {
        public string? Name { get; set; }
        public string? Tag { get; set; }
        public string? Description { get; set; }
        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Motd { get; set; }
        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Banner { get; set; }
        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Icon { get; set; }
        public bool? Recruiting { get; set; }
        public bool? Public { get; set; }
    }

    /// <summary>A partial update to a channel — <c>null</c> leaves that field untouched,
    /// matching <c>PATCH /guilds/{id}/channels/{cid}</c>'s own convention. Mirrors the Rust
    /// SDK's <c>account::guild_admin::ChannelUpdate</c>.</summary>
    public sealed class AccountGuildChannelUpdate
    {
        /// <summary>New channel name (always resent, not three-state).</summary>
        public string Name { get; set; } = "";
        public bool? AnnouncementOnly { get; set; }
        /// <summary><c>null</c> leaves it untouched; <c>""</c> clears it.</summary>
        public string? Topic { get; set; }
        public bool? Public { get; set; }
    }

    /// <summary>Fields for creating or fully replacing a guild event — <c>PUT</c>-style full
    /// replacement on update, matching <c>PATCH /guilds/{id}/events/{eid}</c>'s own
    /// convention. Mirrors the Rust SDK's <c>account::guild_admin::EventFields</c>.</summary>
    public sealed class AccountGuildEventFields
    {
        public Guid? ChannelId { get; set; }
        public string Title { get; set; } = "";
        public string? Description { get; set; }
        public DateTimeOffset StartsAt { get; set; }
        public DateTimeOffset? EndsAt { get; set; }
        public bool Public { get; set; }
    }

    internal sealed class CreateGuildRequest
    {
        [JsonPropertyName("name")]
        public string Name { get; set; } = "";

        [JsonPropertyName("tag")]
        public string Tag { get; set; } = "";

        [JsonPropertyName("description")]
        public string Description { get; set; } = "";
    }

    internal sealed class UpdateGuildRequest
    {
        [JsonPropertyName("name")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public string? Name { get; set; }
        [JsonPropertyName("tag")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public string? Tag { get; set; }
        [JsonPropertyName("description")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public string? Description { get; set; }
        [JsonPropertyName("motd")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public string? Motd { get; set; }
        [JsonPropertyName("banner")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public string? Banner { get; set; }
        [JsonPropertyName("icon")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public string? Icon { get; set; }
        [JsonPropertyName("recruiting")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public bool? Recruiting { get; set; }
        [JsonPropertyName("public")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public bool? Public { get; set; }
    }

    internal sealed class CreateRoleRequest
    {
        [JsonPropertyName("name")]
        public string Name { get; set; } = "";

        [JsonPropertyName("permissions")]
        public List<string> Permissions { get; set; } = new List<string>();

        [JsonPropertyName("description")]
        public string Description { get; set; } = "";

        [JsonPropertyName("signing_key_id")]
        public Guid? SigningKeyId { get; set; }

        [JsonPropertyName("signature")]
        public string? Signature { get; set; }
    }

    internal sealed class UpdateRoleRequest
    {
        [JsonPropertyName("name")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public string? Name { get; set; }
        [JsonPropertyName("permissions")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public List<string>? Permissions { get; set; }
        [JsonPropertyName("description")] [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] public string? Description { get; set; }
        [JsonPropertyName("signing_key_id")] public Guid? SigningKeyId { get; set; }
        [JsonPropertyName("signature")] public string? Signature { get; set; }
    }

    internal sealed class SignatureOnlyRequest
    {
        [JsonPropertyName("signing_key_id")]
        public Guid? SigningKeyId { get; set; }

        [JsonPropertyName("signature")]
        public string? Signature { get; set; }
    }

    internal sealed class SetPermissionOverrideRequest
    {
        [JsonPropertyName("role_index")]
        public int RoleIndex { get; set; }

        [JsonPropertyName("resource_kind")]
        public string ResourceKind { get; set; } = "";

        [JsonPropertyName("resource_id")]
        public Guid ResourceId { get; set; }

        [JsonPropertyName("permission")]
        public string Permission { get; set; } = "";

        [JsonPropertyName("allow")]
        public bool Allow { get; set; }

        [JsonPropertyName("signing_key_id")]
        public Guid? SigningKeyId { get; set; }

        [JsonPropertyName("signature")]
        public string? Signature { get; set; }
    }

    internal sealed class TransferOwnershipRequest
    {
        [JsonPropertyName("to")]
        public Guid To { get; set; }

        [JsonPropertyName("signing_key_id")]
        public Guid? SigningKeyId { get; set; }

        [JsonPropertyName("signature")]
        public string? Signature { get; set; }
    }

    internal sealed class UpdateGuildMemberRequest
    {
        [JsonPropertyName("role_index")]
        public int RoleIndex { get; set; }

        [JsonPropertyName("signing_key_id")]
        public Guid? SigningKeyId { get; set; }

        [JsonPropertyName("signature")]
        public string? Signature { get; set; }
    }

    internal sealed class CreateGuildInviteRequest
    {
        [JsonPropertyName("to")]
        public Guid To { get; set; }
    }

    internal sealed class CreateJoinRequestRequest
    {
        [JsonPropertyName("message")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? Message { get; set; }
    }

    internal sealed class CreateChannelRequest
    {
        [JsonPropertyName("name")]
        public string Name { get; set; } = "";
    }

    internal sealed class UpdateChannelRequest
    {
        [JsonPropertyName("name")]
        public string Name { get; set; } = "";

        [JsonPropertyName("announcement_only")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public bool? AnnouncementOnly { get; set; }

        [JsonPropertyName("topic")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? Topic { get; set; }

        [JsonPropertyName("public")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public bool? Public { get; set; }
    }

    internal sealed class SetFavoriteGamesRequest
    {
        [JsonPropertyName("integrator_ids")]
        public List<Guid> IntegratorIds { get; set; } = new List<Guid>();
    }

    internal sealed class SendGuildMessageRequest
    {
        [JsonPropertyName("body")]
        public string Body { get; set; } = "";
    }

    internal sealed class EventRequestBody
    {
        [JsonPropertyName("channel_id")]
        public Guid? ChannelId { get; set; }

        [JsonPropertyName("title")]
        public string Title { get; set; } = "";

        [JsonPropertyName("description")]
        public string? Description { get; set; }

        [JsonPropertyName("starts_at")]
        public DateTimeOffset StartsAt { get; set; }

        [JsonPropertyName("ends_at")]
        public DateTimeOffset? EndsAt { get; set; }

        [JsonPropertyName("public")]
        public bool Public { get; set; }
    }

    internal sealed class AccountRsvpRequest
    {
        [JsonPropertyName("status")]
        public string Status { get; set; } = "";
    }

    public sealed partial class AccountSession
    {
        /// <summary><c>POST /guilds</c> — creates a new guild, the caller as owner. Not
        /// signature-required.</summary>
        public async Task<AccountGuild> CreateGuildAsync(string name, string tag, string description, CancellationToken ct = default) =>
            await PostAsync<CreateGuildRequest, AccountGuild>(
                "/guilds", new CreateGuildRequest { Name = name, Tag = tag, Description = description }, ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}</c>.</summary>
        public async Task<AccountGuild> GetGuildAsync(Guid guildId, CancellationToken ct = default) =>
            await GetAsync<AccountGuild>($"/guilds/{guildId}", ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/discover{query_string}</c> — <paramref name="queryString"/>
        /// is passed through as-is (including its leading <c>?</c>), built by the caller;
        /// this SDK doesn't replicate the Hub's own query-builder.</summary>
        public async Task<DiscoverGuildsPage> DiscoverGuildsAsync(string queryString, CancellationToken ct = default) =>
            await GetAsync<DiscoverGuildsPage>($"/guilds/discover{queryString}", ct).ConfigureAwait(false);

        /// <summary><c>PATCH /guilds/{id}</c> — ordinary <c>manage_guild</c>-gated metadata
        /// edits. Not signature-required.</summary>
        public async Task<AccountGuild> UpdateGuildAsync(Guid guildId, AccountGuildUpdate update, CancellationToken ct = default) =>
            await PatchAsync<UpdateGuildRequest, AccountGuild>(
                $"/guilds/{guildId}",
                new UpdateGuildRequest
                {
                    Name = update.Name,
                    Tag = update.Tag,
                    Description = update.Description,
                    Motd = update.Motd,
                    Banner = update.Banner,
                    Icon = update.Icon,
                    Recruiting = update.Recruiting,
                    Public = update.Public,
                },
                ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/roles</c>.</summary>
        public async Task<IReadOnlyList<AccountGuildRole>> ListRolesAsync(Guid guildId, CancellationToken ct = default) =>
            await GetAsync<List<AccountGuildRole>>($"/guilds/{guildId}/roles", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/roles</c> — always signs (<c>guild.role.create</c>,
        /// <c>[guild_id, name, permissions comma-joined]</c>).</summary>
        public async Task<AccountGuildRole> CreateRoleAsync(Guid guildId, string name, IReadOnlyList<string> permissions, string description, CancellationToken ct = default)
        {
            var joined = string.Join(",", permissions);
            var (signingKeyId, signature) = Sign("guild.role.create", guildId.ToString(), name, joined);
            return await PostAsync<CreateRoleRequest, AccountGuildRole>(
                $"/guilds/{guildId}/roles",
                new CreateRoleRequest { Name = name, Permissions = new List<string>(permissions), Description = description, SigningKeyId = signingKeyId, Signature = signature },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>PATCH /guilds/{id}/roles/{name_index}</c> — always signs
        /// (<c>guild.role.update</c>, <c>[guild_id, name_index]</c>).</summary>
        public async Task<AccountGuildRole> UpdateRoleAsync(
            Guid guildId, int nameIndex, string? name = null, IReadOnlyList<string>? permissions = null, string? description = null, CancellationToken ct = default)
        {
            var (signingKeyId, signature) = Sign("guild.role.update", guildId.ToString(), nameIndex.ToString());
            return await PatchAsync<UpdateRoleRequest, AccountGuildRole>(
                $"/guilds/{guildId}/roles/{nameIndex}",
                new UpdateRoleRequest
                {
                    Name = name,
                    Permissions = permissions is null ? null : new List<string>(permissions),
                    Description = description,
                    SigningKeyId = signingKeyId,
                    Signature = signature,
                },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>DELETE /guilds/{id}/roles/{name_index}</c> — always signs
        /// (<c>guild.role.delete</c>, <c>[guild_id, name_index]</c>).</summary>
        public async Task DeleteRoleAsync(Guid guildId, int nameIndex, CancellationToken ct = default)
        {
            var (signingKeyId, signature) = Sign("guild.role.delete", guildId.ToString(), nameIndex.ToString());
            await DeleteWithBodyAsync(
                $"/guilds/{guildId}/roles/{nameIndex}",
                new SignatureOnlyRequest { SigningKeyId = signingKeyId, Signature = signature },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>GET /guilds/{id}/permission-overrides?resource_kind=&amp;resource_id=</c>.</summary>
        public async Task<IReadOnlyList<PermissionOverride>> ListPermissionOverridesAsync(Guid guildId, string resourceKind, Guid resourceId, CancellationToken ct = default) =>
            await GetQueryAsync<List<PermissionOverride>>(
                $"/guilds/{guildId}/permission-overrides",
                new[] { ("resource_kind", resourceKind), ("resource_id", resourceId.ToString()) },
                ct).ConfigureAwait(false);

        /// <summary><c>PUT /guilds/{id}/permission-overrides</c> — always signs
        /// (<c>guild.permission_override.set</c>, <c>[guild_id, role_index, resource_kind,
        /// resource_id, permission, allow]</c>).</summary>
        public async Task<PermissionOverride> SetPermissionOverrideAsync(
            Guid guildId, int roleIndex, string resourceKind, Guid resourceId, string permission, bool allow, CancellationToken ct = default)
        {
            var (signingKeyId, signature) = Sign(
                "guild.permission_override.set",
                guildId.ToString(), roleIndex.ToString(), resourceKind, resourceId.ToString(), permission, allow.ToString().ToLowerInvariant());
            return await PutAsync<SetPermissionOverrideRequest, PermissionOverride>(
                $"/guilds/{guildId}/permission-overrides",
                new SetPermissionOverrideRequest
                {
                    RoleIndex = roleIndex,
                    ResourceKind = resourceKind,
                    ResourceId = resourceId,
                    Permission = permission,
                    Allow = allow,
                    SigningKeyId = signingKeyId,
                    Signature = signature,
                },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>DELETE /guilds/{id}/permission-overrides/{override_id}</c> — always
        /// signs (<c>guild.permission_override.delete</c>, <c>[guild_id, override_id]</c>).</summary>
        public async Task DeletePermissionOverrideAsync(Guid guildId, Guid overrideId, CancellationToken ct = default)
        {
            var (signingKeyId, signature) = Sign("guild.permission_override.delete", guildId.ToString(), overrideId.ToString());
            await DeleteWithBodyAsync(
                $"/guilds/{guildId}/permission-overrides/{overrideId}",
                new SignatureOnlyRequest { SigningKeyId = signingKeyId, Signature = signature },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>POST /guilds/{id}/transfer-ownership</c> — owner-only, always signs
        /// (<c>guild.transfer_ownership</c>, <c>[guild_id, current owner, to]</c>).</summary>
        public async Task<AccountGuild> TransferOwnershipAsync(Guid guildId, Guid to, CancellationToken ct = default)
        {
            var (signingKeyId, signature) = Sign("guild.transfer_ownership", guildId.ToString(), IdentityGuid.ToString(), to.ToString());
            return await PostAsync<TransferOwnershipRequest, AccountGuild>(
                $"/guilds/{guildId}/transfer-ownership",
                new TransferOwnershipRequest { To = to, SigningKeyId = signingKeyId, Signature = signature },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>POST /guilds/{id}/integrations/{integrator_id}</c> — associates an
        /// integrator with a guild. Not signature-required.</summary>
        public async Task<AccountGuild> AssociateIntegratorAsync(Guid guildId, Guid integratorId, CancellationToken ct = default) =>
            await PostEmptyAsync<AccountGuild>($"/guilds/{guildId}/integrations/{integratorId}", ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/members</c>.</summary>
        public async Task<IReadOnlyList<AccountGuildMember>> ListMembersAsync(Guid guildId, CancellationToken ct = default) =>
            await GetAsync<List<AccountGuildMember>>($"/guilds/{guildId}/members", ct).ConfigureAwait(false);

        /// <summary><c>PATCH /guilds/{id}/members/{identity_id}</c> — role change; always
        /// signs (<c>guild.member_role.update</c>, <c>[guild_id, identity_id,
        /// role_index]</c>), whether or not this particular change actually escalates (the
        /// only case #697 requires it for) — same unused-but-valid-signature-is-harmless
        /// simplification used throughout this SDK.</summary>
        public async Task<AccountGuildMember> UpdateMemberRoleAsync(Guid guildId, Guid identityId, int roleIndex, CancellationToken ct = default)
        {
            var (signingKeyId, signature) = Sign("guild.member_role.update", guildId.ToString(), identityId.ToString(), roleIndex.ToString());
            return await PatchAsync<UpdateGuildMemberRequest, AccountGuildMember>(
                $"/guilds/{guildId}/members/{identityId}",
                new UpdateGuildMemberRequest { RoleIndex = roleIndex, SigningKeyId = signingKeyId, Signature = signature },
                ct).ConfigureAwait(false);
        }

        /// <summary><c>DELETE /guilds/{id}/members/{identity_id}</c> — kick, not a role
        /// change; reversible via re-invite. Not signature-required.</summary>
        public async Task RemoveMemberAsync(Guid guildId, Guid identityId, CancellationToken ct = default) =>
            await DeleteAsync($"/guilds/{guildId}/members/{identityId}", ct).ConfigureAwait(false);

        /// <summary><c>GET /me/guilds</c>.</summary>
        public async Task<IReadOnlyList<MyGuildMembership>> MyGuildsAsync(CancellationToken ct = default) =>
            await GetAsync<List<MyGuildMembership>>("/me/guilds", ct).ConfigureAwait(false);

        /// <summary><c>GET /me/guild-invites</c> (issue #442).</summary>
        public async Task<IReadOnlyList<MyGuildInvite>> MyGuildInvitesAsync(CancellationToken ct = default) =>
            await GetAsync<List<MyGuildInvite>>("/me/guild-invites", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/invites</c>.</summary>
        public async Task<GuildInvite> CreateGuildInviteAsync(Guid guildId, Guid to, CancellationToken ct = default) =>
            await PostAsync<CreateGuildInviteRequest, GuildInvite>(
                $"/guilds/{guildId}/invites", new CreateGuildInviteRequest { To = to }, ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/invites/{invite_id}/accept</c>.</summary>
        public async Task<AccountGuildMember> AcceptGuildInviteAsync(Guid guildId, Guid inviteId, CancellationToken ct = default) =>
            await PostEmptyAsync<AccountGuildMember>($"/guilds/{guildId}/invites/{inviteId}/accept", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/invites/{invite_id}/decline</c>.</summary>
        public async Task DeclineGuildInviteAsync(Guid guildId, Guid inviteId, CancellationToken ct = default) =>
            await PostEmptyNoResponseAsync($"/guilds/{guildId}/invites/{inviteId}/decline", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/join</c> — only meaningful when the guild's join
        /// policy allows it.</summary>
        public async Task<AccountGuildMember> JoinGuildAsync(Guid guildId, CancellationToken ct = default) =>
            await PostEmptyAsync<AccountGuildMember>($"/guilds/{guildId}/join", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/leave</c>.</summary>
        public async Task LeaveGuildAsync(Guid guildId, CancellationToken ct = default) =>
            await PostEmptyNoResponseAsync($"/guilds/{guildId}/leave", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/join-requests</c> (issue #242).</summary>
        public async Task<GuildJoinRequest> CreateJoinRequestAsync(Guid guildId, string? message = null, CancellationToken ct = default) =>
            await PostAsync<CreateJoinRequestRequest, GuildJoinRequest>(
                $"/guilds/{guildId}/join-requests", new CreateJoinRequestRequest { Message = message }, ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/join-requests</c> — <c>manage_members</c>-gated.</summary>
        public async Task<IReadOnlyList<GuildJoinRequest>> ListJoinRequestsAsync(Guid guildId, CancellationToken ct = default) =>
            await GetAsync<List<GuildJoinRequest>>($"/guilds/{guildId}/join-requests", ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/join-requests/mine</c> — the caller's own pending
        /// request for this guild, or <c>null</c> (issue #256).</summary>
        public async Task<GuildJoinRequest?> MyJoinRequestAsync(Guid guildId, CancellationToken ct = default) =>
            await GetNullableAsync<GuildJoinRequest>($"/guilds/{guildId}/join-requests/mine", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/join-requests/{request_id}/approve</c>.</summary>
        public async Task<AccountGuildMember> ApproveJoinRequestAsync(Guid guildId, Guid requestId, CancellationToken ct = default) =>
            await PostEmptyAsync<AccountGuildMember>($"/guilds/{guildId}/join-requests/{requestId}/approve", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/join-requests/{request_id}/reject</c>.</summary>
        public async Task RejectJoinRequestAsync(Guid guildId, Guid requestId, CancellationToken ct = default) =>
            await PostEmptyNoResponseAsync($"/guilds/{guildId}/join-requests/{requestId}/reject", ct).ConfigureAwait(false);

        /// <summary><c>DELETE /guilds/{id}/join-requests/{request_id}</c> — the applicant
        /// withdrawing their own request.</summary>
        public async Task WithdrawJoinRequestAsync(Guid guildId, Guid requestId, CancellationToken ct = default) =>
            await DeleteAsync($"/guilds/{guildId}/join-requests/{requestId}", ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/favorite-integrators</c> (issue #207).</summary>
        public async Task<FavoriteGames> FavoriteGamesAsync(Guid guildId, CancellationToken ct = default) =>
            await GetAsync<FavoriteGames>($"/guilds/{guildId}/favorite-integrators", ct).ConfigureAwait(false);

        /// <summary><c>PUT /guilds/{id}/favorite-integrators</c> — <c>manage_guild</c>-gated,
        /// full ordered replacement. Not signature-required.</summary>
        public async Task<FavoriteGames> SetFavoriteGamesAsync(Guid guildId, IReadOnlyList<Guid> integratorIds, CancellationToken ct = default) =>
            await PutAsync<SetFavoriteGamesRequest, FavoriteGames>(
                $"/guilds/{guildId}/favorite-integrators", new SetFavoriteGamesRequest { IntegratorIds = new List<Guid>(integratorIds) }, ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/channels</c>.</summary>
        public async Task<IReadOnlyList<AccountGuildChannel>> ListChannelsAsync(Guid guildId, CancellationToken ct = default) =>
            await GetAsync<List<AccountGuildChannel>>($"/guilds/{guildId}/channels", ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/channels</c> — <c>manage_channels</c>-gated. Not
        /// signature-required (structural but reversible).</summary>
        public async Task<AccountGuildChannel> CreateChannelAsync(Guid guildId, string name, CancellationToken ct = default) =>
            await PostAsync<CreateChannelRequest, AccountGuildChannel>(
                $"/guilds/{guildId}/channels", new CreateChannelRequest { Name = name }, ct).ConfigureAwait(false);

        /// <summary><c>PATCH /guilds/{id}/channels/{channel_id}</c>. Not signature-required.</summary>
        public async Task<AccountGuildChannel> UpdateChannelAsync(Guid guildId, Guid channelId, AccountGuildChannelUpdate update, CancellationToken ct = default) =>
            await PatchAsync<UpdateChannelRequest, AccountGuildChannel>(
                $"/guilds/{guildId}/channels/{channelId}",
                new UpdateChannelRequest { Name = update.Name, AnnouncementOnly = update.AnnouncementOnly, Topic = update.Topic, Public = update.Public },
                ct).ConfigureAwait(false);

        /// <summary><c>POST /guilds/{id}/channels/{channel_id}/archive</c>.</summary>
        public async Task<AccountGuildChannel> ArchiveChannelAsync(Guid guildId, Guid channelId, CancellationToken ct = default) =>
            await PostEmptyAsync<AccountGuildChannel>($"/guilds/{guildId}/channels/{channelId}/archive", ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/channels/{channel_id}/messages</c>, cursor-paginated.</summary>
        public async Task<IReadOnlyList<AccountGuildMessage>> ChannelMessagesAsync(
            Guid guildId, Guid channelId, string? before = null, uint? limit = null, CancellationToken ct = default)
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
            return await GetQueryAsync<List<AccountGuildMessage>>($"/guilds/{guildId}/channels/{channelId}/messages", query, ct).ConfigureAwait(false);
        }

        /// <summary><c>POST /guilds/{id}/channels/{channel_id}/messages</c>. Not
        /// signature-required (chat, per #697's own invariants).</summary>
        public async Task<AccountGuildMessage> SendMessageAsync(Guid guildId, Guid channelId, string body, CancellationToken ct = default) =>
            await PostAsync<SendGuildMessageRequest, AccountGuildMessage>(
                $"/guilds/{guildId}/channels/{channelId}/messages", new SendGuildMessageRequest { Body = body }, ct).ConfigureAwait(false);

        /// <summary><c>DELETE /guilds/{id}/channels/{channel_id}/messages/{message_id}</c> —
        /// moderation delete, <c>manage_channels</c>-gated. Not signature-required.</summary>
        public async Task DeleteMessageAsync(Guid guildId, Guid channelId, Guid messageId, CancellationToken ct = default) =>
            await DeleteAsync($"/guilds/{guildId}/channels/{channelId}/messages/{messageId}", ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/events</c>, optionally windowed by
        /// <paramref name="from"/>/<paramref name="to"/> (RFC 3339).</summary>
        public async Task<IReadOnlyList<AccountGuildEvent>> ListEventsAsync(Guid guildId, string? from = null, string? to = null, CancellationToken ct = default)
        {
            var query = new List<(string, string)>();
            if (from != null)
            {
                query.Add(("from", from));
            }
            if (to != null)
            {
                query.Add(("to", to));
            }
            return await GetQueryAsync<List<AccountGuildEvent>>($"/guilds/{guildId}/events", query, ct).ConfigureAwait(false);
        }

        private static EventRequestBody ToEventRequestBody(AccountGuildEventFields fields) => new EventRequestBody
        {
            ChannelId = fields.ChannelId,
            Title = fields.Title,
            Description = fields.Description,
            StartsAt = fields.StartsAt,
            EndsAt = fields.EndsAt,
            Public = fields.Public,
        };

        /// <summary><c>POST /guilds/{id}/events</c>. Not signature-required (reversible
        /// scheduling state).</summary>
        public async Task<AccountGuildEvent> CreateEventAsync(Guid guildId, AccountGuildEventFields fields, CancellationToken ct = default) =>
            await PostAsync<EventRequestBody, AccountGuildEvent>($"/guilds/{guildId}/events", ToEventRequestBody(fields), ct).ConfigureAwait(false);

        /// <summary><c>PATCH /guilds/{id}/events/{event_id}</c> — full replacement, not
        /// partial (matches the server's own <c>UpdateEventRequest</c>).</summary>
        public async Task<AccountGuildEvent> UpdateEventAsync(Guid guildId, Guid eventId, AccountGuildEventFields fields, CancellationToken ct = default) =>
            await PatchAsync<EventRequestBody, AccountGuildEvent>($"/guilds/{guildId}/events/{eventId}", ToEventRequestBody(fields), ct).ConfigureAwait(false);

        /// <summary><c>DELETE /guilds/{id}/events/{event_id}</c>.</summary>
        public async Task DeleteEventAsync(Guid guildId, Guid eventId, CancellationToken ct = default) =>
            await DeleteAsync($"/guilds/{guildId}/events/{eventId}", ct).ConfigureAwait(false);

        /// <summary><c>PUT /guilds/{id}/events/{event_id}/rsvp</c> — always sets the
        /// caller's own RSVP; <paramref name="status"/> is "going", "maybe", or
        /// "not_going".</summary>
        public async Task<Rsvp> RsvpToEventAsync(Guid guildId, Guid eventId, string status, CancellationToken ct = default) =>
            await PutAsync<AccountRsvpRequest, Rsvp>($"/guilds/{guildId}/events/{eventId}/rsvp", new AccountRsvpRequest { Status = status }, ct).ConfigureAwait(false);

        /// <summary><c>GET /guilds/{id}/events/{event_id}/rsvps</c> — the per-member roster.</summary>
        public async Task<IReadOnlyList<RsvpRosterEntry>> EventRsvpsAsync(Guid guildId, Guid eventId, CancellationToken ct = default) =>
            await GetAsync<List<RsvpRosterEntry>>($"/guilds/{guildId}/events/{eventId}/rsvps", ct).ConfigureAwait(false);
    }
}
