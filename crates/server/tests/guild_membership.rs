//! Exercises guild membership, invites, join requests, and member authority.
//! Requires a running `avalon-server` and Postgres; tests are `#[ignore]`.
//! See `make test-live` / `make start`.

mod common;
mod guild_support;

use guild_support::{
    create_guild, seed_identity_session, seed_signing_key, set_recruiting, unique_guild_body,
};

async fn member_count(http: &reqwest::Client, base: &str, token: &str, guild_id: &str) -> usize {
    let members: serde_json::Value =
        common::auth(http.get(format!("{base}/guilds/{guild_id}/members")), token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    members.as_array().unwrap().len()
}

#[tokio::test]
#[ignore]
async fn creating_a_guild_makes_the_owner_a_member() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let members: serde_json::Value = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(
        members[0]["identity_id"].as_str().unwrap(),
        owner_id.to_string()
    );
    assert_eq!(members[0]["role_index"].as_i64().unwrap(), 0);
}

#[tokio::test]
#[ignore]
async fn invite_accept_join_and_leave_flow() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (invitee_id, invitee_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let invite = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/invites")),
        &owner_token,
    )
    .json(&serde_json::json!({ "to": invitee_id }))
    .send()
    .await
    .unwrap();
    assert!(invite.status().is_success(), "{:?}", invite.status());
    let invite_body: serde_json::Value = invite.json().await.unwrap();
    let invite_id = invite_body["id"].as_str().unwrap();

    // Re-inviting while the first invite is still pending is idempotent —
    // same invite id back, not a new row or an error.
    let duplicate = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/invites")),
        &owner_token,
    )
    .json(&serde_json::json!({ "to": invitee_id }))
    .send()
    .await
    .unwrap();
    assert!(duplicate.status().is_success(), "{:?}", duplicate.status());
    let duplicate_body: serde_json::Value = duplicate.json().await.unwrap();
    assert_eq!(duplicate_body["id"].as_str().unwrap(), invite_id);

    // Issue #442: the invitee can discover the invite themselves, without
    // being told its id out of band.
    let my_invites: serde_json::Value =
        common::auth(http.get(format!("{base}/me/guild-invites")), &invitee_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let my_invites = my_invites.as_array().unwrap();
    assert_eq!(my_invites.len(), 1);
    assert_eq!(my_invites[0]["id"].as_str().unwrap(), invite_id);
    assert_eq!(my_invites[0]["guild_id"].as_str().unwrap(), guild_id);

    let accept = common::auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/invites/{invite_id}/accept"
        )),
        &invitee_token,
    )
    .send()
    .await
    .unwrap();
    assert!(accept.status().is_success(), "{:?}", accept.status());

    // Resolved invites must not linger in the "receiving end" listing.
    let my_invites_after: serde_json::Value =
        common::auth(http.get(format!("{base}/me/guild-invites")), &invitee_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert!(my_invites_after.as_array().unwrap().is_empty());

    let members: serde_json::Value = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(members.as_array().unwrap().len(), 2);

    let leave = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/leave")),
        &invitee_token,
    )
    .send()
    .await
    .unwrap();
    assert!(leave.status().is_success(), "{:?}", leave.status());

    let members_after: serde_json::Value = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(members_after.as_array().unwrap().len(), 1);
}

#[tokio::test]
#[ignore]
async fn joining_an_invite_only_guild_directly_is_rejected() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_other_id, other_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();
    // Guilds default to invite-only (ticket design) — this test never
    // touches join_policy, so it always exercises the closed path.
    assert_eq!(body["join_policy"].as_str().unwrap(), "invite_only");

    let join = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        &other_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(join.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn opening_a_guild_lets_a_stranger_join_directly_bypassing_requests_and_invites() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (other_id, other_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let patch = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "join_policy": "open" }))
    .send()
    .await
    .unwrap();
    assert!(patch.status().is_success(), "{:?}", patch.status());
    let patched: serde_json::Value = patch.json().await.unwrap();
    assert_eq!(patched["join_policy"].as_str().unwrap(), "open");

    // No invite, no join-request/approval round trip — straight to member.
    let join = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        &other_token,
    )
    .send()
    .await
    .unwrap();
    assert!(join.status().is_success(), "{:?}", join.status());
    let member: serde_json::Value = join.json().await.unwrap();
    assert_eq!(
        member["identity_id"].as_str().unwrap(),
        other_id.to_string()
    );

    let members = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json::<Vec<serde_json::Value>>()
    .await
    .unwrap();
    assert!(members
        .iter()
        .any(|m| m["identity_id"].as_str().unwrap() == other_id.to_string()));
}

#[tokio::test]
#[ignore]
async fn the_owner_cannot_leave_or_be_removed() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let leave = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/leave")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(leave.status(), reqwest::StatusCode::FORBIDDEN);

    let remove = common::auth(
        http.delete(format!("{base}/guilds/{guild_id}/members/{owner_id}")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(remove.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore]
async fn an_officer_cannot_remove_another_officer_without_manage_roles() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (officer_a_id, officer_a_token) = seed_identity_session(&pool).await;
    let (officer_b_id, officer_b_token) = seed_identity_session(&pool).await;

    let create = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = create.json().await.unwrap();
    let guild_id = body["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;

    for (identity_id, identity_token) in [
        (officer_a_id, &officer_a_token),
        (officer_b_id, &officer_b_token),
    ] {
        let invite = common::auth(
            http.post(format!("{base}/guilds/{guild_id}/invites")),
            &owner_token,
        )
        .json(&serde_json::json!({ "to": identity_id }))
        .send()
        .await
        .unwrap();
        let invite_body: serde_json::Value = invite.json().await.unwrap();
        let invite_id = invite_body["id"].as_str().unwrap();
        common::auth(
            http.post(format!(
                "{base}/guilds/{guild_id}/invites/{invite_id}/accept"
            )),
            identity_token,
        )
        .send()
        .await
        .unwrap();

        // Index 1 is "officer" — see the starter-role ordering asserted in
        // `creating_a_guild_makes_the_creator_the_owner`. Officer grants
        // `manage_members`, so this promotion is an escalation and needs a
        // fresh signature.
        let signature = common::sign_action(
            &signing_key,
            "guild.member_role.update",
            &[guild_id, &identity_id.to_string(), "1"],
        );
        let promote = common::auth(
            http.patch(format!("{base}/guilds/{guild_id}/members/{identity_id}")),
            &owner_token,
        )
        .json(&serde_json::json!({
            "role_index": 1,
            "signing_key_id": signing_key_id,
            "signature": signature,
        }))
        .send()
        .await
        .unwrap();
        assert!(promote.status().is_success(), "{:?}", promote.status());
    }

    // Officer A (manage_members, no manage_roles) tries to remove Officer B.
    let remove = common::auth(
        http.delete(format!("{base}/guilds/{guild_id}/members/{officer_b_id}")),
        &officer_a_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(remove.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Applying to a `recruiting` guild succeeds and shows up for a manager's
/// `GET /guilds/{id}/join-requests` (pending by default).
#[tokio::test]
#[ignore]
async fn applying_to_a_recruiting_guild_succeeds_and_is_listed_pending() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({ "message": "would love to join" }))
    .send()
    .await
    .unwrap();
    assert!(apply.status().is_success(), "{:?}", apply.status());
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    assert_eq!(apply_body["status"].as_str().unwrap(), "pending");

    let pending: serde_json::Value = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let pending = pending.as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0]["id"].as_str().unwrap(),
        apply_body["id"].as_str().unwrap()
    );
}

/// A non-recruiting guild rejects an apply outright — the ticket's own
/// gating, mirroring #154's discovery visibility rule.
#[tokio::test]
#[ignore]
async fn applying_to_a_non_recruiting_guild_is_rejected() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Closed Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    assert!(!guild["recruiting"].as_bool().unwrap());

    let apply = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(apply.status(), reqwest::StatusCode::FORBIDDEN);
}

/// A second apply while the first is still pending is idempotent — same
/// request id back, not a new row or an error, same posture as
/// `invite_accept_join_and_leave_flow`'s duplicate-invite check.
#[tokio::test]
#[ignore]
async fn duplicate_apply_while_pending_is_idempotent() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let first = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    assert!(first.status().is_success(), "{:?}", first.status());
    let first_body: serde_json::Value = first.json().await.unwrap();

    let second = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    assert!(second.status().is_success(), "{:?}", second.status());
    let second_body: serde_json::Value = second.json().await.unwrap();
    assert_eq!(
        second_body["id"].as_str().unwrap(),
        first_body["id"].as_str().unwrap()
    );
}

/// Approving a pending request adds the applicant as a member through the
/// same path an accepted invite uses, and marks the request approved.
#[tokio::test]
#[ignore]
async fn approving_a_join_request_adds_membership() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    assert_eq!(member_count(&http, &base, &owner_token, guild_id).await, 1);

    let approve = common::auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/approve"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(approve.status().is_success(), "{:?}", approve.status());
    let approve_body: serde_json::Value = approve.json().await.unwrap();
    assert_eq!(
        approve_body["identity_id"].as_str().unwrap(),
        applicant_id.to_string()
    );

    assert_eq!(member_count(&http, &base, &owner_token, guild_id).await, 2);

    let all: serde_json::Value = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests?status=all")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let all = all.as_array().unwrap();
    assert_eq!(all[0]["status"].as_str().unwrap(), "approved");
}

/// Rejecting a pending request leaves membership unchanged.
#[tokio::test]
#[ignore]
async fn rejecting_a_join_request_leaves_membership_unchanged() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    let reject = common::auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/reject"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(reject.status().is_success(), "{:?}", reject.status());

    assert_eq!(member_count(&http, &base, &owner_token, guild_id).await, 1);
}

/// The applicant withdrawing their own pending request leaves membership
/// unchanged, and a manager can no longer approve the withdrawn request.
#[tokio::test]
#[ignore]
async fn withdrawing_a_join_request_leaves_membership_unchanged() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    let withdraw = common::auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(withdraw.status().is_success(), "{:?}", withdraw.status());

    assert_eq!(member_count(&http, &base, &owner_token, guild_id).await, 1);

    let approve = common::auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/approve"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::NOT_FOUND);
}

/// A non-manager cannot approve or reject someone else's join request.
#[tokio::test]
#[ignore]
async fn non_manager_cannot_approve_or_reject_a_join_request() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;
    let (_bystander_id, bystander_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    let approve = common::auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/approve"
        )),
        &bystander_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(approve.status(), reqwest::StatusCode::FORBIDDEN);

    let reject = common::auth(
        http.post(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}/reject"
        )),
        &bystander_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(reject.status(), reqwest::StatusCode::FORBIDDEN);
}

/// Only the applicant themself can withdraw their own pending request —
/// not another identity, including the guild owner.
#[tokio::test]
#[ignore]
async fn only_the_applicant_can_withdraw_their_own_join_request() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;
    let (_bystander_id, bystander_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    let apply = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    let withdraw_by_bystander = common::auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &bystander_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(
        withdraw_by_bystander.status(),
        reqwest::StatusCode::NOT_FOUND
    );

    let withdraw_by_owner = common::auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(withdraw_by_owner.status(), reqwest::StatusCode::NOT_FOUND);

    // The request is still pending and can be withdrawn by the actual
    // applicant.
    let withdraw = common::auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(withdraw.status().is_success(), "{:?}", withdraw.status());
}

/// `GET .../join-requests/mine` returns the applicant's own
/// pending request once one exists, and `null` beforehand — no
/// `manage_members` grant required either way, since it's the caller's own
/// data.
#[tokio::test]
#[ignore]
async fn my_join_request_returns_own_pending_request_or_none() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    // Before applying: no pending request of the applicant's own.
    let before = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(before.status().is_success(), "{:?}", before.status());
    let before_body: serde_json::Value = before.json().await.unwrap();
    assert!(before_body.is_null());

    let apply = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({ "message": "would love to join!" }))
    .send()
    .await
    .unwrap();
    let apply_body: serde_json::Value = apply.json().await.unwrap();
    let request_id = apply_body["id"].as_str().unwrap();

    // After applying: the applicant's own pending request comes back,
    // matching the same shape POST already returned.
    let after = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(after.status().is_success(), "{:?}", after.status());
    let after_body: serde_json::Value = after.json().await.unwrap();
    assert_eq!(after_body["id"].as_str().unwrap(), request_id);
    assert_eq!(
        after_body["applicant"].as_str().unwrap(),
        applicant_id.to_string()
    );
    assert_eq!(after_body["status"].as_str().unwrap(), "pending");
    assert_eq!(
        after_body["message"].as_str().unwrap(),
        "would love to join!"
    );
}

/// `GET .../join-requests/mine` never leaks another applicant's pending
/// request for the same guild — each caller only ever sees their own.
#[tokio::test]
#[ignore]
async fn my_join_request_never_returns_another_identitys_request() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;
    let (_bystander_id, bystander_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();

    // The bystander never applied — `mine` reports none for them even
    // though the applicant has a pending request in this same guild.
    let bystander_mine = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &bystander_token,
    )
    .send()
    .await
    .unwrap();
    assert!(
        bystander_mine.status().is_success(),
        "{:?}",
        bystander_mine.status()
    );
    let bystander_body: serde_json::Value = bystander_mine.json().await.unwrap();
    assert!(bystander_body.is_null());

    // The owner also has no pending request of their own here.
    let owner_mine = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &owner_token,
    )
    .send()
    .await
    .unwrap();
    assert!(
        owner_mine.status().is_success(),
        "{:?}",
        owner_mine.status()
    );
    let owner_body: serde_json::Value = owner_mine.json().await.unwrap();
    assert!(owner_body.is_null());
}

/// End-to-end: an applicant can discover their own pending request via
/// `mine`, then withdraw it via the existing `DELETE` route (its
/// motivation — the Hub previously had no way to reach this at all), and
/// `mine` reflects the withdrawal afterward.
#[tokio::test]
#[ignore]
async fn my_join_request_then_withdraw_is_reachable_end_to_end() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (_owner_id, owner_token) = seed_identity_session(&pool).await;
    let (_applicant_id, applicant_token) = seed_identity_session(&pool).await;

    let guild = create_guild(&http, &base, &owner_token, "Recruiting Guild").await;
    let guild_id = guild["id"].as_str().unwrap();
    set_recruiting(&http, &base, &owner_token, guild_id, true).await;

    common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join-requests")),
        &applicant_token,
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .unwrap();

    let mine = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    let mine_body: serde_json::Value = mine.json().await.unwrap();
    let request_id = mine_body["id"].as_str().unwrap().to_string();

    let withdraw = common::auth(
        http.delete(format!(
            "{base}/guilds/{guild_id}/join-requests/{request_id}"
        )),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    assert!(withdraw.status().is_success(), "{:?}", withdraw.status());

    let mine_after = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/join-requests/mine")),
        &applicant_token,
    )
    .send()
    .await
    .unwrap();
    let mine_after_body: serde_json::Value = mine_after.json().await.unwrap();
    assert!(mine_after_body.is_null());
}

/// #697/#698: assigning a role that grants `manage_roles`/`manage_members`
/// via `PATCH /guilds/{id}/members/{identity_id}` is an escalation and
/// requires a fresh signature; an unsigned attempt must be rejected and the
/// target's role must stay unchanged. (An ordinary, non-escalating role
/// assignment staying ambient/unsigned is already covered by
/// `deleting_a_role_still_held_by_a_member_is_rejected` in `guild_roles.rs`.)
#[tokio::test]
#[ignore]
async fn escalating_a_member_role_without_a_signature_is_rejected() {
    let http = reqwest::Client::new();
    let base = common::server_url();
    let pool = common::test_pool().await;
    let (owner_id, owner_token) = seed_identity_session(&pool).await;
    let (other_id, other_token) = seed_identity_session(&pool).await;

    let create_guild = common::auth(http.post(format!("{base}/guilds")), &owner_token)
        .json(&unique_guild_body())
        .send()
        .await
        .expect("create guild failed — is `make start` running?");
    let guild: serde_json::Value = create_guild.json().await.unwrap();
    let guild_id = guild["id"].as_str().unwrap();

    let (signing_key_id, signing_key) = seed_signing_key(&pool, owner_id).await;
    let create_signature = common::sign_action(
        &signing_key,
        "guild.role.create",
        &[guild_id, "Co-Officer", "manage_roles"],
    );
    let create_role = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/roles")),
        &owner_token,
    )
    .json(&serde_json::json!({
        "name": "Co-Officer",
        "permissions": ["manage_roles"],
        "signing_key_id": signing_key_id,
        "signature": create_signature,
    }))
    .send()
    .await
    .unwrap();
    let role: serde_json::Value = create_role.json().await.unwrap();
    let name_index = role["name_index"].as_i64().unwrap();

    let open = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "join_policy": "open" }))
    .send()
    .await
    .unwrap();
    assert!(open.status().is_success());
    let join = common::auth(
        http.post(format!("{base}/guilds/{guild_id}/join")),
        &other_token,
    )
    .send()
    .await
    .unwrap();
    assert!(join.status().is_success());

    let unsigned_assign = common::auth(
        http.patch(format!("{base}/guilds/{guild_id}/members/{other_id}")),
        &owner_token,
    )
    .json(&serde_json::json!({ "role_index": name_index }))
    .send()
    .await
    .unwrap();
    // `owner_id` already has a registered signing key (seeded above for the
    // role-creation signature), so the actionable rejection here is "you
    // have a key, you just didn't sign this request" — not "no key at all".
    assert_eq!(unsigned_assign.status(), reqwest::StatusCode::UNAUTHORIZED);
    let unsigned_body: serde_json::Value = unsigned_assign.json().await.unwrap();
    assert_eq!(unsigned_body["code"], "FRESH_SIGNATURE_REQUIRED");

    let members: serde_json::Value = common::auth(
        http.get(format!("{base}/guilds/{guild_id}/members")),
        &owner_token,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let other_member = members
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["identity_id"].as_str().unwrap() == other_id.to_string())
        .expect("other identity should be a member after joining");
    assert_ne!(other_member["role_index"].as_i64().unwrap(), name_index);
}
