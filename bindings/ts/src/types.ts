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
  mainGuild: string | null
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
    },
  }
}

export interface DeviceRowWire {
  id: string
  public_key: string
}
