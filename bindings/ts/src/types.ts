// Core identity/profile wire shapes shared by AccountSession and
// IntegratorSession — mirrors avalon_protocol::identity::{Identity, Profile}
// and both Rust/C# SDKs' own `MeResponse` mapping.

export type Genre =
  | 'action'
  | 'adventure'
  | 'rpg'
  | 'strategy'
  | 'simulation'
  | 'puzzle'
  | 'racing'
  | 'sports'
  | 'horror'
  | 'sandbox'
  | 'mmo'
  | 'shooter'
  | 'platformer'
  | 'party'

export interface Identity {
  id: string
  createdAt: string
}

export interface Profile {
  identityId: string
  displayName: string
  avatarUrl: string | null
  bio: string | null
  favoriteGenres: Genre[]
  pronouns: string | null
  bannerUrl: string | null
  status: string | null
  links: string[]
  timezone: string | null
  themeColor: string | null
  location: string | null
  // `null` means "not explicitly set," not "no guild" — see
  // `effectiveMainGuild` for the resolved value to build a single-guild UI
  // around.
  mainGuild: string | null
  // `mainGuild` if explicitly set, otherwise the guild this identity
  // joined earliest, computed server-side at read time and never stored.
  // `null` only when the identity has no guild memberships at all.
  effectiveMainGuild: string | null
  // Issue #205's opt-in global search toggle.
  discoverable: boolean
  // Issue #87 — who can see this identity's presence status: "public" |
  // "authenticated_only" | "friends" | "guild_members" | "private".
  presenceVisibility: string
}

/** `GET /me`'s wire response, field-for-field. */
export interface MeResponseWire {
  identity_id: string
  identity_created_at: string
  display_name: string
  avatar_url: string | null
  bio: string | null
  favorite_genres: Genre[]
  pronouns: string | null
  banner_url: string | null
  status: string | null
  links: string[]
  timezone: string | null
  theme_color: string | null
  location: string | null
  main_guild: string | null
  effective_main_guild: string | null
  discoverable: boolean
  presence_visibility: string
}

export function fromMeResponse(body: MeResponseWire): { identity: Identity; profile: Profile } {
  return {
    identity: { id: body.identity_id, createdAt: body.identity_created_at },
    profile: {
      identityId: body.identity_id,
      displayName: body.display_name,
      avatarUrl: body.avatar_url,
      bio: body.bio,
      favoriteGenres: body.favorite_genres,
      pronouns: body.pronouns,
      bannerUrl: body.banner_url,
      status: body.status,
      links: body.links,
      timezone: body.timezone,
      themeColor: body.theme_color,
      location: body.location,
      mainGuild: body.main_guild,
      effectiveMainGuild: body.effective_main_guild ?? null,
      discoverable: body.discoverable ?? false,
      presenceVisibility: body.presence_visibility ?? 'public',
    },
  }
}

export interface DeviceRowWire {
  id: string
  public_key: string
}

/** `GET /ledger/sth/latest`'s wire response, field-for-field — matches
 * `crates/server/src/settlement.rs::SignedTreeHeadResponse`. Hub's own
 * `apps/hub/src/network/verifyNetwork.ts` (issue #232) reconstructs
 * `crates/chain/src/sth.rs::signing_message` from `tree_size`/`root_hash`/
 * `network_id`/`created_at` itself — this type only carries the shape,
 * verification stays Hub's own logic. */
export interface SignedTreeHeadResponse {
  tree_size: number
  // Lowercase hex-encoded RFC 6962 Merkle Tree Hash.
  root_hash: string
  network_id: string
  signing_key_id: string
  // Lowercase hex-encoded Ed25519 signature (64 bytes).
  signature: string
  // RFC 3339.
  created_at: string
  protocol_version: string
}
