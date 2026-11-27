//! Live sign-in against a running Go server (06-go-server §7.5, §9).
//! Ignored by default; run with
//!   REQHARBOR_SERVER_URL=http://127.0.0.1:8080 cargo test -p reqharbor-core --test qa_auth_live -- --ignored --test-threads=1
//!
//! A `reqwest` client with a hand-kept cookie jar plays the browser over
//! loopback: authorize, the hosted sign-in, the redirect to the core's real
//! loopback listener, then the code exchange. The hosted part uses:
//! * the dev provider of a `-tags devauth` build when `REQHARBOR_DEV_AUTH_PATH`
//!   names its form action (for example `/auth/dev`), posting `ar`, `email`
//!   and `name`; otherwise
//! * email and password: signup (with `REQHARBOR_LIVE_SIGNUP_CODE` as the
//!   invite or setup code when the server needs one), falling back to login.

use reqharbor_core::CancellationToken;
use reqharbor_core::auth::{
    self, AuthClient, AuthError, LoopbackConfig, PendingSignIn, ServerIdentity, Session,
};
use reqharbor_core::secrets::{MemorySecretStore, SecretStore};
use reqharbor_core::server_info::{self, DiscoveryStatus};
use std::sync::Arc;
use std::time::Duration;

fn server() -> String {
    std::env::var("REQHARBOR_SERVER_URL")
        .expect("REQHARBOR_SERVER_URL")
        .trim_end_matches('/')
        .to_string()
}

/// A minimal browser: follows redirects by hand, keeps cookies per name.
struct Browser {
    http: reqwest::Client,
    cookies: std::collections::BTreeMap<String, String>,
}

impl Browser {
    fn new() -> Self {
        reqharbor_core::http::install_crypto_provider();
        Browser {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap(),
            cookies: Default::default(),
        }
    }

    fn keep_cookies(&mut self, resp: &reqwest::Response) {
        for v in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Some((pair, _)) = v.to_str().unwrap_or("").split_once(';')
                && let Some((k, val)) = pair.split_once('=')
            {
                self.cookies.insert(k.trim().into(), val.trim().into());
            }
        }
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Send, then follow redirects (to the server and to the loopback).
    async fn go(&mut self, mut req: reqwest::RequestBuilder) -> (u16, String, String) {
        let mut url;
        for _ in 0..10 {
            let resp = req
                .header(reqwest::header::COOKIE, self.cookie_header())
                .send()
                .await
                .unwrap();
            url = resp.url().to_string();
            self.keep_cookies(&resp);
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                let loc = resp.headers()[reqwest::header::LOCATION]
                    .to_str()
                    .unwrap()
                    .to_string();
                let next = reqwest::Url::parse(&url).unwrap().join(&loc).unwrap();
                req = self.http.get(next);
                continue;
            }
            let body = resp.text().await.unwrap_or_default();
            return (status, url, body);
        }
        panic!("too many redirects");
    }
}

fn form(rb: reqwest::RequestBuilder, pairs: &[(&str, &str)]) -> reqwest::RequestBuilder {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    rb.header(
        reqwest::header::CONTENT_TYPE,
        "application/x-www-form-urlencoded",
    )
    .body(body)
}

fn hidden(html: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\" value=\"");
    let start = html.find(&needle)? + needle.len();
    let end = html[start..].find('"')? + start;
    Some(html[start..end].to_string())
}

struct SignedIn {
    session: Session,
    client: AuthClient,
    secrets: Arc<MemorySecretStore>,
    account: auth::Account,
    _dir: tempfile::TempDir,
}

async fn sign_in() -> SignedIn {
    let origin = server_info::normalize_server_url(&server()).unwrap();
    let d = server_info::discover_origin(&origin).await;
    assert_eq!(d.status, DiscoveryStatus::Ok, "{d:?}");
    let pending = PendingSignIn::start(&origin, None, "Live test (Linux)")
        .await
        .unwrap();
    let authorize = pending.authorize_url.clone();
    let waiter = tokio::spawn(pending.wait(
        LoopbackConfig {
            timeout: Duration::from_secs(60),
            ..Default::default()
        },
        CancellationToken::new(),
    ));

    let mut b = Browser::new();
    let (status, page_url, page) = b.go(b.http.get(&authorize)).await;
    assert_eq!(status, 200, "authorize page: {page}");
    let ar = hidden(&page, "ar").expect("the authorize page carries ar");
    let base = reqwest::Url::parse(&page_url).unwrap();
    let email = format!(
        "live-{}@example.test",
        reqharbor_core::model::new_id().to_lowercase()
    );
    let password = "correct horse battery staple";
    let code = std::env::var("REQHARBOR_LIVE_SIGNUP_CODE").unwrap_or_default();
    let (status, _, body) = if let Ok(path) = std::env::var("REQHARBOR_DEV_AUTH_PATH") {
        let pairs = [
            ("ar", ar.as_str()),
            ("email", email.as_str()),
            ("name", "Live Test"),
        ];
        b.go(form(b.http.post(base.join(&path).unwrap()), &pairs))
            .await
    } else {
        let pairs = [
            ("ar", ar.as_str()),
            ("name", "Live Test"),
            ("email", email.as_str()),
            ("password", password),
            ("code", code.as_str()),
        ];
        b.go(form(
            b.http.post(base.join("/auth/password/signup").unwrap()),
            &pairs,
        ))
        .await
    };
    assert_eq!(status, 200, "hosted sign-in ended on: {body}");
    assert!(body.contains("signed in"), "done page: {body}");

    let received = waiter.await.unwrap().expect("the loopback got the code");
    let client = AuthClient::new(&origin).unwrap();
    let resp = received.exchange(&client).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::new());
    let info = d.info.unwrap();
    let (account, session) = auth::complete_sign_in(
        dir.path(),
        secrets.clone(),
        &client,
        &ServerIdentity {
            instance_id: info.instance_id,
            name: info.name,
        },
        None,
        None,
        resp,
    )
    .await
    .unwrap();
    assert_eq!(account.email, email);
    assert!(secrets.keys()[0].starts_with(&format!("session:{origin}:")));
    SignedIn {
        session,
        client,
        secrets,
        account,
        _dir: dir,
    }
}

#[tokio::test]
#[ignore]
async fn live_sign_in_rotation_grace_and_logout() {
    let s = sign_in().await;
    let key = s.session.secret_key().to_string();
    let stored: auth::Tokens =
        serde_json::from_str(&s.secrets.get(&key).unwrap().unwrap()).unwrap();

    // Rotation, then the 60 s grace window returns the identical pair.
    let first = s.client.refresh(&stored.refresh_token).await.unwrap();
    assert_ne!(first.refresh_token, stored.refresh_token);
    let again = s.client.refresh(&stored.refresh_token).await.unwrap();
    assert_eq!(again.access_token, first.access_token);
    assert_eq!(again.refresh_token, first.refresh_token);

    // Devices list this session as current.
    let rotated = Session::new(
        s.client.clone(),
        s.client.base_url(),
        "u",
        s.secrets.clone(),
        auth::Tokens::from_response(&first, reqharbor_core::model::now_ms(), "x"),
    );
    match rotated.sessions().await {
        Ok(rows) => assert!(rows.iter().any(|r| r.current)),
        Err(AuthError::Rejected(m)) => eprintln!("/v1/me/sessions not served yet: {m}"),
        Err(e) => panic!("{e:?}"),
    }

    // Logout: the access token stops working and the session is gone.
    rotated.logout().await.unwrap();
    let err = s.client.refresh(&first.refresh_token).await.unwrap_err();
    assert_eq!(err, AuthError::InvalidGrant);
    assert_eq!(rotated.me().await.unwrap_err(), AuthError::SignInRequired);
}

#[tokio::test]
#[ignore]
async fn live_personal_space_push_and_pull() {
    let s = sign_in().await;
    let token = s.session.access_token().await.unwrap();
    let http = Browser::new().http;
    let resp = http
        .get(format!("{}/v1/me/spaces", s.client.base_url()))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    if resp.status() == 404 {
        eprintln!("personal spaces are not served yet (S6); skipping");
        return;
    }
    let spaces: serde_json::Value = resp.json().await.unwrap();
    let space = spaces["spaces"][0]["spaceId"].as_str().unwrap().to_string();
    let url = format!("{}/v1/spaces/{space}/sync", s.client.base_url());
    let now = reqharbor_core::model::now_ms();
    let push = serde_json::json!({"changes": [{"type": "project", "id": "01JLIVEPROJECT000000000000", "updatedAt": now,
        "deleted": false, "data": {"id": "01JLIVEPROJECT000000000000", "name": "Live"}, "deviceId": "live"}]});
    let r: serde_json::Value = http
        .post(&url)
        .bearer_auth(&token)
        .json(&push)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["results"][0]["status"], "applied", "{r}");
    let pulled: serde_json::Value = http
        .get(format!("{url}?since=0"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        pulled["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "01JLIVEPROJECT000000000000"),
        "{pulled}"
    );
}

/// The C2 round trip through the real engine: device A signs in (the
/// migration binds its fresh Default to Personal), creates a project and
/// syncs; device B with the same account pulls it; B's edit reaches A.
#[tokio::test]
#[ignore]
async fn live_personal_sync_round_trip_through_the_engine() {
    use reqharbor_core::model::Request;
    use reqharbor_core::space_sync::{self, RUN_BUDGET, StoreAccess};
    use reqharbor_core::spaces::{AccountIdent, Scope};
    use reqharbor_core::storage::Store;
    use std::sync::Mutex;

    let s = sign_in().await;
    match s.session.spaces().await {
        Ok(list) => assert!(
            list.spaces.iter().any(|x| x.kind == "personal"),
            "a personal space is created at signup: {list:?}"
        ),
        Err(AuthError::Rejected(m)) => {
            eprintln!("personal spaces are not served yet (S6): {m}; skipping");
            return;
        }
        Err(e) => panic!("{e:?}"),
    }
    let who = AccountIdent::from(&s.account);
    let session = Arc::new(s.session);
    let device = || {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, Mutex::new(store))
    };
    let (_da, a) = device();
    let (_db, b) = device();
    for d in [&a, &b] {
        d.with(|st| st.sign_in_migrate(who.clone(), s.secrets.as_ref()).unwrap());
    }

    let run = space_sync::run(&a, Some(session.clone()), RUN_BUDGET).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    let personal = a.with(|st| st.personal_space().unwrap().space_id.clone());
    let (pid, rid) = a.with(|st| {
        let p = st.create_project("Live sync").unwrap().project.id;
        let c = st.create_collection(&p, "API").unwrap();
        let r = st
            .create_request(
                &c.id,
                None,
                Request {
                    method: "GET".into(),
                    url: "https://example.test/live".into(),
                    ..Request::default()
                },
            )
            .unwrap();
        (p, r.id)
    });
    let run = space_sync::run(&a, Some(session.clone()), RUN_BUDGET).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    assert!(run.report.pushed >= 3, "{:?}", run.report);
    let scope = Scope::Space(personal.clone());
    assert_eq!(
        a.with(|st| reqharbor_core::sync::pending_count_in(st, &scope)),
        0
    );

    let run = space_sync::run(&b, Some(session.clone()), RUN_BUDGET).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    b.with(|st| {
        assert_eq!(st.project(&pid).unwrap().project.name, "Live sync");
        assert!(st.request(&rid).is_some());
        assert_eq!(st.bindings.project_spaces[&pid], personal);
    });

    // B edits; A pulls the edit, with attribution kept out of the entity.
    b.with(|st| st.rename_request(&rid, "Renamed on B").unwrap());
    let run = space_sync::run(&b, Some(session.clone()), RUN_BUDGET).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    let run = space_sync::run(&a, Some(session.clone()), RUN_BUDGET).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    a.with(|st| {
        assert_eq!(st.request(&rid).unwrap().name, "Renamed on B");
        assert!(st.request(&rid).unwrap().extra.get("updatedBy").is_none());
    });

    // Deleting on A removes it from B.
    a.with(|st| st.delete_project(&pid).unwrap());
    space_sync::run(&a, Some(session.clone()), RUN_BUDGET).await;
    space_sync::run(&b, Some(session.clone()), RUN_BUDGET).await;
    assert!(b.with(|st| st.project(&pid).is_err()));
}

// ───────────────────────────── M2: teams (06 §10 M2 outline) ─────────────────────────────

/// Whether the server advertises `features.teams`; the M2 live cases skip
/// otherwise (teams are off by default, `REQHARBOR_TEAMS=false`).
async fn teams_enabled() -> bool {
    let origin = server_info::normalize_server_url(&server()).unwrap();
    let d = server_info::discover_origin(&origin).await;
    let on = d.info.as_ref().is_some_and(|i| i.features.teams);
    if !on {
        eprintln!("features.teams is off on {origin}; skipping");
    }
    on
}

/// Create, invite, join, preview, change roles, refuse a viewer push, remove,
/// and see the removal in `/v1/me/spaces` `departed[]`.
#[tokio::test]
#[ignore]
async fn live_team_invite_join_roles_and_removal() {
    use reqharbor_core::teams;
    if !teams_enabled().await {
        return;
    }
    let owner = sign_in().await;
    let team = teams::create(&owner.session, "Live Acme").await.unwrap();
    assert_eq!(team.role, "owner");
    let space = team.space_id.clone().expect("a team has a space");
    let invite = teams::create_invite(&owner.session, &team.team_id, "editor", Some(1), 7)
        .await
        .unwrap();
    let link = teams::parse_invite_link(&invite.url).unwrap();
    let preview = teams::preview(&link).await.unwrap();
    assert_eq!(preview.team_name, "Live Acme");
    assert_eq!(preview.role, "editor");

    let member = sign_in().await;
    let joined = teams::accept(&member.session, &link.token).await.unwrap();
    assert_eq!(joined.team_id, team.team_id);
    assert!(!joined.already_member);
    // A single-use link is spent.
    assert!(matches!(
        teams::preview(&link).await.unwrap_err(),
        AuthError::Refused { code, .. } if code == "invite_invalid"
    ));
    let me = member.session.spaces().await.unwrap();
    assert!(
        me.spaces
            .iter()
            .any(|s| s.space_id == space && s.role.as_deref() == Some("editor"))
    );

    // A viewer's push is refused as a whole.
    teams::set_role(
        &owner.session,
        &team.team_id,
        &member.account.user_id,
        "viewer",
    )
    .await
    .unwrap();
    let token = member.session.access_token().await.unwrap();
    let push = serde_json::json!({"changes": [{"type": "project", "id": "01JLIVETEAMPROJ00000000000",
        "updatedAt": reqharbor_core::model::now_ms(), "deleted": false,
        "data": {"id": "01JLIVETEAMPROJ00000000000", "name": "Nope"}, "deviceId": "live"}]});
    let resp = Browser::new()
        .http
        .post(format!(
            "{}/v1/spaces/{space}/sync",
            member.client.base_url()
        ))
        .bearer_auth(&token)
        // Team spaces require the version header (06 §10 "M2 as built").
        .header(
            reqharbor_core::sync::VERSION_HEADER,
            reqharbor_core::model::CLIENT_VERSION,
        )
        .json(&push)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "forbidden_role");

    // The billing owner can't be removed by anyone without a transfer.
    let detail = teams::detail(&owner.session, &team.team_id, &owner.account.user_id)
        .await
        .unwrap();
    assert_eq!(detail.members.len(), 2);
    teams::remove_member(&owner.session, &team.team_id, &member.account.user_id, true)
        .await
        .unwrap();
    let after = member.session.spaces().await.unwrap();
    assert!(!after.spaces.iter().any(|s| s.space_id == space));
    assert!(
        after
            .departed
            .iter()
            .any(|d| d.space_id == space && d.reason.as_deref() == Some("removed"))
    );
    teams::delete(&owner.session, &team.team_id).await.unwrap();
}

/// Move a personal project into a team through the engine (new ids), then
/// see it in the team's pull.
#[tokio::test]
#[ignore]
async fn live_team_move_project_through_the_engine() {
    use reqharbor_core::space_sync::{self, RUN_BUDGET};
    use reqharbor_core::spaces::AccountIdent;
    use reqharbor_core::storage::Store;
    use reqharbor_core::{teams, transfer};
    if !teams_enabled().await {
        return;
    }
    let s = sign_in().await;
    let team = teams::create(&s.session, "Live Move").await.unwrap();
    let space = team.space_id.clone().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Mutex::new(Store::open(dir.path()).unwrap());
    let origin = s.client.base_url().to_string();
    let who = AccountIdent {
        server_url: origin.clone(),
        server_instance_id: s.account.server_instance_id.clone(),
        user_id: s.account.user_id.clone(),
        email: s.account.email.clone(),
    };
    let secrets = MemorySecretStore::new();
    store
        .lock()
        .unwrap()
        .sign_in_migrate(who, &secrets)
        .unwrap();
    let session = Arc::new(s.session);
    space_sync::run(&store, Some(session.clone()), RUN_BUDGET).await;
    let pid = {
        let mut st = store.lock().unwrap();
        let p = st.create_project("Payments").unwrap().project.id;
        st.create_collection(&p, "Orders").unwrap();
        p
    };
    space_sync::run(&store, Some(session.clone()), RUN_BUDGET).await;
    let moved = transfer::move_project(
        &store,
        Some(session.clone()),
        &pid,
        Some(&space),
        &|_, _| {},
    )
    .await
    .unwrap();
    assert_ne!(moved.project_id, pid);
    space_sync::run(&store, Some(session.clone()), RUN_BUDGET).await;
    let client =
        reqharbor_core::sync::SyncClient::for_space(&origin, &space, session.clone()).unwrap();
    let pulled = client.pull_all(0).await.unwrap();
    assert!(
        pulled
            .changes
            .iter()
            .any(|c| c.id == moved.project_id && !c.deleted)
    );
    assert!(!pulled.changes.iter().any(|c| c.id == pid));
    teams::delete(&session, &team.team_id).await.unwrap();
}
