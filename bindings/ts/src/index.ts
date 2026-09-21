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
  PresenceStatusWire,
  PresenceUpdate,
  PresenceSubscription,
  ChannelMessageUpdate,
  ConversationMessageUpdate,
  RealtimeSubscription,
  GameBreakdown,
  GameBreakdownEntry,
  ArchivedMessage,
  AttestationProof,
  AttestationAuthenticity,
  AttestationValidity,
  AttestationHistoryEntry,
  Attestation,
} from './accountSession/index.js'

export { IntegratorSession } from './integratorSession.js'
export type { Capability, Friend, GuildMembership, ConversationSummary, VerifiedAttestation } from './integratorSession.js'

export type { Identity, Profile, Genre, SignedTreeHeadResponse } from './types.js'

export { getLatestSth } from './ledger.js'

export {
  listAchievementDefinitions,
  listMilestoneDefinitions,
  getIntegrator,
  listIntegrators,
  getIntegratorRegistry,
  listIssuerKeys,
} from './integratorDirectory.js'
export type {
  IntegratorCategory,
  AchievementDefinition,
  Integrator,
  IntegratorSummary,
  IntegratorsPage,
  RegistryMetric,
  IntegratorRegistry,
  IssuerKey,
} from './integratorDirectory.js'

export { getIdentityIntegratorData } from './identityData.js'
export type { VisibleIntegratorDataInstance } from './identityData.js'

export {
  startRecoveryRequest,
  finishRecoveryRequest,
  getRecoveryRequest,
  finalizeRecoveryRequest,
  getIdentityRecoveryStatus,
} from './recovery.js'
export type { StartRecoveryRequest, RecoveryStartResult, FinishRecoveryRequest } from './recovery.js'

export { lookupCrossNodeLogin, denyCrossNodeLogin, submitCrossNodeLoginGrant } from './crossNodeLogin.js'
export type { CrossNodeLoginLookup } from './crossNodeLogin.js'
export { mintCrossNodeLoginGrant } from './crypto/crossNodeLogin.js'
export type { CrossNodeLoginGrant } from './crypto/crossNodeLogin.js'

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

export { runRegistrationCeremony, runAuthenticationCeremony } from './crypto/webauthn.js'

export {
  deriveSigningKeyFromMnemonic,
  generateMnemonicSigningKey,
  isValidMnemonic,
} from './crypto/mnemonic.js'
export type { GeneratedSigningKey } from './crypto/mnemonic.js'
