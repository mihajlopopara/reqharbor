//! Team, invite, move and copy commands (04 §10 "Teams" and "Spaces"
//! groups; 06 §10 M2). Teams exist only when the account's server
//! advertises `features.teams`; every team command checks that first.
//!
//! Server calls go through the signed-in [`Session`] (refresh once on 401).
//! Anything that changes which spaces this account has (create, join,
//! leave, delete) runs a sync afterwards, so `/v1/me/spaces` binds or
//! releases the space at once.

use crate::auth_rt::AuthState;
use crate::sync_rt::run_sync;
use crate::error::{CmdResult, CommandError};
use crate::state::{AppState, SyncStatus};
use reqharbor_core::auth::Session;
use reqharbor_core::model::ProjectTree;
use reqharbor_core::teams::{
    self, CreatedInvite, InviteInfo, InvitePreview, TeamDetail, TeamSummary,
};
use reqharbor_core::transfer::{self, MoveResult, TransferPreview};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

/// Progress of a move: "Moving… 42 of 120 items" (UX §9).
pub const EVENT_MOVE_PROGRESS: &str = "sync://move-progress";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveProgress {
    pub project_id: String,
    pub done: usize,
    pub total: usize,
}

fn teams_disabled() -> CommandError {
    CommandError::new(
        "teams_disabled",
        "Teams aren't turned on for this server. Ask its admin.",
    )
}

/// The session, when signed in to a server with `features.teams`.
fn team_session(app: &AppHandle) -> CmdResult<Arc<Session>> {
    let auth = app.state::<AuthState>();
    let status = auth.status();
    if status.account.is_none() {
        return Err(CommandError::new("not_signed_in", "You're not signed in."));
    }
    if status.sign_in_required {
        return Err(CommandError::new(
            "sign_in_required",
            "Your session ended. Sign in again to keep syncing.",
        ));
    }
    let on = status
        .server
        .as_ref()
        .and_then(|d| d.info.as_ref())
        .is_some_and(|i| i.features.teams);
    if !on {
        return Err(teams_disabled());
    }
    auth.session()
        .ok_or_else(|| CommandError::new("not_signed_in", "You're not signed in."))
}

/// The signed-in user's id.
fn me(app: &AppHandle) -> String {
    app.state::<AuthState>()
        .account()
        .map(|a| a.user_id)
        .unwrap_or_default()
}

/// A team's space id from the local bindings (for leave and delete).
fn space_of_team(app: &AppHandle, team_id: &str) -> Option<String> {
    let state = app.state::<AppState>();
    let store = state.store();
    store
        .bindings
        .spaces
        .iter()
        .find(|b| b.team_id.as_deref() == Some(team_id))
        .map(|b| b.space_id.clone())
}

/// Sync in the background and tell the UI (a failed sync isn't a failed
/// team action).
fn sync_soon(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = run_sync(&app).await;
    });
}

#[tauri::command]
pub async fn teams_list(app: AppHandle) -> CmdResult<teams::TeamsOverview> {
    let s = team_session(&app)?;
    Ok(teams::list(&s).await?)
}

/// Create a team (409 `limit_teams`), then sync so its space binds.
#[tauri::command]
pub async fn team_create(app: AppHandle, name: String) -> CmdResult<TeamSummary> {
    let s = team_session(&app)?;
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(CommandError::new(
            "invalid",
            "A team name is 1 to 80 characters.",
        ));
    }
    let team = teams::create(&s, &name).await?;
    let _ = run_sync(&app).await;
    Ok(team)
}

#[tauri::command]
pub async fn team_get(app: AppHandle, team_id: String) -> CmdResult<TeamDetail> {
    let s = team_session(&app)?;
    Ok(teams::detail(&s, &team_id, &me(&app)).await?)
}

#[tauri::command]
pub async fn team_rename(app: AppHandle, team_id: String, name: String) -> CmdResult<()> {
    let s = team_session(&app)?;
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(CommandError::new(
            "invalid",
            "A team name is 1 to 80 characters.",
        ));
    }
    teams::rename(&s, &team_id, &name).await?;
    sync_soon(&app);
    Ok(())
}

/// Delete a team (billing owner). This device keeps local copies of its
/// projects (D5); other members learn it from `departed[]`.
#[tauri::command]
pub async fn team_delete(app: AppHandle, team_id: String) -> CmdResult<SyncStatus> {
    let s = team_session(&app)?;
    teams::delete(&s, &team_id).await?;
    if let Some(space) = space_of_team(&app, &team_id) {
        let state = app.state::<AppState>();
        state.store().detach_space(&space, true)?;
    }
    let status = crate::sync_rt::refresh_status(&app);
    let _ = app.emit(
        crate::sync_rt::EVENT_CHANGED,
        &reqharbor_core::sync::SyncReport::default(),
    );
    sync_soon(&app);
    Ok(status)
}

#[tauri::command]
pub async fn team_set_role(
    app: AppHandle,
    team_id: String,
    user_id: String,
    role: String,
) -> CmdResult<()> {
    let s = team_session(&app)?;
    if !matches!(role.as_str(), "owner" | "admin" | "editor" | "viewer") {
        return Err(CommandError::new("invalid", format!("Unknown role {role}")));
    }
    teams::set_role(&s, &team_id, &user_id, &role).await?;
    // Lowering my own role changes what this device may push.
    if user_id == me(&app) {
        sync_soon(&app);
    }
    Ok(())
}

#[tauri::command]
pub async fn team_remove_member(
    app: AppHandle,
    team_id: String,
    user_id: String,
    revoke_invites: bool,
) -> CmdResult<()> {
    let s = team_session(&app)?;
    teams::remove_member(&s, &team_id, &user_id, revoke_invites).await?;
    Ok(())
}

/// Leave a team; its projects stay here as local copies when `keep_copy`
/// ("Keep a personal copy of 2 projects", D5), else they are removed.
#[tauri::command]
pub async fn team_leave(app: AppHandle, team_id: String, keep_copy: bool) -> CmdResult<SyncStatus> {
    let s = team_session(&app)?;
    teams::leave(&s, &team_id).await?;
    if let Some(space) = space_of_team(&app, &team_id) {
        let state = app.state::<AppState>();
        state.store().detach_space(&space, keep_copy)?;
    }
    let status = crate::sync_rt::refresh_status(&app);
    let _ = app.emit(
        crate::sync_rt::EVENT_CHANGED,
        &reqharbor_core::sync::SyncReport::default(),
    );
    Ok(status)
}

#[tauri::command]
pub async fn team_transfer(app: AppHandle, team_id: String, user_id: String) -> CmdResult<()> {
    let s = team_session(&app)?;
    teams::transfer(&s, &team_id, &user_id).await?;
    sync_soon(&app);
    Ok(())
}

#[tauri::command]
pub async fn invite_create(
    app: AppHandle,
    team_id: String,
    role: String,
    max_uses: Option<u32>,
    expires_in_days: u32,
) -> CmdResult<CreatedInvite> {
    let s = team_session(&app)?;
    if role == "admin" && max_uses != Some(1) {
        return Err(CommandError::new("invalid", "Admin links are single-use."));
    }
    if !matches!(expires_in_days, 1 | 7 | 30) {
        return Err(CommandError::new(
            "invalid",
            "Invite links expire after 1, 7 or 30 days.",
        ));
    }
    Ok(teams::create_invite(&s, &team_id, &role, max_uses, expires_in_days).await?)
}

#[tauri::command]
pub async fn invite_list(app: AppHandle, team_id: String) -> CmdResult<Vec<InviteInfo>> {
    let s = team_session(&app)?;
    Ok(teams::invites(&s, &team_id).await?)
}

#[tauri::command]
pub async fn invite_revoke(app: AppHandle, team_id: String, invite_id: String) -> CmdResult<()> {
    let s = team_session(&app)?;
    Ok(teams::revoke_invite(&s, &team_id, &invite_id).await?)
}

/// Parse an invite link and apply the sign-in's plain-http rule (06 §9):
/// the token goes to a non-loopback `http://` server only after "Continue
/// anyway" (now, or remembered for that origin in `knownServers`).
fn parse_link(app: &AppHandle, url: &str, insecure_confirmed: bool) -> CmdResult<teams::InviteLink> {
    let link =
        teams::parse_invite_link(url).map_err(|m| CommandError::new("invite_not_a_link", m))?;
    let acked = crate::auth_rt::insecure_acknowledged(
        &app.state::<AppState>().store().settings,
        &link.origin,
    );
    insecure_gate(&link.origin, insecure_confirmed || acked)?;
    Ok(link)
}

/// Plain http to a non-loopback host needs "Continue anyway" first.
fn insecure_gate(origin: &str, confirmed: bool) -> CmdResult<()> {
    if reqharbor_core::server_info::is_insecure(origin) && !confirmed {
        return Err(CommandError::new(
            "insecure_unconfirmed",
            "This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_tokens_go_to_plain_http_only_after_continue_anyway() {
        assert_eq!(
            insecure_gate("http://192.168.1.5:8080", false).unwrap_err().code,
            "insecure_unconfirmed"
        );
        assert!(insecure_gate("http://192.168.1.5:8080", true).is_ok());
        assert!(insecure_gate("https://rh.acme.dev", false).is_ok());
        assert!(insecure_gate("http://127.0.0.1:8080", false).is_ok());
    }
}

/// `invite_preview`: which server the link is for, and the team behind it
/// (no sign-in needed). The UI compares `serverUrl` with the account's.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteCheck {
    pub server_url: String,
    /// The signed-in account is on another server: "This invite is for
    /// api.acme.dev. You're signed in to reqharbor.app."
    pub other_server: bool,
    pub preview: InvitePreview,
}

#[tauri::command]
pub async fn invite_preview(
    app: AppHandle,
    url: String,
    insecure_confirmed: Option<bool>,
) -> CmdResult<InviteCheck> {
    let link = parse_link(&app, &url, insecure_confirmed.unwrap_or(false))?;
    let account = app.state::<AuthState>().account_origin();
    let preview = teams::preview(&link).await?;
    Ok(InviteCheck {
        other_server: account.is_some_and(|a| a.trim_end_matches('/') != link.origin),
        server_url: link.origin,
        preview,
    })
}

/// What `invite_accept` did: the toast "Joined Acme · syncing 2 projects".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinResult {
    pub team_id: String,
    pub team_name: String,
    pub role: String,
    pub space_id: Option<String>,
    pub already_member: bool,
    /// Projects of the team on this device after the first sync.
    pub projects: usize,
}

#[tauri::command]
pub async fn invite_accept(app: AppHandle, url: String) -> CmdResult<JoinResult> {
    // The signed-in server's own scheme was confirmed at sign-in.
    let link = parse_link(&app, &url, true)?;
    let s = team_session(&app)?;
    if s.origin().trim_end_matches('/') != link.origin {
        return Err(CommandError::new(
            "invite_other_server",
            "This invite is for another server. Sign in there to join.",
        ));
    }
    let joined = teams::accept(&s, &link.token).await?;
    let _ = run_sync(&app).await;
    let projects = joined
        .space_id
        .as_deref()
        .map(|sp| app.state::<AppState>().store().projects_in(sp).len())
        .unwrap_or(0);
    Ok(JoinResult {
        team_id: joined.team_id,
        team_name: joined.team_name,
        role: joined.role,
        space_id: joined.space_id,
        already_member: joined.already_member,
        projects,
    })
}

/// A signup ticket for an invitee without an account (06 §10 M2): pass it
/// to `auth_sign_in_start` as `inviteTicket`.
#[tauri::command]
pub async fn invite_ticket(
    app: AppHandle,
    url: String,
    insecure_confirmed: Option<bool>,
) -> CmdResult<String> {
    let link = parse_link(&app, &url, insecure_confirmed.unwrap_or(false))?;
    Ok(teams::ticket(&link).await?)
}

// ───────────────────────────── moves and copies ─────────────────────────────

fn target(space_id: Option<String>) -> Option<String> {
    space_id.filter(|s| !s.is_empty() && s != "local")
}

/// The Move pre-flight (UX §9): secrets, file paths, plain-text credentials.
#[tauri::command]
pub fn move_preview(
    app: AppHandle,
    project_id: String,
    space_id: Option<String>,
) -> CmdResult<TransferPreview> {
    let state = app.state::<AppState>();
    let store = state.store();
    Ok(store.move_preview(&project_id, target(space_id).as_deref())?)
}

/// Move a project to a team, or (admins) out of one (04 §6.5): online and
/// resumable. On failure nothing moved; calling it again resumes with the
/// same ids. Progress arrives on `sync://move-progress`.
#[tauri::command]
pub async fn move_project_to_space(
    app: AppHandle,
    project_id: String,
    space_id: Option<String>,
) -> CmdResult<MoveResult> {
    let to = target(space_id);
    let session = app.state::<AuthState>().session();
    let result = {
        let state = app.state::<AppState>();
        let _serial = state.sync_lock.lock().await;
        let emitter = app.clone();
        let pid = project_id.clone();
        let progress = move |done: usize, total: usize| {
            let _ = emitter.emit(
                EVENT_MOVE_PROGRESS,
                &MoveProgress {
                    project_id: pid.clone(),
                    done,
                    total,
                },
            );
        };
        transfer::move_project(
            state.store_mutex(),
            session,
            &project_id,
            to.as_deref(),
            &progress,
        )
        .await
    };
    // Success or not, the pending-move list changed.
    crate::sync_rt::refresh_status(&app);
    let moved = result.map_err(CommandError::from)?;
    let _ = app.emit(
        crate::sync_rt::EVENT_CHANGED,
        &reqharbor_core::sync::SyncReport::default(),
    );
    crate::sync_rt::schedule_push(&app);
    Ok(moved)
}

/// Cancel an interrupted move of `projectId`: tombstones for anything the
/// target already took, then the plan goes (04 §6.5 step 4).
#[tauri::command]
pub async fn move_cancel(app: AppHandle, project_id: String) -> CmdResult<()> {
    let session = app.state::<AuthState>().session();
    let state = app.state::<AppState>();
    let ids: Vec<String> = state
        .store()
        .pending_moves()
        .into_iter()
        .filter(|p| p.project_id == project_id)
        .map(|p| p.move_id)
        .collect();
    let _serial = state.sync_lock.lock().await;
    for id in ids {
        transfer::cancel_move(state.store_mutex(), session.clone(), &id)
            .await
            .map_err(CommandError::from)?;
    }
    crate::sync_rt::refresh_status(&app);
    let _ = app.emit(
        crate::sync_rt::EVENT_CHANGED,
        &reqharbor_core::sync::SyncReport::default(),
    );
    Ok(())
}

/// The Copy pre-flight for a team target (UX §9 copy mode).
#[tauri::command]
pub fn copy_preview(
    app: AppHandle,
    kind: String,
    id: String,
    target_project_id: String,
) -> CmdResult<TransferPreview> {
    let state = app.state::<AppState>();
    let store = state.store();
    Ok(store.copy_preview(&kind, &id, &target_project_id)?)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyResult {
    /// The target project's tree.
    pub tree: ProjectTree,
    /// The copy's new id.
    pub copy_id: String,
}

/// Copy to project… / Duplicate to personal… (UX §6.1): a local copy with
/// new ids that then syncs with the target project's space.
#[tauri::command]
pub fn copy_to_project(
    app: AppHandle,
    kind: String,
    id: String,
    target_project_id: String,
    target_collection_id: Option<String>,
    target_folder_id: Option<String>,
) -> CmdResult<CopyResult> {
    let (tree, copy_id) = {
        let state = app.state::<AppState>();
        let mut store = state.store();
        store.copy_to_project(
            &kind,
            &id,
            &target_project_id,
            target_collection_id.as_deref().filter(|s| !s.is_empty()),
            target_folder_id.as_deref().filter(|s| !s.is_empty()),
        )?
    };
    crate::sync_rt::schedule_push(&app);
    Ok(CopyResult { tree, copy_id })
}

/// A whole project copied to a space (Duplicate to personal…; `null` =
/// this device only) with new ids.
#[tauri::command]
pub fn project_duplicate_to(
    app: AppHandle,
    project_id: String,
    space_id: Option<String>,
    name: Option<String>,
) -> CmdResult<ProjectTree> {
    let tree = {
        let state = app.state::<AppState>();
        let mut store = state.store();
        store.duplicate_to_space(
            &project_id,
            target(space_id).as_deref(),
            name.as_deref().filter(|n| !n.trim().is_empty()),
        )?
    };
    crate::sync_rt::schedule_push(&app);
    Ok(tree)
}

/// What `role_change_resolve` did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleChangeResult {
    pub status: SyncStatus,
    /// Personal copies made by "savePersonal".
    pub copies: Vec<String>,
}

/// The role-changed banner (04 §6.8): `savePersonal` copies the affected
/// projects to Personal, then (both) the frozen changes are discarded and
/// the space re-pulls the server's versions.
#[tauri::command]
pub async fn role_change_resolve(
    app: AppHandle,
    space_id: String,
    action: String,
) -> CmdResult<RoleChangeResult> {
    let save = match action.as_str() {
        "savePersonal" => true,
        "discard" => false,
        other => {
            return Err(CommandError::new(
                "invalid",
                format!("Unknown action {other}"),
            ));
        }
    };
    let copies = {
        let state = app.state::<AppState>();
        let mut store = state.store();
        if store.binding(&space_id).is_none() {
            return Err(CommandError::new(
                "not_found",
                format!("Space {space_id} not found"),
            ));
        }
        store.resolve_frozen(&space_id, save)?
    };
    let _ = run_sync(&app).await;
    Ok(RoleChangeResult {
        status: crate::sync_rt::refresh_status(&app),
        copies,
    })
}

/// "Edited by Sam · 3h" for a request of a team project (UX §6.5); null in
/// personal and local projects and when the last edit was mine.
#[tauri::command]
pub fn request_edited_by(
    app: AppHandle,
    request_id: String,
) -> Option<reqharbor_core::spaces::EditedBy> {
    app.state::<AppState>().store().edited_by(&request_id)
}
