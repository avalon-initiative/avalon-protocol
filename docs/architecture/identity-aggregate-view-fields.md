# Identity Aggregate View: Field Reference

Field-by-field reference for the aggregate shape in
[`./identity-aggregate-view.md`](./identity-aggregate-view.md). Split into its
own file so that document can stay focused on the *why* — this one is the
lookup table you come back to once you already understand the two layers.

Each row names the real Rust type backing the field, so this table can be
checked against source directly instead of taken on faith. If this doc and
the actual code ever disagree, the code is right and this doc is stale — the
same discipline [`./worked-ledger-example.md`](./worked-ledger-example.md)
holds itself to.

## Layer 1 — identity, profile, friends, guild membership

Backed by `avalon_protocol::identity::{Identity, Profile}`,
`avalon_protocol::guilds::GuildMember`, and `avalon_protocol::social::Friendship`.

| Field | Type | Used for |
|---|---|---|
| `identity.id` | `IdentityId` (UUID) | The stable, opaque handle everything else hangs off. Never derived from a name/username — see `identity.md`'s "The model." |
| `identity.created_at` | timestamp | When the identity came into existence (`identity.created`). |
| `profile.display_name` | string | Human-facing name; combined with `discriminator` to form the `handle` shown elsewhere. |
| `discriminator` (→ `handle`) | string | Server-chosen, disambiguates same-named identities (`name#1234`); derived into `handle`, never stored/read as its own top-level field by consumers. |
| `profile.avatar_url` | `Option<string>` | Self-chosen profile image. Validated as a well-formed `http`/`https` URL server-side (`is_http_url`). |
| `profile.banner_url` | `Option<string>` (#372) | A second image slot for a profile page header — same validation as `avatar_url`. |
| `profile.bio` | `Option<string>` | Free-text self-description, capped at 500 chars. |
| `profile.status` | `Option<string>` (#372) | A short tagline, distinct from and shorter-capped than `bio`. |
| `profile.pronouns` | `Option<string>` (#155) | Free text, capped at 40 chars. |
| `profile.favorite_genres` | `Vec<Genre>` (#155) | A fixed, small controlled vocabulary — kept useful for matching/filtering, not free text. |
| `profile.links` | `Vec<string>` (#372) | Up to 5 self-reported URLs, each validated as `http`/`https`. |
| `profile.timezone` | `Option<string>` (#372) | Self-reported only — useful for guild event scheduling. **Not** validated against the real IANA tz database yet (documented gap). |
| `profile.theme_color` | `Option<string>` (#372) | A self-chosen 6-digit hex accent color. Purely cosmetic self-expression. |
| `profile.location` | `Option<string>` (#372) | Free text only, e.g. "Pacific Northwest." **Never** IP-derived or geocoded — load-bearing constraint, not a suggestion. |
| `profile.main_guild` | `Option<GuildId>` | A self-chosen pointer to one of the identity's own guild memberships, so an integrator has one easy guild to build around instead of every simultaneous membership. Must reference a guild the identity is actually a member of; clears automatically if that membership ends. |
| `guilds[]` | `GuildMember { guild_id, role, joined_at }` | Every guild membership this identity currently holds, with its role in each. |
| `friends[]` | derived from `Friendship { a, b, since }` | The identity's accepted friend connections (symmetric — either side can be `a` or `b`). |

`profile.main_guild` is `Profile::main_guild` itself. `profile.effective_main_guild`
in the aggregate shape is different: it isn't a `Profile` field at all, just a
read-time fallback (`crates/server/src/handlers.rs`) to the earliest
`GuildMember.joined_at` when `main_guild` is unset — it only ever appears in a
response shape, never in storage or in a `profile.updated` payload. See
[`./identity.md`](./identity.md#what-is-promised-durable).

## Layer 2 — one entry per integrator binding

Backed by `avalon_protocol::integrators::{IntegratorBinding, IntegratorCategory}` and
`avalon_protocol::achievements::AchievementAttestation`.

| Field | Type | Used for |
|---|---|---|
| `type` | `IntegratorCategory` (game/app/service) | Which category this integrator registered as — a label, not a different mechanism; see #282/#275. |
| `integrator_id` | string/slug | Which game/app/service this entry describes. |
| `binding.established_at` | timestamp | When the identity opted into this integrator (consent-driven, identity-initiated — `bindings.md`). |
| `binding.ended_at` | `Option<timestamp>` | When the binding ended, if ever — history stays intact either way. |
| `attestations[].achievement` | `GlobalId` | Which claim (`game:<slug>:achievement:<key>` or `app\|service:<slug>:milestone:<key>`) this attestation is about — see #324's category-driven vocabulary decision. |
| `attestations[].issued_at` | timestamp | When the issuing integrator signed this claim. |
| `attestations[].issuer_key_id` | string | Which of the issuer's operational keys signed it (#80/#84's two-tier model) — lets a compromised key be pinpointed/revoked without implicating the whole integrator. |
| `published_schemas[]` | `game_schema.published` payload (#255) | An integrator's own declared shape for its custom data (e.g. `Character`) — schema only, real and built. |
| *(instance data, e.g. `characters`)* | decided (#381), built (#384) | Actual per-user instance data against a published schema, read via `GET /identities/{id}/integrator-data` — `characters` is one example; a schema can declare any shape. Public by default once published, with a schema-level `private` opt-out and a bidirectional field-level override (#381), enforced server-side before a caller ever sees the data. See [`./identity-aggregate-view.md`](./identity-aggregate-view.md#a-made-up-integrators-full-shape-illustrated) and `integrator-space.md`. |

**Not layer 1 or layer 2 at all**: presence (`status`, "last seen," current
integrator/server) is a third, deliberately ephemeral tier — never a
`ProtocolEvent`, never in scope here. See [`./presence.md`](./presence.md);
don't add presence fields to either table above even though presence
describes "this identity, right now" in a colloquial sense.
