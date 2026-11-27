//! The teams and invites client (04 §5.3, 06 §10 M2) against wiremock: the
//! request shapes the Go server is expected to accept, lenient parsing of
//! its answers, refusal codes, and the `/i#token` link.

use reqharbor_core::auth::{AuthClient, AuthError, Session, Tokens};
use reqharbor_core::model::now_ms;
use reqharbor_core::secrets::MemorySecretStore;
use reqharbor_core::teams::{self, InviteLink, parse_invite_link};
use serde_json::json;
use std::sync::Arc;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn session(server: &MockServer) -> Session {
    Session::new(
        AuthClient::new(&server.uri()).unwrap(),
        &server.uri(),
        "U1",
        Arc::new(MemorySecretStore::new()),
        Tokens {
            access_token: "rha_A".into(),
            access_expires_at: now_ms() + 3_600_000,
            refresh_token: "rhr_R".into(),
            session_id: "S1".into(),
        },
    )
}

fn refused(status: u16, code: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_json(json!({"error": {"code": code, "message": format!("{code} happened")}}))
}

#[tokio::test]
async fn list_create_and_detail() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/teams"))
        .and(header("authorization", "Bearer rha_A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"teams": [
            {"teamId": "T1", "name": "Acme", "role": "owner", "spaceId": "S1", "memberCount": 3,
             "bytesUsed": 10, "quotaBytes": 100, "ownerUserId": "U1", "createdAt": 1700000000000i64}
        ], "ownedCount": 1, "limits": {"ownedTeams": 1, "teamMembers": 100, "teamInvites": null},
           "owner": {"bytesUsed": 6912, "quotaBytes": null}})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/teams"))
        .and(body_json(json!({"name": "Globex"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(
            json!({"teamId": "T2", "name": "Globex", "role": "owner", "spaceId": "S2"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/teams/T1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"teamId": "T1", "name": "Acme", "role": "admin", "spaceId": "S1"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/teams/T1/members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"members": [
            {"userId": "U1", "name": "Priya", "email": "p@acme.dev", "role": "owner", "joinedAt": 1},
            {"userId": "U2", "name": "Sam", "email": "s@acme.dev", "role": "editor"}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/teams/T1/invites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"invites": [
            {"inviteId": "I1", "role": "viewer", "maxUses": null, "uses": 0, "expiresAt": 1,
             "createdBy": {"userId": "U1", "name": "Priya"}}
        ]})))
        .mount(&server)
        .await;
    let s = session(&server);
    let list = teams::list(&s).await.unwrap();
    assert_eq!(list.teams[0].member_count, Some(3));
    assert_eq!(list.owned_count, Some(1));
    assert_eq!(list.limits.team_invites, None);
    assert_eq!(
        list.create_lock().as_deref(),
        Some("You can own up to 1 team on this server.")
    );
    assert_eq!(teams::create(&s, "Globex").await.unwrap().team_id, "T2");
    let d = teams::detail(&s, "T1", "U1").await.unwrap();
    assert_eq!(d.members.len(), 2);
    assert_eq!(
        d.invites.unwrap()[0].max_uses,
        None,
        "admins see invite links"
    );
    assert_eq!(d.me, "U1");
}

#[tokio::test]
async fn member_calls_send_the_documented_bodies_and_refusals_keep_their_code() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/v1/teams/T1/members/U2"))
        .and(body_json(json!({"role": "admin"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "userId": "U2", "name": "Sam Lee", "email": "sam@acme.dev", "avatarUrl": null,
            "role": "admin", "joinedAt": 1795435200000i64, "isBillingOwner": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/teams/T1/members/U3"))
        .and(body_json(json!({"revokeInvites": true})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"userId": "U3", "revokedInvites": 2})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/teams/T1/members/U1"))
        .respond_with(refused(409, "transfer_required"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/teams/T1/transfer"))
        .and(body_json(json!({"userId": "U2"})))
        .respond_with(refused(413, "quota_exceeded"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/teams/T1/leave"))
        .respond_with(refused(409, "last_owner"))
        .mount(&server)
        .await;
    let s = session(&server);
    teams::set_role(&s, "T1", "U2", "admin").await.unwrap();
    teams::remove_member(&s, "T1", "U3", true).await.unwrap();
    let code = |e: AuthError| match e {
        AuthError::Refused { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        code(
            teams::remove_member(&s, "T1", "U1", false)
                .await
                .unwrap_err()
        ),
        "transfer_required"
    );
    assert_eq!(
        code(teams::transfer(&s, "T1", "U2").await.unwrap_err()),
        "quota_exceeded"
    );
    assert_eq!(
        code(teams::leave(&s, "T1").await.unwrap_err()),
        "last_owner"
    );
}

#[tokio::test]
async fn the_server_url_wins_and_teams_disabled_is_a_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/teams/T1/invites"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "inviteId": "I2", "role": "viewer", "maxUses": null, "uses": 0,
            "token": "rhi_abc", "url": "https://rh.example.com/i#rhi_abc"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/teams"))
        .respond_with(refused(403, "teams_disabled"))
        .mount(&server)
        .await;
    let s = session(&server);
    let c = teams::create_invite(&s, "T1", "viewer", None, 30)
        .await
        .unwrap();
    assert_eq!(
        c.url, "https://rh.example.com/i#rhi_abc",
        "REQHARBOR_PUBLIC_URL's link"
    );
    assert!(matches!(
        teams::list(&s).await.unwrap_err(),
        AuthError::Refused { code, .. } if code == "teams_disabled"
    ));
    // The UI copy defers to the server for configured limits.
    assert!(teams::refusal_message("limit_teams", "").is_none());
    assert!(teams::refusal_message("member_not_found", "").is_some());
}

#[tokio::test]
async fn invites_create_preview_accept_and_ticket() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/teams/T1/invites"))
        .and(body_json(json!({"role": "editor", "maxUses": 1, "expiresInDays": 7})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "inviteId": "I1", "token": "tok_123", "role": "editor", "maxUses": 1, "uses": 0, "expiresAt": 2,
            "createdAt": 1, "createdBy": {"userId": "U1", "name": "Priya Shah"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/invites/preview"))
        .and(body_json(json!({"token": "tok_123"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "teamName": "Acme", "inviterName": "Priya Shah", "role": "editor", "expiresAt": 1700000000
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/invites/preview"))
        .and(body_json(json!({"token": "gone"})))
        .respond_with(refused(410, "invite_invalid"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/invites/accept"))
        .and(header("authorization", "Bearer rha_A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "teamId": "T1", "spaceId": "S1", "teamName": "Acme", "role": "editor", "alreadyMember": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/invites/ticket"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ticket": "tk_1", "expiresAt": 1})),
        )
        .mount(&server)
        .await;
    let s = session(&server);
    let created = teams::create_invite(&s, "T1", "editor", Some(1), 7)
        .await
        .unwrap();
    assert_eq!(created.url, format!("{}/i#tok_123", server.uri()));
    let link = parse_invite_link(&created.url).unwrap();
    let p = teams::preview(&link).await.unwrap();
    assert_eq!(p.inviter_name.as_deref(), Some("Priya Shah"));
    assert_eq!(p.expires_at, Some(1_700_000_000_000));
    let gone = InviteLink {
        origin: link.origin.clone(),
        token: "gone".into(),
    };
    assert!(matches!(
        teams::preview(&gone).await.unwrap_err(),
        AuthError::Refused { code, .. } if code == "invite_invalid"
    ));
    let a = teams::accept(&s, &link.token).await.unwrap();
    assert_eq!(a.space_id.as_deref(), Some("S1"));
    assert!(!a.already_member);
    assert_eq!(teams::ticket(&link).await.unwrap(), "tk_1");
    // The preview and ticket never carry the session.
    for r in server.received_requests().await.unwrap() {
        if r.url.path() == "/v1/invites/preview" || r.url.path() == "/v1/invites/ticket" {
            assert!(r.headers.get("authorization").is_none());
        }
    }
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/teams"))
        .and(header("authorization", "Bearer rha_A"))
        .respond_with(refused(401, "session_expired"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "rha_B", "refresh_token": "rhr_B", "expires_in": 3600, "session_id": "S1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/teams"))
        .and(header("authorization", "Bearer rha_B"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"teams": []})))
        .mount(&server)
        .await;
    let s = session(&server);
    let l = teams::list(&s).await.unwrap();
    assert!(
        l.teams.is_empty() && l.create_lock().is_none(),
        "no limits sent: no lock"
    );
    assert_eq!(s.refresh_count(), 1);
}
