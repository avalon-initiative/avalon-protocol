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
    IdentityCreatedPayload, IdentityRecoveredPayload, IdentityRecoveryConfiguredPayload,
    IdentitySigningKeyAddedPayload, IdentitySigningKeyRevokedPayload,
    SIGNING_KEY_KIND_DEVICE_GRANT, SIGNING_KEY_KIND_INCEPTION,
};
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_chain_wire::{
    chain_owner, needs_author_signature, parse_hash, signer_of, verify_author_signature,
};
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes, display_name_permitted, identity_created_signing_bytes,
    recovery_approval_signing_bytes, signing_key_revoked_signing_bytes, IdentityId,
};
use avalon_protocol::ids::GlobalId;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use std::collections::BTreeSet;

use crate::{identity_chain_store, IndexError};

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

/// A key of an identity as the projection knows it.
enum KeyState {
    Active([u8; 32]),
    Revoked,
    Unknown,
}

async fn key_state(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
    signing_key_id: Uuid,
) -> Result<KeyState, IndexError> {
    let row = sqlx::query(
        "SELECT public_key, revoked_at IS NOT NULL AS revoked FROM indexer_identity_signing_keys \
         WHERE identity_id = $1 AND signing_key_id = $2",
    )
    .bind(identity_id)
    .bind(signing_key_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(KeyState::Unknown);
    };
    if row.try_get::<bool, _>("revoked")? {
        return Ok(KeyState::Revoked);
    }
    let raw: Vec<u8> = row.try_get("public_key")?;
    Ok(<[u8; 32]>::try_from(raw).map_or(KeyState::Revoked, KeyState::Active))
}

/// The active key a proof names. A key not projected yet defers the event (it may still arrive,
/// possibly from another shard); a revoked one refuses it.
async fn signer_key(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
    signing_key_id: Uuid,
    role: &str,
) -> Result<[u8; 32], IndexError> {
    match key_state(tx, identity_id, signing_key_id).await? {
        KeyState::Active(key) => Ok(key),
        KeyState::Revoked => Err(reject(format!("{role} key is revoked"))),
        KeyState::Unknown => Err(IndexError::AwaitingKey(format!(
            "{role} key is not projected for this identity yet"
        ))),
    }
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
            let approver = signer_key(
                tx,
                added.identity_id,
                added.approved_by_signing_key_id,
                "approving",
            )
            .await?;
            if added.signing_key_id != grant_id {
                return Err(reject("device key id is not the id of the grant"));
            }
            let (seq, prev_hash) = signed_position(event)?;
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
    let revoker = signer_key(
        tx,
        revoked.identity_id,
        revoked.revoked_by_signing_key_id,
        "revoking",
    )
    .await?;
    let (seq, prev_hash) = signed_position(event)?;
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
    let key = signer_key(tx, signer, key_id, "authoring").await?;
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

/// Checks `identity.recovered`: it is signed by the new key it introduces (key id = the request
/// id), and embeds valid approvals of exactly this request and key from at least the threshold of
/// the guardians in the owner's accepted `identity.recovery_configured`. Guardians sign only
/// these approvals, never an event of the owner's chain.
async fn verify_recovered(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
) -> Result<(), IndexError> {
    let network_id = key_event_network(origin)?;
    let owner = chain_owner(event).ok_or_else(|| reject("issuer is not an identity"))?;
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

    let accepted = identity_chain_store::accepted_events(tx, owner).await?;
    let configured: IdentityRecoveryConfiguredPayload = accepted
        .iter()
        .rev()
        .find(|e| e.kind == "identity.recovery_configured")
        .and_then(|e| serde_json::from_value(e.payload.clone()).ok())
        .ok_or_else(|| {
            IndexError::AwaitingKey("the recovery configuration is not projected yet".to_string())
        })?;
    let mut approved = BTreeSet::new();
    let mut awaiting_key = false;
    for approval in &payload.approvals {
        if !configured.guardian_ids.contains(&approval.guardian_id)
            || approved.contains(&approval.guardian_id)
        {
            continue;
        }
        let key = match key_state(tx, approval.guardian_id, approval.signing_key_id).await? {
            KeyState::Active(key) => key,
            KeyState::Revoked => continue,
            KeyState::Unknown => {
                awaiting_key = true;
                continue;
            }
        };
        let bytes = recovery_approval_signing_bytes(
            network_id,
            &owner,
            payload.request_id,
            &approval.guardian_id,
            approval.signing_key_id,
            &new_key,
        );
        let valid = decode_signature(&approval.signature)
            .is_ok_and(|signature| verify_with_key(&key, &bytes, &signature));
        if valid {
            approved.insert(approval.guardian_id);
        }
    }
    if (approved.len() as i64) < i64::from(configured.threshold.max(1)) {
        return Err(if awaiting_key {
            IndexError::AwaitingKey("a guardian key is not projected yet".to_string())
        } else {
            reject("identity.recovered carries fewer valid guardian approvals than the threshold")
        });
    }
    Ok(())
}

/// Verifies `event` before any table (including the identity chain) is touched.
/// `origin` is `None` when the indexer was built without a local origin: nothing that needs one
/// is accepted then.
pub async fn verify(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
) -> Result<Verified, IndexError> {
    match event.kind.as_str() {
        "identity.created" => {
            let origin = origin.ok_or_else(|| reject("no origin to verify identity.created"))?;
            verify_created(event, &origin.network_id).map(Verified::Creation)
        }
        "identity.signing_key_added" => verify_key_added(tx, event, origin)
            .await
            .map(|()| Verified::Other),
        "identity.signing_key_revoked" => verify_key_revoked(tx, event, origin)
            .await
            .map(|()| Verified::Other),
        "identity.recovered" => verify_recovered(tx, event, origin)
            .await
            .map(|()| Verified::Other),
        kind if needs_author_signature(kind) => verify_author(tx, event, origin)
            .await
            .map(|()| Verified::Other),
        _ => Ok(Verified::Other),
    }
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
