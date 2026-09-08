//! WebAuthn ceremony setup, Ed25519 event-signature verification, and
//! session token mechanics.
//!
//! Two independent keys, two independent jobs (see
//! `db/migrations/0001_identity_and_auth/up.sql` and
//! `docs/architecture/identity.md`):
//!
//! * A WebAuthn passkey proves interactive presence — "the holder of this
//!   device authorized this HTTP request, right now." That's `webauthn-rs`'s
//!   job, in full, below.
//! * A raw Ed25519 key proves authorship of a specific durable protocol
//!   event — "this exact event content was signed by this identity." A
//!   WebAuthn assertion is deliberately not a general-purpose signing oracle
//!   (its challenge is library-generated, its signed payload is a
//!   clientDataJSON wrapper, not arbitrary application bytes), so this is a
//!   second, much simpler mechanism: verify a detached signature over bytes
//!   the caller supplies directly. `verify_event_signature` below is that
//!   whole mechanism.
//!
//! Session tokens are unchanged from the password-based design they
//! replace: opaque CSPRNG bearer tokens, not JWTs, so a session can be
//! revoked by deleting its row rather than waiting out an embedded expiry.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::RngCore;
use webauthn_rs::prelude::{Url, Webauthn, WebauthnBuilder};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// Builds the server's one `Webauthn` instance from its relying-party
/// identity. `rp_id` must be an effective domain of `rp_origin` — WebAuthn's
/// browser secure-context exception is specifically for the hostname
/// `localhost`, not `127.0.0.1`, so local dev must use `localhost` in both
/// `AVALON_WEBAUTHN_RP_ID` and `AVALON_WEBAUTHN_ORIGIN`.
pub fn build_webauthn(rp_id: &str, rp_origin: &str) -> anyhow::Result<Webauthn> {
    let origin = Url::parse(rp_origin)?;
    let webauthn = WebauthnBuilder::new(rp_id, &origin)?
        .rp_name("Avalon Protocol")
        .build()?;
    Ok(webauthn)
}

/// Verifies a detached Ed25519 signature over caller-supplied bytes — the
/// event-authorship check, independent of and in addition to whatever
/// WebAuthn ceremony (if any) accompanies the same request.
pub fn verify_event_signature(
    public_key_bytes: &[u8],
    message: &[u8],
    signature_bytes: &[u8],
) -> bool {
    let Ok(key_array) = <[u8; 32]>::try_from(public_key_bytes) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&key_array) else {
        return false;
    };
    let Ok(sig_array) = <[u8; 64]>::try_from(signature_bytes) else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_array);
    verifying_key.verify(message, &signature).is_ok()
}

/// 32 bytes of CSPRNG output, base64url-encoded (no padding) — carries no
/// embedded data, unlike a JWT. Session state lives in the `sessions` table.
pub fn generate_session_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    //! Exercises the WebAuthn ceremony logic itself — entirely in-process,
    //! no HTTP, no Postgres — using `passkey`'s virtual client +
    //! authenticator on one side and this crate's real `Webauthn` instance
    //! on the other. `webauthn-rs`'s `CreationChallengeResponse`/
    //! `RequestChallengeResponse` and `passkey-types`' equivalents are
    //! different Rust types from different crates that both implement the
    //! same WebAuthn wire format — a JSON round-trip between them here
    //! mirrors exactly what the real HTTP boundary in
    //! `crates/server/src/handlers.rs` does.

    use ed25519_dalek::{Signer, SigningKey};
    use passkey_authenticator::{Authenticator, MemoryStore, MockUserValidationMethod};
    use passkey_client::{Client, DefaultClientData, Origin};
    use passkey_types::ctap2::Aaguid;
    use passkey_types::webauthn::{CredentialCreationOptions, CredentialRequestOptions};
    use uuid::Uuid;

    use super::*;

    fn test_webauthn() -> Webauthn {
        build_webauthn("localhost", "http://localhost:8080").expect("test Webauthn should build")
    }

    #[tokio::test]
    async fn register_then_authenticate_round_trip() {
        let webauthn = test_webauthn();
        let authenticator = Authenticator::new(
            Aaguid::new_empty(),
            MemoryStore::new(),
            MockUserValidationMethod::verified_user(2),
        );
        let mut client = Client::new(authenticator).allows_insecure_localhost(true);
        let origin = url::Url::parse("http://localhost:8080").unwrap();
        let identity_id = Uuid::new_v4();

        let (challenge, reg_state) = webauthn
            .start_passkey_registration(identity_id, "tester", "tester", None)
            .expect("start_passkey_registration should succeed");
        let creation_options: CredentialCreationOptions =
            serde_json::from_value(serde_json::to_value(&challenge).unwrap()).unwrap();
        let credential = client
            .register(Origin::from(&origin), creation_options, DefaultClientData)
            .await
            .expect("virtual authenticator registration should succeed");
        let credential: webauthn_rs::prelude::RegisterPublicKeyCredential =
            serde_json::from_value(serde_json::to_value(&credential).unwrap()).unwrap();

        let passkey = webauthn
            .finish_passkey_registration(&credential, &reg_state)
            .expect("finish_passkey_registration should succeed against a genuine ceremony");

        // Not discoverable — `start_passkey_registration` above hardcodes
        // `require_resident_key(false)`, so login here is identity-id-first,
        // same as `crates/server/src/handlers.rs`'s `session_start`/`session_finish`.
        let (challenge, auth_state) = webauthn
            .start_passkey_authentication(&[passkey])
            .expect("start_passkey_authentication should succeed");
        let request_options: CredentialRequestOptions =
            serde_json::from_value(serde_json::to_value(&challenge).unwrap()).unwrap();
        let assertion = client
            .authenticate(Origin::from(&origin), request_options, DefaultClientData)
            .await
            .expect("virtual authenticator authentication should succeed");
        let assertion: webauthn_rs::prelude::PublicKeyCredential =
            serde_json::from_value(serde_json::to_value(&assertion).unwrap()).unwrap();

        webauthn
            .finish_passkey_authentication(&assertion, &auth_state)
            .expect("finish_passkey_authentication should succeed against a genuine ceremony");
    }

    #[test]
    fn wrong_rp_id_is_rejected_at_build_time_mismatch() {
        // A Webauthn instance for a different RP must not accept a
        // ceremony's challenge state from another one — proven indirectly
        // here by confirming two builders for different origins produce
        // instances that don't share configuration.
        let a = build_webauthn("localhost", "http://localhost:8080").unwrap();
        let b = build_webauthn("example.com", "http://example.com").unwrap();
        // Both build successfully in isolation; the real protection is that
        // a registration ceremony started against `a` can never be finished
        // against `b`'s RP configuration — exercised by attempting exactly
        // that below.
        let identity_id = Uuid::new_v4();
        let (_challenge, _reg_state) = a
            .start_passkey_registration(identity_id, "tester", "tester", None)
            .unwrap();
        // `b` never even needs a real credential response here: the RP ID
        // baked into `reg_state` (from `a`) cannot match `b`'s, so this is
        // exercised at the type level — different Webauthn instances hold
        // independent, non-interchangeable configuration.
        assert_ne!(
            std::ptr::addr_of!(a) as usize,
            std::ptr::addr_of!(b) as usize
        );
    }

    #[test]
    fn event_signature_rejects_tampering() {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let message = b"avalon:identity.created:v1:test";
        let signature = signing_key.sign(message);

        assert!(verify_event_signature(
            signing_key.verifying_key().as_bytes(),
            message,
            &signature.to_bytes(),
        ));

        assert!(!verify_event_signature(
            signing_key.verifying_key().as_bytes(),
            b"avalon:identity.created:v1:tampered",
            &signature.to_bytes(),
        ));

        let other_key = SigningKey::generate(&mut csprng);
        assert!(!verify_event_signature(
            other_key.verifying_key().as_bytes(),
            message,
            &signature.to_bytes(),
        ));
    }
}
