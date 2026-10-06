# Ledger payload field inventory

Every field of every ledger payload type, classified against decision #1299 (inventory ticket #1318). The machine-checked source is `crates/protocol/data/payload-field-classification.json`; `cargo test -p avalon-protocol --test payload_field_inventory` fails when a payload struct in `event_payloads.rs` (or `GuildLink`) gains, loses or renames a field without an entry, and when a content or personal-data field is missing from the table below.

Changing what a payload carries is a contract change (signing bytes, vectors, SDKs), so nothing here alters a wire format; the dispositions are proposals feeding the freeze list (#1156).

## Classes

- `authority_state`: keys, membership, roles, policy flags, revocations. Stays inline in full.
- `pseudonymous_identifier`: random or self-certifying ids and handles. Stays inline.
- `content`: labels and free text that are not about one person (guild names, catalogue text). Needs a disposition.
- `personal_data`: data about or chosen by a person (display name, device label, instance data). Needs a disposition.

## Dispositions

- `commitment`: move behind a committed object, salted and removable (#1313, #1314, #1315).
- `inline_redactable`: stays inline; a signed redaction makes conforming nodes drop the payload (#1316).
- `keep_inline`: bounded, non-free-text value (enum, colour code); no change.
- `remove`: leaves the ledger; kept node-local or dropped.

## Summary

| Class | Fields |
| --- | --- |
| authority_state | 53 |
| pseudonymous_identifier | 140 |
| content | 41 |
| personal_data | 18 |

| Disposition (content and personal data only) | Fields |
| --- | --- |
| commitment | 25 |
| inline_redactable | 23 |
| remove | 5 |
| keep_inline | 6 |

Not ledger payloads, so not inventoried here: node addresses and peer lists (known-list, node requests, witness announcements) are node-to-node messages, not ledger events. The kind to payload type mapping is in the JSON.

## Content and personal-data fields

| Field | Class | Disposition | Reason |
| --- | --- | --- | --- |
| `IdentityCreatedPayload.display_name` | personal_data | commitment | Free-text name of a person; leaves public events for an owner-signed salted profile object. (#1125 #1314) |
| `IdentitySigningKeyAddedPayload.device_label` | personal_data | remove | Free-text device name (e.g. a person's phone) is not authority state; keep it node-local or in the committed profile object. (#1306) |
| `IdentityPasskeyRegisteredPayload.credential_id` | personal_data | remove | Stable per-device credential handle; passkeys become node-local and authorised by the signing key rather than mirrored credential data. (#1306) |
| `IdentityPasskeyRegisteredPayload.passkey_data` | personal_data | remove | Serialized WebAuthn credential (public key, counter, backup flags) fingerprints a device and is not portable authority. (#1306) |
| `IdentityPasskeyRegisteredPayload.label` | personal_data | remove | Free-text device name; same as device_label. (#1306) |
| `IdentityRecoveryCancelledPayload.reason` | content | inline_redactable | Optional free-text explanation; make it a bounded code or keep it inline and redactable. (#1316) |
| `IdentityRecoveredPayload.device_label` | personal_data | remove | Free-text device name; same as device_label on key events. (#1306) |
| `ProfileUpdatedPayload.display_name` | personal_data | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.avatar_url` | personal_data | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. A URL also tells a fetcher's address to a third party. (#1125 #1314) |
| `ProfileUpdatedPayload.bio` | personal_data | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.favorite_genres` | content | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.pronouns` | personal_data | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.banner_url` | personal_data | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.status` | content | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.links` | personal_data | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.timezone` | personal_data | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.theme_color` | content | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `ProfileUpdatedPayload.location` | personal_data | commitment | Profile field; the whole profile moves behind the owner-signed salted commitment. (#1125 #1314) |
| `GameRegisteredPayload.name` | content | inline_redactable | Integrator-defined display text; stays inline and removable. (#1316) |
| `GameRegisteredPayload.developer` | personal_data | inline_redactable | Can name a person or carry contact details; stays inline and removable (or move behind an integrator profile commitment). (#1316) |
| `PermissionRevokedPayload.reason` | content | inline_redactable | Optional free-text explanation; make it a bounded code or keep it inline and redactable. (#1316) |
| `IssuerKeyRevokedPayload.reason` | content | inline_redactable | Optional free-text explanation; make it a bounded code or keep it inline and redactable. (#1316) |
| `GuildCreatedPayload.name` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildCreatedPayload.tag` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildCreatedPayload.description` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildLink.label` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildLink.url` | personal_data | commitment | Guild metadata is a non-unique label; moves into the committed guild object. A URL can point at a person's own pages. (#1315) |
| `GuildUpdatedPayload.name` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildUpdatedPayload.tag` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildUpdatedPayload.description` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildUpdatedPayload.motd` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildUpdatedPayload.banner` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildUpdatedPayload.icon` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildUpdatedPayload.links` | content | commitment | Guild metadata is a non-unique label; moves into the committed guild object. (#1315) |
| `GuildRoleBadgePayload.icon` | content | keep_inline | Bounded cosmetic identifier, not free text. |
| `GuildRoleBadgePayload.color` | content | keep_inline | Bounded cosmetic value (colour code). |
| `GuildRoleDefinedPayload.name` | content | inline_redactable | Free-text role label on the guild chain; inline and removable by a moderator. (#1316) |
| `GuildRoleDefinedPayload.description` | content | inline_redactable | Free text on the guild chain; inline and removable by a moderator. (#1316) |
| `GuildRoleDefinedPayload.badge` | content | keep_inline | Nested badge payload, classified separately. |
| `GuildMemberRemovedPayload.reason` | content | inline_redactable | Free-text removal reason about a person; make it a bounded code or keep it inline and redactable. (#1316) |
| `GuildChannelCreatedPayload.name` | content | inline_redactable | Free-text channel label on the guild chain; inline and removable by a moderator. (#1316) |
| `GuildChannelRenamedPayload.name` | content | inline_redactable | Free-text channel label on the guild chain; inline and removable by a moderator. (#1316) |
| `GuildChannelRenamedPayload.topic` | content | inline_redactable | Free-text channel topic; inline and removable by a moderator. (#1316) |
| `GameSchemaPublishedPayload.proto_source` | content | inline_redactable | Integrator-authored schema text (comments and names are free text); inline and removable, with the validation consequence tracked in #1316. (#1316) |
| `GameSchemaMappingPublishedPayload.description` | content | inline_redactable | Integrator-authored free text; inline and removable. (#1316) |
| `GameSchemaMappingPublishedPayload.field_correspondence` | content | keep_inline | Structural field-name mapping that the mapping logic reads; bounded by the schemas, not prose. |
| `GameDataPublishedPayload.instance` | personal_data | inline_redactable | Integrator-defined data about a person (arbitrary JSON); inline and removable, and private-visibility schemas should not be public events at all. (#1316) |
| `GameDataDeletedPayload.reason` | content | inline_redactable | Optional free-text explanation; make it a bounded code or keep it inline and redactable. (#1316) |
| `ClaimDefinedPayload.name` | content | inline_redactable | Integrator-defined catalogue text; stays inline and removable. (#1316) |
| `ClaimDefinedPayload.description` | content | inline_redactable | Integrator-defined catalogue text; stays inline and removable. (#1316) |
| `ClaimDefinedPayload.icon` | content | keep_inline | Bounded cosmetic identifier, not free text. |
| `ClaimDefinedPayload.icon_url` | content | inline_redactable | External URL that third parties can use to track fetchers; inline and removable. (#1316) |
| `ClaimDefinitionUpdatedPayload.name` | content | inline_redactable | Integrator-defined catalogue text; stays inline and removable. (#1316) |
| `ClaimDefinitionUpdatedPayload.description` | content | inline_redactable | Integrator-defined catalogue text; stays inline and removable. (#1316) |
| `ClaimDefinitionUpdatedPayload.icon` | content | keep_inline | Bounded cosmetic identifier, not free text. |
| `ClaimDefinitionUpdatedPayload.icon_url` | content | inline_redactable | External URL that third parties can use to track fetchers; inline and removable. (#1316) |
| `ClaimIssuedPayload.evidence` | personal_data | inline_redactable | Integrator-defined JSON (up to 4 KB) about a person; inline and removable. (#1316) |
| `ClaimRevokedPayload.reason` | content | inline_redactable | Free-text revocation reason about a person's claim; inline and removable. (#1316) |
| `ProfileUpdatedPayload.main_guild` | content | commitment | Not free text, but it is a profile preference and rides in the same committed object. (#1314) |
