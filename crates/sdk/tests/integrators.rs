//! Exercises integrator registration and issuer-key management (issue
//! #741/#746) against a real, running `avalon-server`. Gated `--ignored`
//! since it needs live infra — see `make test-live` / `make start`.

use avalon_sdk::integrators::{
    InitialKey, IntegratorsListSort, ListIntegratorsQuery, NewIntegrator, NewIssuerKey,
};
use avalon_sdk::{AvalonClient, AvalonConfig};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::SigningKey;
use uuid::Uuid;

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

/// `register_integrator` -> `add_issuer_key` -> `integrator_whoami` (this
/// ticket's own named round trip), plus `list_integrators`/`get_integrator`/
/// `list_issuer_keys` confirming the registration is visible through every
/// public read this ticket adds.
#[tokio::test]
#[ignore]
async fn register_integrator_then_add_issuer_key_then_whoami_round_trips() {
    let root_key = SigningKey::generate(&mut rand::rng());
    let suffix = Uuid::new_v4().simple().to_string();
    let slug = format!("sdk-int-{}", &suffix[..12]);

    let bootstrap = AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        integrator_credential_key_id: String::new(),
        integrator_slug: None,
        signing_key: None,
        retry: Default::default(),
    });

    let registered = bootstrap
        .register_integrator(NewIntegrator {
            slug: slug.clone(),
            name: "SDK Integrators Test".to_string(),
            owner_name: "Test Studio".to_string(),
            requested_capabilities: vec!["achievements.issue".to_string()],
            initial_key: InitialKey {
                algorithm: "ed25519".to_string(),
                public_key: BASE64.encode(root_key.verifying_key().as_bytes()),
            },
            category: None,
        })
        .await
        .expect("register_integrator should succeed against a real server");
    assert_eq!(registered.slug, slug);
    assert_eq!(registered.category, "game");

    // A second client, configured with the credential `register_integrator`
    // just minted — the shape any real caller would build one in.
    let client = AvalonClient::new(AvalonConfig {
        server_url: server_url(),
        integrator_credential_key_id: registered.credential.key_id.clone(),
        integrator_slug: Some(slug.clone()),
        signing_key: Some(root_key.to_bytes()),
        retry: Default::default(),
    });

    let whoami = client
        .integrator_whoami()
        .await
        .expect("integrator_whoami should succeed with the just-registered credential");
    assert_eq!(whoami.integrator_id, registered.id);

    let operational_key = SigningKey::generate(&mut rand::rng());
    let added = client
        .add_issuer_key(NewIssuerKey {
            algorithm: "ed25519".to_string(),
            public_key: BASE64.encode(operational_key.verifying_key().as_bytes()),
            role: "operational".to_string(),
            purpose: None,
            valid_until: None,
        })
        .await
        .expect("add_issuer_key should succeed with a root credential");
    assert_eq!(added.role, "operational");
    assert!(added.revoked_at.is_none());

    let keys = client
        .list_issuer_keys(&slug)
        .await
        .expect("list_issuer_keys should succeed");
    assert_eq!(
        keys.len(),
        2,
        "the initial root key plus the added operational key"
    );
    assert!(keys.iter().any(|k| k.key_id == added.key_id));

    let revoked = client
        .revoke_issuer_key(added.key_id, Some("no longer needed"))
        .await
        .expect("revoke_issuer_key should succeed for a key this integrator owns");
    assert!(revoked.revoked_at.is_some());

    let public = bootstrap
        .get_integrator(&slug)
        .await
        .expect("get_integrator should succeed for a real slug");
    assert_eq!(public.id, registered.id);
    assert_eq!(public.owner_name, "Test Studio");

    let listed = bootstrap
        .list_integrators(ListIntegratorsQuery {
            q: Some(slug.clone()),
            sort: IntegratorsListSort::Newest,
            limit: None,
            cursor: None,
        })
        .await
        .expect("list_integrators should succeed");
    assert!(listed.iter().any(|i| i.slug == slug));
}
