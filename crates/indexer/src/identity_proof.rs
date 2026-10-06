//! Projection-time proof that an event may change an identity's keys or profile.
//!
//! Self-authenticating kinds (`identity.created`, `identity.signing_key_added`,
//! `identity.signing_key_revoked`) are checked against the signature they carry and the identity's
//! own key chain; the check does not depend on which shard delivered the event. Kinds that carry
//! no proof are accepted only from an authoritative origin (see [`EventOrigin::is_authoritative`]).
//! Residual: the inception `signing_key_added` id is not bound by any signature or checked against the
//! creation ticket (not stored), so only home shards, core and the local shard may deliver it.
//! Anything refused is returned as [`IndexError::Rejected`] before any table is touched.

use avalon_protocol::ed25519_key::{parse_ed25519_public_key, verify_strict_signature};
use avalon_protocol::event_payloads::{
    IdentityCreatedPayload, IdentitySigningKeyAddedPayload, IdentitySigningKeyRevokedPayload,
    SIGNING_KEY_KIND_DEVICE_GRANT, SIGNING_KEY_KIND_INCEPTION,
};
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_chain_wire::parse_hash;
use avalon_protocol::identity_id::{
    device_grant_approval_signing_bytes, display_name_permitted, identity_created_signing_bytes,
    signing_key_revoked_signing_bytes, IdentityId,
};
use avalon_protocol::ids::GlobalId;
use avalon_protocol::shard::CORE_SHARD_ID;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

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

    /// Whether this origin may author identity-state events that carry no proof: this node's own
    /// stream, or the core shard of this node's own network (`local_network`). Every other
    /// shard is limited to events that prove themselves.
    pub fn is_authoritative(&self, local_network: Option<&str>) -> bool {
        self.local || (self.shard_id == CORE_SHARD_ID && local_network == Some(&self.network_id))
    }
}

/// An `identity.created` event whose proof checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCreation {
    pub identity_id: IdentityId,
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
/// the bytes bound to `origin`'s network and shard.
pub fn verify_created(
    event: &ProtocolEvent,
    origin: &EventOrigin,
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
        &origin.network_id,
        &origin.shard_id,
        created.ticket_id,
        &created.identity_id,
        &key,
        &created.display_name,
    );
    if !verify_with_key(&key, &bytes, &signature) {
        return Err(reject(
            "identity.created signature does not verify for this network and shard",
        ));
    }
    Ok(VerifiedCreation {
        identity_id: created.identity_id,
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

/// Key events change what authenticates as the identity. The signatures cover key ids and chain
/// position, but the chain hash also covers unsigned fields (timestamp, label), so a delivering
/// shard could still fork the chain; only a shard the identity itself created on, the core shard
/// or this node's own shard may deliver them. Residual: the inception key's id is not signed or
/// checked at projection, so a hostile home shard could deliver it under a fresh `signing_key_id`.
async fn require_key_authority(
    tx: &mut Transaction<'_, Postgres>,
    identity_id: IdentityId,
    origin: Option<&EventOrigin>,
    local_network: Option<&str>,
) -> Result<(), IndexError> {
    let Some(origin) = origin else {
        return Err(reject("no origin to authorize a key event"));
    };
    if origin.is_authoritative(local_network) {
        return Ok(());
    }
    let home: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM indexer_identity_homes \
         WHERE identity_id = $1 AND network_id = $2 AND shard_id = $3)",
    )
    .bind(identity_id)
    .bind(&origin.network_id)
    .bind(&origin.shard_id)
    .fetch_one(&mut **tx)
    .await?;
    if home {
        Ok(())
    } else {
        Err(reject(format!(
            "shard {} is not a shard this identity was created on",
            origin.shard_id
        )))
    }
}

async fn verify_key_added(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
    local_network: Option<&str>,
) -> Result<(), IndexError> {
    if event.version != 2 {
        return Err(reject("identity.signing_key_added must be version 2"));
    }
    let added: IdentitySigningKeyAddedPayload = serde_json::from_value(event.payload.clone())
        .map_err(|_| reject("identity.signing_key_added payload is malformed"))?;
    require_issuer_is(event, added.identity_id)?;
    require_key_authority(tx, added.identity_id, origin, local_network).await?;
    let key = decode_key(&added.public_key)?;
    match added.kind.as_str() {
        SIGNING_KEY_KIND_INCEPTION => {
            if !added.identity_id.matches_key(&key) {
                return Err(reject("inception key does not derive the identity id"));
            }
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
    local_network: Option<&str>,
) -> Result<(), IndexError> {
    if event.version != 2 {
        return Err(reject("identity.signing_key_revoked must be version 2"));
    }
    let revoked: IdentitySigningKeyRevokedPayload =
        serde_json::from_value(event.payload.clone())
            .map_err(|_| reject("identity.signing_key_revoked payload is malformed"))?;
    require_issuer_is(event, revoked.identity_id)?;
    require_key_authority(tx, revoked.identity_id, origin, local_network).await?;
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

/// Kinds that change an identity's keys, credentials, recovery state or profile but carry no
/// signature a mirror can check.
fn is_unproven_identity_state(kind: &str) -> bool {
    kind == "profile.updated"
        || (kind.starts_with("identity.")
            && !matches!(
                kind,
                "identity.created" | "identity.signing_key_added" | "identity.signing_key_revoked"
            ))
}

/// Verifies `event` before any table (including the identity chain) is touched.
/// `origin` is `None` when the indexer was built without a local origin: nothing that needs one
/// is accepted then.
pub async fn verify(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    origin: Option<&EventOrigin>,
    local_network: Option<&str>,
) -> Result<Verified, IndexError> {
    match event.kind.as_str() {
        "identity.created" => {
            let origin = origin.ok_or_else(|| reject("no origin to verify identity.created"))?;
            verify_created(event, origin).map(Verified::Creation)
        }
        "identity.signing_key_added" => verify_key_added(tx, event, origin, local_network)
            .await
            .map(|()| Verified::Other),
        "identity.signing_key_revoked" => verify_key_revoked(tx, event, origin, local_network)
            .await
            .map(|()| Verified::Other),
        kind if is_unproven_identity_state(kind) => {
            if origin.is_some_and(|o| o.is_authoritative(local_network)) {
                Ok(Verified::Other)
            } else {
                Err(reject(format!(
                    "{kind} carries no proof and is accepted only from the core shard or this node's own shard"
                )))
            }
        }
        _ => Ok(Verified::Other),
    }
}

#[cfg(test)]
mod tests {
    use avalon_protocol::identity_id::{TestIdentity, TEST_NETWORK_ID, TEST_SHARD_ID};
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

    fn origin() -> EventOrigin {
        EventOrigin::mirrored(TEST_NETWORK_ID, TEST_SHARD_ID)
    }

    #[test]
    fn a_correctly_signed_creation_verifies() {
        let who = TestIdentity::new();
        let event = created_event(&who, who.created_payload("Ada"));
        let verified = verify_created(&event, &origin()).unwrap();
        assert_eq!(verified.identity_id, who.id);
        assert_eq!(verified.display_name, "Ada");
    }

    #[test]
    fn a_creation_signed_for_another_shard_or_network_is_refused() {
        let who = TestIdentity::new();
        let event = created_event(&who, who.created_payload("Ada"));
        let other_shard = EventOrigin::mirrored(TEST_NETWORK_ID, "game:slug/1");
        let other_network = EventOrigin::mirrored("another-network", TEST_SHARD_ID);
        assert!(verify_created(&event, &other_shard).is_err());
        assert!(verify_created(&event, &other_network).is_err());
    }

    #[test]
    fn a_creation_with_a_forged_signature_or_foreign_key_is_refused() {
        let who = TestIdentity::new();
        let attacker = TestIdentity::new();
        // Right id and key, signature by another key.
        let mut forged = who.created_payload("Ada");
        forged.signature = attacker.created_payload("Ada").signature;
        assert!(verify_created(&created_event(&who, forged), &origin()).is_err());
        // The attacker's own key under the victim's id.
        let mut swapped = attacker.created_payload("Ada");
        swapped.identity_id = who.id;
        assert!(verify_created(&created_event(&who, swapped), &origin()).is_err());
        // A name altered after signing.
        let mut renamed = who.created_payload("Ada");
        renamed.display_name = "Mallory".to_string();
        assert!(verify_created(&created_event(&who, renamed), &origin()).is_err());
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
            TEST_SHARD_ID,
            ticket,
            &victim.id,
            &attacker.public_key(),
            "Ada",
        );
        let mut payload = victim.created_payload("Ada");
        payload.ticket_id = ticket;
        payload.public_key = BASE64.encode(attacker.public_key());
        payload.signature = BASE64.encode(attacker.signing_key.sign(&bytes).to_bytes());
        let err = verify_created(&created_event(&victim, payload), &origin()).unwrap_err();
        assert!(err.to_string().contains("not derived"), "{err}");
    }

    #[test]
    fn a_creation_whose_envelope_names_another_identity_is_refused() {
        let who = TestIdentity::new();
        let victim = TestIdentity::new();
        let mut event = created_event(&who, who.created_payload("Ada"));
        event.issuer = GlobalId::new("identity", &victim.id.to_string(), "self", "created");
        assert!(verify_created(&event, &origin()).is_err());
    }

    #[test]
    fn only_core_and_the_local_shard_are_authoritative() {
        let net = Some("n");
        assert!(EventOrigin::mirrored("n", "core").is_authoritative(net));
        assert!(EventOrigin::local("n", "game:slug/1").is_authoritative(net));
        assert!(!EventOrigin::mirrored("n", "game:slug/1").is_authoritative(net));
        assert!(!EventOrigin::mirrored("n", "service:x").is_authoritative(net));
        // Another network's core shard, or no local network at all, is not authoritative.
        assert!(!EventOrigin::mirrored("other", "core").is_authoritative(net));
        assert!(!EventOrigin::mirrored("n", "core").is_authoritative(None));
    }

    #[test]
    fn unproven_kinds_are_identified() {
        for kind in [
            "profile.updated",
            "identity.passkey_registered",
            "identity.passkey_revoked",
            "identity.recovered",
            "identity.recovery_configured",
            "identity.something_new",
        ] {
            assert!(is_unproven_identity_state(kind), "{kind}");
        }
        for kind in [
            "identity.created",
            "identity.signing_key_added",
            "identity.signing_key_revoked",
            "friend.requested",
            "guild.created",
        ] {
            assert!(!is_unproven_identity_state(kind), "{kind}");
        }
    }
}
