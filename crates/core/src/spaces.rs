//! Personal and team spaces on the client (04 §6, §7; 06-go-server §9).
//!
//! * **Pure-legacy mode** (`account.json` has no bindings): every project is
//!   bound to the synthetic `legacy` scope, backed by `sync-state.json` and
//!   `workspace.sync.cursor`. Files and behaviour are byte-identical to the
//!   token-sync releases.
//! * **Account mode** (`account.json` has `migratedAt`): [`Bindings`] in
//!   `account.json` are authoritative. A project without an entry in
//!   `projectSpaces` is "This device only" and never produces traffic. Each
//!   bound space keeps its own cursor and bookkeeping ([`SpaceSync`]) in
//!   `sync-spaces.json` (plus a backup), and `sync-state.json` keeps only the
//!   device id.
//!
//! Bindings change **only** through an explicit local action (rule B1):
//! the sign-in migration, the first-sync dialog, Sync location…, the legacy
//! merge, resolving a space notice, or a project that is new to this device
//! arriving in a space. Pulled data never rebinds anything (B2–B5 live in
//! `sync.rs`).

use crate::model::*;
use crate::secrets::SecretStore;
use crate::storage::{Result, Store, StoreError, SyncState, atomic_write, sync_key};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// The synthetic space id of a token-synced (legacy) workspace.
pub const LEGACY_SPACE: &str = "legacy";
pub const SPACES_FILE: &str = "sync-spaces.json";
pub const SPACES_BACKUP: &str = "sync-spaces.backup.json";
/// After this many runs that deferred deletes (B7), deletes apply anyway.
pub const MAX_DEFERRED_RUNS: u32 = 3;
/// The clock counts as fixed when the server's clock is this close (04 §6.9).
pub const CLOCK_OK_MS: i64 = 60_000;

/// Where a sync step reads and writes its bookkeeping (04 §6.4 `Scope`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// Token sync: `/v1/sync` with an `rh_` token.
    Legacy,
    /// An account space: `/v1/spaces/{id}/sync` with a session.
    Space(String),
}

impl Scope {
    pub fn id(&self) -> &str {
        match self {
            Scope::Legacy => LEGACY_SPACE,
            Scope::Space(s) => s,
        }
    }

    pub fn from_id(id: &str) -> Scope {
        if id == LEGACY_SPACE {
            Scope::Legacy
        } else {
            Scope::Space(id.to_string())
        }
    }

    /// API path prefix: `/v1` or `/v1/spaces/{id}`.
    pub fn prefix(&self) -> String {
        match self {
            Scope::Legacy => "/v1".into(),
            Scope::Space(id) => format!(
                "/v1/spaces/{}",
                percent_encoding::utf8_percent_encode(id, percent_encoding::NON_ALPHANUMERIC)
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceKind {
    Personal,
    Team,
    Legacy,
    /// A kind from a newer server; synced like a team space.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Editor,
    Admin,
    Owner,
    /// A role from a newer server: treated as read-only (the safe side).
    #[serde(other)]
    Unknown,
}

impl Role {
    pub fn parse(s: &str) -> Role {
        serde_json::from_value(serde_json::Value::String(s.to_string())).unwrap_or(Role::Unknown)
    }
    pub fn can_write(&self) -> bool {
        matches!(self, Role::Editor | Role::Admin | Role::Owner)
    }
}

/// Why a binding is paused.
pub const PAUSED_FOREIGN: &str = "foreignAccount";

/// 04 §6.1 `BindingState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BindingState {
    Active,
    /// Excluded from pull, push and the first-sync checklist until the user
    /// resolves the notice (`foreignAccount`: another server, instance or
    /// user than the signed-in account, 06 §9).
    Paused {
        reason: String,
    },
    /// A transient 403/404 on a space call or a failed `/v1/me/spaces`
    /// (04 §6.7): retried, local data untouched.
    Unavailable {
        since: i64,
    },
    /// Only after an authoritative `/v1/me/spaces` without the space.
    Removed {
        at: i64,
        reason: String,
    },
    /// A state from a newer build: treated like paused.
    #[serde(other)]
    Unknown,
}

fn active() -> BindingState {
    BindingState::Active
}

impl BindingState {
    pub fn name(&self) -> &'static str {
        match self {
            BindingState::Active => "active",
            BindingState::Paused { .. } | BindingState::Unknown => "paused",
            BindingState::Unavailable { .. } => "unavailable",
            BindingState::Removed { .. } => "removed",
        }
    }
}

/// One space this device syncs (account.json `spaces[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceBinding {
    pub space_id: String,
    pub kind: SpaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Normalised origin of the server the space lives on.
    #[serde(default)]
    pub server_origin: String,
    #[serde(default)]
    pub server_instance_id: String,
    /// The account that bound it (with its email for the notice copy).
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub user_email: String,
    #[serde(default = "active")]
    pub state: BindingState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_bytes: Option<u64>,
    /// When the user closed the space notice with × ("decide later", UX
    /// §6.9): after [`DECIDE_LATER_MS`] Keep is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decide_later_at: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// How long a space notice closed with × waits before Keep is applied.
pub const DECIDE_LATER_MS: i64 = 7 * 24 * 3_600_000;

impl SpaceBinding {
    /// Display name: "Personal", the team's name, or the id.
    pub fn name(&self) -> String {
        match self.kind {
            SpaceKind::Personal => "Personal".into(),
            SpaceKind::Legacy => "Legacy".into(),
            _ => self
                .team_name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| self.space_id.clone()),
        }
    }

    pub fn is_active(&self) -> bool {
        self.state == BindingState::Active
    }

    /// Whether this binding belongs to `who`: same origin, user and
    /// instance (06 §9: a new `instanceId` for the same origin is another
    /// server). An account whose instance is unknown (no discovery yet) owns
    /// nothing: nothing binds or pushes until discovery fills it in. A
    /// binding without an instance (written before it was known) adopts the
    /// account's.
    pub fn belongs_to(&self, who: &AccountIdent) -> bool {
        self.same_user(who)
            && !who.server_instance_id.is_empty()
            && (self.server_instance_id.is_empty()
                || self.server_instance_id == who.server_instance_id)
    }

    /// Same origin and user, whatever the instance.
    fn same_user(&self, who: &AccountIdent) -> bool {
        self.server_origin == who.server_url && self.user_id == who.user_id
    }

    /// Paused because another account was signed in.
    pub fn paused_foreign(&self) -> bool {
        matches!(&self.state, BindingState::Paused { reason } if reason == PAUSED_FOREIGN)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LegacyMode {
    Sync,
    /// After the merge: no pulls or pushes, share calls keep working.
    SharesOnly,
}

/// account.json `legacy`: the token-synced workspace (its token lives in the
/// secret store under [`legacy_key`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyLink {
    pub worker_url: String,
    pub remote_workspace_id: String,
    pub mode: LegacyMode,
}

/// Secret-store entry of a legacy token (04 §3.3).
pub fn legacy_key(remote_workspace_id: &str) -> String {
    format!("legacy:{remote_workspace_id}")
}

/// The binding part of `account.json` (04 §6.1), flattened into
/// [`crate::auth::AccountFile`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bindings {
    /// When the sign-in migration ran; its presence means account mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spaces: Vec<SpaceBinding>,
    /// projectId → spaceId. Authoritative: a missing entry is local-only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub project_spaces: BTreeMap<String, String>,
    /// The only space the Global environment syncs with (B4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy: Option<LegacyLink>,
    /// The first-sync checklist hasn't been answered yet (UX §10).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub first_sync_pending: bool,
    /// The user kept the Global environment on this device ("Keep all on
    /// this device", or Keep for another account's space): it never binds
    /// to a space by itself again (AC-ONB-5).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub global_local: bool,
    /// The migration found a fresh device: its untouched Default binds to
    /// Personal once Personal is known (UX §10 "Fresh device"). Cleared then.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh_device: bool,
}

impl Bindings {
    pub fn is_empty(&self) -> bool {
        self == &Bindings::default()
    }
}

/// The signed-in account as the store sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountIdent {
    pub server_url: String,
    pub server_instance_id: String,
    pub user_id: String,
    pub email: String,
}

impl From<&crate::auth::Account> for AccountIdent {
    fn from(a: &crate::auth::Account) -> Self {
        AccountIdent {
            server_url: a.server_url.clone(),
            server_instance_id: a.server_instance_id.clone(),
            user_id: a.user_id.clone(),
            email: a.email.clone(),
        }
    }
}

/// Who last changed an entity (pulled `updatedBy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attribution {
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub name: String,
}

/// A per-space problem for the status line, tooltip and Settings › Sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceIssue {
    /// `quota` | `serverFull` | `clockSkew` | `clockFixed` | `clientTooOld` |
    /// `unavailable` | `legacyDisabled` | `error`.
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub at: i64,
}

impl SpaceIssue {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        SpaceIssue {
            code: code.into(),
            message: message.into(),
            at: now_ms(),
        }
    }
}

/// `sync-spaces.json` → `spaces[id]` (04 §6.1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceSync {
    #[serde(default)]
    pub cursor: u64,
    /// Bookkeeping as in `sync-state.json`, without the device id.
    #[serde(default)]
    pub state: SyncState,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attribution: BTreeMap<String, Attribution>,
    /// Keys frozen after a role downgrade (M2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frozen: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<SpaceIssue>,
    /// Pulled entities ignored because they belong elsewhere (B2/B3).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub foreign: u64,
    /// Consecutive runs whose deletes were deferred (B7).
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub deferred_runs: u32,
    /// `"type:id"` → the `updatedAt` the server rejected with `clock_skew`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skewed: BTreeMap<String, i64>,
    /// `serverTime − local now` at the last push response (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_offset_ms: Option<i64>,
    /// Resumable cross-space moves (M2b); kept verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_moves: Vec<serde_json::Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}
fn is_zero32(n: &u32) -> bool {
    *n == 0
}

/// `sync-spaces.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpacesFile {
    #[serde(default = "one")]
    pub schema_version: u32,
    #[serde(default)]
    pub spaces: BTreeMap<String, SpaceSync>,
    #[serde(flatten)]
    pub extra: Extra,
}

fn one() -> u32 {
    1
}

impl Default for SpacesFile {
    fn default() -> Self {
        SpacesFile {
            schema_version: 1,
            spaces: BTreeMap::new(),
            extra: Extra::new(),
        }
    }
}

static EMPTY_STATE: LazyLock<SyncState> = LazyLock::new(SyncState::default);
static EMPTY_SPACE: LazyLock<SpaceSync> = LazyLock::new(SpaceSync::default);

/// What a project's binding looks like to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLocation {
    pub project_id: String,
    /// `None`: this device only.
    pub space_id: Option<String>,
    /// `personal` | `team` | `legacy` | `local`.
    pub kind: String,
    /// "Personal", the team name, "Legacy workspace" or "This device only".
    pub label: String,
    pub role: Option<Role>,
    /// The binding's state, or `signedOut` for an account space while no
    /// account is signed in; `None` for local-only projects.
    pub state: Option<String>,
}

/// One row of the first-sync checklist (UX §10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstSyncCandidate {
    pub project_id: String,
    pub name: String,
    pub collections: usize,
    pub requests: usize,
}

/// What [`Store::sign_in_migrate`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// The migration ran now (false: it had run before).
    pub migrated: bool,
    /// Token sync moved into account mode (`legacy` binding).
    pub legacy: bool,
    /// Bindings of another server or account that are now paused.
    pub paused: Vec<String>,
    /// The first-sync checklist has projects to offer.
    pub first_sync: bool,
    /// A fresh device (only an untouched Default): no checklist; the UI
    /// says "Signed in · syncing N projects…" after the first run (UX §10).
    pub fresh_device: bool,
}

// ───────────────────────────── the store side ─────────────────────────────

impl Store {
    /// Account mode: `account.json` holds authoritative bindings (04 §6.1).
    pub fn account_mode(&self) -> bool {
        self.bindings.migrated_at.is_some()
    }

    /// This installation's id (LWW tie-breaker), the same for every space.
    pub fn device_id(&self) -> &str {
        &self.sync_state.device_id
    }

    pub fn binding(&self, space_id: &str) -> Option<&SpaceBinding> {
        self.bindings.spaces.iter().find(|b| b.space_id == space_id)
    }

    pub fn binding_mut(&mut self, space_id: &str) -> Option<&mut SpaceBinding> {
        self.bindings
            .spaces
            .iter_mut()
            .find(|b| b.space_id == space_id)
    }

    /// The personal space of the signed-in account, when bound.
    pub fn personal_space(&self) -> Option<&SpaceBinding> {
        let who = self.account.as_ref()?;
        self.bindings
            .spaces
            .iter()
            .find(|b| b.kind == SpaceKind::Personal && b.belongs_to(who))
    }

    /// The scope a project syncs with; `None` = this device only.
    pub fn scope_of_project(&self, project_id: &str) -> Option<Scope> {
        if !self.account_mode() {
            return Some(Scope::Legacy);
        }
        self.bindings
            .project_spaces
            .get(project_id)
            .map(|s| Scope::from_id(s))
    }

    /// The scope the Global environment syncs with (B4).
    pub fn global_scope(&self) -> Option<Scope> {
        if !self.account_mode() {
            return Some(Scope::Legacy);
        }
        self.bindings.global_space.as_deref().map(Scope::from_id)
    }

    /// The scope of an environment: its project's, or the Global's.
    pub fn scope_of_env(&self, env: &Environment) -> Option<Scope> {
        match &env.project_id {
            Some(pid) => self.scope_of_project(pid),
            None => self.global_scope(),
        }
    }

    pub fn in_scope(&self, scope: &Scope, project_id: &str) -> bool {
        self.scope_of_project(project_id).as_ref() == Some(scope)
    }

    pub fn global_in_scope(&self, scope: &Scope) -> bool {
        self.global_scope().as_ref() == Some(scope)
    }

    /// The bookkeeping of `scope` (04 §6.1 `space_state_mut` hides the two
    /// backends): `sync-state.json` in pure-legacy mode, `sync-spaces.json`
    /// otherwise.
    pub fn space_state(&self, scope: &Scope) -> &SyncState {
        if !self.account_mode() {
            return &self.sync_state;
        }
        self.spaces_state
            .spaces
            .get(scope.id())
            .map(|s| &s.state)
            .unwrap_or(&EMPTY_STATE)
    }

    pub fn space_state_mut(&mut self, scope: &Scope) -> &mut SyncState {
        if !self.account_mode() {
            return &mut self.sync_state;
        }
        &mut self
            .spaces_state
            .spaces
            .entry(scope.id().to_string())
            .or_default()
            .state
    }

    /// The per-space extras (cursor, attribution, issue…); account mode only.
    pub fn space_sync(&self, scope: &Scope) -> &SpaceSync {
        self.spaces_state
            .spaces
            .get(scope.id())
            .unwrap_or(&EMPTY_SPACE)
    }

    pub fn space_sync_mut(&mut self, scope: &Scope) -> &mut SpaceSync {
        self.spaces_state
            .spaces
            .entry(scope.id().to_string())
            .or_default()
    }

    /// Where the next pull of `scope` starts; `None` when not connected.
    pub fn scope_cursor(&self, scope: &Scope) -> Option<u64> {
        if !self.account_mode() {
            return self.workspace.sync.as_ref().map(|s| s.cursor);
        }
        Some(self.space_sync(scope).cursor)
    }

    pub fn set_scope_cursor(&mut self, scope: &Scope, cursor: u64) {
        if !self.account_mode() {
            if let Some(s) = self.workspace.sync.as_mut() {
                s.cursor = cursor;
            }
            return;
        }
        self.space_sync_mut(scope).cursor = cursor;
    }

    /// Whether `scope` may pull and push now: pure-legacy needs a token;
    /// legacy in account mode needs mode `sync` and a token; a space needs
    /// an active binding of the signed-in account.
    pub fn scope_syncable(&self, scope: &Scope) -> bool {
        match scope {
            Scope::Legacy if !self.account_mode() => self.workspace.sync.is_some(),
            Scope::Legacy => {
                self.legacy_token.is_some()
                    && self
                        .bindings
                        .legacy
                        .as_ref()
                        .is_some_and(|l| l.mode == LegacyMode::Sync)
            }
            Scope::Space(id) => match (self.binding(id), &self.account) {
                (Some(b), Some(who)) => b.is_active() && b.belongs_to(who),
                _ => false,
            },
        }
    }

    /// Scopes to sync, in run order: personal → legacy → teams (04 §6.6).
    pub fn sync_scopes(&self) -> Vec<Scope> {
        if !self.account_mode() {
            return if self.workspace.sync.is_some() {
                vec![Scope::Legacy]
            } else {
                vec![]
            };
        }
        let mut out = Vec::new();
        let mut spaces: Vec<&SpaceBinding> = self.bindings.spaces.iter().collect();
        spaces.sort_by_key(|b| match b.kind {
            SpaceKind::Personal => 0,
            _ => 2,
        });
        for b in spaces.iter().filter(|b| b.kind == SpaceKind::Personal) {
            let s = Scope::Space(b.space_id.clone());
            if self.scope_syncable(&s) {
                out.push(s);
            }
        }
        if self.scope_syncable(&Scope::Legacy) {
            out.push(Scope::Legacy);
        }
        for b in spaces.iter().filter(|b| b.kind != SpaceKind::Personal) {
            let s = Scope::Space(b.space_id.clone());
            if self.scope_syncable(&s) {
                out.push(s);
            }
        }
        out
    }

    /// Anything to sync at all (gates `schedule_push`, 04 §6.6).
    pub fn sync_active(&self) -> bool {
        if !self.account_mode() {
            return self.workspace.sync.is_some();
        }
        self.scope_syncable(&Scope::Legacy) || self.account.is_some()
    }

    /// Persist the binding part of `account.json`.
    pub fn save_bindings(&self) -> Result<()> {
        let bindings = self.bindings.clone();
        crate::auth::AccountFile::update(self.dir(), move |f| {
            f.bindings = bindings;
        })
        .map(|_| ())
    }

    /// Persist `sync-spaces.json` and its backup (account mode only).
    pub fn save_spaces_state(&self) -> Result<()> {
        if !self.account_mode() {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&self.spaces_state)
            .map_err(|e| StoreError::Io(e.to_string()))?;
        atomic_write(&self.dir().join(SPACES_FILE), &bytes)?;
        atomic_write(&self.dir().join(SPACES_BACKUP), &bytes)
    }

    /// The viewer guard (04 §6.8): every project-tree mutator calls it.
    pub fn write_guard(&self, project_id: &str) -> Result<()> {
        let Some(Scope::Space(id)) = self.scope_of_project(project_id) else {
            return Ok(());
        };
        match self.binding(&id).and_then(|b| b.role) {
            Some(r) if !r.can_write() => Err(StoreError::ReadOnlyRole(format!(
                "Viewers can't change team projects. Duplicate to personal to experiment ({}).",
                self.binding(&id).map(|b| b.name()).unwrap_or_default()
            ))),
            _ => Ok(()),
        }
    }

    /// The binding a new project gets (04 §6.1 "New projects"): signed in →
    /// Personal (D11); only token sync → legacy; otherwise none.
    pub fn default_location(&self) -> Option<String> {
        if !self.account_mode() {
            return None;
        }
        if let Some(p) = self.personal_space()
            && p.is_active()
        {
            return Some(p.space_id.clone());
        }
        if self.account.is_none() && self.scope_syncable(&Scope::Legacy) {
            return Some(LEGACY_SPACE.into());
        }
        None
    }

    /// Bind a freshly created project (no tombstones, nothing synced).
    pub(crate) fn bind_new_project(&mut self, project_id: &str, space: Option<String>) {
        if !self.account_mode() {
            return;
        }
        match space {
            Some(s) => {
                self.bindings
                    .project_spaces
                    .insert(project_id.to_string(), s);
            }
            None => {
                self.bindings.project_spaces.remove(project_id);
            }
        }
    }

    /// Where every project lives, for the grouped switcher (UX §5.1).
    pub fn project_locations(&self) -> Vec<ProjectLocation> {
        self.project_list()
            .into_iter()
            .map(|p| self.location_of(&p.id))
            .collect()
    }

    pub fn location_of(&self, project_id: &str) -> ProjectLocation {
        let local = || ProjectLocation {
            project_id: project_id.to_string(),
            space_id: None,
            kind: "local".into(),
            label: "This device only".into(),
            role: None,
            state: None,
        };
        if !self.account_mode() {
            return if self.workspace.sync.is_some() {
                ProjectLocation {
                    space_id: Some(LEGACY_SPACE.into()),
                    kind: "legacy".into(),
                    label: "Legacy workspace".into(),
                    state: Some("active".into()),
                    ..local()
                }
            } else {
                local()
            };
        }
        let Some(space) = self.bindings.project_spaces.get(project_id) else {
            return local();
        };
        if space == LEGACY_SPACE {
            let mode = self.bindings.legacy.as_ref().map(|l| l.mode);
            return ProjectLocation {
                space_id: Some(LEGACY_SPACE.into()),
                kind: "legacy".into(),
                label: "Legacy workspace".into(),
                state: Some(match mode {
                    Some(LegacyMode::Sync) if self.legacy_token.is_some() => "active".into(),
                    _ => "paused".into(),
                }),
                ..local()
            };
        }
        match self.binding(space) {
            Some(b) => ProjectLocation {
                space_id: Some(b.space_id.clone()),
                kind: match b.kind {
                    SpaceKind::Personal => "personal".into(),
                    _ => "team".into(),
                },
                label: b.name(),
                role: b.role,
                state: Some(match &self.account {
                    None => "signedOut".into(),
                    Some(who) if !b.belongs_to(who) => "paused".into(),
                    Some(_) => b.state.name().into(),
                }),
                ..local()
            },
            // A binding without its space: treated as local-only.
            None => local(),
        }
    }

    /// The fresh-device toast for the signed-in account: its projects in
    /// Personal and teams, and how many teams it has.
    pub fn fresh_device_notice(&self) -> String {
        let Some(who) = self.account.as_ref() else {
            return fresh_device_message(0, 0);
        };
        let mine: Vec<&SpaceBinding> = self
            .bindings
            .spaces
            .iter()
            .filter(|b| b.is_active() && b.belongs_to(who))
            .collect();
        let projects = self
            .bindings
            .project_spaces
            .values()
            .filter(|s| mine.iter().any(|b| &b.space_id == *s))
            .count();
        let teams = mine
            .iter()
            .filter(|b| b.kind != SpaceKind::Personal)
            .count();
        fresh_device_message(projects, teams)
    }

    /// Projects of a space, by name.
    pub fn projects_in(&self, space_id: &str) -> Vec<Project> {
        self.project_list()
            .into_iter()
            .filter(|p| {
                self.bindings.project_spaces.get(&p.id).map(String::as_str) == Some(space_id)
            })
            .collect()
    }

    // ─────────────────────────── account lifecycle ───────────────────────────

    /// Set (or clear) the signed-in account, pausing every binding of another
    /// server, instance or user (06 §9). Returns the newly paused space ids.
    pub fn set_account(&mut self, who: Option<AccountIdent>) -> Vec<String> {
        self.account = who;
        self.pause_foreign()
    }

    /// Pause bindings that don't belong to the signed-in account, and
    /// resume the ones paused for another account that belong to it again
    /// (A → B → A: A's projects sync again, no notice). A binding of the
    /// same origin and user is left alone while the account's instance is
    /// unknown. Returns the newly paused space ids.
    pub fn pause_foreign(&mut self) -> Vec<String> {
        let Some(who) = self.account.clone() else {
            return Vec::new();
        };
        let mut paused = Vec::new();
        for b in &mut self.bindings.spaces {
            if b.belongs_to(&who) {
                if b.paused_foreign() {
                    b.state = BindingState::Active;
                    b.decide_later_at = None;
                }
            } else if who.server_instance_id.is_empty() && b.same_user(&who) {
                // Unknown instance: neither ours nor foreign yet.
            } else if !matches!(b.state, BindingState::Paused { .. }) {
                b.state = BindingState::Paused {
                    reason: PAUSED_FOREIGN.into(),
                };
                b.decide_later_at = None;
                paused.push(b.space_id.clone());
            }
        }
        paused
    }

    /// The one-time sign-in migration (04 §7):
    /// * with token sync: the token moves to the secret store, the cursor
    ///   and bookkeeping move into `sync-spaces.json["legacy"]`, every
    ///   project and the Global bind to `legacy`, and `sync` is removed from
    ///   `workspace.json` (so a downgraded client stops syncing);
    /// * without: projects stay unbound until the first-sync dialog, except
    ///   an untouched Default on a fresh device, which binds to Personal.
    ///
    /// Later sign-ins only pause foreign bindings. The secret is written
    /// first, so a failure leaves today's files untouched.
    pub fn sign_in_migrate(
        &mut self,
        who: AccountIdent,
        secrets: &dyn SecretStore,
    ) -> std::result::Result<MigrationReport, StoreError> {
        let mut report = MigrationReport::default();
        if self.account_mode() {
            report.paused = self.set_account(Some(who));
            self.save_bindings()?;
            return Ok(report);
        }
        report.migrated = true;
        let mut bindings = Bindings {
            migrated_at: Some(now_ms()),
            ..Bindings::default()
        };
        let mut spaces = SpacesFile::default();
        if let Some(cfg) = self.workspace.sync.clone() {
            secrets
                .set(&legacy_key(&cfg.remote_workspace_id), &cfg.token)
                .map_err(|e| StoreError::Io(e.to_string()))?;
            report.legacy = true;
            let mut state = self.sync_state.clone();
            state.device_id = String::new();
            spaces.spaces.insert(
                LEGACY_SPACE.into(),
                SpaceSync {
                    cursor: cfg.cursor,
                    state,
                    ..SpaceSync::default()
                },
            );
            for pid in self.projects.keys() {
                bindings
                    .project_spaces
                    .insert(pid.clone(), LEGACY_SPACE.into());
            }
            bindings.global_space = Some(LEGACY_SPACE.into());
            bindings.legacy = Some(LegacyLink {
                worker_url: cfg.worker_url.clone(),
                remote_workspace_id: cfg.remote_workspace_id.clone(),
                mode: LegacyMode::Sync,
            });
            self.legacy_token = Some(cfg.token.clone());
        } else if self.is_fresh_device() {
            bindings.fresh_device = true;
            report.fresh_device = true;
        } else {
            bindings.first_sync_pending = true;
        }
        self.bindings = bindings;
        self.spaces_state = spaces;
        self.account = Some(who);
        // Order: sync-spaces.json, account.json, then workspace.json without
        // `sync`, then the emptied sync-state.json.
        self.save_spaces_state()?;
        self.save_bindings()?;
        if self.workspace.sync.take().is_some() {
            self.save_workspace()?;
            self.sync_state.reset_for_new_remote();
            self.save_sync_state()?;
        }
        report.first_sync = self.bindings.first_sync_pending;
        Ok(report)
    }

    /// Only an untouched Default project (and an empty Global): nothing to
    /// ask about at the first sign-in (UX §10 "Fresh device").
    pub fn is_fresh_device(&self) -> bool {
        self.projects.values().all(is_untouched_default)
            && self
                .environments
                .iter()
                .all(|e| e.project_id.is_none() && e.variables.is_empty())
    }

    /// Projects the first-sync checklist offers: unbound ones (legacy-bound
    /// projects are handled by the merge), never paused or removed ones.
    pub fn first_sync_candidates(&self) -> Vec<FirstSyncCandidate> {
        if !self.account_mode() || !self.bindings.first_sync_pending {
            return Vec::new();
        }
        self.project_list()
            .into_iter()
            .filter(|p| !self.bindings.project_spaces.contains_key(&p.id))
            .filter_map(|p| {
                let t = self.projects.get(&p.id)?;
                Some(FirstSyncCandidate {
                    project_id: p.id.clone(),
                    name: p.name.clone(),
                    collections: t.collections.len(),
                    requests: t.requests.len(),
                })
            })
            .collect()
    }

    /// The first-sync answer (AC-ONB-5): bind the checked projects to the
    /// personal space; the rest stay on this device. The Global environment
    /// goes to Personal with them (when nothing else holds it); with no
    /// project checked ("Keep all on this device", ×) it stays here too.
    pub fn first_sync_apply(&mut self, project_ids: &[String]) -> Result<usize> {
        let personal = self
            .personal_space()
            .map(|b| b.space_id.clone())
            .ok_or_else(|| {
                StoreError::Invalid("Your personal space isn't available yet. Try again.".into())
            })?;
        let mut n = 0;
        for pid in project_ids {
            if self.projects.contains_key(pid) && !self.bindings.project_spaces.contains_key(pid) {
                self.bindings
                    .project_spaces
                    .insert(pid.clone(), personal.clone());
                n += 1;
            }
        }
        if self.bindings.global_space.is_none() {
            if n > 0 {
                self.bindings.global_space = Some(personal);
                self.bindings.global_local = false;
            } else {
                self.bindings.global_local = true;
            }
        }
        self.bindings.first_sync_pending = false;
        self.save_bindings()?;
        Ok(n)
    }

    /// Sync location… (AC-PROJ-3, B1) between Personal (or a writable space)
    /// and This device only:
    /// * to a space: the project binds there and every entity is stamped
    ///   newer, so a tombstone the space may hold from an earlier "stop
    ///   syncing" can't hide it;
    /// * to this device: tombstones go to the old space for everything it
    ///   holds ("Removes it from your other devices. This copy stays.").
    ///
    /// Team moves (new ids, online) are M2's `move_project_to_space`.
    pub fn set_project_location(&mut self, project_id: &str, target: Option<&str>) -> Result<()> {
        self.project(project_id)?;
        let current = self.bindings.project_spaces.get(project_id).cloned();
        if current.as_deref() == target {
            return Ok(());
        }
        if !self.account_mode() {
            return Err(StoreError::Invalid(
                "Sign in to choose where a project syncs.".into(),
            ));
        }
        if let Some(from) = &current {
            let from_kind = self.binding(from).map(|b| b.kind);
            if matches!(from_kind, Some(SpaceKind::Team | SpaceKind::Unknown)) {
                return Err(StoreError::Invalid(
                    "Moving a project out of a team arrives with teams. Use Copy to project… instead."
                        .into(),
                ));
            }
        }
        if current.as_deref() == Some(LEGACY_SPACE) && target.is_some() {
            // B6: the only same-id transfer out of legacy is the merge, which
            // stops token sync in the same step. Tombstones here would delete
            // the project for every device still on the token.
            return Err(StoreError::Invalid(
                "This project syncs with a token. Use Merge into my personal space… in Settings › Sync to move it."
                    .into(),
            ));
        }
        if let Some(to) = target {
            match self.binding(to) {
                Some(b) if b.kind == SpaceKind::Personal => {}
                Some(_) => {
                    return Err(StoreError::Invalid(
                        "Moving a project to a team arrives with teams.".into(),
                    ));
                }
                None => return Err(StoreError::NotFound(format!("Space {to}"))),
            }
        }
        // Leave the old space: tombstones for what it knows about.
        if let Some(from) = current {
            let scope = Scope::from_id(&from);
            for (t, id) in self.project_entities(project_id) {
                let key = sync_key(t, &id);
                let st = self.space_state_mut(&scope);
                if st.synced.remove(&key).is_some() {
                    st.tombstones.retain(|x| x.id != id);
                    st.tombstones.push(Tombstone {
                        id: id.clone(),
                        entity_type: t,
                        deleted_at: now_ms(),
                        extra: Extra::new(),
                    });
                }
                st.origins.remove(&key);
                st.sync_skipped.remove(&key);
                st.resend.remove(&key);
            }
            self.bindings.project_spaces.remove(project_id);
        }
        if let Some(to) = target {
            self.bindings
                .project_spaces
                .insert(project_id.to_string(), to.to_string());
            self.touch_project(project_id)?;
        }
        self.save_bindings()?;
        self.save_sync_state()
    }

    /// Every synced entity of a project: the project, its tree and its
    /// environments.
    pub fn project_entities(&self, project_id: &str) -> Vec<(EntityType, String)> {
        let mut out = Vec::new();
        if let Some(t) = self.projects.get(project_id) {
            out.push((EntityType::Project, project_id.to_string()));
            out.extend(
                t.collections
                    .iter()
                    .map(|c| (EntityType::Collection, c.id.clone())),
            );
            out.extend(t.folders.iter().map(|f| (EntityType::Folder, f.id.clone())));
            out.extend(
                t.requests
                    .iter()
                    .map(|r| (EntityType::Request, r.id.clone())),
            );
        }
        out.extend(
            self.environments
                .iter()
                .filter(|e| e.project_id.as_deref() == Some(project_id))
                .map(|e| (EntityType::Environment, e.id.clone())),
        );
        out
    }

    /// Stamp every editable entity of a project as just changed.
    fn touch_project(&mut self, project_id: &str) -> Result<()> {
        let now = now_ms();
        let bump = |at: &mut i64| *at = now.max(*at + 1);
        if let Some(t) = self.projects.get_mut(project_id) {
            if t.project.is_editable() {
                bump(&mut t.project.updated_at);
            }
            for c in t.collections.iter_mut().filter(|c| c.is_editable()) {
                bump(&mut c.updated_at);
            }
            for f in t.folders.iter_mut().filter(|f| f.is_editable()) {
                bump(&mut f.updated_at);
            }
            for r in t.requests.iter_mut().filter(|r| r.is_editable()) {
                bump(&mut r.updated_at);
            }
        }
        for e in self
            .environments
            .iter_mut()
            .filter(|e| e.project_id.as_deref() == Some(project_id) && e.is_editable())
        {
            bump(&mut e.updated_at);
        }
        self.save_project(project_id)?;
        self.save_environments()
    }

    /// Merge the legacy workspace into Personal (AC-ONB-3, B6): one local
    /// transaction with the **same ids**. Legacy projects and the Global
    /// rebind to Personal and are re-sent there; the legacy space switches to
    /// shares-only, so no further legacy pull can disagree. Idempotent.
    pub fn legacy_merge(&mut self) -> Result<usize> {
        let personal = self
            .personal_space()
            .map(|b| b.space_id.clone())
            .ok_or_else(|| {
                StoreError::Invalid("Your personal space isn't available yet. Try again.".into())
            })?;
        let legacy = self
            .bindings
            .legacy
            .as_mut()
            .ok_or_else(|| StoreError::Invalid("No token-synced workspace to merge.".into()))?;
        legacy.mode = LegacyMode::SharesOnly;
        let moved: Vec<String> = self
            .bindings
            .project_spaces
            .iter()
            .filter(|(_, s)| s.as_str() == LEGACY_SPACE)
            .map(|(p, _)| p.clone())
            .collect();
        for pid in &moved {
            self.bindings
                .project_spaces
                .insert(pid.clone(), personal.clone());
        }
        if self.bindings.global_space.as_deref() != Some(personal.as_str()) {
            self.bindings.global_space = Some(personal.clone());
        }
        // The re-sent versions keep their device ids (contract C3), so an
        // equal version already on Personal (another device merged first)
        // is a no-op rather than a conflict.
        let personal_scope = Scope::Space(personal);
        let legacy_state = self.space_state(&Scope::Legacy).clone();
        let st = self.space_state_mut(&personal_scope);
        for (k, v) in &legacy_state.synced {
            if !st.synced.contains_key(k) {
                st.resend.insert(k.clone(), *v);
                if let Some(o) = legacy_state.origins.get(k) {
                    st.origins.insert(k.clone(), o.clone());
                }
            }
        }
        self.save_bindings()?;
        self.save_sync_state()?;
        Ok(moved.len())
    }

    /// Move a stray `sync` block (see [`Store::stray_sync`]) out of
    /// `workspace.json`: its token goes to the secret store under
    /// [`legacy_key`] first, then the block is removed from disk. Returns
    /// its remote workspace id. A failed secret write leaves the file as is.
    pub fn secure_stray_sync(
        &mut self,
        secrets: &dyn SecretStore,
    ) -> std::result::Result<Option<String>, StoreError> {
        let Some(cfg) = self.stray_sync.clone() else {
            return Ok(None);
        };
        secrets
            .set(&legacy_key(&cfg.remote_workspace_id), &cfg.token)
            .map_err(|e| StoreError::Io(e.to_string()))?;
        self.stray_sync = None;
        self.save_workspace()?;
        Ok(Some(cfg.remote_workspace_id))
    }

    /// Forget the legacy workspace (its share links keep working on the
    /// server). Projects still bound to it become local-only.
    pub fn legacy_disconnect(&mut self, secrets: &dyn SecretStore) -> Result<()> {
        if let Some(l) = self.bindings.legacy.take() {
            let _ = secrets.delete(&legacy_key(&l.remote_workspace_id));
        }
        self.bindings
            .project_spaces
            .retain(|_, s| s.as_str() != LEGACY_SPACE);
        if self.bindings.global_space.as_deref() == Some(LEGACY_SPACE) {
            // Like its projects, the Global stays on this device.
            self.bindings.global_space = None;
            self.bindings.global_local = true;
        }
        self.spaces_state.spaces.remove(LEGACY_SPACE);
        self.legacy_token = None;
        self.save_bindings()?;
        self.save_spaces_state()
    }

    /// Resolve a space notice (paused for another account, or removed):
    /// keep turns its projects into local-only copies (team projects are
    /// renamed "X (from Team)"); remove drops them from this device. No
    /// tombstone is sent either way. Returns the affected project ids.
    pub fn space_notice_resolve(&mut self, space_id: &str, keep: bool) -> Result<Vec<String>> {
        let binding = self
            .binding(space_id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(format!("Space {space_id}")))?;
        let projects: Vec<String> = self
            .bindings
            .project_spaces
            .iter()
            .filter(|(_, s)| s.as_str() == space_id)
            .map(|(p, _)| p.clone())
            .collect();
        for pid in &projects {
            self.bindings.project_spaces.remove(pid);
            if keep {
                if binding.kind != SpaceKind::Personal
                    && let Some(t) = self.projects.get_mut(pid)
                {
                    t.project.name = format!("{} (from {})", t.project.name, binding.name());
                    self.save_project(pid)?;
                }
            } else if self.projects.len() > 1 {
                self.forget_project(pid);
                self.environments
                    .retain(|e| e.project_id.as_deref() != Some(pid.as_str()));
            }
        }
        if self.bindings.global_space.as_deref() == Some(space_id) {
            // Never hand one account's Global to another (06 §9): it stays
            // on this device.
            self.bindings.global_space = None;
            self.bindings.global_local = true;
        }
        self.bindings.spaces.retain(|b| b.space_id != space_id);
        self.spaces_state.spaces.remove(space_id);
        self.save_all()?;
        self.save_bindings()?;
        Ok(projects)
    }

    /// × on the space notice means "decide later" (UX §6.9): remember when,
    /// so Keep applies after [`DECIDE_LATER_MS`]. Only the first × counts.
    pub fn space_notice_later(&mut self, space_ids: &[String], now: i64) -> Result<()> {
        let mut changed = false;
        for id in space_ids {
            if let Some(b) = self.binding_mut(id)
                && b.decide_later_at.is_none()
                && needs_notice(b)
            {
                b.decide_later_at = Some(now);
                changed = true;
            }
        }
        if changed {
            self.save_bindings()?;
        }
        Ok(())
    }

    /// Apply the default (Keep: local-only copies, nothing deleted) to every
    /// notice left undecided for [`DECIDE_LATER_MS`]. Only while signed in,
    /// so a notice never resolves behind a signed-out user's back. Returns
    /// the resolved space ids.
    pub fn apply_overdue_notices(&mut self, now: i64) -> Vec<String> {
        if self.account.is_none() {
            return Vec::new();
        }
        let due: Vec<String> = self
            .bindings
            .spaces
            .iter()
            .filter(|b| {
                needs_notice(b)
                    && b.decide_later_at
                        .is_some_and(|t| now - t >= DECIDE_LATER_MS)
            })
            .map(|b| b.space_id.clone())
            .collect();
        due.into_iter()
            .filter(|id| self.space_notice_resolve(id, true).is_ok())
            .collect()
    }

    /// Apply an authoritative `GET /v1/me/spaces` (04 §6.6 step 1, §6.7):
    /// add bindings for new spaces of this account, refresh roles and usage,
    /// bring unavailable spaces back, and mark a bound space Removed only
    /// when the list (a 200 for the same server and user) lacks it.
    pub fn apply_my_spaces(&mut self, list: &crate::auth::MySpaces) -> bool {
        let Some(who) = self.account.clone() else {
            return false;
        };
        if who.server_instance_id.is_empty() {
            // Unknown server instance: bind nothing until discovery has it.
            return false;
        }
        let mut changed = false;
        let listed: BTreeSet<&str> = list.spaces.iter().map(|s| s.space_id.as_str()).collect();
        for s in &list.spaces {
            let kind = match s.kind.as_str() {
                "personal" => SpaceKind::Personal,
                "team" => SpaceKind::Team,
                _ => SpaceKind::Unknown,
            };
            let role = s.role.as_deref().map(Role::parse);
            match self.binding_mut(&s.space_id) {
                Some(b) if b.belongs_to(&who) => {
                    let before = b.clone();
                    b.role = role.or(b.role);
                    b.team_name = s.team_name.clone().or(b.team_name.take());
                    b.bytes_used = s.bytes_used;
                    b.quota_bytes = s.quota_bytes;
                    if matches!(
                        b.state,
                        BindingState::Unavailable { .. } | BindingState::Removed { .. }
                    ) || b.paused_foreign()
                    {
                        b.state = BindingState::Active;
                        b.decide_later_at = None;
                    }
                    if b.server_instance_id.is_empty() {
                        b.server_instance_id = who.server_instance_id.clone();
                    }
                    changed |= *b != before;
                }
                // Same id, another account: leave it paused.
                Some(_) => {}
                None => {
                    self.bindings.spaces.push(SpaceBinding {
                        space_id: s.space_id.clone(),
                        kind,
                        team_id: s.team_id.clone(),
                        team_name: s.team_name.clone(),
                        role,
                        server_origin: who.server_url.clone(),
                        server_instance_id: who.server_instance_id.clone(),
                        user_id: who.user_id.clone(),
                        user_email: who.email.clone(),
                        state: BindingState::Active,
                        bytes_used: s.bytes_used,
                        quota_bytes: s.quota_bytes,
                        decide_later_at: None,
                        extra: Extra::new(),
                    });
                    changed = true;
                }
            }
        }
        let departed = |id: &str| {
            list.departed
                .iter()
                .find(|d| d.space_id == id)
                .and_then(|d| d.reason.clone())
                .unwrap_or_else(|| "removed".into())
        };
        for b in &mut self.bindings.spaces {
            if b.belongs_to(&who)
                && !listed.contains(b.space_id.as_str())
                && !matches!(b.state, BindingState::Removed { .. })
            {
                b.state = BindingState::Removed {
                    at: now_ms(),
                    reason: departed(&b.space_id),
                };
                changed = true;
            }
        }
        // The Global follows Personal unless token sync still holds it, the
        // first-sync checklist is still unanswered (nothing uploads before
        // it, AC-ONB-5) or the user kept it on this device.
        if self.bindings.global_space.is_none()
            && !self.bindings.first_sync_pending
            && !self.bindings.global_local
            && let Some(p) = self.personal_space()
        {
            self.bindings.global_space = Some(p.space_id.clone());
            changed = true;
        }
        // A fresh device's untouched Default syncs to Personal (UX §10),
        // once: a later "Keep all on this device" is never overridden.
        if self.bindings.fresh_device
            && let Some(p) = self.personal_space().map(|b| b.space_id.clone())
        {
            let fresh: Vec<String> = self
                .projects
                .values()
                .filter(|t| {
                    is_untouched_default(t)
                        && !self.bindings.project_spaces.contains_key(&t.project.id)
                })
                .map(|t| t.project.id.clone())
                .collect();
            for id in fresh {
                self.bindings.project_spaces.insert(id, p.clone());
            }
            self.bindings.fresh_device = false;
            changed = true;
        }
        changed
    }

    /// Mark a space unavailable after a 403/404 on a space call (04 §6.7).
    pub fn mark_unavailable(&mut self, space_id: &str) {
        if let Some(b) = self.binding_mut(space_id)
            && b.is_active()
        {
            b.state = BindingState::Unavailable { since: now_ms() };
        }
    }

    // ─────────────────────────── clock skew (04 §6.9) ───────────────────────────

    /// Record the server clock of a push response.
    pub fn record_server_time(&mut self, scope: &Scope, server_time: Option<i64>, now: i64) {
        if !self.account_mode() {
            return;
        }
        if let Some(t) = server_time {
            self.space_sync_mut(scope).clock_offset_ms = Some(t - now);
        }
    }

    /// The clock is close to the server's again.
    pub fn clock_ok(&self, scope: &Scope) -> bool {
        self.space_sync(scope)
            .clock_offset_ms
            .is_some_and(|o| o.abs() < CLOCK_OK_MS)
    }

    /// Restamp entities rejected for clock skew (once the clock is fixed) to
    /// `max(now, synced_at + 1)`. Only entities whose remote version is
    /// unchanged since `synced_at` are restamped; one that the server changed
    /// meanwhile already became a conflict copy during the pull (06 §9).
    pub fn restamp_skewed(&mut self, scope: &Scope, now: i64) -> usize {
        if !self.account_mode() || !self.clock_ok(scope) {
            return 0;
        }
        let skewed = std::mem::take(&mut self.space_sync_mut(scope).skewed);
        let mut n = 0;
        let mut touched_projects = BTreeSet::new();
        let mut touched_envs = false;
        for (key, at) in skewed {
            let Some((t, id)) = key.split_once(':') else {
                continue;
            };
            let Some(t) = EntityType::parse(t) else {
                continue;
            };
            let synced = self.space_state(scope).synced.get(&key).copied();
            let stamp = now.max(synced.unwrap_or(0) + 1);
            if let Some(pid) = self.restamp_entity(t, id, at, stamp) {
                n += 1;
                match pid {
                    Some(p) => {
                        touched_projects.insert(p);
                    }
                    None => touched_envs = true,
                }
            }
        }
        for p in touched_projects {
            let _ = self.save_project(&p);
        }
        if touched_envs {
            let _ = self.save_environments();
        }
        if n > 0 {
            self.space_sync_mut(scope).issue =
                Some(SpaceIssue::new("clockFixed", clock_fixed_message(n)));
        }
        n
    }

    /// Restamp one entity whose `updatedAt` is still `at`. Returns
    /// `Some(Some(project))` for a tree entity, `Some(None)` for an
    /// environment, `None` when it changed meanwhile or is gone.
    fn restamp_entity(
        &mut self,
        t: EntityType,
        id: &str,
        at: i64,
        stamp: i64,
    ) -> Option<Option<String>> {
        if t == EntityType::Environment {
            let e = self.environments.iter_mut().find(|e| e.id == id)?;
            if e.updated_at != at {
                return None;
            }
            e.updated_at = stamp;
            return Some(None);
        }
        for (pid, tree) in self.projects.iter_mut() {
            let slot = match t {
                EntityType::Project if tree.project.id == id => Some(&mut tree.project.updated_at),
                EntityType::Collection => tree
                    .collections
                    .iter_mut()
                    .find(|c| c.id == id)
                    .map(|c| &mut c.updated_at),
                EntityType::Folder => tree
                    .folders
                    .iter_mut()
                    .find(|f| f.id == id)
                    .map(|f| &mut f.updated_at),
                EntityType::Request => tree
                    .requests
                    .iter_mut()
                    .find(|r| r.id == id)
                    .map(|r| &mut r.updated_at),
                _ => None,
            };
            if let Some(slot) = slot {
                if *slot != at {
                    return None;
                }
                *slot = stamp;
                return Some(Some(pid.clone()));
            }
        }
        None
    }
}

/// A binding that waits for a SpaceNoticeDialog answer.
fn needs_notice(b: &SpaceBinding) -> bool {
    matches!(b.state, BindingState::Removed { .. }) || b.paused_foreign()
}

/// An empty, never-renamed "Default" project.
pub fn is_untouched_default(t: &ProjectTree) -> bool {
    t.project.name == "Default" && t.collections.is_empty()
}

/// "Your computer's clock is ahead by about 7 minutes. Changes can't sync
/// until it's fixed." (UX §6.7)
pub fn clock_skew_message(ahead_ms: i64) -> String {
    let minutes = ((ahead_ms as f64) / 60_000.0).round().max(1.0) as i64;
    format!(
        "Your computer's clock is ahead by about {minutes} minute{}. Changes can't sync until it's fixed.",
        if minutes == 1 { "" } else { "s" }
    )
}

/// "Your clock was corrected; 3 changes will sync now." (04 §6.9)
pub fn clock_fixed_message(n: usize) -> String {
    format!(
        "Your clock was corrected; {n} change{} will sync now.",
        if n == 1 { "" } else { "s" }
    )
}

/// The fresh-device toast (UX §10): "Signed in · syncing 4 projects from
/// your account and 1 team."
pub fn fresh_device_message(projects: usize, teams: usize) -> String {
    let n = |k: usize, w: &str| format!("{k} {w}{}", if k == 1 { "" } else { "s" });
    let teams = if teams > 0 {
        format!(" and {}", n(teams, "team"))
    } else {
        String::new()
    };
    format!(
        "Signed in · syncing {} from your account{teams}.",
        n(projects, "project")
    )
}

pub const SERVER_FULL_MESSAGE: &str =
    "The sync server is out of space. Your changes are saved on this device.";
pub const CLIENT_TOO_OLD_MESSAGE: &str = "Update ReqHarbor to keep syncing.";
pub const LEGACY_DISABLED_MESSAGE: &str =
    "This server no longer accepts token sync. Sign in instead.";

/// "Personal storage full (25 MB)." / "Team storage full (25 MB). Ask an
/// admin to clean up." (UX §6.7)
pub fn quota_message(kind: SpaceKind, quota_bytes: Option<u64>) -> String {
    let size = quota_bytes
        .map(|q| format!(" ({} MB)", (q as f64 / 1_048_576.0).round() as u64))
        .unwrap_or_default();
    match kind {
        SpaceKind::Personal => format!("Personal storage full{size}."),
        SpaceKind::Legacy => format!("Workspace storage full{size}."),
        _ => format!("Team storage full{size}. Ask an admin to clean up."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_state_serde_is_tagged_and_forward_compatible() {
        let b: BindingState =
            serde_json::from_value(serde_json::json!({"kind": "active"})).unwrap();
        assert_eq!(b, BindingState::Active);
        let p = BindingState::Paused {
            reason: PAUSED_FOREIGN.into(),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "paused", "reason": "foreignAccount"})
        );
        let future: BindingState =
            serde_json::from_value(serde_json::json!({"kind": "archived", "x": 1})).unwrap();
        assert_eq!(future, BindingState::Unknown);
        assert_eq!(future.name(), "paused");
    }

    #[test]
    fn roles_and_kinds_from_newer_servers_are_safe() {
        assert_eq!(Role::parse("owner"), Role::Owner);
        assert_eq!(Role::parse("maintainer"), Role::Unknown);
        assert!(!Role::Unknown.can_write());
        assert!(!Role::Viewer.can_write());
        assert!(Role::Editor.can_write());
        let k: SpaceKind = serde_json::from_value(serde_json::json!("org")).unwrap();
        assert_eq!(k, SpaceKind::Unknown);
    }

    #[test]
    fn belongs_to_needs_a_known_matching_instance() {
        let who = AccountIdent {
            server_url: "https://a".into(),
            server_instance_id: "I1".into(),
            user_id: "u1".into(),
            email: "e".into(),
        };
        let b = SpaceBinding {
            space_id: "s".into(),
            kind: SpaceKind::Personal,
            team_id: None,
            team_name: None,
            role: None,
            server_origin: "https://a".into(),
            server_instance_id: "I1".into(),
            user_id: "u1".into(),
            user_email: "e".into(),
            state: BindingState::Active,
            bytes_used: None,
            quota_bytes: None,
            decide_later_at: None,
            extra: Extra::new(),
        };
        assert!(b.belongs_to(&who));
        // A binding written before its instance was known adopts it.
        let legacy_binding = SpaceBinding {
            server_instance_id: String::new(),
            ..b.clone()
        };
        assert!(legacy_binding.belongs_to(&who));
        // A rebuilt server (new instance id) is another server.
        let rebuilt = SpaceBinding {
            server_instance_id: "I2".into(),
            ..b.clone()
        };
        assert!(!rebuilt.belongs_to(&who));
        // An account whose instance is unknown owns nothing yet.
        let unknown = AccountIdent {
            server_instance_id: String::new(),
            ..who.clone()
        };
        assert!(!b.belongs_to(&unknown) && !rebuilt.belongs_to(&unknown));
        assert!(
            !SpaceBinding {
                user_id: "u2".into(),
                ..b.clone()
            }
            .belongs_to(&who)
        );
    }

    #[test]
    fn scope_paths() {
        assert_eq!(Scope::Legacy.prefix(), "/v1");
        assert_eq!(Scope::Space("s1".into()).prefix(), "/v1/spaces/s1");
        assert_eq!(Scope::from_id("legacy"), Scope::Legacy);
        assert_eq!(Scope::Space("a/b".into()).prefix(), "/v1/spaces/a%2Fb");
    }

    #[test]
    fn copy() {
        assert_eq!(
            clock_skew_message(7 * 60_000 + 10_000),
            "Your computer's clock is ahead by about 7 minutes. Changes can't sync until it's fixed."
        );
        assert_eq!(
            clock_fixed_message(3),
            "Your clock was corrected; 3 changes will sync now."
        );
        assert_eq!(
            fresh_device_message(4, 1),
            "Signed in · syncing 4 projects from your account and 1 team."
        );
        assert_eq!(
            fresh_device_message(1, 0),
            "Signed in · syncing 1 project from your account."
        );
        assert_eq!(
            quota_message(SpaceKind::Personal, Some(25 * 1_048_576)),
            "Personal storage full (25 MB)."
        );
        assert_eq!(
            quota_message(SpaceKind::Team, Some(25 * 1_048_576)),
            "Team storage full (25 MB). Ask an admin to clean up."
        );
    }
}
