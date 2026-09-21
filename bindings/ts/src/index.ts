// bindings/ts — TypeScript reference SDK for Avalon Protocol (issue #701).
// Self-contained: no dependency on packages/api-client, apps/hub, or
// apps/mobile-hub. See docs/architecture/sdk.md for the full surface.
export { AvalonClient } from './client.js'
export type { AvalonClientConfig } from './client.js'

export { AccountSession, AccountDeviceLogin } from './accountSession/index.js'
export type {
  AccountCredentials,
  AccountSigningKey,
  ProfileUpdate,
  Passkey,
  Device,
  DeviceGrant,
  GuardianSettings,
  RecoveryRequest,
  GuardianRequest,
  GuardianOf,
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
  Conversation,
  ConversationMessage,
  IntegratorConnection,
  ConnectionGrant,
  MyConnection,
  Guild,
  GuildLink,
  DiscoverGuildSummary,
  DiscoverGuildsPage,
  FavoriteGameEntry,
  FavoriteGames,
  Role,
  RoleBadge,
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
  Rsvp,
  RsvpRosterEntry,
  GuildUpdate,
  ChannelUpdate,
  EventFields,
} from './accountSession/index.js'

export { IntegratorSession } from './integratorSession.js'
export type { Capability, Friend, GuildMembership, ConversationSummary, VerifiedAttestation } from './integratorSession.js'

export type { Identity, Profile, Genre } from './types.js'

export * from './errors.js'

export {
  canonicalMessage,
  generateSigningKey,
  publicKeyFromSecretKey,
  sign,
  verify,
  bytesToBase64,
  base64ToBytes,
} from './crypto/signing.js'
export type { SigningKeyPair, SignatureFields } from './crypto/signing.js'
