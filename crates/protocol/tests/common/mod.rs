//! Helpers shared by the conformance runners.

pub fn error_json(e: &avalon_protocol::signing_bytes::SigningBytesError) -> serde_json::Value {
    use avalon_protocol::signing_bytes::{SigningBytesError as E, VersionKind};
    let what = |k: &VersionKind| match k {
        VersionKind::Layout => "layout",
        VersionKind::Rules => "rules",
        VersionKind::HashAlgo => "hash_algo",
        VersionKind::SigAlgo => "sig_algo",
        VersionKind::CriticalExtension => "critical_extension",
    };
    match e {
        E::TagMismatch => serde_json::json!({"error": "tag_mismatch"}),
        E::Truncated => serde_json::json!({"error": "truncated"}),
        E::InvalidUtf8 => serde_json::json!({"error": "invalid_utf8"}),
        E::TrailingBytes => serde_json::json!({"error": "trailing_bytes"}),
        E::FieldTooLong => serde_json::json!({"error": "field_too_long"}),
        E::NeedsNewerVersion { what: k, required } => {
            serde_json::json!({"error": "needs_newer_version", "what": what(k), "required": required})
        }
        E::UnsupportedVersion { what: k, value } => {
            serde_json::json!({"error": "unsupported_version", "what": what(k), "value": value})
        }
        E::ExtensionsUnsorted => serde_json::json!({"error": "extensions_unsorted"}),
        E::ExtensionDuplicate(_) => serde_json::json!({"error": "extension_duplicate"}),
        E::ExtensionReservedFlags(_) => serde_json::json!({"error": "extension_reserved_flags"}),
        E::ExtensionsTooLarge => serde_json::json!({"error": "extensions_too_large"}),
        E::InvalidHex => serde_json::json!({"error": "invalid_hex"}),
    }
}
