//! Moving and copying projects between spaces (04 §6.5, §6.7, §6.8; 06 §10
//! M2). Every cross-space transfer uses **new ids** [DEV-5]:
//!
//! * [`move_project`] (Move to team, AC-PROJ-4; an admin's move out of a
//!   team, AC-PROJ-5) is online and resumable: a plan with the id map is
//!   persisted in `sync-spaces.json`, the copy is pushed to the target
//!   (a retry reuses the same ids, so it converges), and only when the
//!   target holds every entity does one local step insert the copy, delete
//!   the source (tombstones in the source space) and drop the plan. Until
//!   then the local project is untouched ("Nothing was moved").
//! * [`Store::copy_to_project`] and [`Store::duplicate_to_space`] make a
//!   local copy with new ids that then syncs normally; the source is never
//!   touched.
//! * [`Store::save_dirty_as_personal`] and [`Store::resolve_frozen`] answer
//!   the role-changed banner (04 §6.8): Save as personal copy, or Discard
//!   (unfreeze and re-pull the server's versions).

use crate::auth::Session;
use crate::model::*;
use crate::space_sync::{self, StoreAccess};
use crate::spaces::{Role, Scope, SpaceKind};
use crate::storage::{Result, Store, StoreError, sync_key};
use crate::sync::{self, Change, PushStatus, SyncError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// A move in progress, persisted under the target space (or the source
/// when the target is this device) in `sync-spaces.json` `pendingMoves`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovePlan {
    pub move_id: String,
    pub project_id: String,
    /// The source space (`None`: this device only).
    pub from: Option<String>,
    /// The target space (`None`: this device only).
    pub to: Option<String>,
    /// Old id → new id for the project and everything in it.
    pub id_map: BTreeMap<String, String>,
    /// `"type:newId"` → the `updatedAt` the target accepted.
    #[serde(default)]
    pub applied: BTreeMap<String, i64>,
    pub created_at: i64,
    /// The target space's pull cursor when the plan was made. Pulls skip the
    /// copy's ids until the move finishes, so finishing rewinds the target
    /// to here: a teammate's newer edit or delete of the partial copy then
    /// applies over it (equal versions are no-ops). `None` (older plans):
    /// from 0.
    #[serde(default)]
    pub target_cursor: Option<u64>,
}

/// An unfinished move (UX §9): resume it or cancel it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMove {
    pub move_id: String,
    pub project_id: String,
    pub project_name: String,
    /// The target space (`None`: this device only).
    pub to: Option<String>,
    pub target_name: String,
    /// Items the target already holds, of `total`.
    pub done: usize,
    pub total: usize,
    /// The copy is already saved here (only the source's removal is left):
    /// it can be finished, not cancelled.
    pub copy_saved: bool,
}

/// What a finished move did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveResult {
    pub move_id: String,
    /// The moved copy's (new) project id.
    pub project_id: String,
    /// Entities copied.
    pub items: usize,
}

/// Why a transfer failed. The local project is unchanged either way.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TransferError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Sync(#[from] SyncError),
    /// The target refused some changes (per-change `rejected`): the reason
    /// the server gave, e.g. `entity_too_large` or `forbidden_role`.
    #[error("{message}")]
    Rejected { reason: String, message: String },
}

/// One plain-text credential the pre-flight lists (UX §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRow {
    pub request_id: String,
    pub request_name: String,
    /// "Authorization header", "Basic auth password", "Bearer token", …
    pub field: String,
    /// Where to fix it: `headers` | `auth` | `ws`.
    pub tab: String,
}

/// The Move / Copy pre-flight (UX §9): what never uploads, what teammates
/// will see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferPreview {
    /// The moved project, or the copied collection, folder or request.
    pub name: String,
    pub target_name: String,
    pub target_kind: String,
    pub member_count: Option<u64>,
    /// Entities that will be copied.
    pub items: usize,
    /// Secret variables (values never leave this device).
    pub secret_variables: usize,
    /// Binary bodies and form files whose paths stay on this device.
    pub file_paths: usize,
    pub credentials: Vec<CredentialRow>,
    /// Copy mode: `{{variables}}` the copy uses that the target project's
    /// environments don't define (environments aren't copied).
    pub unresolved_variables: Vec<String>,
    /// Shares of the source stay in its space and keep working.
    pub shares_stay: bool,
}

fn space_kind(store: &Store, space: Option<&str>) -> Option<SpaceKind> {
    let sp = space?;
    if sp == crate::spaces::LEGACY_SPACE {
        return Some(SpaceKind::Legacy);
    }
    store.binding(sp).map(|b| b.kind)
}

fn is_team(kind: Option<SpaceKind>) -> bool {
    matches!(kind, Some(SpaceKind::Team | SpaceKind::Unknown))
}

fn space_name(store: &Store, space: Option<&str>) -> String {
    match space {
        None => "This device only".into(),
        Some(sp) if sp == crate::spaces::LEGACY_SPACE => "Legacy workspace".into(),
        Some(sp) => store
            .binding(sp)
            .map(|b| b.name())
            .unwrap_or_else(|| sp.to_string()),
    }
}

/// Re-id a project tree and its environments with `id_map` (missing ids are
/// added): the copy a move pushes, or a local duplicate.
fn reid(
    tree: &ProjectTree,
    envs: &[Environment],
    id_map: &mut BTreeMap<String, String>,
) -> (ProjectTree, Vec<Environment>) {
    let mut map =
        |old: &str| -> String { id_map.entry(old.to_string()).or_insert_with(new_id).clone() };
    let pid = map(&tree.project.id);
    let mut out = tree.clone();
    out.project.id = pid.clone();
    for c in &mut out.collections {
        c.id = map(&c.id);
        c.project_id = pid.clone();
    }
    for f in &mut out.folders {
        f.id = map(&f.id);
        f.collection_id = map(&f.collection_id);
        f.parent_folder_id = f.parent_folder_id.as_deref().map(&mut map);
    }
    for r in &mut out.requests {
        r.id = map(&r.id);
        r.collection_id = map(&r.collection_id);
        r.folder_id = r.folder_id.as_deref().map(&mut map);
    }
    let envs: Vec<Environment> = envs
        .iter()
        .map(|e| Environment {
            id: map(&e.id),
            project_id: Some(pid.clone()),
            ..e.clone()
        })
        .collect();
    out.project.active_environment_id = tree
        .project
        .active_environment_id
        .as_deref()
        .and_then(|a| id_map.get(a).cloned());
    (out, envs)
}

fn value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The changes that put a (re-id'd) project into `to`, parents first.
fn copy_changes(tree: &ProjectTree, envs: &[Environment], device: &str, team: bool) -> Vec<Change> {
    let mut out = Vec::new();
    let mut push = |t: EntityType, id: &str, updated_at: i64, data: Value| {
        out.push(Change {
            entity_type: t,
            id: id.to_string(),
            updated_at,
            deleted: false,
            data: Some(data),
            rev: None,
            device_id: Some(device.to_string()),
            updated_by: None,
        });
    };
    push(
        EntityType::Project,
        &tree.project.id,
        tree.project.updated_at,
        value(&tree.project),
    );
    for c in &tree.collections {
        push(EntityType::Collection, &c.id, c.updated_at, value(c));
    }
    for f in &tree.folders {
        push(EntityType::Folder, &f.id, f.updated_at, value(f));
    }
    for r in &tree.requests {
        let mut v = value(r);
        if team {
            sync::strip_device_local_paths(&mut v);
        }
        push(EntityType::Request, &r.id, r.updated_at, v);
    }
    for e in envs {
        push(
            EntityType::Environment,
            &e.id,
            e.updated_at,
            value(&sync::strip_env_secrets(e)),
        );
    }
    out
}

impl Store {
    fn project_envs(&self, pid: &str) -> Vec<Environment> {
        self.environments
            .iter()
            .filter(|e| e.project_id.as_deref() == Some(pid))
            .cloned()
            .collect()
    }

    /// Who may move `pid` to `target` (AC-PROJ-4/5, B6). Returns the
    /// source space.
    pub fn move_check(&self, pid: &str, target: Option<&str>) -> Result<Option<String>> {
        let tree = self.project(pid)?;
        if !self.account_mode() {
            return Err(StoreError::Invalid(
                "Sign in to move projects between spaces.".into(),
            ));
        }
        crate::storage::ensure_tree_editable(tree, "move")?;
        let from = self.bindings.project_spaces.get(pid).cloned();
        if from.as_deref() == target {
            return Err(StoreError::Invalid(format!(
                "\u{201c}{}\u{201d} is already there.",
                tree.project.name
            )));
        }
        if from.as_deref() == Some(crate::spaces::LEGACY_SPACE) {
            return Err(StoreError::Invalid(
                "This project syncs with a token. Merge it into your personal space first (Settings › Sync)."
                    .into(),
            ));
        }
        if let Some(f) = from.as_deref() {
            let b = self
                .binding(f)
                .ok_or_else(|| StoreError::NotFound(format!("Space {f}")))?;
            if is_team(Some(b.kind)) && !matches!(b.role, Some(Role::Admin | Role::Owner)) {
                return Err(StoreError::ReadOnlyRole(format!(
                    "Only admins can move a project out of {}. Use Copy to project… instead.",
                    b.name()
                )));
            }
        }
        if target == Some(crate::spaces::LEGACY_SPACE) {
            return Err(StoreError::Invalid(
                "Projects can't be moved into a token-synced workspace.".into(),
            ));
        }
        self.space_write_guard(target)?;
        if let Some(t) = target
            && !self.scope_syncable(&Scope::Space(t.to_string()))
        {
            return Err(StoreError::Invalid(format!(
                "{} isn't syncing right now. Try again later.",
                space_name(self, Some(t))
            )));
        }
        Ok(from)
    }

    /// Every move waiting to finish (resume or cancel).
    pub fn pending_moves(&self) -> Vec<MovePlan> {
        self.spaces_state
            .spaces
            .values()
            .flat_map(|s| s.pending_moves.iter())
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    }

    fn save_plan(&mut self, plan: &MovePlan) -> Result<()> {
        self.drop_plan(&plan.move_id);
        let home = plan
            .to
            .clone()
            .or_else(|| plan.from.clone())
            .unwrap_or_else(|| "local".into());
        self.space_sync_mut(&Scope::from_id(&home))
            .pending_moves
            .push(value(plan));
        self.save_spaces_state()
    }

    fn drop_plan(&mut self, move_id: &str) {
        for s in self.spaces_state.spaces.values_mut() {
            s.pending_moves
                .retain(|v| v.get("moveId").and_then(Value::as_str) != Some(move_id));
        }
        self.spaces_state
            .spaces
            .retain(|k, v| k != "local" || !v.pending_moves.is_empty());
    }

    /// The plan to move `pid` to `target`: the pending one when a move of
    /// the same project to the same place was interrupted (same ids, so
    /// the retry converges), else a new one. Persisted before any push. A
    /// pending plan whose source is already gone (the copy was saved, the
    /// source's removal interrupted) is returned as is, to be finished.
    pub fn move_plan(&mut self, pid: &str, target: Option<&str>) -> Result<MovePlan> {
        if let Some(p) = self
            .pending_moves()
            .into_iter()
            .find(|p| p.project_id == pid && p.to.as_deref() == target)
        {
            if self.projects.contains_key(pid) {
                self.move_check(pid, target)?;
            }
            return Ok(p);
        }
        let from = self.move_check(pid, target)?;
        let mut plan = MovePlan {
            move_id: new_id(),
            project_id: pid.to_string(),
            from,
            to: target.map(str::to_string),
            id_map: BTreeMap::new(),
            applied: BTreeMap::new(),
            created_at: now_ms(),
            target_cursor: target.and_then(|t| self.scope_cursor(&Scope::from_id(t))),
        };
        self.move_copy(&mut plan)?;
        self.save_plan(&plan)?;
        Ok(plan)
    }

    /// The copy a plan describes, from the project as it is now (ids of
    /// entities created since the plan are added to it).
    pub fn move_copy(&self, plan: &mut MovePlan) -> Result<(ProjectTree, Vec<Environment>)> {
        let tree = self.project(&plan.project_id)?.clone();
        let envs = self.project_envs(&plan.project_id);
        Ok(reid(&tree, &envs, &mut plan.id_map))
    }

    /// The changes still to push for a plan.
    pub fn move_changes(&self, plan: &mut MovePlan) -> Result<Vec<Change>> {
        let (tree, envs) = self.move_copy(plan)?;
        let team = is_team(space_kind(self, plan.to.as_deref()));
        let device = self.device_id().to_string();
        Ok(copy_changes(&tree, &envs, &device, team)
            .into_iter()
            .filter(|c| plan.applied.get(&sync_key(c.entity_type, &c.id)) != Some(&c.updated_at))
            .collect())
    }

    /// Keys the target accepted whose source entity was deleted since (a
    /// failed move, an edit, then the resume): the target must drop them.
    pub fn move_stale_keys(&self, plan: &mut MovePlan) -> Result<Vec<String>> {
        let (tree, envs) = self.move_copy(plan)?;
        let device = self.device_id().to_string();
        let live: BTreeSet<String> = copy_changes(&tree, &envs, &device, false)
            .iter()
            .map(|c| sync_key(c.entity_type, &c.id))
            .collect();
        Ok(plan
            .applied
            .keys()
            .filter(|k| !live.contains(*k))
            .cloned()
            .collect())
    }

    /// Ids of the copies of unfinished moves that aren't on this device
    /// yet: a pull skips them (the copy arrives with the move's own local
    /// step, never as a second project bound to the target).
    pub fn unfinished_move_ids(&self) -> BTreeSet<String> {
        self.pending_moves()
            .into_iter()
            .filter(|p| {
                p.id_map
                    .get(&p.project_id)
                    .is_none_or(|c| !self.projects.contains_key(c))
            })
            .flat_map(|p| p.id_map.into_values())
            .collect()
    }

    /// The copy of a plan is already saved on this device.
    pub fn move_copy_saved(&self, plan: &MovePlan) -> bool {
        plan.id_map
            .get(&plan.project_id)
            .is_some_and(|c| self.projects.contains_key(c))
    }

    /// The source of a plan is gone (its removal already ran).
    pub fn move_source_gone(&self, plan: &MovePlan) -> bool {
        !self.projects.contains_key(&plan.project_id)
    }

    /// Unfinished moves for the UI ("Finish move to Acme" / Cancel).
    pub fn pending_move_list(&self) -> Vec<PendingMove> {
        self.pending_moves()
            .into_iter()
            .map(|p| {
                let name = self
                    .projects
                    .get(&p.project_id)
                    .map(|t| t.project.name.clone())
                    .unwrap_or_default();
                PendingMove {
                    copy_saved: self.move_copy_saved(&p),
                    target_name: space_name(self, p.to.as_deref()),
                    project_name: name,
                    done: p.applied.len(),
                    total: p.id_map.len(),
                    move_id: p.move_id,
                    project_id: p.project_id,
                    to: p.to,
                }
            })
            .collect()
    }

    /// Record a plan's progress (after each pushed batch).
    pub fn record_move_progress(&mut self, plan: &MovePlan) -> Result<()> {
        self.save_plan(plan)
    }

    /// Step one of the local step: save the copy, its environments, its
    /// binding and its synced markers. The source is untouched, so a crash
    /// here leaves both (never neither); `finish_move` then completes.
    pub fn finish_move_copy(&mut self, plan: &MovePlan) -> Result<String> {
        let mut plan = plan.clone();
        let (mut tree, envs) = self.move_copy(&mut plan)?;
        let pid = tree.project.id.clone();
        tree.project.workspace_id = self.workspace.id.clone();
        self.projects.insert(pid.clone(), tree);
        self.removed_projects.remove(&pid);
        let ids: BTreeSet<&str> = envs.iter().map(|e| e.id.as_str()).collect();
        self.environments.retain(|e| !ids.contains(e.id.as_str()));
        self.environments.extend(envs.iter().cloned());
        self.save_project(&pid)?;
        self.save_environments()?;
        match &plan.to {
            Some(to) => {
                self.bindings.project_spaces.insert(pid.clone(), to.clone());
                let scope = Scope::from_id(to);
                let st = self.space_state_mut(&scope);
                for (k, at) in &plan.applied {
                    st.synced.insert(k.clone(), *at);
                }
            }
            None => {
                self.bindings.project_spaces.remove(&pid);
            }
        }
        self.save_bindings()?;
        self.save_sync_state()?;
        Ok(pid)
    }

    /// The local step of a move (04 §6.5 step 3): save the copy bound to
    /// the target and marked synced there, then delete the source
    /// (tombstones in its space) and drop the plan. Idempotent: with the
    /// source already gone and the copy here, only the plan is dropped.
    /// Returns the copy's project id.
    pub fn finish_move(&mut self, plan: &MovePlan) -> Result<String> {
        let old = plan.project_id.clone();
        if self.move_source_gone(plan) {
            let copy = plan.id_map.get(&old).cloned().unwrap_or_default();
            if !self.projects.contains_key(&copy) {
                return Err(StoreError::NotFound(format!("Project {old}")));
            }
            // A crash after the source's removal may have skipped the
            // rewind; redo it (a no-op when it already happened).
            self.rewind_move_target(plan);
            self.drop_plan(&plan.move_id);
            self.save_spaces_state()?;
            return Ok(copy);
        }
        let was_active = self.settings.active_project_id.as_deref() == Some(old.as_str());
        // A copy saved by an interrupted finish is kept as it is now (it may
        // have been edited since); only the source's removal is left.
        let pid = if self.move_copy_saved(plan) {
            plan.id_map[&old].clone()
        } else {
            self.finish_move_copy(plan)?
        };
        // Rewind before removing the source: the removal persists the
        // spaces state, so the rewind is on disk before the source is gone.
        self.rewind_move_target(plan);
        self.remove_project_tombstoned(&old)?;
        if was_active {
            self.settings.active_project_id = Some(pid.clone());
            self.save_settings()?;
        }
        self.drop_plan(&plan.move_id);
        self.save_bindings()?;
        self.save_sync_state()?;
        Ok(pid)
    }

    /// Rewind the target space's cursor to where it stood when the plan was
    /// made, so the next pull applies teammates' edits and deletes to the
    /// partial copy that the move's pulls skipped. Equal versions re-pulled
    /// are no-ops.
    fn rewind_move_target(&mut self, plan: &MovePlan) {
        if let Some(to) = &plan.to {
            let scope = Scope::from_id(to);
            let since = plan.target_cursor.unwrap_or(0);
            if self.scope_cursor(&scope).is_some_and(|c| c > since) {
                self.set_scope_cursor(&scope, since);
            }
        }
    }

    /// Forget a plan whose pushes were undone (or never happened).
    pub fn forget_move(&mut self, move_id: &str) -> Result<()> {
        self.drop_plan(move_id);
        self.save_spaces_state()
    }

    // ───────────────────────────── copies ─────────────────────────────

    /// Copy a collection, folder or request into another project with new
    /// ids (Copy to project…, Duplicate to personal…, UX §6.1). A request
    /// or folder lands in `target_collection` (and `target_folder`); without
    /// one, a collection named after the source's is created. The source is
    /// never touched, so viewers may copy out of a team. Returns the target
    /// project's tree and the copy's id.
    pub fn copy_to_project(
        &mut self,
        kind: &str,
        id: &str,
        target_project: &str,
        target_collection: Option<&str>,
        target_folder: Option<&str>,
    ) -> Result<(ProjectTree, String)> {
        self.project(target_project)?;
        self.write_guard(target_project)?;
        let src_pid = self.project_of(
            match kind {
                "collection" => crate::storage::NodeKind::Collection,
                "folder" => crate::storage::NodeKind::Folder,
                "request" => crate::storage::NodeKind::Request,
                _ => return Err(StoreError::Invalid(format!("Unknown kind {kind}"))),
            },
            id,
        )?;
        let src = self.project(&src_pid)?.clone();
        let now = now_ms();
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        let mut fresh = |old: &str| map.entry(old.to_string()).or_insert_with(new_id).clone();
        let target_tree = self.project(target_project)?.clone();
        let next_sort = |v: Vec<i64>| v.into_iter().max().unwrap_or(-1) + 1;
        let (collections, folders, requests, copy_id) = match kind {
            "collection" => {
                let c = src
                    .collections
                    .iter()
                    .find(|c| c.id == id)
                    .ok_or_else(|| StoreError::NotFound(format!("Collection {id}")))?;
                crate::storage::ensure_tree_editable(
                    &ProjectTree {
                        collections: vec![c.clone()],
                        folders: src
                            .folders
                            .iter()
                            .filter(|f| f.collection_id == id)
                            .cloned()
                            .collect(),
                        requests: src
                            .requests
                            .iter()
                            .filter(|r| r.collection_id == id)
                            .cloned()
                            .collect(),
                        ..ProjectTree::new(src.project.clone())
                    },
                    "copy",
                )?;
                let new_c = Collection {
                    id: fresh(&c.id),
                    project_id: target_project.to_string(),
                    sort_order: next_sort(
                        target_tree
                            .collections
                            .iter()
                            .map(|x| x.sort_order)
                            .collect(),
                    ),
                    created_at: now,
                    updated_at: now,
                    ..c.clone()
                };
                let fs: Vec<Folder> = src
                    .folders
                    .iter()
                    .filter(|f| f.collection_id == id)
                    .map(|f| Folder {
                        id: fresh(&f.id),
                        collection_id: new_c.id.clone(),
                        parent_folder_id: f.parent_folder_id.as_deref().map(&mut fresh),
                        created_at: now,
                        updated_at: now,
                        ..f.clone()
                    })
                    .collect();
                let rs: Vec<Request> = src
                    .requests
                    .iter()
                    .filter(|r| r.collection_id == id)
                    .map(|r| Request {
                        id: fresh(&r.id),
                        collection_id: new_c.id.clone(),
                        folder_id: r.folder_id.as_deref().map(&mut fresh),
                        created_at: now,
                        updated_at: now,
                        ..r.clone()
                    })
                    .collect();
                let cid = new_c.id.clone();
                (vec![new_c], fs, rs, cid)
            }
            _ => {
                // A folder or a request: find (or make) the target collection.
                let source_collection = match kind {
                    "folder" => src
                        .folders
                        .iter()
                        .find(|f| f.id == id)
                        .map(|f| f.collection_id.clone()),
                    _ => src
                        .requests
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.collection_id.clone()),
                }
                .ok_or_else(|| StoreError::NotFound(format!("{kind} {id}")))?;
                let mut new_collections = Vec::new();
                let target_cid = match target_collection {
                    Some(c) => {
                        if !target_tree.collections.iter().any(|x| x.id == c) {
                            return Err(StoreError::NotFound(format!("Collection {c}")));
                        }
                        c.to_string()
                    }
                    None => {
                        let sc = src
                            .collections
                            .iter()
                            .find(|c| c.id == source_collection)
                            .cloned()
                            .ok_or_else(|| {
                                StoreError::NotFound(format!("Collection {source_collection}"))
                            })?;
                        let c = Collection {
                            id: new_id(),
                            project_id: target_project.to_string(),
                            sort_order: next_sort(
                                target_tree
                                    .collections
                                    .iter()
                                    .map(|x| x.sort_order)
                                    .collect(),
                            ),
                            created_at: now,
                            updated_at: now,
                            ..sc
                        };
                        let cid = c.id.clone();
                        new_collections.push(c);
                        cid
                    }
                };
                if let Some(f) = target_folder
                    && !target_tree
                        .folders
                        .iter()
                        .any(|x| x.id == f && x.collection_id == target_cid)
                {
                    return Err(StoreError::NotFound(format!("Folder {f}")));
                }
                let target_folder = target_folder.map(str::to_string);
                if kind == "request" {
                    let r = src.requests.iter().find(|r| r.id == id).cloned().unwrap();
                    crate::storage::ensure_editable(&r)?;
                    let siblings = target_tree
                        .requests
                        .iter()
                        .filter(|x| x.collection_id == target_cid && x.folder_id == target_folder)
                        .map(|x| x.sort_order)
                        .collect();
                    let nr = Request {
                        id: fresh(&r.id),
                        collection_id: target_cid.clone(),
                        folder_id: target_folder.clone(),
                        sort_order: next_sort(siblings),
                        created_at: now,
                        updated_at: now,
                        ..r
                    };
                    let rid = nr.id.clone();
                    (new_collections, Vec::new(), vec![nr], rid)
                } else {
                    let ids = crate::storage::descendant_folders(&src, id);
                    let sub = ProjectTree {
                        folders: src
                            .folders
                            .iter()
                            .filter(|f| ids.contains(&f.id))
                            .cloned()
                            .collect(),
                        requests: src
                            .requests
                            .iter()
                            .filter(|r| r.folder_id.as_ref().is_some_and(|f| ids.contains(f)))
                            .cloned()
                            .collect(),
                        ..ProjectTree::new(src.project.clone())
                    };
                    crate::storage::ensure_tree_editable(&sub, "copy")?;
                    let siblings = target_tree
                        .folders
                        .iter()
                        .filter(|x| {
                            x.collection_id == target_cid && x.parent_folder_id == target_folder
                        })
                        .map(|x| x.sort_order)
                        .collect();
                    let top_sort = next_sort(siblings);
                    let fs: Vec<Folder> = sub
                        .folders
                        .iter()
                        .map(|f| {
                            let top = f.id == id;
                            Folder {
                                id: fresh(&f.id),
                                collection_id: target_cid.clone(),
                                parent_folder_id: if top {
                                    target_folder.clone()
                                } else {
                                    f.parent_folder_id.as_deref().map(&mut fresh)
                                },
                                sort_order: if top { top_sort } else { f.sort_order },
                                created_at: now,
                                updated_at: now,
                                ..f.clone()
                            }
                        })
                        .collect();
                    let rs: Vec<Request> = sub
                        .requests
                        .iter()
                        .map(|r| Request {
                            id: fresh(&r.id),
                            collection_id: target_cid.clone(),
                            folder_id: r.folder_id.as_deref().map(&mut fresh),
                            created_at: now,
                            updated_at: now,
                            ..r.clone()
                        })
                        .collect();
                    let fid = fresh(id);
                    (new_collections, fs, rs, fid)
                }
            }
        };
        let t = self
            .projects
            .get_mut(target_project)
            .ok_or_else(|| StoreError::NotFound(format!("Project {target_project}")))?;
        t.collections.extend(collections);
        t.folders.extend(folders);
        t.requests.extend(requests);
        self.save_project(target_project)?;
        Ok((self.project(target_project)?.clone(), copy_id))
    }

    /// A whole project copied into `space` (`None`: this device only) with
    /// new ids, environments included (Duplicate to personal…, the
    /// role-changed "Save as personal copy", kept removed spaces).
    pub fn duplicate_to_space(
        &mut self,
        pid: &str,
        space: Option<&str>,
        name: Option<&str>,
    ) -> Result<ProjectTree> {
        self.space_write_guard(space)?;
        let src = self.project(pid)?.clone();
        let envs = self.project_envs(pid);
        let mut map = BTreeMap::new();
        let (mut tree, envs) = reid(&src, &envs, &mut map);
        let now = now_ms();
        tree.project.name = name
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} copy", src.project.name));
        tree.project.sort_order = self
            .projects
            .values()
            .map(|t| t.project.sort_order)
            .max()
            .unwrap_or(0)
            + 1;
        tree.project.created_at = now;
        tree.project.updated_at = now;
        let bump = |at: &mut i64| *at = now.max(*at);
        for c in &mut tree.collections {
            bump(&mut c.updated_at);
        }
        for f in &mut tree.folders {
            bump(&mut f.updated_at);
        }
        for r in &mut tree.requests {
            bump(&mut r.updated_at);
        }
        let new_pid = tree.project.id.clone();
        self.projects.insert(new_pid.clone(), tree.clone());
        self.environments.extend(envs.into_iter().map(|mut e| {
            e.updated_at = now.max(e.updated_at);
            e
        }));
        self.save_project(&new_pid)?;
        self.save_environments()?;
        if self.account_mode() {
            self.bind_new_project(&new_pid, space.map(str::to_string));
            self.save_bindings()?;
        }
        Ok(tree)
    }

    /// Projects of `space` holding frozen or held keys, or (with
    /// `all_dirty`) any unsynced change.
    fn projects_with_changes(&self, space: &str, all_dirty: bool) -> Vec<String> {
        let scope = Scope::from_id(space);
        let ss = self.space_sync(&scope);
        let mut keys: BTreeSet<String> = ss.frozen.iter().chain(ss.held.iter()).cloned().collect();
        if all_dirty {
            keys.extend(sync::dirty_keys_in(self, &scope));
        }
        let mut out = Vec::new();
        for pid in self.projects_in(space).into_iter().map(|p| p.id) {
            let touched = self
                .project_entities(&pid)
                .iter()
                .any(|(t, id)| keys.contains(&sync_key(*t, id)));
            if touched {
                out.push(pid);
            }
        }
        out
    }

    /// Copy every project of `space` that has unsynced (or frozen) changes
    /// to Personal (this device only when Personal isn't bound), named
    /// "X (my changes)" (AC-ROLE-3, edge case 3). Returns the copies' ids.
    pub fn save_dirty_as_personal(&mut self, space: &str) -> Result<Vec<String>> {
        let personal = self
            .personal_space()
            .filter(|b| b.is_active())
            .map(|b| b.space_id.clone());
        let mut out = Vec::new();
        for pid in self.projects_with_changes(space, true) {
            let name = format!("{} (my changes)", self.project(&pid)?.project.name);
            let id = match self.held_roots(space, &pid) {
                // Only containers deleted on the server hold the changes:
                // copy just those, not the rest of the team's project.
                Some(roots) => {
                    let t = self.create_project_at(&name, personal.clone())?;
                    for (kind, id) in roots {
                        self.copy_to_project(kind, &id, &t.project.id, None, None)?;
                    }
                    t.project.id
                }
                None => {
                    self.duplicate_to_space(&pid, personal.as_deref(), Some(&name))?
                        .project
                        .id
                }
            };
            out.push(id);
        }
        Ok(out)
    }

    /// The held collections and folders of `pid` (outermost first) when
    /// every unsynced change of the project lies under one of them; `None`
    /// when the project itself is held or changes lie elsewhere.
    fn held_roots(&self, space: &str, pid: &str) -> Option<Vec<(&'static str, String)>> {
        let scope = Scope::from_id(space);
        let ss = self.space_sync(&scope);
        let mine: BTreeSet<String> = self
            .project_entities(pid)
            .iter()
            .map(|(t, id)| sync_key(*t, id))
            .collect();
        let held: BTreeSet<&String> = ss.held.iter().filter(|k| mine.contains(*k)).collect();
        if held.is_empty() || held.contains(&sync_key(EntityType::Project, pid)) {
            return None;
        }
        let under = |k: &str| -> Vec<String> {
            let (t, id) = k.split_once(':').unwrap_or(("", ""));
            EntityType::parse(t)
                .map(|t| sync::ancestors(self, t, id))
                .unwrap_or_default()
        };
        let roots: Vec<&String> = held
            .iter()
            .copied()
            .filter(|k| !under(k).iter().any(|a| held.contains(a)))
            .collect();
        let changes: BTreeSet<String> = ss
            .frozen
            .iter()
            .cloned()
            .chain(sync::dirty_keys_in(self, &scope))
            .filter(|k| mine.contains(k))
            .collect();
        let covered = changes
            .iter()
            .all(|k| roots.contains(&k) || under(k).iter().any(|a| roots.contains(&a)));
        if !covered {
            return None;
        }
        Some(
            roots
                .into_iter()
                .filter_map(|k| {
                    let (t, id) = k.split_once(':')?;
                    let kind = match t {
                        "collection" => "collection",
                        "folder" => "folder",
                        _ => return None,
                    };
                    Some((kind, id.to_string()))
                })
                .collect(),
        )
    }

    /// Answer the role-changed banner (04 §6.8): `save` first copies the
    /// affected projects to Personal; then the frozen keys are discarded
    /// (their tombstones dropped, edits made clean, never-synced items
    /// removed), held deletes apply, and the space re-pulls from 0 so the
    /// server's versions come back. Returns the copies' ids.
    pub fn resolve_frozen(&mut self, space: &str, save: bool) -> Result<Vec<String>> {
        let copies = if save {
            self.save_dirty_as_personal(space)?
        } else {
            Vec::new()
        };
        let scope = Scope::from_id(space);
        let (frozen, held) = {
            let ss = self.space_sync_mut(&scope);
            (std::mem::take(&mut ss.frozen), std::mem::take(&mut ss.held))
        };
        for key in &frozen {
            let Some((t, id)) = key.split_once(':') else {
                continue;
            };
            let Some(t) = EntityType::parse(t) else {
                continue;
            };
            let synced = self.space_state(&scope).synced.get(key).copied();
            let st = self.space_state_mut(&scope);
            st.tombstones
                .retain(|x| !(x.id == id && x.entity_type == t));
            if t == EntityType::Project {
                // Its children's tombstones waited with it (never pushed):
                // they go too, so the re-pull restores the whole project.
                st.tombstones.retain(|x| {
                    x.extra.get(sync::TOMBSTONE_PROJECT).and_then(Value::as_str) != Some(id)
                });
            }
            match (synced, local_updated(self, t, id)) {
                // On the server: made clean, so the re-pull overwrites it.
                (Some(_), Some(at)) => {
                    self.space_state_mut(&scope).synced.insert(key.clone(), at);
                }
                // Never synced: it only exists here.
                (None, Some(_)) => {
                    sync::remove_local(self, t, id);
                }
                _ => {}
            }
        }
        for key in &held {
            if let Some((t, id)) = key.split_once(':')
                && let Some(t) = EntityType::parse(t)
            {
                for (rt, rid) in sync::remove_local(self, t, id) {
                    let k = sync_key(rt, &rid);
                    let st = self.space_state_mut(&scope);
                    st.synced.remove(&k);
                    st.origins.remove(&k);
                    st.tombstones.retain(|x| x.id != rid);
                }
            }
        }
        let ss = self.space_sync_mut(&scope);
        if ss
            .issue
            .as_ref()
            .is_some_and(|i| i.code == "roleChanged" || i.code == "heldDelete")
        {
            ss.issue = None;
        }
        self.set_scope_cursor(&scope, 0);
        self.save_all()?;
        self.save_bindings()?;
        Ok(copies)
    }
}

fn local_updated(store: &Store, t: EntityType, id: &str) -> Option<i64> {
    match t {
        EntityType::Project => store.projects.get(id).map(|p| p.project.updated_at),
        EntityType::Collection => store.collection(id).map(|c| c.updated_at),
        EntityType::Folder => store
            .projects
            .values()
            .flat_map(|t| t.folders.iter())
            .find(|f| f.id == id)
            .map(|f| f.updated_at),
        EntityType::Request => store.request(id).map(|r| r.updated_at),
        EntityType::Environment => store.environment(id).map(|e| e.updated_at),
    }
}

// ───────────────────────────── the pre-flight ─────────────────────────────

/// A value typed as plain text: something left once `{{variables}}` and a
/// leading auth scheme are taken out.
fn plain_text(v: &str) -> bool {
    let mut rest = v.to_string();
    while let Some(start) = rest.find("{{") {
        match rest[start..].find("}}") {
            Some(end) => rest.replace_range(start..start + end + 2, ""),
            None => break,
        }
    }
    let t = rest.trim();
    let t = ["Bearer", "Basic", "Token", "bearer", "basic", "token"]
        .iter()
        .find_map(|s| t.strip_prefix(s))
        .unwrap_or(t);
    !t.trim().is_empty()
}

fn auth_rows(a: &Auth, row: &dyn Fn(&str) -> CredentialRow, prefix: &str) -> Vec<CredentialRow> {
    let set = |v: &Option<String>| v.as_deref().is_some_and(plain_text);
    let label = |f: &str| format!("{prefix}{f}");
    let mut out = Vec::new();
    match &a.auth_type {
        AuthType::Basic if set(&a.password) => out.push(row(&label("Basic auth password"))),
        AuthType::Bearer if set(&a.token) => out.push(row(&label("Bearer token"))),
        AuthType::Apikey if a.api_key.as_ref().is_some_and(|k| plain_text(&k.value)) => {
            out.push(row(&label("API key")))
        }
        // A scheme from a newer ReqHarbor: this build can't tell which of its
        // fields are secret, so any value counts (as the share scanner does).
        AuthType::Other(_) if crate::share::auth_credential_fields(a) > 0 => {
            out.push(row(&label("Auth from a newer ReqHarbor")))
        }
        _ => {}
    }
    out
}

/// Credentials typed as plain text in a request (UX §9). Disabled headers
/// count too (they upload), and so do blocks of fields from a newer
/// ReqHarbor, which may hold secrets (the share scanner's rule, 04 §8).
pub fn plain_credentials(r: &Request) -> Vec<CredentialRow> {
    let row = |field: &str, tab: &str| CredentialRow {
        request_id: r.id.clone(),
        request_name: r.name.clone(),
        field: field.to_string(),
        tab: tab.to_string(),
    };
    let mut out = Vec::new();
    for h in &r.headers {
        let k = h.key.trim().to_ascii_lowercase();
        if matches!(
            k.as_str(),
            "authorization" | "proxy-authorization" | "cookie"
        ) && plain_text(&h.value)
        {
            let label = match k.as_str() {
                "cookie" => "Cookie header",
                "proxy-authorization" => "Proxy-Authorization header",
                _ => "Authorization header",
            };
            out.push(row(label, "headers"));
        }
    }
    out.extend(auth_rows(&r.auth, &|f| row(f, "auth"), ""));
    if let Some(sio) = r.ws.as_ref().and_then(|w| w.socket_io.as_ref())
        && sio.auth.as_deref().is_some_and(plain_text)
    {
        out.push(row("Socket.IO auth", "ws"));
    }
    let unknown = crate::share::unknown_field_blocks(r);
    if unknown > 0 {
        out.push(row(
            &format!(
                "{unknown} block{} of fields from a newer ReqHarbor",
                if unknown == 1 { "" } else { "s" }
            ),
            "request",
        ));
    }
    out
}

/// Credentials typed as plain text in a collection's auth.
pub fn collection_credentials(c: &Collection) -> Vec<CredentialRow> {
    let row = |field: &str| CredentialRow {
        request_id: c.id.clone(),
        request_name: c.name.clone(),
        field: field.to_string(),
        tab: "collection".to_string(),
    };
    auth_rows(&c.auth, &row, "Collection auth: ")
}

fn file_paths(r: &Request) -> usize {
    let binary = r
        .body
        .binary_path
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty()) as usize;
    let form = r
        .body
        .form
        .iter()
        .flatten()
        .filter(|f| f.kind == FormFieldKind::File && !f.value.trim().is_empty())
        .count();
    let grpc = r.grpc.as_ref().map_or(0, |g| g.local_paths());
    binary + form + grpc
}

fn request_text(r: &Request) -> Vec<String> {
    let mut v = vec![r.url.clone()];
    v.extend(
        r.params
            .iter()
            .flat_map(|k| [k.key.clone(), k.value.clone()]),
    );
    v.extend(
        r.headers
            .iter()
            .flat_map(|k| [k.key.clone(), k.value.clone()]),
    );
    v.extend(r.body.text.clone());
    if let Some(g) = &r.body.graphql {
        v.push(g.query.clone());
        v.push(g.variables.clone());
    }
    v.extend(
        r.body
            .form_encoded
            .iter()
            .flatten()
            .flat_map(|k| [k.key.clone(), k.value.clone()]),
    );
    v.extend(
        r.body
            .form
            .iter()
            .flatten()
            .flat_map(|k| [k.key.clone(), k.value.clone()]),
    );
    v.extend(
        [&r.auth.username, &r.auth.password, &r.auth.token]
            .into_iter()
            .flatten()
            .cloned(),
    );
    if let Some(k) = &r.auth.api_key {
        v.push(k.key.clone());
        v.push(k.value.clone());
    }
    v
}

impl Store {
    fn target_space_of_project(&self, pid: &str) -> Option<String> {
        self.bindings.project_spaces.get(pid).cloned()
    }

    /// The pre-flight of moving `pid` to `target` (UX §9).
    pub fn move_preview(&self, pid: &str, target: Option<&str>) -> Result<TransferPreview> {
        self.move_check(pid, target)?;
        let tree = self.project(pid)?;
        let envs = self.project_envs(pid);
        Ok(TransferPreview {
            name: tree.project.name.clone(),
            target_name: space_name(self, target),
            target_kind: target_kind(self, target),
            member_count: target
                .and_then(|t| self.binding(t))
                .and_then(|b| b.member_count),
            items: 1
                + tree.collections.len()
                + tree.folders.len()
                + tree.requests.len()
                + envs.len(),
            secret_variables: envs
                .iter()
                .flat_map(|e| e.variables.iter())
                .filter(|v| v.secret)
                .count(),
            file_paths: tree.requests.iter().map(file_paths).sum(),
            credentials: tree
                .collections
                .iter()
                .flat_map(collection_credentials)
                .chain(tree.requests.iter().flat_map(plain_credentials))
                .collect(),
            unresolved_variables: Vec::new(),
            shares_stay: target.is_some(),
        })
    }

    /// The pre-flight of copying a collection, folder or request into
    /// `target_project` (UX §9 copy mode): adds the variables the target's
    /// environments don't define.
    pub fn copy_preview(
        &self,
        kind: &str,
        id: &str,
        target_project: &str,
    ) -> Result<TransferPreview> {
        let node = match kind {
            "collection" => crate::storage::NodeKind::Collection,
            "folder" => crate::storage::NodeKind::Folder,
            "request" => crate::storage::NodeKind::Request,
            _ => return Err(StoreError::Invalid(format!("Unknown kind {kind}"))),
        };
        let src = self.project(&self.project_of(node, id)?)?;
        let target = self.project(target_project)?;
        let source_collection: Option<String> = match kind {
            "collection" => Some(id.to_string()),
            "folder" => src
                .folders
                .iter()
                .find(|f| f.id == id)
                .map(|f| f.collection_id.clone()),
            _ => src
                .requests
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.collection_id.clone()),
        };
        let (name, requests, folders, collections): (String, Vec<&Request>, usize, usize) =
            match kind {
                "collection" => {
                    let c = src.collections.iter().find(|c| c.id == id);
                    (
                        c.map(|c| c.name.clone()).unwrap_or_default(),
                        src.requests
                            .iter()
                            .filter(|r| r.collection_id == id)
                            .collect(),
                        src.folders.iter().filter(|f| f.collection_id == id).count(),
                        1,
                    )
                }
                "folder" => {
                    let ids = crate::storage::descendant_folders(src, id);
                    (
                        src.folders
                            .iter()
                            .find(|f| f.id == id)
                            .map(|f| f.name.clone())
                            .unwrap_or_default(),
                        src.requests
                            .iter()
                            .filter(|r| r.folder_id.as_ref().is_some_and(|f| ids.contains(f)))
                            .collect(),
                        ids.len(),
                        0,
                    )
                }
                _ => {
                    let r: Vec<&Request> = src.requests.iter().filter(|r| r.id == id).collect();
                    (
                        r.first().map(|r| r.name.clone()).unwrap_or_default(),
                        r,
                        0,
                        0,
                    )
                }
            };
        let defined: BTreeSet<String> = self
            .project_envs(target_project)
            .iter()
            .flat_map(|e| e.variables.iter().map(|v| v.key.trim().to_string()))
            .collect();
        let mut unresolved = BTreeSet::new();
        for r in &requests {
            for text in request_text(r) {
                for name in crate::vars::references(&text) {
                    if !name.starts_with('$') && !defined.contains(&name) {
                        unresolved.insert(name);
                    }
                }
            }
        }
        let space = self.target_space_of_project(target_project);
        Ok(TransferPreview {
            name,
            target_name: target.project.name.clone(),
            target_kind: target_kind(self, space.as_deref()),
            member_count: space
                .as_deref()
                .and_then(|t| self.binding(t))
                .and_then(|b| b.member_count),
            items: collections + folders + requests.len(),
            secret_variables: 0,
            file_paths: requests.iter().map(|r| file_paths(r)).sum(),
            // The source collection's auth goes along with a collection, and
            // with a folder or request copied into a new collection.
            credentials: src
                .collections
                .iter()
                .filter(|c| Some(c.id.as_str()) == source_collection.as_deref())
                .flat_map(collection_credentials)
                .chain(requests.iter().flat_map(|r| plain_credentials(r)))
                .collect(),
            unresolved_variables: unresolved.into_iter().collect(),
            shares_stay: false,
        })
    }
}

fn target_kind(store: &Store, space: Option<&str>) -> String {
    match space_kind(store, space) {
        None => "local",
        Some(SpaceKind::Personal) => "personal",
        Some(SpaceKind::Legacy) => "legacy",
        Some(_) => "team",
    }
    .into()
}

// ───────────────────────────── the online move ─────────────────────────────

/// Push what a plan still needs; record each accepted change in the plan.
async fn push_plan(
    store: &impl StoreAccess,
    client: &sync::SyncClient,
    plan: &mut MovePlan,
    progress: &(dyn Fn(usize, usize) + Send + Sync),
) -> std::result::Result<(), TransferError> {
    let changes = store.with(|s| s.move_changes(plan))?;
    let total = plan.applied.len() + changes.len();
    progress(plan.applied.len(), total);
    for batch in sync::batch_changes(changes, sync::BATCH, sync::MAX_PUSH_BYTES) {
        let mut refused: Option<(String, String)> = None;
        for p in sync::push_batch_timed(client, batch).await? {
            for r in &p.results {
                let Some(c) = p
                    .sent
                    .iter()
                    .find(|c| c.id == r.id && c.entity_type == r.entity_type)
                else {
                    continue;
                };
                match r.status {
                    // Stale: our own earlier attempt (same ids) already landed.
                    PushStatus::Applied | PushStatus::Stale => {
                        plan.applied
                            .insert(sync_key(c.entity_type, &c.id), c.updated_at);
                    }
                    PushStatus::Rejected | PushStatus::Unknown => {
                        refused.get_or_insert_with(|| {
                            let reason = r.reason.clone().unwrap_or_else(|| "rejected".into());
                            let what = match reason.as_str() {
                                "entity_too_large" => "an item is over 512 KB",
                                "forbidden_role" => "your role doesn't allow it",
                                "clock_skew" => "your computer's clock is ahead",
                                _ => "the server refused an item",
                            };
                            (reason, format!("Nothing was moved: {what}."))
                        });
                    }
                }
            }
        }
        store.with(|s| s.record_move_progress(plan))?;
        progress(plan.applied.len(), total);
        if let Some((reason, message)) = refused {
            return Err(TransferError::Rejected { reason, message });
        }
    }
    Ok(())
}

/// Move a project to another space (04 §6.5): online and resumable. The
/// local project changes only once the target holds the whole copy.
/// `progress(done, total)` reports pushed items.
pub async fn move_project(
    store: &impl StoreAccess,
    session: Option<Arc<Session>>,
    project_id: &str,
    target: Option<&str>,
    progress: &(dyn Fn(usize, usize) + Send + Sync),
) -> std::result::Result<MoveResult, TransferError> {
    // A plan of this project to another place is undone first (UX §9).
    let older: Vec<String> = store.with(|s| {
        s.pending_moves()
            .into_iter()
            .filter(|p| p.project_id == project_id && p.to.as_deref() != target)
            .map(|p| p.move_id)
            .collect()
    });
    for id in older {
        let saved = store.with(|s| {
            s.pending_moves()
                .into_iter()
                .find(|p| p.move_id == id)
                .filter(|p| s.move_copy_saved(p))
                .map(|p| space_name(s, p.to.as_deref()))
        });
        if let Some(name) = saved {
            return Err(StoreError::Invalid(format!(
                "Finish the move to {name} first: its copy is already saved here."
            ))
            .into());
        }
        cancel_move(store, session.clone(), &id).await?;
    }
    let mut plan = store.with(|s| s.move_plan(project_id, target))?;
    // With the source gone or the copy already saved, only the local step is
    // left: never push the source over a copy that may have changed since.
    let gone = store.with(|s| s.move_source_gone(&plan) || s.move_copy_saved(&plan));
    if let (Some(to), false) = (plan.to.clone(), gone) {
        let session = session.ok_or(SyncError::SignInRequired)?;
        let client = store.with(|s| space_sync::space_client(s, &to, &session))?;
        push_plan(store, &client, &mut plan, progress).await?;
        // Entities deleted here since an earlier attempt: drop their copies.
        let stale = store.with(|s| s.move_stale_keys(&mut plan))?;
        if !stale.is_empty() {
            let device = store.with(|s| s.device_id().to_string());
            push_deletes(&client, &stale, &device).await?;
            for k in &stale {
                plan.applied.remove(k);
            }
            store.with(|s| s.record_move_progress(&plan))?;
        }
    }
    let items = plan.id_map.len();
    let pid = store.with(|s| s.finish_move(&plan))?;
    Ok(MoveResult {
        move_id: plan.move_id,
        project_id: pid,
        items,
    })
}

/// Cancel a move (04 §6.5 step 4): tombstones for anything the target
/// already accepted, then the plan goes. The local project never changed.
pub async fn cancel_move(
    store: &impl StoreAccess,
    session: Option<Arc<Session>>,
    move_id: &str,
) -> std::result::Result<(), TransferError> {
    let Some(plan) = store.with(|s| s.pending_moves().into_iter().find(|p| p.move_id == move_id))
    else {
        return Ok(());
    };
    let copy_here = plan
        .id_map
        .get(&plan.project_id)
        .is_some_and(|c| store.with(|s| s.projects.contains_key(c)));
    if copy_here {
        // Undoing would delete a copy that is already this device's project.
        let name = store.with(|s| space_name(s, plan.to.as_deref()));
        return Err(StoreError::Invalid(format!(
            "The move to {name} is almost done: its copy is already saved here. Finish it instead."
        ))
        .into());
    }
    if let (Some(to), false) = (plan.to.clone(), plan.applied.is_empty()) {
        let session = session.ok_or(SyncError::SignInRequired)?;
        let client = store.with(|s| space_sync::space_client(s, &to, &session))?;
        let device = store.with(|s| s.device_id().to_string());
        let keys: Vec<String> = plan.applied.keys().cloned().collect();
        push_deletes(&client, &keys, &device).await?;
    }
    store.with(|s| s.forget_move(move_id))?;
    Ok(())
}

/// Push tombstones for `"type:id"` keys, children first so a partial run
/// never strands orphans.
async fn push_deletes(
    client: &sync::SyncClient,
    keys: &[String],
    device: &str,
) -> std::result::Result<(), TransferError> {
    let now = now_ms();
    let mut deletes: Vec<Change> = keys
        .iter()
        .filter_map(|k| {
            let (t, id) = k.split_once(':')?;
            Some(Change {
                entity_type: EntityType::parse(t)?,
                id: id.to_string(),
                updated_at: now,
                deleted: true,
                data: None,
                rev: None,
                device_id: Some(device.to_string()),
                updated_by: None,
            })
        })
        .collect();
    deletes.sort_by_key(|c| std::cmp::Reverse(c.entity_type.depth()));
    for batch in sync::batch_changes(deletes, sync::BATCH, sync::MAX_PUSH_BYTES) {
        sync::push_batch_timed(client, batch).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_ignores_variables_and_schemes() {
        assert!(!plain_text("{{token}}"));
        assert!(!plain_text("Bearer {{token}}"));
        assert!(!plain_text("  "));
        assert!(plain_text("Bearer abc123"));
        assert!(plain_text("sid=42; {{x}}"));
    }

    #[test]
    fn credentials_and_paths_are_listed() {
        let mut r = Request {
            id: "r1".into(),
            name: "Create order".into(),
            ..Request::default()
        };
        let kv = |k: &str, v: &str| -> KeyValue {
            serde_json::from_value(serde_json::json!({"key": k, "value": v, "enabled": true}))
                .unwrap()
        };
        r.headers.push(kv("Authorization", "Bearer s3cret"));
        r.headers.push(kv("Cookie", "{{cookie}}"));
        r.auth.auth_type = AuthType::Basic;
        r.auth.password = Some("hunter2".into());
        r.body.binary_path = Some("/tmp/x.bin".into());
        let rows = plain_credentials(&r);
        let fields: Vec<&str> = rows.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, ["Authorization header", "Basic auth password"]);
        assert_eq!(rows[1].tab, "auth");
        assert_eq!(file_paths(&r), 1);
    }

    #[test]
    fn the_scan_covers_disabled_headers_collection_auth_and_newer_fields() {
        let r: Request = serde_json::from_value(serde_json::json!({
            "id": "r1", "collectionId": "c1", "name": "Login", "method": "POST", "url": "https://x",
            "headers": [{"key": "Cookie", "value": "sid=1", "enabled": false}],
            "auth": {"type": "oauth2", "clientSecret": "abc"},
            "body": {"type": "none"},
            "grpcMetadata": {"x-api-key": "k"}
        }))
        .unwrap();
        let fields: Vec<String> = plain_credentials(&r).into_iter().map(|c| c.field).collect();
        assert_eq!(
            fields,
            [
                "Cookie header",
                "Auth from a newer ReqHarbor",
                "1 block of fields from a newer ReqHarbor"
            ]
        );
        let c: Collection = serde_json::from_value(serde_json::json!({
            "id": "c1", "projectId": "p", "name": "Billing",
            "auth": {"type": "basic", "username": "billing", "password": "hunter2"}
        }))
        .unwrap();
        let rows = collection_credentials(&c);
        assert_eq!(rows[0].field, "Collection auth: Basic auth password");
        assert_eq!(
            (rows[0].tab.as_str(), rows[0].request_name.as_str()),
            ("collection", "Billing")
        );
        // A variable reference isn't plain text.
        let c2 = Collection {
            auth: serde_json::from_value(
                serde_json::json!({"type": "bearer", "token": "{{token}}"}),
            )
            .unwrap(),
            ..c
        };
        assert!(collection_credentials(&c2).is_empty());
    }

    #[test]
    fn reid_maps_every_reference() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let pid = store.create_project("P").unwrap().project.id;
        let c = store.create_collection(&pid, "C").unwrap();
        let f = store.create_folder(&c.id, None, "F").unwrap();
        let g = store.create_folder(&c.id, Some(&f.id), "G").unwrap();
        store
            .create_request(&c.id, Some(&g.id), Request::default())
            .unwrap();
        let env = store.create_environment(Some(&pid), "E").unwrap();
        store.set_active_environment(&pid, Some(&env.id)).unwrap();
        let t = store.project(&pid).unwrap().clone();
        let envs = store.project_envs(&pid);
        let mut map = BTreeMap::new();
        let (copy, cenvs) = reid(&t, &envs, &mut map);
        assert_eq!(map.len(), 6);
        assert_ne!(copy.project.id, pid);
        assert_eq!(copy.collections[0].project_id, copy.project.id);
        let child = copy.folders.iter().find(|x| x.name == "G").unwrap();
        let parent = copy.folders.iter().find(|x| x.name == "F").unwrap();
        assert_eq!(child.parent_folder_id.as_ref(), Some(&parent.id));
        assert_eq!(copy.requests[0].folder_id.as_ref(), Some(&child.id));
        assert_eq!(cenvs[0].project_id.as_ref(), Some(&copy.project.id));
        assert_eq!(
            copy.project.active_environment_id.as_ref(),
            Some(&cenvs[0].id)
        );
        // The same map gives the same ids again (resumable moves).
        let (again, _) = reid(&t, &envs, &mut map);
        assert_eq!(again, copy);
    }
}
