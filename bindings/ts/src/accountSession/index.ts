// Barrel: importing this module (transitively, via ../index.ts) applies
// every domain file's prototype augmentation to AccountSession — importing
// core.ts alone would leave those methods missing at runtime even though
// the merged `declare module` types would still type-check.
export { AccountSession, findOwnSigningKeyId } from './core.js'
export type { AccountCredentials, AccountSigningKey, AccountSessionInit, ProfileUpdate } from './core.js'
export { AccountDeviceLogin, startAccountDeviceLoginRequest } from './deviceLogin.js'

import './passkeys.js'
import './devices.js'
import './recovery.js'
import './social.js'
import './conversations.js'
import './guildAdmin.js'
import './integrations.js'
import './realtime.js'
import './achievements.js'

export type { Passkey } from './passkeys.js'
export type { Device, DeviceGrant } from './devices.js'
export type { GuardianSettings, RecoveryRequest, GuardianRequest, GuardianOf } from './recovery.js'
export type {
  PresenceStatus,
  Friendship,
  FriendRequest,
  Block,
  PublicProfile,
  DiscoveryCandidate,
  SearchResultIdentity,
  Presence,
  HistoryEntry,
  PublicIdentityProfile,
  GuildAnnouncementAlert,
} from './social.js'
export type { Conversation, ConversationMessage } from './conversations.js'
export type { IntegratorConnection, ConnectionGrant, MyConnection } from './integrations.js'
export type {
  PresenceStatusWire,
  PresenceUpdate,
  PresenceSubscription,
  ChannelMessageUpdate,
  ConversationMessageUpdate,
  RealtimeSubscription,
} from './realtime.js'
export type {
  Guild,
  GuildLink,
  DiscoverGuildSummary,
  DiscoverGuildsPage,
  FavoriteGameEntry,
  FavoriteGames,
  Role,
  RoleBadge,
  RoleBadgeUpdate,
  PermissionOverride,
  GuildMember,
  MyGuildMembership,
  GuildInvite,
  MyGuildInvite,
  GuildJoinRequest,
  GuildChannel,
  GuildMessage,
  RsvpCounts,
  GuildEvent,
  RsvpStatus,
  Rsvp,
  RsvpRosterEntry,
  GuildUpdate,
  ChannelUpdate,
  EventFields,
  GameBreakdown,
  GameBreakdownEntry,
  ArchivedMessage,
} from './guildAdmin.js'
export type {
  AttestationProof,
  AttestationAuthenticity,
  AttestationValidity,
  AttestationHistoryEntry,
  Attestation,
} from './achievements.js'
