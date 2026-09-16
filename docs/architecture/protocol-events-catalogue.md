# Protocol Events: Kind Catalogue

The full, row-by-row list of `ProtocolEvent` kinds for
[`./protocol-events.md`](./protocol-events.md) — pulled into its own file so
that document can stay focused on the design properties and versioning
policy around events, rather than being interrupted by a ~40-row table.

Naming: `<domain>.<past-tense-verb>`. **Normative as of #82**: every row
marked "done" below has a real, enum-backed `ProtocolEventKindVariant`
(`crates/protocol/src/events.rs`) and a typed payload struct
(`crates/protocol/src/event_payloads.rs`) — the server builds and
serializes that struct, never an ad-hoc `serde_json::json!({...})`, so
this table's "Payload (canonical)" column is kept honest by the compiler,
not just by convention. A row with no "(done)" marker describes a kind
that isn't emitted anywhere yet — `ProtocolEventKind::Other` is what a
decoder sees for it today, and it gets a real variant exactly when its
emitter is built (see `ProtocolEventKindVariant`'s own doc comment for why
that's deliberate, not an oversight). "Network" as signer means the node
records the fact on behalf of an authenticated actor and is the
milestone-1 stand-in until actor signatures exist.

| Kind | Issuer → subject | Payload (canonical) | Drives | Signed by |
|---|---|---|---|---|
| `identity.created` (done) | identity → identity | `identity_id`, `display_name` (issue #510: the globally-unique handle itself, no discriminator) | identities | identity's Ed25519 event-signing key (#73, done) |
| `identity.signing_key_added` (done) | identity → identity | `signing_key_id`, `public_key`, `device_label`, `approved_by_signing_key_id` | identity signing keys | the approving device's key (#135, done) |
| `identity.signing_key_revoked` (done) | identity → identity | `signing_key_id` | identity signing keys | network (milestone-1 stand-in, #135, done) |
| `identity.recovery_configured` (done) | identity → identity | `guardian_ids`, `threshold` | recovery guardian settings | identity key (session-authenticated, #201, done) |
| `identity.recovery_requested` (done) | identity → identity | `request_id`, `threshold` | recovery requests | network (milestone-1 stand-in — the requester by definition has no session; #201, done) |
| `identity.recovery_approved` (done) | identity (guardian) → identity | `request_id`, `guardian_id`, `approvals_count`, `threshold`, `delay_ends_at` | recovery requests/approvals | network (milestone-1 stand-in, #201, done) |
| `identity.recovery_cancelled` (done) | identity (owner or guardian) → identity | `request_id`, `cancelled_by`, `reason` | recovery requests | network (milestone-1 stand-in, #201, done) |
| `identity.recovered` (done) | identity → identity | `request_id`, `device_label` | identity keys | network (milestone-1 stand-in, #201, done) |
| `profile.updated` (done) | identity → identity | sparse — only the changed promised-durable fields are present at all: `display_name`, `avatar_url`, `bio`, `favorite_genres`, `pronouns`, `banner_url`, `status`, `links`, `timezone`, `theme_color`, `location`, `main_guild` (#86, widened by #155; discriminator removed by #510) | profiles | identity key |
| `game.registered` (done) | integrator → integrator | `game_id`, `slug`, `name`, `developer`, `category`, `requested_capabilities`, `initial_key` (`key_id`/`algorithm`/`public_key`) | integrators, registry | integrator key |
| `game.binding_established` (done) | identity → integrator | `binding_id`, `identity_id`, `game_id`, `slug` | bindings, registry identities | identity key |
| `game.binding_ended` (done) | identity → integrator | `binding_id`, `identity_id`, `game_id`, `slug` | bindings | identity key |
| `permission.granted` (done) | identity → integrator (per capability) | `binding_id`, `identity_id`, `game_id`, `capability` | permission grants | identity key |
| `permission.revoked` (done) | identity → integrator (per capability) | `binding_id`, `identity_id`, `game_id`, `capability`, `reason` (optional — present only when the revoke was a side effect of ending the whole binding) | permission grants | identity key |
| `issuer.registered` (done) | issuer → network | `issuer_ref`, `network_id`, `registered_at` — issue #481's per-network admission record, distinct from `game.registered`'s own integrator-registration-with-initial-key event above | issuer network registrations | network (auto-admission on first valid write, or explicit registration) |
| `issuer.key_added` (done) | issuer → issuer | `game_id`, `slug`, `key_id`, `algorithm`, `public_key`, `role`, `valid_until` | issuer keys | existing root issuer key |
| `issuer.key_revoked` (done) | issuer → issuer | `game_id`, `slug`, `key_id`, `revoked_at`, `reason` | issuer keys | existing root issuer key |
| `issuer.key_expired` | issuer → issuer | key id | issuer keys | issuer key or network |
| `issuer.suspended` / `.reinstated` / `.revoked` / `.deprecated` | network or issuer → issuer | reason, effective at | issuer status | operator (audited) or issuer |
| `friend.requested` / `.accepted` / `.removed` (done) | identity → identity | `.requested`: `from`, `to`, `actor`; `.accepted`: `from`, `to`, `actor`; `.removed`: `a`, `b`, `actor` | friendships | acting identity's key — decided promised-durable; see [social-graph.md](./social-graph.md) |
| `guild.created` (done) | identity → guild | `guild_id`, `name`, `tag`, `description`, `owner` | guilds | founder key |
| `guild.updated` (done) | guild → guild | full replace, always all fields: `guild_id`, `name`, `tag`, `description`, `motd`, `banner`, `icon`, `links`, `recruiting`, `public`, `game_breakdown_public`, `join_policy`, `roster_visibility`, `actor` | guilds | acting officer's key |
| `guild.role_defined` (done) | identity → guild | `guild_id`, `name_index`, `name`, `permissions`, `description`, `badge` (`icon`/`color`), `actor` | role definitions | acting member's key |
| `guild.role_deleted` (done) | identity → guild | `guild_id`, `name_index`, `actor` | role definitions | acting member's key |
| `guild.member_added` (done) | guild → identity | `guild_id`, `identity_id`, `role_index`, `via` (`"invite"`/`"join_request"`/`"direct_join"`), `actor` | rosters, history | acting member's key |
| `guild.member_removed` (done) | guild → identity | `guild_id`, `identity_id`, `reason` (`"left"`/`"removed"`), `actor` | rosters, history | acting member's key |
| `guild.role_changed` (done) | guild → identity | `guild_id`, `identity_id`, `role_index`, `actor` | rosters, history | acting member's key |
| `guild.owner_transferred` (done) | guild → identity | `guild_id`, `from`, `to` | guilds | acting owner's key |
| `guild.game_associated` (done) | guild → integrator | `guild_id`, `game_id`, `actor` | associations | guild officer key |
| `guild.favorite_games_updated` (done) | guild → guild | `guild_id`, `game_ids`, `actor` | favorites | acting officer's key |
| `guild.channel_created` (done) | guild → channel | `guild_id`, `channel_id`, `name`, `actor` | channels | acting officer's key |
| `guild.channel_renamed` (done) | guild → channel | `guild_id`, `channel_id`, `name`, `announcement_only`, `topic`, `public` (#458), `actor` | channels | acting officer's key |
| `guild.channel_archived` (done) | guild → channel | `guild_id`, `channel_id`, `actor` | channels | acting officer's key |
| `game_schema.published` (done) | integrator → schema | `id`, `game_id`, `slug`, `version`, `proto_source`, `supersedes`, `default_visibility`, `field_visibility` | schema discovery (#255) | integrator key |
| `game_schema_mapping.published` (done) | integrator → mapping | `id`, `integrator_id`, `slug`, `from_schema_id`, `to_schema_id`, `description`, `field_correspondence` | mapping discovery (#491) | integrator key |
| `achievement.defined` (done) | integrator → achievement id | `id`, `game_id`, `slug`, `key`, `name`, `description`, `schema`, `icon`, `icon_url`, `version` | definitions | integrator key |
| `achievement.definition_updated` (done) | integrator → achievement id | same shape as `.defined` above | definitions | integrator key |
| `achievement.definition_retired` (done) | integrator → achievement id | `id`, `game_id`, `slug`, `key` | definitions | integrator key |
| `achievement.issued` (done) | integrator → identity | `id`, `issuer`, `subject`, `achievement`, `evidence`, `proof` (`key_id`/`algorithm`/`bytes`) | attestations | issuer key |
| `achievement.revoked` (done) | integrator → attestation | `id`, `attestation_id`, `issuer`, `reason_code`, `reason` | attestation status | issuer key |
| `milestone.defined` / `.definition_updated` / `.definition_retired` (done) | app/service → milestone id | same fields as the `achievement.*` rows above | definitions | app/service key |
| `milestone.issued` / `.revoked` (done) | app/service → identity / attestation | same fields as `achievement.issued`/`.revoked` above | attestations | issuer key |
| `attestation.superseded` | integrator → attestation | old ref, new ref | attestation status | issuer key |
| `game_event.result_issued` | integrator → identity | `achievement.issued` with the game-event schema | attestations, registry | issuer key |
| `integrator.recognition_published` (done) | integrator → integrator | `recognizer_id`, `recognized_id`, `scope` (claim-type strings) | recognition graph | integrator key |
| `integrator.recognition_revoked` (done) | integrator → integrator | `recognizer_id`, `recognized_id` | recognition graph | integrator key |

Conventions: `issuer` and `subject` are `GlobalId`s
(`crates/protocol/src/ids.rs`) — namespaced, e.g.
`game:ashen-realms:achievement:dragon_slayer`, so two integrators' `dragon_slayer`
never collide ([`./provenance.md`](./provenance.md)). Each kind has exactly one
payload schema per version. `achievement.*`/`milestone.*` share one payload
struct per row (`crate::event_payloads::Claim*Payload`) — the claim-vocabulary
split (#324/#325) is which *kind string* gets used, never a payload
difference.
