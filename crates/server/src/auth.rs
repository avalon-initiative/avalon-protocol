//! Password hashing and session token mechanics.
//!
//! Argon2id for passwords, opaque CSPRNG tokens for sessions — not JWTs, so a
//! session can be revoked by deleting its row rather than waiting out an
//! embedded expiry. Mirrors WorldZero's own auth design for consistency
//! between the two related projects (see `docs/specs/Auth_Spec.md` there).

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing should not fail for a valid salt")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// 32 bytes of CSPRNG output, base64url-encoded (no padding) — carries no
/// embedded data, unlike a JWT. Session state lives in the `sessions` table.
pub fn generate_session_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
