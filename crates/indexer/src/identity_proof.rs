//! Projection-time proof that an event may change an identity's keys or profile.
//!
//! Self-authenticating kinds (`identity.created`, `identity.signing_key_added`,
//! `identity.signing_key_revoked`) are checked against the signature they carry and the identity's
//! own key chain; the check does not depend on which shard delivered the event. The inception key
//! event is bound to the creation ticket's key id. Every other chained kind carries its author's
//! signature in its chain position and is verified against the author's active key, whichever shard
//! or node delivered it. Anything refused is returned as [`IndexError::Rejected`] before any table
//! is touched.

use avalon_protocol::ed25519_key::{parse_ed25519_public_key, verify_strict_signature};
use avalon_protocol::event_payloads::{
    FriendAcceptedPayload, FriendRelationshipReversedPayload, FriendRemovedPayload,
    FriendRequestedPayload, GuildMemberAddedPayload, GuildMemberRemovedPayload,
    GuildMembershipReversedPayload, IdentityCreatedPayload, IdentityRecoveredPayload,
    IdentityRecoveryConfiguredPayload, IdentitySigningKeyAddedPayload,
    IdentitySigningKeyRevokedPayload, RecoveryApprovalProof, SIGNING_KEY_KIND_DEVICE_GRANT,
    SIGNING_KEY_KIND_INCEPTION,
};
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_chain_wire::{
    chain_owner, event_hash, needs_author_signature, parse_hash, signer_of, verify_author_signature,
};
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes, display_name_permitted, identity_created_signing_bytes,
    recovery_approval_signing_bytes, recovery_request_signing_bytes,
    recovery_withdrawal_signing_bytes, signing_key_revoked_signing_bytes, IdentityId,
    RecoveryApprovalBinding,
};
use avalon_protocol::identity_keys::KeyAt;
use avalon_protocol::ids::GlobalId;
use avalon_protocol::ledger_entry::payload_hash;
use avalon_protocol::recovery_rules::{delay_elapsed, parse_hex32, required_approvals};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use std::collections::BTreeSet;

use crate::head_attestor::{chain_event_attestation, EntryAttestation, HeadAttestor};
use crate::identity_chain_store::{self, Supersession};
use crate::identity_keys::{self, AcceptedChain};
use crate::IndexError;

/// Where an event came from: the ledger stream (network and shard) it was read from, and whether
/// this node authored that stream itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventOrigin {
    pub network_id: String,
    pub shard_id: String,
    pub local: bool,
}

impl EventOrigin {
    /// An event authored by this node into its own shard.
    pub fn local(network_id: impl Into<String>, shard_id: impl Into<String>) -> Self {
        Self {
            network_id: network_id.into(),
            shard_id: shard_id.into(),
            local: true,
        }
    }

    /// An event read from another node's shard.
    pub fn mirrored(network_id: impl Into<String>, shard_id: impl Into<String>) -> Self {
        Self {
            network_id: network_id.into(),
            shard_id: shard_id.into(),
            local: false,
        }
    }
}

/// An `identity.created` event whose proof checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCreation {
    pub identity_id: IdentityId,
    /// The ticket id, which is the id of the inception signing key.
    pub ticket_id: Uuid,
    pub inception_key: [u8; 32],
    pub display_name: String,
}

/// What [`verify`] established about an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verified {
    Creation(VerifiedCreation),
    Other,
}

fn reject(reason: impl Into<String>) -> IndexError {
    IndexError::Rejected(reason.into())
}

/// The identity id embedded in an `identity:<id>:...` global id.
fn embedded_identity(id: &GlobalId) -> Option<IdentityId> {
    let mut parts = id.as_str().splitn(4, ':');
    if parts.next()? != "identity" {
        return None;
    }
    parts.next()?.parse().ok()
}

fn require_issuer_is(event: &ProtocolEvent, identity_id: IdentityId) -> Result<(), IndexError> {
    if embedded_identity(&event.issuer) == Some(identity_id)
        && embedded_identity(&event.subject) == Some(identity_id)
    {
        Ok(())
    } else {
        Err(reject(
            "issuer and subject do not name the identity the payload is about",
        ))
    }
}

fn decode_key(b64: &str) -> Result<[u8; 32], IndexError> {
    let raw = BASE64
        .decode(b64)
        .map_err(|_| reject("public key is not base64"))?;
    let key: [u8; 32] = raw
        .try_into()
        .map_err(|_| reject("public key is not 32 bytes"))?;
    parse_ed25519_public_key(&key).ok_or_else(|| reject("public key is not acceptable"))?;
    Ok(key)
}

fn decode_signature(b64: &str) -> Result<[u8; 64], IndexError> {
    BASE64
        .decode(b64)
        .ok()
        .and_then(|raw| <[u8; 64]>::try_from(raw).ok())
        .ok_or_else(|| reject("signature is not 64 bytes of base64"))
}

fn verify_with_key(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    parse_ed25519_public_key(key)
        .is_some_and(|key| verify_strict_signature(&key, message, signature))
}

/// Checks an `identity.created` event: v2 payload, an inception key that derives the id, issuer
/// and subject naming that id, a permitted display name, and the inception key's signature over
/// the bytes bound to `network_id`. The shard that delivered the event is not part of the proof.
pub fn verify_created(
    event: &ProtocolEvent,
    network_id: &str,
) -> Result<VerifiedCreation, IndexError> {
    if event.version != 2 {
        return Err(reject("identity.created must be version 2"));
    }
    let created: IdentityCreatedPayload = serde_json::from_value(event.payload.clone())
        .map_err(|_| reject("identity.created payload is malformed"))?;
    let key = decode_key(&created.public_key)?;
    if !created.identity_id.matches_key(&key) {
        return Err(reject("identity id is not derived from the inception key"));
    }
    require_issuer_is(event, created.identity_id)?;
    if !display_name_permitted(&created.display_name) {
        return Err(IndexError::DisplayNameNotPermitted);
    }
    let signature = decode_signature(&created.signature)?;
    let bytes = identity_created_signing_bytes(
        network_id,
        created.ticket_id,
        &created.identity_id,
        &key,
        &created.display_name,
    );
    if !verify_with_key(&key, &bytes, &signature) {
        return Err(reject(
            "identity.created signature does not verify for this network",
        ));
    }
    Ok(VerifiedCreation {
        identity_id: created.identity_id,
        ticket_id: created.ticket_id,
        inception_key: key,
        display_name: created.display_name,
    })
}

/// The chain position the event carries, which a key event's signature must cover.
fn signed_position(event: &ProtocolEvent) -> Result<(u64, Option<[u8; 32]>), IndexError> {
    let position = event
        .identity_chain
        .as_ref()
        .ok_or_else(|| reject("key event carries no identity chain position"))?;
    let prev_hash = position
        .prev_hash
        .as_deref()
        .map(|h| parse_hash(h).ok_or_else(|| reject("chain position prev_hash is malformed")))
        .transpose()?;
    Ok((position.seq, prev_hash))
}

/// The network a key event's signature is bound to: the network of the stream it was read from.
fn key_event_network(origin: Option<&EventOrigin>) -> Result<&str, IndexError> {
    origin
        .map(|o| o.network_id.as_str())
        .ok_or_else(|| reject("no origin to verify a key event"))
}

/// The inception key event must name the key id the creation ticket fixed, so no one can register
/// the identity's own key under another id.
async fn require_ticket_key(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
    signing_key_id: Uuid,
    key: &[u8; 32],
) -> Result<(), IndexError> {
    let stored: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT public_key FROM indexer_identity_signing_keys \
         WHERE identity_id = $1 AND signing_key_id = $2",
    )
    .bind(identity_id)
    .bind(signing_key_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(stored) = stored {
        return if stored.as_slice() == key.as_slice() {
            Ok(())
        } else {
            Err(reject("inception key id belongs to another key"))
        };
    }
    let created: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM indexer_identity_signing_keys WHERE identity_id = $1)",
    )
    .bind(identity_id)
    .fetch_one(&mut **tx)
    .await?;
    if created {
        Err(reject(
            "inception key id is not the id of the creation ticket",
        ))
    } else {
        Err(IndexError::AwaitingKey(
            "identity creation is not projected yet".to_string(),
        ))
    }
}

/// The public key `signing_key_id` of `identity_id` had at the chain position `seq` (extending
/// `prev_hash`), judged from the history that position builds on. Waits while that history is incomplete.
async fn judge_signer(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
    signing_key_id: Uuid,
    seq: u64,
    prev_hash: Option<&[u8; 32]>,
    role: &str,
) -> Result<[u8; 32], IndexError> {
    let chain = identity_keys::load_branch(tx, identity_id, seq, prev_hash).await?;
    identity_keys::require_valid(chain.key_at(signing_key_id, seq)?, role)
}

async fn verify_key_added(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
) -> Result<(), IndexError> {
    if event.version != 2 {
        return Err(reject("identity.signing_key_added must be version 2"));
    }
    let added: IdentitySigningKeyAddedPayload = serde_json::from_value(event.payload.clone())
        .map_err(|_| reject("identity.signing_key_added payload is malformed"))?;
    require_issuer_is(event, added.identity_id)?;
    let network_id = key_event_network(origin)?;
    let key = decode_key(&added.public_key)?;
    match added.kind.as_str() {
        SIGNING_KEY_KIND_INCEPTION => {
            if !added.identity_id.matches_key(&key) {
                return Err(reject("inception key does not derive the identity id"));
            }
            if event.identity_chain.is_some() {
                return Err(reject("the inception key event is not part of a chain"));
            }
            require_ticket_key(tx, added.identity_id, added.signing_key_id, &key).await?;
        }
        SIGNING_KEY_KIND_DEVICE_GRANT => {
            let (Some(grant_id), Some(signature)) = (added.grant_id, &added.approval_signature)
            else {
                return Err(reject("device grant carries no approval signature"));
            };
            let signature = decode_signature(signature)?;
            if added.signing_key_id != grant_id {
                return Err(reject("device key id is not the id of the grant"));
            }
            let (seq, prev_hash) = signed_position(event)?;
            let approver = judge_signer(
                tx,
                added.identity_id,
                added.approved_by_signing_key_id,
                seq,
                prev_hash.as_ref(),
                "approving",
            )
            .await?;
            let bytes = device_grant_approval_signing_bytes(
                network_id,
                grant_id,
                &added.identity_id,
                added.approved_by_signing_key_id,
                &key,
                seq,
                prev_hash.as_ref(),
            );
            if !verify_with_key(&approver, &bytes, &signature) {
                return Err(reject(
                    "device grant approval signature does not verify under the approving key",
                ));
            }
        }
        other => {
            return Err(reject(format!(
                "signing key kind {other:?} carries no verifiable proof"
            )));
        }
    }
    Ok(())
}

async fn verify_key_revoked(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
) -> Result<(), IndexError> {
    if event.version != 2 {
        return Err(reject("identity.signing_key_revoked must be version 2"));
    }
    let revoked: IdentitySigningKeyRevokedPayload =
        serde_json::from_value(event.payload.clone())
            .map_err(|_| reject("identity.signing_key_revoked payload is malformed"))?;
    require_issuer_is(event, revoked.identity_id)?;
    let network_id = key_event_network(origin)?;
    let signature = decode_signature(&revoked.signature)?;
    let (seq, prev_hash) = signed_position(event)?;
    let revoker = judge_signer(
        tx,
        revoked.identity_id,
        revoked.revoked_by_signing_key_id,
        seq,
        prev_hash.as_ref(),
        "revoking",
    )
    .await?;
    let bytes = signing_key_revoked_signing_bytes(
        network_id,
        &revoked.identity_id,
        revoked.signing_key_id,
        revoked.revoked_by_signing_key_id,
        seq,
        prev_hash.as_ref(),
    );
    if !verify_with_key(&revoker, &bytes, &signature) {
        return Err(reject(
            "revocation signature does not verify under the revoking key",
        ));
    }
    Ok(())
}

fn payload_as<T: serde::de::DeserializeOwned>(event: &ProtocolEvent) -> Result<T, IndexError> {
    serde_json::from_value(event.payload.clone())
        .map_err(|_| reject(format!("{} payload is malformed", event.kind)))
}

fn guild_of(id: &GlobalId) -> Option<Uuid> {
    let mut parts = id.as_str().splitn(4, ':');
    if parts.next()? != "guild" {
        return None;
    }
    parts.next()?.parse().ok()
}

/// Requires that the identities the projections act on are the signer or, for the explicit
/// admin kinds, named consistently: a signature by B must never change A's profile, passkeys,
/// friendships or roster rows.
fn require_binding(event: &ProtocolEvent, signer: IdentityId) -> Result<(), IndexError> {
    let own = |id: IdentityId| id == signer;
    let subject = embedded_identity(&event.subject);
    let bad = |what: &str| reject(format!("{} {what}", event.kind));
    match event.kind.as_str() {
        "profile.updated"
        | "identity.recovery_configured"
        | "identity.recovery_cancelled"
        | "identity.recovered"
        | "identity.passkey_registered"
        | "identity.passkey_revoked" => {
            if !subject.is_some_and(own) {
                return Err(bad("subject is not the signer"));
            }
            if event.kind == "identity.recovery_cancelled" {
                let by: Option<IdentityId> = event
                    .payload
                    .get("cancelled_by")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                if !by.is_some_and(own) {
                    return Err(bad("is not cancelled by the owner"));
                }
            }
            if event.kind.starts_with("identity.passkey_") {
                let p: serde_json::Value = event.payload.clone();
                let id: Option<IdentityId> = p
                    .get("identity_id")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                if !id.is_some_and(own) {
                    return Err(bad("payload names another identity"));
                }
            }
        }
        "friend.requested" => {
            let p: FriendRequestedPayload = payload_as(event)?;
            if !(own(p.from) && own(p.actor) && subject == Some(p.to) && p.to != p.from) {
                return Err(bad("is not authored by the requester"));
            }
        }
        "friend.accepted" => {
            let p: FriendAcceptedPayload = payload_as(event)?;
            if !(own(p.to) && own(p.actor) && subject == Some(p.from) && p.to != p.from) {
                return Err(bad("is not authored by the recipient"));
            }
        }
        "friend.removed" => {
            let p: FriendRemovedPayload = payload_as(event)?;
            let other = if own(p.a) { p.b } else { p.a };
            if !(own(p.actor) && (own(p.a) || own(p.b)) && p.a != p.b && subject == Some(other)) {
                return Err(bad("is not authored by a party to the friendship"));
            }
        }
        "friend.relationship_reversed" => {
            let p: FriendRelationshipReversedPayload = payload_as(event)?;
            if !(own(p.identity_id) && subject == Some(p.counterparty_id)) {
                return Err(bad("is not authored by the identity it reverses"));
            }
        }
        "guild.member_added" => {
            let p: GuildMemberAddedPayload = payload_as(event)?;
            // A join, invite acceptance or direct join is the member's own act; only a
            // join-request approval is an admin act naming another member.
            let own_act = p.via != "join_request";
            if !own(p.actor) || guild_of(&event.subject) != Some(p.guild_id) {
                return Err(bad("is not authored by its actor for its guild"));
            }
            if own_act && !own(p.identity_id) {
                return Err(bad("adds another identity outside an admin act"));
            }
        }
        "guild.member_removed" => {
            let p: GuildMemberRemovedPayload = payload_as(event)?;
            if !own(p.actor) || guild_of(&event.subject) != Some(p.guild_id) {
                return Err(bad("is not authored by its actor for its guild"));
            }
            if p.reason != "removed" && !own(p.identity_id) {
                return Err(bad("removes another identity outside an admin act"));
            }
        }
        "guild.membership_reversed" => {
            let p: GuildMembershipReversedPayload = payload_as(event)?;
            if !(own(p.identity_id) && guild_of(&event.subject) == Some(p.guild_id)) {
                return Err(bad("is not authored by the identity it reverses"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Checks a chained event's author signature: the event names its signer (the issuer's identity)
/// and key, the key is active in the signer's key chain, and its signature covers the event and
/// its chain position on this network. An unknown key defers the event.
async fn verify_author(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
) -> Result<(), IndexError> {
    let network_id = key_event_network(origin)?;
    let signer = signer_of(event).ok_or_else(|| reject("issuer is not an identity"))?;
    let key_id = event
        .identity_chain
        .as_ref()
        .and_then(|p| p.signing_key_id)
        .ok_or_else(|| reject(format!("{} carries no author signature", event.kind)))?;
    require_binding(event, signer)?;
    let (seq, prev_hash) = signed_position(event)?;
    let key = judge_signer(tx, signer, key_id, seq, prev_hash.as_ref(), "authoring").await?;
    match verify_author_signature(event, network_id, &key) {
        Ok(true) => Ok(()),
        Ok(false) => Err(reject(format!(
            "{} author signature does not verify under the authoring key",
            event.kind
        ))),
        Err(_) => Err(reject(format!(
            "{} author signature is malformed",
            event.kind
        ))),
    }
}

/// A proof's position hash: absent, or 64 lowercase hex characters.
fn position_hash(prev_hash: &Option<String>) -> Option<Option<[u8; 32]>> {
    match prev_hash {
        None => Some(None),
        Some(text) => parse_hex32(text).map(Some),
    }
}

/// The identity id in an `identity:<id>:...` global id string.
fn identity_of_ref(text: &str) -> Option<IdentityId> {
    let mut parts = text.splitn(4, ':');
    if parts.next()? != "identity" {
        return None;
    }
    parts.next()?.parse().ok()
}

/// The ledger entry an accepted chain event sits in (see [`chain_event_attestation`]).
async fn attest_chain_event(
    tx: &mut Transaction<'_, Postgres>,
    heads: &dyn HeadAttestor,
    event: &ProtocolEvent,
) -> Result<EntryAttestation, IndexError> {
    chain_event_attestation(tx, heads, event)
        .await?
        .ok_or_else(|| {
            IndexError::AwaitingKey(format!("{} is not in a ledger entry yet", event.kind))
        })
}

/// Waits until cosigned heads cover the entry and the latest cosigned head is a full recovery delay
/// later than the one that first covered it, both by witness-signed quorum time.
fn require_old_enough(entry: &EntryAttestation, what: &str) -> Result<(), IndexError> {
    match (entry.anchor_at, entry.reference_at) {
        (Some(anchor), Some(latest)) if delay_elapsed(anchor, latest) => Ok(()),
        (Some(_), Some(_)) => Err(IndexError::AwaitingKey(format!(
            "{what} is not older than the recovery delay yet"
        ))),
        _ => Err(IndexError::AwaitingKey(format!(
            "{what} is not covered by a cosigned tree head yet"
        ))),
    }
}

/// What a recovery's guardian approvals must all bind.
struct ApprovalContext<'a> {
    heads: &'a dyn HeadAttestor,
    network_id: &'a str,
    owner: IdentityId,
    request_id: Uuid,
    new_key: &'a [u8; 32],
    request_entry_hash: &'a [u8; 32],
    config_event_hash: &'a [u8; 32],
}

enum Counted {
    Yes,
    No,
    Awaiting,
}

/// Whether the guardian's key signed the approval at a position its own accepted chain extends, and no
/// valid withdrawal rides with it. A guardian chain not accepted far enough yet leaves it undecided.
async fn check_approval(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ApprovalContext<'_>,
    proof: &RecoveryApprovalProof,
) -> Result<Counted, IndexError> {
    let Some(prev) = position_hash(&proof.prev_hash) else {
        return Ok(Counted::No);
    };
    let chain = match identity_keys::load(tx, Some(ctx.heads), proof.guardian_id).await {
        Ok(chain) => chain,
        Err(IndexError::AwaitingKey(_)) => return Ok(Counted::Awaiting),
        Err(other) => return Err(other),
    };
    let Some(key) = guardian_key(&chain, proof.signing_key_id, proof.seq, prev.as_ref())? else {
        return Ok(Counted::Awaiting);
    };
    let Some(key) = key else {
        return Ok(Counted::No);
    };
    // A key the guardian's chain has since revoked no longer vouches for anything.
    if chain.is_revoked(proof.signing_key_id) {
        return Ok(Counted::No);
    }
    let bytes = recovery_approval_signing_bytes(
        ctx.network_id,
        &ctx.owner,
        ctx.request_id,
        &proof.guardian_id,
        proof.signing_key_id,
        ctx.new_key,
        &RecoveryApprovalBinding {
            request_entry_hash: ctx.request_entry_hash,
            config_event_hash: ctx.config_event_hash,
            guardian_seq: proof.seq,
            guardian_prev_hash: prev.as_ref(),
        },
    );
    let signed = decode_signature(&proof.signature)
        .is_ok_and(|signature| verify_with_key(&key, &bytes, &signature));
    if !signed {
        return Ok(Counted::No);
    }
    let Some(withdrawal) = &proof.withdrawal else {
        return Ok(Counted::Yes);
    };
    let Some(withdrawn_prev) = position_hash(&withdrawal.prev_hash) else {
        return Ok(Counted::Yes);
    };
    let Some(withdrawer) = guardian_key(
        &chain,
        withdrawal.signing_key_id,
        withdrawal.seq,
        withdrawn_prev.as_ref(),
    )?
    else {
        return Ok(Counted::Awaiting);
    };
    let Some(withdrawer) = withdrawer else {
        return Ok(Counted::Yes);
    };
    let bytes = recovery_withdrawal_signing_bytes(
        ctx.network_id,
        &ctx.owner,
        ctx.request_id,
        &proof.guardian_id,
        withdrawal.signing_key_id,
        withdrawal.seq,
        withdrawn_prev.as_ref(),
    );
    let withdrawn = decode_signature(&withdrawal.signature)
        .is_ok_and(|signature| verify_with_key(&withdrawer, &bytes, &signature));
    Ok(if withdrawn { Counted::No } else { Counted::Yes })
}

/// The guardian's key at a signed position: `Ok(None)` while the guardian's chain is not accepted far
/// enough, `Ok(Some(None))` when the position or key is not valid, `Ok(Some(Some(key)))` when it is.
fn guardian_key(
    chain: &AcceptedChain,
    signing_key_id: Uuid,
    seq: u64,
    prev_hash: Option<&[u8; 32]>,
) -> Result<Option<Option<[u8; 32]>>, IndexError> {
    match chain.require_extends(seq, prev_hash) {
        Ok(()) => {}
        Err(IndexError::AwaitingKey(_)) => return Ok(None),
        Err(IndexError::Rejected(_)) => return Ok(Some(None)),
        Err(other) => return Err(other),
    }
    Ok(Some(match chain.key_at(signing_key_id, seq)? {
        KeyAt::Valid(key) => Some(key),
        _ => None,
    }))
}

/// Checks `identity.recovered`: new-key signature, no owner cancel, the current configuration and request
/// marker covered a delay by cosigned heads, and threshold approvals valid at their guardians' positions.
async fn verify_recovered(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
    heads: Option<&dyn HeadAttestor>,
) -> Result<(), IndexError> {
    let network_id = key_event_network(origin)?;
    let owner = chain_owner(event).ok_or_else(|| reject("issuer is not an identity"))?;
    require_binding(event, owner)?;
    let payload: IdentityRecoveredPayload = serde_json::from_value(event.payload.clone())
        .map_err(|_| reject("identity.recovered payload is malformed"))?;
    let new_key = decode_key(&payload.new_signing_public_key)?;
    let signed_by = event.identity_chain.as_ref().and_then(|p| p.signing_key_id);
    if signed_by != Some(payload.request_id) {
        return Err(reject("identity.recovered is not signed by the new key"));
    }
    if !matches!(
        verify_author_signature(event, network_id, &new_key),
        Ok(true)
    ) {
        return Err(reject(
            "identity.recovered does not verify under the new key",
        ));
    }

    let request = &payload.request;
    let config_hash = parse_hex32(&request.config_event_hash)
        .ok_or_else(|| reject("recovery request names a malformed configuration hash"))?;
    let entry_hash = parse_hex32(&payload.request_entry_hash)
        .ok_or_else(|| reject("recovery request entry hash is malformed"))?;
    if request.request_id != payload.request_id
        || request.new_signing_public_key != payload.new_signing_public_key
    {
        return Err(reject("the embedded request is not this recovery's"));
    }
    let request_bytes = recovery_request_signing_bytes(
        network_id,
        &owner,
        payload.request_id,
        &new_key,
        &config_hash,
    );
    if !decode_signature(&request.signature)
        .is_ok_and(|signature| verify_with_key(&new_key, &request_bytes, &signature))
    {
        return Err(reject("recovery request is not signed by the new key"));
    }

    let (seq, prev_hash) = signed_position(event)?;
    let chain = identity_keys::load_beside(tx, heads, owner, Some(event)).await?;
    // A recovery is single-use: its request id and its new key can never be installed twice, so
    // a stolen recovery key cannot replay an old recovered at a later head.
    let consumed = chain.events.iter().any(|e| {
        e.id != event.id
            && e.kind == "identity.recovered"
            && e.payload.get("request_id") == Some(&serde_json::json!(payload.request_id))
    });
    let key_known: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM indexer_identity_signing_keys \
         WHERE identity_id = $1 AND (signing_key_id = $2 OR public_key = $3))",
    )
    .bind(owner)
    .bind(payload.request_id)
    .bind(new_key.as_slice())
    .fetch_one(&mut **tx)
    .await?;
    let this_hash = event_hash(event).map_err(|_| reject("identity.recovered cannot be hashed"))?;
    let redelivered = chain
        .events
        .iter()
        .any(|e| e.id == event.id && event_hash(e).ok() == Some(this_hash));
    if key_known && redelivered {
        // The same recovery redelivered from another shard.
        return Ok(());
    }
    if consumed || key_known {
        return Err(reject(
            "identity.recovered reuses a consumed recovery request or an existing key",
        ));
    }
    chain.require_extends(seq, prev_hash.as_ref())?;
    // Only the accepted head (the last unforked one) can be extended, so no recovery sits below an
    // owner event it would otherwise skip.
    if seq != chain.head_seq() + 1 {
        return Err(reject("identity.recovered must extend the accepted head"));
    }

    let vetoed = chain.events.iter().any(|e| {
        e.kind == "identity.recovery_cancelled"
            && e.payload.get("request_id") == Some(&serde_json::json!(payload.request_id))
    });
    if vetoed {
        return Err(reject("the owner cancelled this recovery request"));
    }
    let config_event = chain
        .events
        .iter()
        .rfind(|e| e.kind == "identity.recovery_configured")
        .ok_or_else(|| reject("the owner has no recovery configuration below the recovery"))?;
    if event_hash(config_event).ok() != Some(config_hash) {
        return Err(reject(
            "the request relies on a configuration that is not the owner's current one",
        ));
    }
    let configured: IdentityRecoveryConfiguredPayload =
        serde_json::from_value(config_event.payload.clone())
            .map_err(|_| reject("recovery configuration is malformed"))?;

    let heads =
        heads.ok_or_else(|| reject("this node has no cosigned head source for recoveries"))?;
    let config_entry = attest_chain_event(tx, heads, config_event).await?;
    let marker = heads
        .by_entry_hash(tx, &payload.request_entry_hash)
        .await?
        .ok_or_else(|| {
            IndexError::AwaitingKey("the recovery request entry is not held yet".into())
        })?;
    let marker_hash = payload_hash(
        &serde_json::to_value(request).map_err(|_| reject("request is not serializable"))?,
    )
    .map_err(|_| reject("request payload is not canonical"))?;
    if marker.kind != "identity.recovery_requested"
        || marker.payload_hash != hex::encode(marker_hash)
        || identity_of_ref(&marker.subject) != Some(owner)
    {
        return Err(reject(
            "the request entry is not this recovery's request marker",
        ));
    }

    let ctx = ApprovalContext {
        heads,
        network_id,
        owner,
        request_id: payload.request_id,
        new_key: &new_key,
        request_entry_hash: &entry_hash,
        config_event_hash: &config_hash,
    };
    let mut judged = BTreeSet::new();
    let mut approved = BTreeSet::new();
    let mut awaiting = false;
    for proof in &payload.approvals {
        // Only a guardian's first proof is judged, so repeats cannot multiply the chain loads.
        if !configured.guardian_ids.contains(&proof.guardian_id)
            || !judged.insert(proof.guardian_id)
        {
            continue;
        }
        match check_approval(tx, &ctx, proof).await? {
            Counted::Yes => {
                approved.insert(proof.guardian_id);
            }
            Counted::No => {}
            Counted::Awaiting => awaiting = true,
        }
    }
    if approved.len() < required_approvals(configured.threshold) {
        return Err(if awaiting {
            IndexError::AwaitingKey("a guardian chain is not accepted far enough yet".to_string())
        } else {
            reject("identity.recovered carries fewer valid guardian approvals than the threshold")
        });
    }
    require_old_enough(&config_entry, "the recovery configuration")?;
    require_old_enough(&marker, "the recovery request")?;
    Ok(())
}

/// Waits when a revocation of the event's signing key is on the ledger no later than the event, or
/// when either is not dated by a cosigned head yet: revocation is final. The same rule resolves the
/// chain, so an event kept out here stays out when it is delivered in any other order.
async fn require_not_superseded(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    heads: Option<&dyn HeadAttestor>,
) -> Result<(), IndexError> {
    match identity_chain_store::supersession(tx, heads, event).await? {
        Supersession::Stands => Ok(()),
        Supersession::Superseded => Err(IndexError::AwaitingKey(format!(
            "{} is signed by a key revoked on the ledger no later than it",
            event.kind
        ))),
        Supersession::Pending => Err(IndexError::AwaitingKey(format!(
            "{} and a revocation of its key are not both dated by cosigned heads yet",
            event.kind
        ))),
    }
}

/// Verifies `event` before any table (including the identity chain) is touched.
/// `origin` is `None` when the indexer was built without a local origin: nothing that needs one
/// is accepted then.
pub async fn verify(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
    heads: Option<&dyn HeadAttestor>,
) -> Result<Verified, IndexError> {
    let verified = match event.kind.as_str() {
        "identity.created" => {
            let origin = origin.ok_or_else(|| reject("no origin to verify identity.created"))?;
            return verify_created(event, &origin.network_id).map(Verified::Creation);
        }
        "identity.signing_key_added" => verify_key_added(tx, event, origin).await,
        "identity.signing_key_revoked" => verify_key_revoked(tx, event, origin).await,
        "identity.recovered" => verify_recovered(tx, event, origin, heads).await,
        kind if needs_author_signature(kind) => verify_author(tx, event, origin).await,
        _ => return Ok(Verified::Other),
    };
    verified?;
    if event.identity_chain.is_some() && chain_owner(event).is_some() {
        require_not_superseded(tx, event, heads).await?;
    }
    Ok(Verified::Other)
}

#[cfg(test)]
mod tests {
    use avalon_protocol::identity_id::{TestIdentity, TEST_NETWORK_ID};
    use time::OffsetDateTime;

    use super::*;

    fn created_event(who: &TestIdentity, payload: IdentityCreatedPayload) -> ProtocolEvent {
        let gid = GlobalId::new("identity", &who.id.to_string(), "self", "created");
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: gid.clone(),
            subject: gid,
            payload: serde_json::to_value(payload).unwrap(),
            timestamp: OffsetDateTime::now_utc(),
            version: 2,
            identity_chain: None,
        }
    }

    #[test]
    fn a_correctly_signed_creation_verifies() {
        let who = TestIdentity::new();
        let event = created_event(&who, who.created_payload("Ada"));
        let verified = verify_created(&event, TEST_NETWORK_ID).unwrap();
        assert_eq!(verified.identity_id, who.id);
        assert_eq!(verified.display_name, "Ada");
    }

    #[test]
    fn a_creation_signed_for_another_network_is_refused_but_any_shard_verifies() {
        let who = TestIdentity::new();
        let event = created_event(&who, who.created_payload("Ada"));
        assert!(verify_created(&event, "another-network").is_err());
        assert!(verify_created(&event, TEST_NETWORK_ID).is_ok());
    }

    #[test]
    fn a_creation_with_a_forged_signature_or_foreign_key_is_refused() {
        let who = TestIdentity::new();
        let attacker = TestIdentity::new();
        // Right id and key, signature by another key.
        let mut forged = who.created_payload("Ada");
        forged.signature = attacker.created_payload("Ada").signature;
        assert!(verify_created(&created_event(&who, forged), TEST_NETWORK_ID).is_err());
        // The attacker's own key under the victim's id.
        let mut swapped = attacker.created_payload("Ada");
        swapped.identity_id = who.id;
        assert!(verify_created(&created_event(&who, swapped), TEST_NETWORK_ID).is_err());
        // A name altered after signing.
        let mut renamed = who.created_payload("Ada");
        renamed.display_name = "Mallory".to_string();
        assert!(verify_created(&created_event(&who, renamed), TEST_NETWORK_ID).is_err());
    }

    #[test]
    fn a_creation_signed_by_a_key_that_does_not_derive_the_id_is_refused() {
        use ed25519_dalek::Signer as _;
        let victim = TestIdentity::new();
        let attacker = TestIdentity::new();
        // The attacker signs, with their own key, bytes that name the victim's id.
        let ticket = Uuid::new_v4();
        let bytes = identity_created_signing_bytes(
            TEST_NETWORK_ID,
            ticket,
            &victim.id,
            &attacker.public_key(),
            "Ada",
        );
        let mut payload = victim.created_payload("Ada");
        payload.ticket_id = ticket;
        payload.public_key = BASE64.encode(attacker.public_key());
        payload.signature = BASE64.encode(attacker.signing_key.sign(&bytes).to_bytes());
        let err = verify_created(&created_event(&victim, payload), TEST_NETWORK_ID).unwrap_err();
        assert!(err.to_string().contains("not derived"), "{err}");
    }

    #[test]
    fn a_creation_whose_envelope_names_another_identity_is_refused() {
        let who = TestIdentity::new();
        let victim = TestIdentity::new();
        let mut event = created_event(&who, who.created_payload("Ada"));
        event.issuer = GlobalId::new("identity", &victim.id.to_string(), "self", "created");
        assert!(verify_created(&event, TEST_NETWORK_ID).is_err());
    }
}
