//! Shard identifier grammar: `core`, `{namespace}:{owner}[/{instance}]`, or
//! `node:<key-hash>` (self-certifying — see [`SELF_CERTIFYING_NAMESPACE`]).
//!
//! `namespace` is `game`, `app` or `service`; `owner` is the integrator slug
//! and is the only component used for key and authority resolution;
//! `instance` distinguishes sibling shards operated by the same owner.
//! `node:<key-hash>` is a distinct, third form: see
//! `crate::shard_identity` for its derivation and verification.

/// The reserved shard label of a network's pinned core authority.
pub const CORE_SHARD_ID: &str = "core";

/// Longest permitted `instance` component.
pub const MAX_INSTANCE_LEN: usize = 64;

/// Namespace of a self-certifying shard id (`node:<key-hash>`) — see
/// `crate::shard_identity`.
pub const SELF_CERTIFYING_NAMESPACE: &str = "node";

/// Length in hex characters of a self-certifying id's key hash: SHA-256
/// (32 bytes), full-length, lowercase hex.
pub const KEY_HASH_HEX_LEN: usize = 64;

/// A syntactically valid shard identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedShardId<'a> {
    Core,
    Owned {
        namespace: &'a str,
        owner: &'a str,
        instance: Option<&'a str>,
    },
    /// `node:<key-hash>` — self-certifying, resolved from the id alone
    /// (`crate::shard_identity::resolve_self_certifying_key`), never through
    /// [`shard_authority`].
    SelfCertifying {
        key_hash_hex: &'a str,
    },
}

/// Why a string is not a valid shard identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShardIdError {
    Empty,
    UnknownNamespace(String),
    EmptyOwner,
    InvalidOwner(String),
    InvalidInstance(String),
    InvalidKeyHash(String),
}

impl std::fmt::Display for ShardIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShardIdError::Empty => write!(f, "shard id is empty"),
            ShardIdError::UnknownNamespace(ns) => write!(
                f,
                "unknown shard namespace {ns:?} (expected `core`, `node:<key-hash>`, or \
                 `game|app|service:<owner>[/<instance>]`)"
            ),
            ShardIdError::EmptyOwner => write!(f, "shard id has an empty owner"),
            ShardIdError::InvalidOwner(o) => write!(
                f,
                "invalid shard owner {o:?} (must not contain whitespace, ':' or '/')"
            ),
            ShardIdError::InvalidInstance(i) => write!(
                f,
                "invalid shard instance {i:?} (1-{MAX_INSTANCE_LEN} chars of [a-z0-9-], starting with [a-z0-9])"
            ),
            ShardIdError::InvalidKeyHash(h) => write!(
                f,
                "invalid self-certifying key hash {h:?} ({KEY_HASH_HEX_LEN} lowercase hex chars expected)"
            ),
        }
    }
}

impl std::error::Error for ShardIdError {}

fn valid_instance(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    s.len() <= MAX_INSTANCE_LEN
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_key_hash_hex(s: &str) -> bool {
    s.len() == KEY_HASH_HEX_LEN
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Parses and validates a shard identifier.
pub fn parse_shard_id(id: &str) -> Result<ParsedShardId<'_>, ShardIdError> {
    if id.is_empty() {
        return Err(ShardIdError::Empty);
    }
    if id == CORE_SHARD_ID {
        return Ok(ParsedShardId::Core);
    }
    let Some((namespace, rest)) = id.split_once(':') else {
        return Err(ShardIdError::UnknownNamespace(id.to_string()));
    };
    if namespace == SELF_CERTIFYING_NAMESPACE {
        return if valid_key_hash_hex(rest) {
            Ok(ParsedShardId::SelfCertifying { key_hash_hex: rest })
        } else {
            Err(ShardIdError::InvalidKeyHash(rest.to_string()))
        };
    }
    if !matches!(namespace, "game" | "app" | "service") {
        return Err(ShardIdError::UnknownNamespace(namespace.to_string()));
    }
    let (owner, instance) = match rest.split_once('/') {
        Some((owner, instance)) => (owner, Some(instance)),
        None => (rest, None),
    };
    if owner.is_empty() {
        return Err(ShardIdError::EmptyOwner);
    }
    if owner
        .chars()
        .any(|c| c.is_whitespace() || c == ':' || c == '/')
    {
        return Err(ShardIdError::InvalidOwner(owner.to_string()));
    }
    if let Some(instance) = instance {
        if !valid_instance(instance) {
            return Err(ShardIdError::InvalidInstance(instance.to_string()));
        }
    }
    Ok(ParsedShardId::Owned {
        namespace,
        owner,
        instance,
    })
}

/// `(namespace, owner)` used for key and authority resolution, or `None` for
/// `core`, a self-certifying `node:<key-hash>` id (resolved via
/// `crate::shard_identity` instead), and anything that does not parse.
pub fn shard_authority(id: &str) -> Option<(&str, &str)> {
    match parse_shard_id(id) {
        Ok(ParsedShardId::Owned {
            namespace, owner, ..
        }) => Some((namespace, owner)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_parses() {
        assert_eq!(parse_shard_id("core"), Ok(ParsedShardId::Core));
        assert_eq!(shard_authority("core"), None);
    }

    #[test]
    fn plain_owner_has_no_instance() {
        assert_eq!(
            parse_shard_id("game:wow"),
            Ok(ParsedShardId::Owned {
                namespace: "game",
                owner: "wow",
                instance: None
            })
        );
    }

    #[test]
    fn instance_parses_and_authority_ignores_it() {
        assert_eq!(
            parse_shard_id("app:wow/eu-2"),
            Ok(ParsedShardId::Owned {
                namespace: "app",
                owner: "wow",
                instance: Some("eu-2")
            })
        );
        assert_eq!(shard_authority("game:wow/2"), Some(("game", "wow")));
        assert_eq!(shard_authority("game:wow"), Some(("game", "wow")));
    }

    #[test]
    fn invalid_ids_are_rejected() {
        assert_eq!(parse_shard_id(""), Err(ShardIdError::Empty));
        assert!(matches!(
            parse_shard_id("wow"),
            Err(ShardIdError::UnknownNamespace(_))
        ));
        assert!(matches!(
            parse_shard_id("identity:wow"),
            Err(ShardIdError::UnknownNamespace(_))
        ));
        assert_eq!(parse_shard_id("game:"), Err(ShardIdError::EmptyOwner));
        assert_eq!(parse_shard_id("game:/1"), Err(ShardIdError::EmptyOwner));
        assert!(matches!(
            parse_shard_id("game:a b"),
            Err(ShardIdError::InvalidOwner(_))
        ));
        for bad in [
            "game:wow/",
            "game:wow/-x",
            "game:wow/A",
            "game:wow/a_b",
            "game:wow/a/b",
        ] {
            assert!(
                matches!(parse_shard_id(bad), Err(ShardIdError::InvalidInstance(_))),
                "{bad}"
            );
        }
        assert_eq!(shard_authority("game:wow/"), None);
    }

    #[test]
    fn instance_length_bounds() {
        let ok = format!("game:wow/{}", "a".repeat(64));
        let too_long = format!("game:wow/{}", "a".repeat(65));
        assert!(parse_shard_id(&ok).is_ok());
        assert!(parse_shard_id(&too_long).is_err());
    }

    #[test]
    fn self_certifying_id_parses() {
        let hash = "ab".repeat(32);
        assert_eq!(
            parse_shard_id(&format!("node:{hash}")),
            Ok(ParsedShardId::SelfCertifying {
                key_hash_hex: &hash
            })
        );
        // Self-certifying ids resolve no `(namespace, owner)` — they carry
        // no registry-resolvable authority at all.
        assert_eq!(shard_authority(&format!("node:{hash}")), None);
    }

    #[test]
    fn self_certifying_id_rejects_wrong_length_or_case() {
        assert!(matches!(
            parse_shard_id("node:ab"),
            Err(ShardIdError::InvalidKeyHash(_))
        ));
        assert!(matches!(
            parse_shard_id(&format!("node:{}", "AB".repeat(32))),
            Err(ShardIdError::InvalidKeyHash(_))
        ));
        assert!(matches!(
            parse_shard_id(&format!("node:{}", "zz".repeat(32))),
            Err(ShardIdError::InvalidKeyHash(_))
        ));
        assert!(matches!(
            parse_shard_id("node:"),
            Err(ShardIdError::InvalidKeyHash(_))
        ));
    }
}
