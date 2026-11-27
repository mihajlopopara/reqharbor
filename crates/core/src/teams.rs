//! Teams and invites on the client (04 §5.3–§5.4, 06-go-server §10 "M2
//! outline"): typed calls to `/v1/teams*` and `/v1/invites/*` over a
//! signed-in [`Session`], plus the `/i#<token>` invite link.
//!
//! Every shape is parsed leniently (unknown fields ignored, most fields
//! optional) so a newer server keeps working. A refusal carries the server's
//! code in [`AuthError::Refused`] (`forbidden_role`, `last_owner`,
//! `transfer_required`, `limit_teams`, `limit_members`, `limit_invites`,
//! `invite_invalid`, `not_a_member`, `quota_exceeded`, …).
//!
//! Teams exist only when the server advertises `features.teams` (06 §10);
//! the caller checks that before offering any of this.

use crate::auth::{AuthClient, AuthError, Session};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The four roles, by rank (04 §5.3): viewer 1, editor 2, admin 3, owner 4.
pub fn role_rank(role: &str) -> u8 {
    match role {
        "owner" => 4,
        "admin" => 3,
        "editor" => 2,
        "viewer" => 1,
        _ => 0,
    }
}

/// Seconds, milliseconds or an ISO date, as ms (shared with `/v1/me`).
fn lenient_ms<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Number(n) => n
            .as_i64()
            .map(|n| if n < 100_000_000_000 { n * 1000 } else { n }),
        _ => None,
    })
}

/// A user reference (`createdBy`, `invitedBy`); `name` is null for a former
/// member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRef {
    #[serde(default, alias = "id")]
    pub user_id: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// One row of `GET /v1/teams` (also the body of `POST /v1/teams` and
/// `GET /v1/teams/{id}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSummary {
    #[serde(alias = "id")]
    pub team_id: String,
    #[serde(default)]
    pub name: String,
    /// My role: `owner` | `admin` | `editor` | `viewer`.
    #[serde(default)]
    pub role: String,
    /// The team's one space (D13a).
    #[serde(default)]
    pub space_id: Option<String>,
    #[serde(default)]
    pub member_count: Option<u64>,
    #[serde(default)]
    pub bytes_used: Option<u64>,
    #[serde(default)]
    pub quota_bytes: Option<u64>,
    /// The billing owner (04 §4 `teams.owner_user_id`).
    #[serde(default)]
    pub owner_user_id: Option<String>,
    #[serde(default, deserialize_with = "lenient_ms")]
    pub created_at: Option<i64>,
}

/// One row of `GET /v1/teams/{id}/members`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    #[serde(alias = "id")]
    pub user_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub role: String,
    #[serde(default, deserialize_with = "lenient_ms")]
    pub joined_at: Option<i64>,
}

/// One active invite link (`GET /v1/teams/{id}/invites`). The token is
/// never listed: it is shown once, at creation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteInfo {
    #[serde(alias = "id")]
    pub invite_id: String,
    #[serde(default)]
    pub role: String,
    /// `null` = unlimited.
    #[serde(default)]
    pub max_uses: Option<u32>,
    #[serde(default)]
    pub uses: u32,
    #[serde(default, deserialize_with = "lenient_ms")]
    pub expires_at: Option<i64>,
    #[serde(default, deserialize_with = "lenient_ms")]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub created_by: Option<UserRef>,
}

/// `POST /v1/teams/{id}/invites`: the link is shown once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedInvite {
    pub invite: InviteInfo,
    /// `{server}/i#{token}`.
    pub url: String,
}

/// `POST /v1/invites/preview` (no auth).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitePreview {
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub team_name: String,
    #[serde(default)]
    pub inviter_name: Option<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default, deserialize_with = "lenient_ms")]
    pub expires_at: Option<i64>,
}

/// `POST /v1/invites/accept`: `alreadyMember` when nothing changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteAccepted {
    #[serde(default)]
    pub team_id: String,
    #[serde(default)]
    pub space_id: Option<String>,
    #[serde(default)]
    pub team_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub already_member: bool,
}

/// Team detail for the UI: the team, its members, and the invite links
/// (admins and owners only; `None` otherwise).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamDetail {
    pub team: TeamSummary,
    pub members: Vec<TeamMember>,
    pub invites: Option<Vec<InviteInfo>>,
    /// The signed-in user's id (for "(you)" and own-row actions).
    pub me: String,
}

/// An invite link `https://host/i#<token>`, split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteLink {
    /// Normalised origin of the server that issued it.
    pub origin: String,
    pub token: String,
}

pub const NOT_AN_INVITE: &str = "That isn't a ReqHarbor invite link.";

/// Parse a pasted invite link (UX §8). The token lives in the fragment, so
/// it never reaches a server log; a missing fragment or another path is not
/// an invite.
pub fn parse_invite_link(input: &str) -> Result<InviteLink, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(NOT_AN_INVITE.into());
    }
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let url = url::Url::parse(&with_scheme).map_err(|_| NOT_AN_INVITE.to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(NOT_AN_INVITE.into());
    }
    let path = url.path().trim_end_matches('/');
    let token = url.fragment().unwrap_or("").trim();
    if path != "/i" || token.is_empty() || token.len() > 512 {
        return Err(NOT_AN_INVITE.into());
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~')
    {
        return Err(NOT_AN_INVITE.into());
    }
    let origin = crate::server_info::normalize_server_url(&url.origin().ascii_serialization())
        .map_err(|_| NOT_AN_INVITE.to_string())?;
    Ok(InviteLink {
        origin,
        token: token.to_string(),
    })
}

/// The link shown once after creating an invite.
pub fn invite_url(origin: &str, token: &str) -> String {
    format!("{}/i#{token}", origin.trim_end_matches('/'))
}

fn enc(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

/// An authenticated team call (refresh once on 401).
async fn call<T: serde::de::DeserializeOwned>(
    session: &Session,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<T, AuthError> {
    session
        .call(|t| {
            let method = method.clone();
            let body = body.clone();
            async move {
                session
                    .client()
                    .api(method, path, Some(&t), body.as_ref())
                    .await
            }
        })
        .await
}

/// Server limits from `GET /v1/teams` (`null` = no cap).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamLimits {
    #[serde(default)]
    pub owned_teams: Option<u32>,
    #[serde(default)]
    pub team_members: Option<u32>,
    #[serde(default)]
    pub team_invites: Option<u32>,
}

/// Storage the caller is billed for (personal space plus owned teams).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerUsage {
    #[serde(default)]
    pub bytes_used: Option<u64>,
    #[serde(default)]
    pub quota_bytes: Option<u64>,
}

/// `GET /v1/teams`: my teams, how many I own, and the server's limits
/// (Create team is disabled at `limits.ownedTeams`, UX §7.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamsOverview {
    #[serde(default)]
    pub teams: Vec<TeamSummary>,
    #[serde(default)]
    pub owned_count: Option<u32>,
    #[serde(default)]
    pub limits: TeamLimits,
    #[serde(default)]
    pub owner: Option<OwnerUsage>,
}

impl TeamsOverview {
    /// Why Create team is disabled: "You can own up to 10 teams on this
    /// server." (`None` while below the limit or when there is none).
    pub fn create_lock(&self) -> Option<String> {
        let max = self.limits.owned_teams?;
        (self.owned_count? >= max).then(|| {
            format!(
                "You can own up to {max} team{} on this server.",
                if max == 1 { "" } else { "s" }
            )
        })
    }
}

#[derive(Deserialize)]
struct MemberList {
    #[serde(default)]
    members: Vec<TeamMember>,
}

#[derive(Deserialize)]
struct InviteList {
    #[serde(default)]
    invites: Vec<InviteInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InviteCreated {
    #[serde(flatten)]
    invite: InviteInfo,
    #[serde(default)]
    token: String,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct Ticket {
    #[serde(default)]
    ticket: String,
}

/// `GET /v1/teams`: teams I belong to (personal teams are hidden), my
/// owned-team count and the server's limits.
pub async fn list(session: &Session) -> Result<TeamsOverview, AuthError> {
    call(session, Method::GET, "/v1/teams", None).await
}

/// `POST /v1/teams {name}` (409 `limit_teams` past the owned-team limit).
pub async fn create(session: &Session, name: &str) -> Result<TeamSummary, AuthError> {
    call(
        session,
        Method::POST,
        "/v1/teams",
        Some(json!({ "name": name })),
    )
    .await
}

/// `GET /v1/teams/{id}`.
pub async fn get(session: &Session, team_id: &str) -> Result<TeamSummary, AuthError> {
    call(
        session,
        Method::GET,
        &format!("/v1/teams/{}", enc(team_id)),
        None,
    )
    .await
}

/// `GET /v1/teams/{id}/members`.
pub async fn members(session: &Session, team_id: &str) -> Result<Vec<TeamMember>, AuthError> {
    let l: MemberList = call(
        session,
        Method::GET,
        &format!("/v1/teams/{}/members", enc(team_id)),
        None,
    )
    .await?;
    Ok(l.members)
}

/// The detail view: the team, its members and (admin+) its invite links.
pub async fn detail(session: &Session, team_id: &str, me: &str) -> Result<TeamDetail, AuthError> {
    let team = get(session, team_id).await?;
    let members = members(session, team_id).await?;
    let invites = if role_rank(&team.role) >= 3 {
        Some(invites(session, team_id).await?)
    } else {
        None
    };
    Ok(TeamDetail {
        team,
        members,
        invites,
        me: me.to_string(),
    })
}

/// `PATCH /v1/teams/{id} {name}` (admin).
pub async fn rename(session: &Session, team_id: &str, name: &str) -> Result<(), AuthError> {
    call::<Value>(
        session,
        Method::PATCH,
        &format!("/v1/teams/{}", enc(team_id)),
        Some(json!({ "name": name })),
    )
    .await
    .map(|_| ())
}

/// `DELETE /v1/teams/{id}` (billing owner).
pub async fn delete(session: &Session, team_id: &str) -> Result<(), AuthError> {
    call::<Value>(
        session,
        Method::DELETE,
        &format!("/v1/teams/{}", enc(team_id)),
        None,
    )
    .await
    .map(|_| ())
}

/// `PATCH /v1/teams/{id}/members/{uid} {role}`.
pub async fn set_role(
    session: &Session,
    team_id: &str,
    user_id: &str,
    role: &str,
) -> Result<(), AuthError> {
    call::<Value>(
        session,
        Method::PATCH,
        &format!("/v1/teams/{}/members/{}", enc(team_id), enc(user_id)),
        Some(json!({ "role": role })),
    )
    .await
    .map(|_| ())
}

/// `DELETE /v1/teams/{id}/members/{uid} {revokeInvites}`.
pub async fn remove_member(
    session: &Session,
    team_id: &str,
    user_id: &str,
    revoke_invites: bool,
) -> Result<(), AuthError> {
    call::<Value>(
        session,
        Method::DELETE,
        &format!("/v1/teams/{}/members/{}", enc(team_id), enc(user_id)),
        Some(json!({ "revokeInvites": revoke_invites })),
    )
    .await
    .map(|_| ())
}

/// `POST /v1/teams/{id}/leave` (not for the billing owner or last owner).
pub async fn leave(session: &Session, team_id: &str) -> Result<(), AuthError> {
    call::<Value>(
        session,
        Method::POST,
        &format!("/v1/teams/{}/leave", enc(team_id)),
        None,
    )
    .await
    .map(|_| ())
}

/// `POST /v1/teams/{id}/transfer {userId}` (billing owner).
pub async fn transfer(session: &Session, team_id: &str, user_id: &str) -> Result<(), AuthError> {
    call::<Value>(
        session,
        Method::POST,
        &format!("/v1/teams/{}/transfer", enc(team_id)),
        Some(json!({ "userId": user_id })),
    )
    .await
    .map(|_| ())
}

/// `GET /v1/teams/{id}/invites` (admin).
pub async fn invites(session: &Session, team_id: &str) -> Result<Vec<InviteInfo>, AuthError> {
    let l: InviteList = call(
        session,
        Method::GET,
        &format!("/v1/teams/{}/invites", enc(team_id)),
        None,
    )
    .await?;
    Ok(l.invites)
}

/// `POST /v1/teams/{id}/invites {role, maxUses, expiresInDays}` (admin;
/// admin links only by an owner and single-use, AC-INV-1).
pub async fn create_invite(
    session: &Session,
    team_id: &str,
    role: &str,
    max_uses: Option<u32>,
    expires_in_days: u32,
) -> Result<CreatedInvite, AuthError> {
    let c: InviteCreated = call(
        session,
        Method::POST,
        &format!("/v1/teams/{}/invites", enc(team_id)),
        Some(json!({ "role": role, "maxUses": max_uses, "expiresInDays": expires_in_days })),
    )
    .await?;
    let url = match c.url.filter(|u| !u.trim().is_empty()) {
        Some(u) => u,
        None if !c.token.is_empty() => invite_url(session.origin(), &c.token),
        None => return Err(AuthError::Protocol("invite without a token".into())),
    };
    Ok(CreatedInvite {
        invite: c.invite,
        url,
    })
}

/// `DELETE /v1/teams/{id}/invites/{iid}` (admin).
pub async fn revoke_invite(
    session: &Session,
    team_id: &str,
    invite_id: &str,
) -> Result<(), AuthError> {
    call::<Value>(
        session,
        Method::DELETE,
        &format!("/v1/teams/{}/invites/{}", enc(team_id), enc(invite_id)),
        None,
    )
    .await
    .map(|_| ())
}

/// `POST /v1/invites/preview {token}` (no auth; 410 `invite_invalid`).
pub async fn preview(link: &InviteLink) -> Result<InvitePreview, AuthError> {
    AuthClient::new(&link.origin)?
        .api(
            Method::POST,
            "/v1/invites/preview",
            None,
            Some(&json!({ "token": link.token })),
        )
        .await
}

/// `POST /v1/invites/accept {token}`.
pub async fn accept(session: &Session, token: &str) -> Result<InviteAccepted, AuthError> {
    call(
        session,
        Method::POST,
        "/v1/invites/accept",
        Some(json!({ "token": token })),
    )
    .await
}

/// `POST /v1/invites/ticket {token}` (no auth): a 10-minute signup ticket
/// for the authorize URL, so the token itself never appears in a URL.
pub async fn ticket(link: &InviteLink) -> Result<String, AuthError> {
    let t: Ticket = AuthClient::new(&link.origin)?
        .api(
            Method::POST,
            "/v1/invites/ticket",
            None,
            Some(&json!({ "token": link.token })),
        )
        .await?;
    if t.ticket.is_empty() {
        return Err(AuthError::Protocol(
            "ticket response without a ticket".into(),
        ));
    }
    Ok(t.ticket)
}

/// The UX copy for a team or invite refusal, by code (UX §7, §8). `None`
/// keeps the server's own message, which names its configured limits
/// (`limit_teams`, `limit_members`, `limit_invites`, `quota_exceeded`) and
/// the validation problem (`validation_failed`).
pub fn refusal_message(code: &str, team: &str) -> Option<String> {
    let object = if team.is_empty() { "this team" } else { team };
    Some(match code {
        "last_owner" => "A team needs at least one owner".into(),
        "transfer_required" => "Transfer ownership first".into(),
        "invite_invalid" => "This invite link is no longer valid.".into(),
        "forbidden_role" => "Your role doesn't allow that.".into(),
        "not_a_member" => format!("You're no longer a member of {object}."),
        "team_not_found" => "This team no longer exists.".into(),
        "member_not_found" => "That person isn't in this team any more.".into(),
        "invite_not_found" => "That invite link is already gone.".into(),
        "teams_disabled" => "Teams aren't turned on for this server.".into(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_links_parse_and_reject() {
        let l = parse_invite_link("https://rh.acme.dev/i#k3J9-Qz_x").unwrap();
        assert_eq!(l.origin, "https://rh.acme.dev");
        assert_eq!(l.token, "k3J9-Qz_x");
        let bare = parse_invite_link("  rh.acme.dev/i#abc ").unwrap();
        assert_eq!(bare.origin, "https://rh.acme.dev");
        assert_eq!(
            parse_invite_link("http://192.168.1.5:8080/i/#tok").unwrap(),
            InviteLink {
                origin: "http://192.168.1.5:8080".into(),
                token: "tok".into()
            }
        );
        for bad in [
            "",
            "https://rh.acme.dev/i",
            "https://rh.acme.dev/i#",
            "https://rh.acme.dev/s/abc#tok",
            "ftp://rh.acme.dev/i#tok",
            "https://rh.acme.dev/i#tok with space",
        ] {
            assert_eq!(parse_invite_link(bad).unwrap_err(), NOT_AN_INVITE, "{bad}");
        }
    }

    #[test]
    fn shapes_parse_leniently() {
        let t: TeamSummary = serde_json::from_value(json!({
            "id": "T1", "name": "Acme", "role": "owner", "memberCount": 8, "createdAt": 1700000000,
            "somethingNew": true
        }))
        .unwrap();
        assert_eq!(t.team_id, "T1");
        assert_eq!(t.created_at, Some(1_700_000_000_000));
        let m: TeamMember =
            serde_json::from_value(json!({"userId": "U", "name": "Sam", "role": "editor"}))
                .unwrap();
        assert_eq!(m.email, "");
        let i: InviteInfo = serde_json::from_value(json!({"inviteId": "I", "role": "viewer", "maxUses": null, "uses": 2, "createdBy": {"userId": "U", "name": null}})).unwrap();
        assert_eq!(i.max_uses, None);
        assert_eq!(i.created_by.unwrap().name, None);
        assert_eq!(role_rank("admin"), 3);
        assert_eq!(role_rank("nope"), 0);
    }

    #[test]
    fn refusal_copy() {
        assert_eq!(
            refusal_message("last_owner", "Acme").unwrap(),
            "A team needs at least one owner"
        );
        assert!(refusal_message("whatever", "Acme").is_none());
    }
}
