//! Fails when a ledger payload struct gains, loses or renames a field without
//! `data/payload-field-classification.json` (#1318, decision #1299) being updated.

use std::collections::{BTreeMap, BTreeSet};

use avalon_protocol::events::ProtocolEventKind;
use serde_json::Value;

const CLASSIFICATION: &str = include_str!("../data/payload-field-classification.json");
const INVENTORY_DOC: &str =
    include_str!("../../../docs/maintainers/ledger-payload-field-inventory.md");
const EVENT_PAYLOADS_SRC: &str = include_str!("../src/event_payloads.rs");
const GUILDS_SRC: &str = include_str!("../src/guilds.rs");

/// Named-field struct name -> field names, for every `pub struct` in `src` accepted by `want`.
fn struct_fields(src: &str, want: impl Fn(&str) -> bool) -> BTreeMap<String, BTreeSet<String>> {
    let file = syn::parse_file(src).expect("source parses");
    let mut out = BTreeMap::new();
    for item in file.items {
        let syn::Item::Struct(s) = item else { continue };
        let name = s.ident.to_string();
        if !matches!(s.vis, syn::Visibility::Public(_)) || !want(&name) {
            continue;
        }
        let syn::Fields::Named(named) = s.fields else {
            continue;
        };
        let mut fields = BTreeSet::new();
        for f in named.named {
            for attr in f.attrs.iter().filter(|a| a.path().is_ident("serde")) {
                let _ = attr.parse_nested_meta(|m| {
                    assert!(
                        !m.path.is_ident("rename"),
                        "{name}: serde rename is not supported by the field check; use the Rust name"
                    );
                    if m.input.peek(syn::Token![=]) {
                        m.value()?.parse::<syn::Expr>()?;
                    }
                    Ok(())
                });
            }
            fields.insert(f.ident.expect("named field").to_string());
        }
        out.insert(name, fields);
    }
    out
}

fn load() -> Value {
    serde_json::from_str(CLASSIFICATION).expect("classification JSON parses")
}

fn names(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|s| s.as_str().expect("string").to_string())
        .collect()
}

#[test]
fn every_payload_field_is_classified() {
    let doc = load();
    let mut actual = struct_fields(EVENT_PAYLOADS_SRC, |n| n.ends_with("Payload"));
    actual.extend(struct_fields(GUILDS_SRC, |n| n == "GuildLink"));

    let classified = doc["payloads"].as_object().expect("payloads object");
    let mut problems = Vec::new();
    for (name, fields) in &actual {
        let Some(entry) = classified.get(name) else {
            problems.push(format!("payload type {name} is not classified"));
            continue;
        };
        let listed: BTreeSet<String> = entry["fields"]
            .as_object()
            .expect("fields object")
            .keys()
            .cloned()
            .collect();
        for f in fields.difference(&listed) {
            problems.push(format!("{name}.{f} is not classified"));
        }
        for f in listed.difference(fields) {
            problems.push(format!("{name}.{f} is classified but no longer exists"));
        }
    }
    for name in classified.keys() {
        if !actual.contains_key(name) {
            problems.push(format!("classified type {name} no longer exists"));
        }
    }
    assert!(
        problems.is_empty(),
        "update crates/protocol/data/payload-field-classification.json and \
         docs/maintainers/ledger-payload-field-inventory.md:\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_event_kind_maps_to_a_classified_payload() {
    let doc = load();
    let kinds = doc["kinds"].as_object().expect("kinds object");
    let payloads = doc["payloads"].as_object().expect("payloads object");
    let nested = doc["nested"].as_object().expect("nested object");

    for kind in ProtocolEventKind::KNOWN {
        assert!(
            kinds.contains_key(kind.as_str()),
            "event kind {} has no payload mapping",
            kind.as_str()
        );
    }
    let known: BTreeSet<&str> = ProtocolEventKind::KNOWN
        .iter()
        .map(|k| k.as_str())
        .collect();
    let mut used = BTreeSet::new();
    for (kind, ty) in kinds {
        assert!(known.contains(kind.as_str()), "{kind} is not a known kind");
        let ty = ty.as_str().expect("type name");
        assert!(
            payloads.contains_key(ty),
            "{kind} maps to unknown type {ty}"
        );
        used.insert(ty.to_string());
    }
    for ty in payloads.keys() {
        assert!(
            used.contains(ty) || nested.contains_key(ty),
            "{ty} is neither mapped from a kind nor listed as nested"
        );
    }
}

#[test]
fn classifications_are_consistent() {
    let doc = load();
    let classes = names(&doc["classes"]);
    let dispositions = names(&doc["dispositions"]);
    for (ty, entry) in doc["payloads"].as_object().unwrap() {
        for (field, f) in entry["fields"].as_object().unwrap() {
            let id = format!("{ty}.{field}");
            let class = f["class"].as_str().unwrap_or_default();
            let disposition = f["disposition"].as_str().unwrap_or_default();
            assert!(classes.contains(class), "{id}: unknown class {class}");
            assert!(
                dispositions.contains(disposition),
                "{id}: unknown disposition {disposition}"
            );
            let sensitive = class == "content" || class == "personal_data";
            if sensitive {
                let reason = f["reason"].as_str().unwrap_or_default();
                assert!(!reason.is_empty(), "{id}: {class} needs a reason");
            } else {
                assert_eq!(disposition, "none", "{id}: {class} takes no disposition");
            }
            if class == "personal_data" {
                assert_ne!(
                    disposition, "keep_inline",
                    "{id}: personal data cannot stay unredactable"
                );
            }
            if sensitive && disposition == "none" {
                panic!("{id}: {class} needs a disposition");
            }
        }
    }
}

#[test]
fn inventory_doc_lists_every_non_authority_field() {
    let doc = load();
    for (ty, entry) in doc["payloads"].as_object().unwrap() {
        for (field, f) in entry["fields"].as_object().unwrap() {
            let class = f["class"].as_str().unwrap();
            if class == "content" || class == "personal_data" {
                let id = format!("`{ty}.{field}`");
                assert!(INVENTORY_DOC.contains(&id), "inventory doc is missing {id}");
            }
        }
    }
}
