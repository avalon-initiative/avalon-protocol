# Protocol Events: Kind Catalogue

The full, row-by-row list of `ProtocolEvent` kinds for
[`./protocol-events.md`](./protocol-events.md) — pulled into its own file so
that document can stay focused on the design properties and versioning
policy around events, rather than being interrupted by a ~40-row table.

Naming: `<domain>.<past-tense-verb>`. This table is the starting point that
[#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) finalizes;
until then it is proposed, not normative. "Network" as signer means the node
records the fact on behalf of an authenticated actor and is the
milestone-1 stand-in until actor signatures exist.

| Kind | Issuer → subject | Payload (canonical) | Drives | Signed by |
|---|---|---|---|---|
| `identity.created` | identity → identity | identity id | identities | identity's Ed25519 event-signing key (#73, done) |
| `identity.signing_key_added` | identity → identity | new signing key id/public key, device label, approving key id | identity signing keys | the approving device's key (#135, done) |
| `identity.signing_key_revoked` | identity → identity | revoked signing key id | identity signing keys | network (milestone-1 stand-in, #135, done) |
| `identity.recovery_configured` | identity → identity | guardian ids, threshold | recovery guardian settings | identity key (session-authenticated, #201, done) |
| `identity.recovery_requested` | identity → identity | request id, threshold | recovery requests | network (milestone-1 stand-in — the requester by definition has no session; #201, done) |
| `identity.recovery_approved` | identity (guardian) → identity | request id, guardian id, approvals count, threshold, delay end | recovery requests/approvals | network (milestone-1 stand-in, #201, done) |
| `identity.recovery_cancelled` | identity (owner or guardian) → identity | request id, cancelled by, reason | recovery requests | network (milestone-1 stand-in, #201, done) |
| `identity.recovered` | identity → identity | request id, new device label | identity keys | network (milestone-1 stand-in, #201, done) |
| `profile.updated` | identity → identity | changed promised-durable fields (`display_name`, `discriminator`, `avatar_url`, `bio`, `favorite_genres`, `pronouns`) | profiles | identity key |
| `game.registered` | integrator → integrator | slug, name, developer, requested capabilities, initial key | integrators, registry | integrator key |
| `game.binding_established` | identity → integrator | identity, integrator | bindings, registry identities | identity key |
| `game.binding_ended` | identity → integrator | binding ref | bindings | identity key |
| `permission.granted` | identity → integrator (per capability) | binding, capability | permission grants | identity key |
| `permission.revoked` | identity → integrator (per capability) | binding, capability, reason | permission grants | identity key |
| `issuer.registered` | issuer → issuer | issuer id, initial key set | issuers | issuer key |
| `issuer.key_added` | issuer → issuer | key id, public key, algorithm, validity window | issuer keys | existing issuer key |
| `issuer.key_revoked` | issuer → issuer | key id, reason (`rotated`, `compromised`, …) | issuer keys | issuer key |
| `issuer.key_expired` | issuer → issuer | key id | issuer keys | issuer key or network |
| `issuer.suspended` / `.reinstated` / `.revoked` / `.deprecated` | network or issuer → issuer | reason, effective at | issuer status | operator (audited) or issuer |
| `friend.requested` / `.accepted` / `.removed` | identity → identity | the two identities, actor | friendships | acting identity's key — decided promised-durable; see [social-graph.md](./social-graph.md) |
| `guild.created` | identity → guild | name, tag, description, founder | guilds | founder key |
| `guild.updated` | guild → guild | changed fields (motd, banner, links, recruiting, join_policy, ...) | guilds | acting officer's key |
| `guild.role_defined` / `.role_deleted` | identity → guild | name_index, name, permissions, description, badge (icon, color), actor | role definitions | acting member's key |
| `guild.member_added` / `.member_removed` | guild → identity | role, actor | rosters, history | acting member's key |
| `guild.role_changed` | guild → identity | old role, new role, actor | rosters, history | acting member's key |
| `guild.owner_transferred` | guild → identity | old owner, new owner, actor | guilds | acting owner's key |
| `guild.game_associated` | guild → integrator | guild, integrator | associations | guild officer key |
| `guild.favorite_games_updated` | guild → guild | favorited game ids, actor | favorites | acting officer's key |
| `guild.channel_created` / `.channel_renamed` / `.channel_archived` | guild → channel | channel id, name, actor | channels | acting officer's key |
| `game_schema.published` | integrator → schema | integrator id, version, `.proto` source, superseded_by | schema discovery (#255) | integrator key |
| `achievement.defined` | integrator → achievement id | name, description, schema | definitions | integrator key |
| `achievement.definition_updated` | integrator → achievement id | name, description, schema, version | definitions | integrator key |
| `achievement.definition_retired` | integrator → achievement id | achievement id | definitions | integrator key |
| `achievement.issued` | integrator → identity | achievement id, attestation id, evidence ref | attestations | issuer key |
| `achievement.revoked` | integrator → attestation | attestation ref, reason code, reason | attestation status | issuer key |
| `milestone.defined` / `.definition_updated` / `.definition_retired` | app/service → milestone id | same fields as the `achievement.*` row above | definitions | app/service key |
| `milestone.issued` / `.revoked` | app/service → identity / attestation | same fields as `achievement.issued`/`.revoked` above | attestations | issuer key |
| `attestation.superseded` | integrator → attestation | old ref, new ref | attestation status | issuer key |
| `game_event.result_issued` | integrator → identity | `achievement.issued` with the game-event schema | attestations, registry | issuer key |
| `recognition.published` | integrator → issuer | recognized claim types / scopes | recognition graph | integrator key |

Conventions: `issuer` and `subject` are `GlobalId`s
(`crates/protocol/src/ids.rs`) — namespaced, e.g.
`game:ashen-realms:achievement:dragon_slayer`, so two integrators' `dragon_slayer`
never collide ([`./provenance.md`](./provenance.md)). Each kind has exactly one
payload schema per version.
