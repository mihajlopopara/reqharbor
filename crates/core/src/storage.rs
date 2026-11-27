//! Local persistence (PRODUCT §3 storage layout, AC-COL-1/2/3/6, AC-ENV-6).
//!
//! Layout inside the caller-provided app data directory:
//!
//! ```text
//! workspace.json            { schemaVersion, workspace }
//! projects/<projectId>.json { schemaVersion, project, collections, folders, requests }
//! environments.json         { schemaVersion, environments }
//! settings.json             { schemaVersion, settings }
//! sync-state.json           { schemaVersion, tombstones, synced }   (sync bookkeeping)
//! history.jsonl             one HistoryEntry per line (see [`crate::history`])
//! ```
//!
//! Every write is atomic (temp file + fsync + rename), so a force quit never
//! leaves a half-written file. A file that fails to parse is renamed to
//! `<name>.corrupt-<ts>.json` and reported, never silently dropped.

use crate::history::History;
use crate::model::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Backup copy of `sync-state.json`, read when the main file is corrupt.
const SYNC_STATE_BACKUP: &str = "sync-state.backup.json";
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum StoreError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("I/O error: {0}")]
    Io(String),
    /// The request was written by a newer ReqHarbor and is read-only here
    /// (AC-FWD-3/4).
    #[error("{0}")]
    ReadOnlyNewer(String),
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A file that could not be parsed and was moved aside (AC-COL-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorruptFile {
    pub original_path: String,
    pub renamed_to: String,
    pub error: String,
}

/// Number of descendants, for delete confirmations (AC-COL-2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildCounts {
    pub collections: usize,
    pub folders: usize,
    pub requests: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Collection,
    Folder,
    Request,
}

/// Sync bookkeeping persisted in `sync-state.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "SyncStateFileRepr", into = "SyncStateFileRepr")]
pub struct SyncState {
    /// Stable ULID of this installation; LWW tie-breaker (contract C3).
    pub device_id: String,
    pub tombstones: Vec<Tombstone>,
    /// Tombstones whose `type` this build does not know (left by a newer
    /// build using the same data dir). Kept verbatim, never pushed.
    pub unknown_tombstones: Vec<serde_json::Value>,
    /// `"type:id"` -> `updatedAt` last known to be on the server.
    pub synced: BTreeMap<String, i64>,
    /// `"type:id"` -> deviceId that produced the local version, when it came
    /// from another device (absent = this device).
    pub origins: BTreeMap<String, String>,
    /// Entities not synced because they are too large (contract C2).
    pub sync_skipped: BTreeMap<String, SyncSkipped>,
    pub last_sync_at: Option<i64>,
    /// Pulled changes whose `type` this build does not know (AC-FWD-5),
    /// latest per `(type, id)`. Kept so a newer build can apply them.
    pub unknown_changes: Vec<serde_json::Value>,
    /// True once [`SyncState::unknown_changes`] hit its cap and older entries
    /// were dropped: a newer build should pull from cursor 0 to recover them.
    pub unknown_changes_truncated: bool,
    /// The server's `syncEpoch` from the last pull. A different epoch (or a
    /// pull cursor below the one sent) means the server was restored from a
    /// backup (06-go-server §9).
    pub sync_epoch: Option<String>,
    /// After a server restore: `"type:id"` -> the `updatedAt` the entity had
    /// when it was last known to be on the server. Such entities are re-sent
    /// (with their original device id) but do not count as local edits.
    pub resend: BTreeMap<String, i64>,
    /// Sync `type`s the server accepts (`GET /v1/server` → `sync.types`);
    /// `None` = the five base types (old server, the Worker, or unknown).
    pub server_types: Option<Vec<String>>,
    /// When `server_types` was last refreshed (Unix ms).
    pub server_checked_at: Option<i64>,
    pub extra: Extra,
}

/// On-disk shape of [`SyncState`]: tombstones of unknown types live in the
/// same `tombstones` array as the known ones.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncStateFileRepr {
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    tombstones: Vec<serde_json::Value>,
    #[serde(default)]
    synced: BTreeMap<String, i64>,
    #[serde(default)]
    origins: BTreeMap<String, String>,
    #[serde(default)]
    sync_skipped: BTreeMap<String, SyncSkipped>,
    #[serde(default)]
    last_sync_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unknown_changes: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unknown_changes_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sync_epoch: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    resend: BTreeMap<String, i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_checked_at: Option<i64>,
    #[serde(flatten)]
    extra: Extra,
}

impl From<SyncStateFileRepr> for SyncState {
    fn from(r: SyncStateFileRepr) -> Self {
        let mut tombstones = Vec::new();
        let mut unknown_tombstones = Vec::new();
        for v in r.tombstones {
            match serde_json::from_value::<Tombstone>(v.clone()) {
                Ok(t) => tombstones.push(t),
                Err(_) => unknown_tombstones.push(v),
            }
        }
        SyncState {
            device_id: r.device_id,
            tombstones,
            unknown_tombstones,
            synced: r.synced,
            origins: r.origins,
            sync_skipped: r.sync_skipped,
            last_sync_at: r.last_sync_at,
            unknown_changes: r.unknown_changes,
            unknown_changes_truncated: r.unknown_changes_truncated,
            sync_epoch: r.sync_epoch,
            resend: r.resend,
            server_types: r.server_types,
            server_checked_at: r.server_checked_at,
            extra: r.extra,
        }
    }
}

impl From<SyncState> for SyncStateFileRepr {
    fn from(s: SyncState) -> Self {
        let mut tombstones: Vec<serde_json::Value> = s
            .tombstones
            .iter()
            .filter_map(|t| serde_json::to_value(t).ok())
            .collect();
        tombstones.extend(s.unknown_tombstones);
        SyncStateFileRepr {
            device_id: s.device_id,
            tombstones,
            synced: s.synced,
            origins: s.origins,
            sync_skipped: s.sync_skipped,
            last_sync_at: s.last_sync_at,
            unknown_changes: s.unknown_changes,
            unknown_changes_truncated: s.unknown_changes_truncated,
            sync_epoch: s.sync_epoch,
            resend: s.resend,
            server_types: s.server_types,
            server_checked_at: s.server_checked_at,
            extra: s.extra,
        }
    }
}

/// An entity excluded from sync until it changes again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSkipped {
    /// `updatedAt` of the version that was skipped; a newer edit retries.
    pub updated_at: i64,
    /// `"too_large"`, or the reason the server rejected the change with.
    pub reason: String,
}

impl SyncState {
    /// Forget everything about a remote, keeping this device's id.
    pub fn reset_for_new_remote(&mut self) {
        let device_id = std::mem::take(&mut self.device_id);
        *self = SyncState {
            device_id,
            ..SyncState::default()
        };
    }
}

pub fn sync_key(t: EntityType, id: &str) -> String {
    format!("{}:{id}", t.as_str())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceFile {
    schema_version: u32,
    workspace: Workspace,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectFile {
    schema_version: u32,
    #[serde(flatten)]
    tree: ProjectTree,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentsFile {
    schema_version: u32,
    environments: Vec<Environment>,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsFile {
    schema_version: u32,
    settings: Settings,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncStateFile {
    schema_version: u32,
    #[serde(flatten)]
    state: SyncState,
}

/// Write `bytes` to `path` atomically: temp file in the same dir, fsync, rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| StoreError::Io("path has no parent".into()))?;
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{file_name}.tmp-{}", new_id()));
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(Into::into)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| StoreError::Io(e.to_string()))?;
    atomic_write(path, &bytes)
}

/// Rename an unreadable file to `<stem>.corrupt-<ts>.json`.
fn quarantine(path: &Path, error: String, report: &mut Vec<CorruptFile>) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let target = path.with_file_name(format!("{stem}.corrupt-{}.json", now_ms()));
    let renamed = std::fs::rename(path, &target).is_ok();
    report.push(CorruptFile {
        original_path: path.to_string_lossy().into_owned(),
        renamed_to: if renamed {
            target.to_string_lossy().into_owned()
        } else {
            String::new()
        },
        error,
    });
}

/// Read and parse a JSON file. `Ok(None)` when missing; corrupt files are
/// quarantined and also yield `Ok(None)`.
fn read_json<T: DeserializeOwned>(path: &Path, report: &mut Vec<CorruptFile>) -> Result<Option<T>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            quarantine(path, e.to_string(), report);
            Ok(None)
        }
    }
}

/// The local store. Mutations persist immediately (atomically).
pub struct Store {
    dir: PathBuf,
    pub workspace: Workspace,
    /// Projects keyed by id.
    pub projects: BTreeMap<String, ProjectTree>,
    pub environments: Vec<Environment>,
    pub settings: Settings,
    pub sync_state: SyncState,
    pub history: History,
    /// Files quarantined while opening (AC-COL-6).
    pub corrupt: Vec<CorruptFile>,
    /// Projects removed in this session whose files `save_all` deletes.
    /// Other files in `projects/` are never touched (QA-R1-021).
    pub removed_projects: BTreeSet<String>,
    /// Unknown top-level keys of `workspace.json`, `environments.json` and
    /// `settings.json`, written back unchanged (AC-FWD-1).
    pub file_extras: FileExtras,
}

/// Unknown file-level keys, per file (AC-FWD-1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileExtras {
    pub workspace: Extra,
    pub environments: Extra,
    pub settings: Extra,
}

impl Store {
    /// Open (or initialise) the store in `dir`. First launch creates
    /// workspace "My Workspace" with project "Default" and a Global
    /// environment (AC-COL-1).
    pub fn open(dir: impl Into<PathBuf>) -> Result<Store> {
        let dir = dir.into();
        std::fs::create_dir_all(dir.join("projects"))?;
        let mut corrupt = Vec::new();

        let ws_file: Option<WorkspaceFile> = read_json(&dir.join("workspace.json"), &mut corrupt)?;
        let fresh_workspace = ws_file.is_none();
        let mut file_extras = FileExtras::default();
        let workspace = match ws_file {
            Some(f) => {
                file_extras.workspace = f.extra;
                f.workspace
            }
            None => {
                let now = now_ms();
                Workspace {
                    id: new_id(),
                    name: "My Workspace".into(),
                    created_at: now,
                    updated_at: now,
                    sync: None,
                    extra: Extra::new(),
                }
            }
        };

        let mut projects = BTreeMap::new();
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir.join("projects"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                name.ends_with(".json") && !name.contains(".corrupt-") && !name.starts_with('.')
            })
            .collect();
        entries.sort();
        // `<id>.json` files win over other files carrying the same id
        // (e.g. a user's backup copy), which are otherwise left alone.
        let mut extra = Vec::new();
        for path in entries {
            if let Some(file) = read_json::<ProjectFile>(&path, &mut corrupt)? {
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                if stem.as_deref() == Some(file.tree.project.id.as_str()) {
                    projects.insert(file.tree.project.id.clone(), file.tree);
                } else {
                    extra.push(file.tree);
                }
            }
        }
        for tree in extra {
            projects.entry(tree.project.id.clone()).or_insert(tree);
        }

        let environments =
            read_json::<EnvironmentsFile>(&dir.join("environments.json"), &mut corrupt)?
                .map(|f| {
                    file_extras.environments = f.extra;
                    f.environments
                })
                .unwrap_or_default();
        let settings = read_json::<SettingsFile>(&dir.join("settings.json"), &mut corrupt)?
            .map(|f| {
                file_extras.settings = f.extra;
                f.settings
            })
            .unwrap_or_default();
        // A corrupt sync-state falls back to its backup so pending tombstones
        // are not lost (QA-R1-028).
        let before = corrupt.len();
        let mut sync_state =
            read_json::<SyncStateFile>(&dir.join("sync-state.json"), &mut corrupt)?
                .map(|f| f.state);
        if sync_state.is_none() && corrupt.len() > before {
            sync_state = read_json::<SyncStateFile>(&dir.join(SYNC_STATE_BACKUP), &mut corrupt)?
                .map(|f| f.state);
        }
        let mut sync_state = sync_state.unwrap_or_default();
        let new_device = sync_state.device_id.is_empty();
        if new_device {
            sync_state.device_id = new_id();
        }
        let history = History::open(dir.join("history.jsonl"))?;

        let mut store = Store {
            dir,
            workspace,
            projects,
            environments,
            settings,
            sync_state,
            history,
            corrupt,
            removed_projects: BTreeSet::new(),
            file_extras,
        };
        if fresh_workspace {
            store.save_workspace()?;
        }
        if new_device {
            store.save_sync_state()?;
        }
        if store.projects.is_empty() && fresh_workspace {
            store.create_project("Default")?;
        }
        if !store.environments.iter().any(|e| e.project_id.is_none()) {
            let now = now_ms();
            store.environments.insert(
                0,
                Environment {
                    id: new_id(),
                    workspace_id: store.workspace.id.clone(),
                    project_id: None,
                    name: "Global".into(),
                    variables: Vec::new(),
                    sort_order: 0,
                    created_at: now,
                    updated_at: now,
                    extra: Extra::new(),
                },
            );
            store.save_environments()?;
        }
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Drop a project from memory; its file is removed by the next
    /// [`Store::save_all`]. Used by sync when a remote delete arrives.
    pub fn forget_project(&mut self, id: &str) -> Option<ProjectTree> {
        let tree = self.projects.remove(id)?;
        self.removed_projects.insert(id.to_string());
        Some(tree)
    }

    // ------------------------------------------------------------ persistence

    pub fn save_workspace(&self) -> Result<()> {
        write_json(
            &self.dir.join("workspace.json"),
            &WorkspaceFile {
                schema_version: SCHEMA_VERSION,
                workspace: self.workspace.clone(),
                extra: self.file_extras.workspace.clone(),
            },
        )
    }

    fn project_path(&self, id: &str) -> PathBuf {
        self.dir.join("projects").join(format!("{id}.json"))
    }

    pub fn save_project(&self, id: &str) -> Result<()> {
        let tree = self.project(id)?;
        write_json(
            &self.project_path(id),
            &ProjectFile {
                schema_version: SCHEMA_VERSION,
                tree: tree.clone(),
            },
        )
    }

    pub fn save_environments(&self) -> Result<()> {
        write_json(
            &self.dir.join("environments.json"),
            &EnvironmentsFile {
                schema_version: SCHEMA_VERSION,
                environments: self.environments.clone(),
                extra: self.file_extras.environments.clone(),
            },
        )
    }

    pub fn save_settings(&self) -> Result<()> {
        write_json(
            &self.dir.join("settings.json"),
            &SettingsFile {
                schema_version: SCHEMA_VERSION,
                settings: self.settings.clone(),
                extra: self.file_extras.settings.clone(),
            },
        )
    }

    pub fn save_sync_state(&self) -> Result<()> {
        let file = SyncStateFile {
            schema_version: SCHEMA_VERSION,
            state: self.sync_state.clone(),
        };
        write_json(&self.dir.join("sync-state.json"), &file)?;
        write_json(&self.dir.join(SYNC_STATE_BACKUP), &file)
    }

    /// Persist everything (used after sync pulls).
    pub fn save_all(&self) -> Result<()> {
        self.save_workspace()?;
        for id in self.projects.keys() {
            self.save_project(id)?;
        }
        // Remove only the files of projects removed in this session.
        for id in &self.removed_projects {
            if !self.projects.contains_key(id) {
                let _ = std::fs::remove_file(self.project_path(id));
            }
        }
        self.save_environments()?;
        self.save_sync_state()
    }

    // ---------------------------------------------------------------- lookups

    /// Projects ordered by `sortOrder` then name.
    pub fn project_list(&self) -> Vec<Project> {
        let mut list: Vec<Project> = self.projects.values().map(|t| t.project.clone()).collect();
        list.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.name.cmp(&b.name)));
        list
    }

    pub fn project(&self, id: &str) -> Result<&ProjectTree> {
        self.projects
            .get(id)
            .ok_or_else(|| StoreError::NotFound(format!("Project {id}")))
    }

    fn project_mut(&mut self, id: &str) -> Result<&mut ProjectTree> {
        self.projects
            .get_mut(id)
            .ok_or_else(|| StoreError::NotFound(format!("Project {id}")))
    }

    /// Project id owning a collection / folder / request.
    pub fn project_of(&self, kind: NodeKind, id: &str) -> Result<String> {
        for (pid, t) in &self.projects {
            let found = match kind {
                NodeKind::Collection => t.collections.iter().any(|c| c.id == id),
                NodeKind::Folder => t.folders.iter().any(|f| f.id == id),
                NodeKind::Request => t.requests.iter().any(|r| r.id == id),
            };
            if found {
                return Ok(pid.clone());
            }
        }
        Err(StoreError::NotFound(format!("{kind:?} {id}")))
    }

    pub fn collection(&self, id: &str) -> Option<&Collection> {
        self.projects
            .values()
            .flat_map(|t| t.collections.iter())
            .find(|c| c.id == id)
    }

    pub fn request(&self, id: &str) -> Option<&Request> {
        self.projects
            .values()
            .flat_map(|t| t.requests.iter())
            .find(|r| r.id == id)
    }

    /// Descendant counts of a project / collection / folder.
    pub fn child_counts(&self, kind: &str, id: &str) -> Result<ChildCounts> {
        match kind {
            "project" => {
                let t = self.project(id)?;
                Ok(ChildCounts {
                    collections: t.collections.len(),
                    folders: t.folders.len(),
                    requests: t.requests.len(),
                })
            }
            "collection" => {
                let t = self.project(&self.project_of(NodeKind::Collection, id)?)?;
                Ok(ChildCounts {
                    collections: 0,
                    folders: t.folders.iter().filter(|f| f.collection_id == id).count(),
                    requests: t.requests.iter().filter(|r| r.collection_id == id).count(),
                })
            }
            "folder" => {
                let t = self.project(&self.project_of(NodeKind::Folder, id)?)?;
                let folders = descendant_folders(t, id);
                Ok(ChildCounts {
                    collections: 0,
                    folders: folders.len() - 1,
                    requests: t
                        .requests
                        .iter()
                        .filter(|r| r.folder_id.as_ref().is_some_and(|f| folders.contains(f)))
                        .count(),
                })
            }
            "request" => Ok(ChildCounts::default()),
            other => Err(StoreError::Invalid(format!("Unknown kind {other}"))),
        }
    }

    // ---------------------------------------------------------- tombstones

    fn tombstone(&mut self, entity_type: EntityType, id: &str) {
        // Only entities the server knows about need a tombstone.
        if self
            .sync_state
            .synced
            .contains_key(&sync_key(entity_type, id))
        {
            self.sync_state.tombstones.retain(|t| t.id != id);
            self.sync_state.tombstones.push(Tombstone {
                id: id.to_string(),
                entity_type,
                deleted_at: now_ms(),
                extra: Extra::new(),
            });
        }
    }

    fn persist_tombstones(&self) -> Result<()> {
        self.save_sync_state()
    }

    // ---------------------------------------------------------------- projects

    pub fn create_project(&mut self, name: &str) -> Result<ProjectTree> {
        let name = non_empty(name)?;
        let now = now_ms();
        let sort_order = self
            .projects
            .values()
            .map(|t| t.project.sort_order)
            .max()
            .unwrap_or(-1)
            + 1;
        let project = Project {
            id: new_id(),
            workspace_id: self.workspace.id.clone(),
            name,
            sort_order,
            active_environment_id: None,
            created_at: now,
            updated_at: now,
            extra: Extra::new(),
        };
        let tree = ProjectTree::new(project);
        let id = tree.project.id.clone();
        self.projects.insert(id.clone(), tree.clone());
        self.save_project(&id)?;
        Ok(tree)
    }

    pub fn rename_project(&mut self, id: &str, name: &str) -> Result<Project> {
        let name = non_empty(name)?;
        let t = self.project_mut(id)?;
        ensure_entity_editable(t.project.is_editable(), &t.project.name, "This project")?;
        t.project.name = name;
        t.project.updated_at = touch(t.project.updated_at);
        let p = t.project.clone();
        self.save_project(id)?;
        Ok(p)
    }

    pub fn set_active_environment(
        &mut self,
        project_id: &str,
        env_id: Option<&str>,
    ) -> Result<Project> {
        if let Some(eid) = env_id
            && !self
                .environments
                .iter()
                .any(|e| e.id == eid && e.project_id.as_deref() == Some(project_id))
        {
            return Err(StoreError::NotFound(format!(
                "Environment {eid} in project {project_id}"
            )));
        }
        let t = self.project_mut(project_id)?;
        if t.project.active_environment_id.as_deref() == env_id {
            return Ok(t.project.clone());
        }
        ensure_entity_editable(t.project.is_editable(), &t.project.name, "This project")?;
        t.project.active_environment_id = env_id.map(str::to_string);
        t.project.updated_at = touch(t.project.updated_at);
        let p = t.project.clone();
        self.save_project(project_id)?;
        Ok(p)
    }

    pub fn duplicate_project(&mut self, id: &str) -> Result<ProjectTree> {
        let src = self.project(id)?.clone();
        if !src.project.is_editable() {
            return Err(read_only_newer_named(
                &quoted(&src.project.name, "a project"),
                "copy",
            ));
        }
        first_read_only(&src, |_| true, |_| true, |_| true, "copy")?;
        if let Some(e) = self
            .environments
            .iter()
            .find(|e| e.project_id.as_deref() == Some(id) && !e.is_editable())
        {
            return Err(read_only_newer_named(
                &quoted(&e.name, "an environment"),
                "copy",
            ));
        }
        let now = now_ms();
        let new_pid = new_id();
        let sort_order = self
            .projects
            .values()
            .map(|t| t.project.sort_order)
            .max()
            .unwrap_or(0)
            + 1;
        let mut tree = ProjectTree::new(Project {
            id: new_pid.clone(),
            name: format!("{} copy", src.project.name),
            sort_order,
            active_environment_id: None,
            created_at: now,
            updated_at: now,
            ..src.project.clone()
        });
        tree.extra = src.extra.clone();
        for c in &src.collections {
            let (cols, folders, reqs) = copy_collection(&src, c, &new_pid, now, None);
            tree.collections.extend(cols);
            tree.folders.extend(folders);
            tree.requests.extend(reqs);
        }
        // Copy the project's environments too, keeping the active one active
        // (QA-R2-005).
        let envs: Vec<Environment> = self
            .environments
            .iter()
            .filter(|e| e.project_id.as_deref() == Some(id))
            .cloned()
            .collect();
        for mut e in envs {
            let old_id = std::mem::replace(&mut e.id, new_id());
            if src.project.active_environment_id.as_deref() == Some(old_id.as_str()) {
                tree.project.active_environment_id = Some(e.id.clone());
            }
            e.project_id = Some(new_pid.clone());
            e.created_at = now;
            e.updated_at = now;
            self.environments.push(e);
        }
        self.projects.insert(new_pid.clone(), tree.clone());
        self.save_project(&new_pid)?;
        self.save_environments()?;
        Ok(tree)
    }

    pub fn delete_project(&mut self, id: &str) -> Result<()> {
        if self.projects.len() <= 1 {
            return Err(StoreError::Invalid(
                "The last project cannot be deleted".into(),
            ));
        }
        first_read_only(self.project(id)?, |_| true, |_| true, |_| true, "delete")?;
        if let Some(e) = self
            .environments
            .iter()
            .find(|e| e.project_id.as_deref() == Some(id) && !e.is_editable())
        {
            return Err(read_only_newer_named(
                &quoted(&e.name, "an environment"),
                "delete",
            ));
        }
        let tree = self
            .projects
            .remove(id)
            .ok_or_else(|| StoreError::NotFound(format!("Project {id}")))?;
        for r in &tree.requests {
            self.tombstone(EntityType::Request, &r.id);
        }
        for f in &tree.folders {
            self.tombstone(EntityType::Folder, &f.id);
        }
        for c in &tree.collections {
            self.tombstone(EntityType::Collection, &c.id);
        }
        self.tombstone(EntityType::Project, id);
        let env_ids: Vec<String> = self
            .environments
            .iter()
            .filter(|e| e.project_id.as_deref() == Some(id))
            .map(|e| e.id.clone())
            .collect();
        for eid in &env_ids {
            self.tombstone(EntityType::Environment, eid);
        }
        self.environments
            .retain(|e| e.project_id.as_deref() != Some(id));
        let _ = std::fs::remove_file(self.project_path(id));
        self.save_environments()?;
        self.persist_tombstones()
    }

    /// Move a project to `index` in the project list.
    pub fn move_project(&mut self, id: &str, index: usize) -> Result<Vec<Project>> {
        let mut list = self.project_list();
        let pos = list
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("Project {id}")))?;
        let p = list.remove(pos);
        ensure_entity_editable(p.is_editable(), &p.name, "This project")?;
        list.insert(index.min(list.len()), p);
        let now = now_ms();
        // Read-only projects (AC-FWD-4) keep their sort order; the others
        // are numbered around them.
        let order: Vec<String> = list.iter().map(|p| p.id.clone()).collect();
        let current: HashMap<String, i64> =
            list.iter().map(|p| (p.id.clone(), p.sort_order)).collect();
        let fixed: HashSet<String> = list
            .iter()
            .filter(|p| !p.is_editable())
            .map(|p| p.id.clone())
            .collect();
        let Some(numbers) = renumber_around(&order, &current, &fixed) else {
            let p = list
                .iter()
                .find(|p| !p.is_editable())
                .expect("a fixed project");
            return Err(read_only_newer_named(&quoted(&p.name, "a project"), "move"));
        };
        for (pid, n) in numbers {
            let t = self.project_mut(&pid)?;
            if t.project.sort_order != n {
                t.project.sort_order = n;
                t.project.updated_at = now;
                self.save_project(&pid)?;
            }
        }
        Ok(self.project_list())
    }

    // ------------------------------------------------------------ collections

    pub fn create_collection(&mut self, project_id: &str, name: &str) -> Result<Collection> {
        let name = non_empty(name)?;
        let now = now_ms();
        let t = self.project_mut(project_id)?;
        let sort_order = t
            .collections
            .iter()
            .map(|c| c.sort_order)
            .max()
            .unwrap_or(-1)
            + 1;
        let c = Collection {
            id: new_id(),
            project_id: project_id.to_string(),
            name,
            description: String::new(),
            auth: Auth::none(),
            sort_order,
            created_at: now,
            updated_at: now,
            extra: Extra::new(),
        };
        t.collections.push(c.clone());
        self.save_project(project_id)?;
        Ok(c)
    }

    /// Update name, description and auth (AC-COL-5). Position fields are kept.
    pub fn update_collection(&mut self, update: &Collection) -> Result<Collection> {
        let name = non_empty(&update.name)?;
        let pid = self.project_of(NodeKind::Collection, &update.id)?;
        let t = self.project_mut(&pid)?;
        let c = t
            .collections
            .iter_mut()
            .find(|c| c.id == update.id)
            .ok_or_else(|| StoreError::NotFound(format!("Collection {}", update.id)))?;
        ensure_entity_editable(c.is_editable(), &c.name, "This collection")?;
        let mut auth = update.auth.clone();
        merge_auth_extra(&mut auth, &c.auth);
        // Auth from a newer ReqHarbor (an unknown scheme) can't be edited
        // here, and this build never writes such auth itself (AC-FWD-3).
        if auth != c.auth && !(auth.is_known() && c.auth.is_known()) {
            return Err(StoreError::ReadOnlyNewer(format!(
                "The auth of \u{201c}{}\u{201d} was set up with a newer version of ReqHarbor and is read-only here. Update ReqHarbor to edit it.",
                c.name
            )));
        }
        c.name = name;
        c.description = update.description.clone();
        c.auth = auth;
        merge_extra(&mut c.extra, &update.extra);
        c.updated_at = touch(c.updated_at);
        let out = c.clone();
        self.save_project(&pid)?;
        Ok(out)
    }

    pub fn duplicate_collection(&mut self, id: &str) -> Result<ProjectTree> {
        let pid = self.project_of(NodeKind::Collection, id)?;
        let src = self.project(&pid)?.clone();
        let c = src
            .collections
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        first_read_only(
            &src,
            |x| x.id == id,
            |f| f.collection_id == id,
            |r| r.collection_id == id,
            "copy",
        )?;
        let now = now_ms();
        let (mut cols, folders, reqs) =
            copy_collection(&src, &c, &pid, now, Some(format!("{} copy", c.name)));
        let max = src
            .collections
            .iter()
            .map(|c| c.sort_order)
            .max()
            .unwrap_or(0);
        cols[0].sort_order = max + 1;
        let t = self.project_mut(&pid)?;
        t.collections.extend(cols);
        t.folders.extend(folders);
        t.requests.extend(reqs);
        self.save_project(&pid)?;
        Ok(self.project(&pid)?.clone())
    }

    pub fn delete_collection(&mut self, id: &str) -> Result<ProjectTree> {
        let pid = self.project_of(NodeKind::Collection, id)?;
        first_read_only(
            self.project(&pid)?,
            |_| false,
            |f| f.collection_id == id,
            |r| r.collection_id == id,
            "delete",
        )?;
        let t = self.project_mut(&pid)?;
        let reqs: Vec<String> = t
            .requests
            .iter()
            .filter(|r| r.collection_id == id)
            .map(|r| r.id.clone())
            .collect();
        let folders: Vec<String> = t
            .folders
            .iter()
            .filter(|f| f.collection_id == id)
            .map(|f| f.id.clone())
            .collect();
        t.requests.retain(|r| r.collection_id != id);
        t.folders.retain(|f| f.collection_id != id);
        t.collections.retain(|c| c.id != id);
        for r in &reqs {
            self.tombstone(EntityType::Request, r);
        }
        for f in &folders {
            self.tombstone(EntityType::Folder, f);
        }
        self.tombstone(EntityType::Collection, id);
        self.save_project(&pid)?;
        self.persist_tombstones()?;
        Ok(self.project(&pid)?.clone())
    }

    // ---------------------------------------------------------------- folders

    pub fn create_folder(
        &mut self,
        collection_id: &str,
        parent_folder_id: Option<&str>,
        name: &str,
    ) -> Result<Folder> {
        let name = non_empty(name)?;
        let pid = self.project_of(NodeKind::Collection, collection_id)?;
        let t = self.project_mut(&pid)?;
        if let Some(parent) = parent_folder_id
            && !t
                .folders
                .iter()
                .any(|f| f.id == parent && f.collection_id == collection_id)
        {
            return Err(StoreError::NotFound(format!("Folder {parent}")));
        }
        let now = now_ms();
        let sort_order = next_sort(t, collection_id, parent_folder_id);
        let f = Folder {
            id: new_id(),
            collection_id: collection_id.to_string(),
            parent_folder_id: parent_folder_id.map(str::to_string),
            name,
            sort_order,
            created_at: now,
            updated_at: now,
            extra: Extra::new(),
        };
        t.folders.push(f.clone());
        self.save_project(&pid)?;
        Ok(f)
    }

    pub fn rename_folder(&mut self, id: &str, name: &str) -> Result<Folder> {
        let name = non_empty(name)?;
        let pid = self.project_of(NodeKind::Folder, id)?;
        let t = self.project_mut(&pid)?;
        let f = t
            .folders
            .iter_mut()
            .find(|f| f.id == id)
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        ensure_entity_editable(f.is_editable(), &f.name, "This folder")?;
        f.name = name;
        f.updated_at = touch(f.updated_at);
        let out = f.clone();
        self.save_project(&pid)?;
        Ok(out)
    }

    pub fn duplicate_folder(&mut self, id: &str) -> Result<ProjectTree> {
        let pid = self.project_of(NodeKind::Folder, id)?;
        let src = self.project(&pid)?.clone();
        let folder = src
            .folders
            .iter()
            .find(|f| f.id == id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        let now = now_ms();
        let mut id_map = std::collections::HashMap::new();
        let ids = descendant_folders(&src, id);
        first_read_only(
            &src,
            |_| false,
            |f| ids.contains(&f.id),
            |r| r.folder_id.as_ref().is_some_and(|f| ids.contains(f)),
            "copy",
        )?;
        for fid in &ids {
            id_map.insert(fid.clone(), new_id());
        }
        let mut folders = Vec::new();
        for f in src.folders.iter().filter(|f| ids.contains(&f.id)) {
            let mut nf = f.clone();
            nf.id = id_map[&f.id].clone();
            nf.parent_folder_id = f
                .parent_folder_id
                .as_ref()
                .map(|p| id_map.get(p).cloned().unwrap_or_else(|| p.clone()));
            nf.created_at = now;
            nf.updated_at = now;
            if f.id == id {
                nf.name = format!("{} copy", folder.name);
                nf.sort_order = next_sort(
                    &src,
                    &folder.collection_id,
                    folder.parent_folder_id.as_deref(),
                );
            }
            folders.push(nf);
        }
        let reqs: Vec<Request> = src
            .requests
            .iter()
            .filter(|r| r.folder_id.as_ref().is_some_and(|f| ids.contains(f)))
            .map(|r| {
                let mut nr = r.clone();
                nr.id = new_id();
                nr.folder_id = r.folder_id.as_ref().and_then(|f| id_map.get(f).cloned());
                nr.created_at = now;
                nr.updated_at = now;
                nr
            })
            .collect();
        let t = self.project_mut(&pid)?;
        t.folders.extend(folders);
        t.requests.extend(reqs);
        self.save_project(&pid)?;
        Ok(self.project(&pid)?.clone())
    }

    pub fn delete_folder(&mut self, id: &str) -> Result<ProjectTree> {
        let pid = self.project_of(NodeKind::Folder, id)?;
        let ids = descendant_folders(self.project(&pid)?, id);
        first_read_only(
            self.project(&pid)?,
            |_| false,
            |f| f.id != id && ids.contains(&f.id),
            |r| r.folder_id.as_ref().is_some_and(|f| ids.contains(f)),
            "delete",
        )?;
        let t = self.project_mut(&pid)?;
        let reqs: Vec<String> = t
            .requests
            .iter()
            .filter(|r| r.folder_id.as_ref().is_some_and(|f| ids.contains(f)))
            .map(|r| r.id.clone())
            .collect();
        t.requests.retain(|r| !reqs.contains(&r.id));
        t.folders.retain(|f| !ids.contains(&f.id));
        for r in &reqs {
            self.tombstone(EntityType::Request, r);
        }
        for f in &ids {
            self.tombstone(EntityType::Folder, f);
        }
        self.save_project(&pid)?;
        self.persist_tombstones()?;
        Ok(self.project(&pid)?.clone())
    }

    // --------------------------------------------------------------- requests

    /// Save a (scratch or history) request into a collection/folder under a
    /// new id (AC-REQ-11, AC-HIST-4).
    pub fn create_request(
        &mut self,
        collection_id: &str,
        folder_id: Option<&str>,
        template: Request,
    ) -> Result<Request> {
        ensure_editable(&template)?;
        if !is_valid_method(&template.method) {
            return Err(StoreError::Invalid(format!(
                "Invalid method {}",
                template.method
            )));
        }
        let pid = self.project_of(NodeKind::Collection, collection_id)?;
        let t = self.project_mut(&pid)?;
        if let Some(fid) = folder_id
            && !t
                .folders
                .iter()
                .any(|f| f.id == fid && f.collection_id == collection_id)
        {
            return Err(StoreError::NotFound(format!("Folder {fid}")));
        }
        let now = now_ms();
        let mut r = template;
        r.id = new_id();
        r.collection_id = collection_id.to_string();
        r.folder_id = folder_id.map(str::to_string);
        if r.name.trim().is_empty() {
            r.name = "Untitled request".into();
        }
        r.sort_order = next_sort(t, collection_id, folder_id);
        r.created_at = now;
        r.updated_at = now;
        t.requests.push(r.clone());
        self.save_project(&pid)?;
        Ok(r)
    }

    /// Persist edits to an existing request (⌘S). Placement fields
    /// (collection, folder, sort order) are kept; use [`Store::move_node`].
    ///
    /// Unknown keys (`extra`, at every level) that the stored version has and
    /// the update lacks are kept: the UI may rebuild objects without them.
    /// Requests from a newer ReqHarbor are refused (`ReadOnlyNewer`).
    pub fn save_request(&mut self, update: &Request) -> Result<Request> {
        let pid = self.project_of(NodeKind::Request, &update.id)?;
        if let Some(stored) = self.request(&update.id) {
            ensure_editable(stored)?;
        }
        ensure_editable(update)?;
        if !is_valid_method(&update.method) {
            return Err(StoreError::Invalid(format!(
                "Invalid method {}",
                update.method
            )));
        }
        let t = self.project_mut(&pid)?;
        let r = t
            .requests
            .iter_mut()
            .find(|r| r.id == update.id)
            .ok_or_else(|| StoreError::NotFound(update.id.clone()))?;
        let keep = (
            r.collection_id.clone(),
            r.folder_id.clone(),
            r.sort_order,
            r.created_at,
        );
        let mut next = update.clone();
        merge_request_extra(&mut next, r);
        *r = next;
        (r.collection_id, r.folder_id, r.sort_order, r.created_at) = keep;
        r.updated_at = touch(r.updated_at);
        let out = r.clone();
        self.save_project(&pid)?;
        Ok(out)
    }

    pub fn rename_request(&mut self, id: &str, name: &str) -> Result<Request> {
        let name = non_empty(name)?;
        let pid = self.project_of(NodeKind::Request, id)?;
        self.ensure_request_editable(id)?;
        let t = self.project_mut(&pid)?;
        let r = t
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        r.name = name;
        r.updated_at = touch(r.updated_at);
        let out = r.clone();
        self.save_project(&pid)?;
        Ok(out)
    }

    pub fn duplicate_request(&mut self, id: &str) -> Result<Request> {
        let pid = self.project_of(NodeKind::Request, id)?;
        self.ensure_request_editable(id)?;
        let t = self.project_mut(&pid)?;
        let src = t
            .requests
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        let now = now_ms();
        let mut r = src.clone();
        r.id = new_id();
        r.name = format!("{} copy", src.name);
        r.sort_order = next_sort(t, &src.collection_id, src.folder_id.as_deref());
        r.created_at = now;
        r.updated_at = now;
        t.requests.push(r.clone());
        self.save_project(&pid)?;
        Ok(r)
    }

    pub fn delete_request(&mut self, id: &str) -> Result<ProjectTree> {
        let pid = self.project_of(NodeKind::Request, id)?;
        let t = self.project_mut(&pid)?;
        t.requests.retain(|r| r.id != id);
        self.tombstone(EntityType::Request, id);
        self.save_project(&pid)?;
        self.persist_tombstones()?;
        Ok(self.project(&pid)?.clone())
    }

    /// Drag-and-drop move/reorder (AC-COL-3). Moves a folder or request to
    /// `(collection, folder)` at `index` among its new siblings (folders and
    /// requests are ordered separately), or a collection to `index` within
    /// its project. Moves across projects are rejected.
    pub fn move_node(
        &mut self,
        kind: NodeKind,
        id: &str,
        target_collection_id: Option<&str>,
        target_folder_id: Option<&str>,
        index: usize,
    ) -> Result<ProjectTree> {
        let pid = self.project_of(kind, id)?;
        if kind == NodeKind::Request {
            self.ensure_request_editable(id)?;
        }
        let now = now_ms();
        // Work on a copy so a move that would rewrite a read-only request
        // (AC-FWD-3) changes nothing.
        let frozen = ReadOnlyEntities::of(self.project(&pid)?);
        let before = &frozen.requests;
        let mut tree = self.project(&pid)?.clone();
        let t = &mut tree;
        match kind {
            NodeKind::Collection => {
                if let Some(c) = t.collections.iter().find(|c| c.id == id) {
                    ensure_entity_editable(c.is_editable(), &c.name, "This collection")?;
                }
                let mut order: Vec<String> =
                    sorted_ids(t.collections.iter().map(|c| (c.sort_order, c.id.clone())));
                order.retain(|c| c != id);
                order.insert(index.min(order.len()), id.to_string());
                // Read-only collections (AC-FWD-4) keep their sort order.
                let current: HashMap<String, i64> = t
                    .collections
                    .iter()
                    .map(|c| (c.id.clone(), c.sort_order))
                    .collect();
                let fixed: HashSet<String> = frozen.collections.keys().cloned().collect();
                let Some(numbers) = renumber_around(&order, &current, &fixed) else {
                    let c = frozen
                        .collections
                        .values()
                        .next()
                        .expect("a fixed collection");
                    return Err(read_only_newer_named(
                        &quoted(&c.name, "a collection"),
                        "move",
                    ));
                };
                for (cid, n) in numbers {
                    if let Some(c) = t.collections.iter_mut().find(|c| c.id == cid)
                        && c.sort_order != n
                    {
                        c.sort_order = n;
                        c.updated_at = now;
                    }
                }
            }
            NodeKind::Folder | NodeKind::Request => {
                let coll = target_collection_id
                    .ok_or_else(|| StoreError::Invalid("targetCollectionId is required".into()))?;
                if !t.collections.iter().any(|c| c.id == coll) {
                    return Err(StoreError::Invalid(
                        "Items can only move within the same project".into(),
                    ));
                }
                if let Some(f) = target_folder_id {
                    if !t
                        .folders
                        .iter()
                        .any(|x| x.id == f && x.collection_id == coll)
                    {
                        return Err(StoreError::NotFound(format!("Folder {f}")));
                    }
                    if kind == NodeKind::Folder && descendant_folders(t, id).iter().any(|d| d == f)
                    {
                        return Err(StoreError::Invalid(
                            "A folder cannot be moved into itself".into(),
                        ));
                    }
                }
                if kind == NodeKind::Folder {
                    if let Some(f) = t.folders.iter().find(|f| f.id == id) {
                        ensure_entity_editable(f.is_editable(), &f.name, "This folder")?;
                    }
                    let moved = descendant_folders(t, id);
                    if let Some(r) = t.requests.iter().find(|r| {
                        !r.is_editable()
                            && r.collection_id != coll
                            && r.folder_id.as_ref().is_some_and(|x| moved.contains(x))
                    }) {
                        return Err(read_only_newer(r, "move"));
                    }
                    // Re-home the folder and its subtree.
                    for f in t.folders.iter_mut().filter(|f| moved.contains(&f.id)) {
                        if f.collection_id != coll {
                            f.collection_id = coll.to_string();
                            f.updated_at = now;
                        }
                    }
                    for r in t
                        .requests
                        .iter_mut()
                        .filter(|r| r.folder_id.as_ref().is_some_and(|x| moved.contains(x)))
                    {
                        if r.collection_id != coll {
                            r.collection_id = coll.to_string();
                            r.updated_at = now;
                        }
                    }
                    if let Some(f) = t.folders.iter_mut().find(|f| f.id == id) {
                        f.parent_folder_id = target_folder_id.map(str::to_string);
                        f.updated_at = now;
                    }
                    let mut order = sorted_ids(
                        t.folders
                            .iter()
                            .filter(|f| {
                                f.id != id
                                    && f.collection_id == coll
                                    && f.parent_folder_id.as_deref() == target_folder_id
                            })
                            .map(|f| (f.sort_order, f.id.clone())),
                    );
                    order.insert(index.min(order.len()), id.to_string());
                    // Read-only sibling folders (AC-FWD-4) keep their order.
                    let current: HashMap<String, i64> = t
                        .folders
                        .iter()
                        .filter(|f| order.contains(&f.id))
                        .map(|f| (f.id.clone(), f.sort_order))
                        .collect();
                    let fixed: HashSet<String> = frozen
                        .folders
                        .keys()
                        .filter(|k| order.contains(k))
                        .cloned()
                        .collect();
                    let Some(numbers) = renumber_around(&order, &current, &fixed) else {
                        let f = order
                            .iter()
                            .find_map(|x| frozen.folders.get(x))
                            .ok_or_else(|| StoreError::Invalid("Cannot reorder".into()))?;
                        return Err(read_only_newer_named(&quoted(&f.name, "a folder"), "move"));
                    };
                    for (fid, n) in numbers {
                        if let Some(f) = t.folders.iter_mut().find(|f| f.id == fid)
                            && (f.sort_order != n || f.id == id)
                        {
                            f.sort_order = n;
                            f.updated_at = now;
                        }
                    }
                } else {
                    if let Some(r) = t.requests.iter_mut().find(|r| r.id == id) {
                        r.collection_id = coll.to_string();
                        r.folder_id = target_folder_id.map(str::to_string);
                        r.updated_at = now;
                    }
                    let mut order = sorted_ids(
                        t.requests
                            .iter()
                            .filter(|r| {
                                r.id != id
                                    && r.collection_id == coll
                                    && r.folder_id.as_deref() == target_folder_id
                            })
                            .map(|r| (r.sort_order, r.id.clone())),
                    );
                    order.insert(index.min(order.len()), id.to_string());
                    // Read-only siblings keep their sort order; the others
                    // are numbered around them.
                    let current: HashMap<String, i64> = t
                        .requests
                        .iter()
                        .filter(|r| order.contains(&r.id))
                        .map(|r| (r.id.clone(), r.sort_order))
                        .collect();
                    let fixed: HashSet<String> = before
                        .keys()
                        .filter(|k| order.contains(k))
                        .cloned()
                        .collect();
                    let Some(numbers) = renumber_around(&order, &current, &fixed) else {
                        let r = order
                            .iter()
                            .find_map(|x| before.get(x))
                            .ok_or_else(|| StoreError::Invalid("Cannot reorder".into()))?;
                        return Err(read_only_newer(r, "move"));
                    };
                    for (rid, n) in numbers {
                        if let Some(r) = t.requests.iter_mut().find(|r| r.id == rid)
                            && (r.sort_order != n || r.id == id)
                        {
                            r.sort_order = n;
                            r.updated_at = now;
                        }
                    }
                }
            }
        }
        frozen.check_kept(&tree, "move")?;
        *self.project_mut(&pid)? = tree;
        self.save_project(&pid)?;
        Ok(self.project(&pid)?.clone())
    }

    /// Insert a whole imported collection (OpenAPI, share link).
    pub fn insert_collection_tree(
        &mut self,
        project_id: &str,
        mut collection: Collection,
        folders: Vec<Folder>,
        requests: Vec<Request>,
    ) -> Result<ProjectTree> {
        let t = self.project_mut(project_id)?;
        collection.project_id = project_id.to_string();
        collection.sort_order = t
            .collections
            .iter()
            .map(|c| c.sort_order)
            .max()
            .unwrap_or(-1)
            + 1;
        t.collections.push(collection);
        t.folders.extend(folders);
        t.requests.extend(requests);
        self.save_project(project_id)?;
        Ok(self.project(project_id)?.clone())
    }

    /// `ReadOnlyNewer` when the stored request came from a newer ReqHarbor.
    pub fn ensure_request_editable(&self, id: &str) -> Result<()> {
        match self.request(id) {
            Some(r) => ensure_editable(r),
            None => Ok(()),
        }
    }

    // ----------------------------------------------------------- environments

    pub fn global_environment(&self) -> Option<&Environment> {
        self.environments.iter().find(|e| e.project_id.is_none())
    }

    pub fn environment(&self, id: &str) -> Option<&Environment> {
        self.environments.iter().find(|e| e.id == id)
    }

    /// Variable scope for a project: its active environment, then Global
    /// (AC-ENV-2). `environment_override` replaces the active one when given.
    pub fn var_scope(&self, project_id: Option<&str>) -> crate::vars::VarScope {
        let active = project_id
            .and_then(|pid| self.projects.get(pid))
            .and_then(|t| t.project.active_environment_id.as_deref())
            .and_then(|eid| self.environment(eid));
        crate::vars::VarScope::new(active, self.global_environment())
    }

    pub fn create_environment(
        &mut self,
        project_id: Option<&str>,
        name: &str,
    ) -> Result<Environment> {
        let name = non_empty(name)?;
        let Some(pid) = project_id else {
            return Err(StoreError::Invalid(
                "Only one Global environment is allowed".into(),
            ));
        };
        self.project(pid)?;
        let now = now_ms();
        let sort_order = self
            .environments
            .iter()
            .filter(|e| e.project_id.as_deref() == Some(pid))
            .map(|e| e.sort_order)
            .max()
            .unwrap_or(-1)
            + 1;
        let env = Environment {
            id: new_id(),
            workspace_id: self.workspace.id.clone(),
            project_id: Some(pid.to_string()),
            name,
            variables: vec![],
            sort_order,
            created_at: now,
            updated_at: now,
            extra: Extra::new(),
        };
        self.environments.push(env.clone());
        self.save_environments()?;
        Ok(env)
    }

    /// Update name and variables (AC-ENV-6).
    pub fn update_environment(&mut self, update: &Environment) -> Result<Environment> {
        let name = non_empty(&update.name)?;
        check_variable_names(&update.variables)?;
        let e = self
            .environments
            .iter_mut()
            .find(|e| e.id == update.id)
            .ok_or_else(|| StoreError::NotFound(format!("Environment {}", update.id)))?;
        ensure_entity_editable(e.is_editable(), &e.name, "This environment")?;
        e.name = name;
        let mut vars = update.variables.clone();
        merge_list_extra(&mut vars, &e.variables, var_key, var_mut, var_ref);
        e.variables = vars;
        merge_extra(&mut e.extra, &update.extra);
        e.updated_at = touch(e.updated_at);
        let out = e.clone();
        self.save_environments()?;
        Ok(out)
    }

    pub fn duplicate_environment(&mut self, id: &str) -> Result<Environment> {
        let src = self
            .environment(id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        if !src.is_editable() {
            return Err(read_only_newer_named(
                &quoted(&src.name, "an environment"),
                "copy",
            ));
        }
        let pid = src.project_id.clone().ok_or_else(|| {
            StoreError::Invalid("The Global environment cannot be duplicated".into())
        })?;
        check_variable_names(&src.variables)?;
        let mut env = self.create_environment(Some(&pid), &format!("{} copy", src.name))?;
        env.variables = src.variables;
        self.update_environment(&env)
    }

    pub fn delete_environment(&mut self, id: &str) -> Result<()> {
        let env = self
            .environment(id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        if env.project_id.is_none() {
            return Err(StoreError::Invalid(
                "The Global environment cannot be deleted".into(),
            ));
        }
        self.environments.retain(|e| e.id != id);
        self.tombstone(EntityType::Environment, id);
        // Clear active selections pointing at it.
        // A read-only project (AC-FWD-4) keeps its now dangling selection,
        // which resolves to no environment.
        let affected: Vec<String> = self
            .projects
            .values()
            .filter(|t| {
                t.project.active_environment_id.as_deref() == Some(id) && t.project.is_editable()
            })
            .map(|t| t.project.id.clone())
            .collect();
        for pid in affected {
            self.set_active_environment(&pid, None)?;
        }
        self.save_environments()?;
        self.persist_tombstones()
    }
}

/// Display name used in read-only messages.
fn quoted(name: &str, fallback: &str) -> String {
    if name.trim().is_empty() {
        fallback.to_string()
    } else {
        format!("\u{201c}{name}\u{201d}")
    }
}

fn quoted_name(r: &Request) -> String {
    quoted(&r.name, "a request")
}

/// `ReadOnlyNewer` for an action on a container that would rewrite, copy or
/// remove the read-only request `r` (AC-FWD-3): moves, duplicates and
/// cascading deletes never touch such a request.
pub fn read_only_newer(r: &Request, verb: &str) -> StoreError {
    read_only_newer_named(&quoted_name(r), verb)
}

/// [`read_only_newer`] for any entity, given its quoted display name.
fn read_only_newer_named(name: &str, verb: &str) -> StoreError {
    let advice = if verb == "delete" {
        format!("Delete {name} on its own first, or update ReqHarbor.")
    } else {
        format!("Update ReqHarbor to {verb} it.")
    };
    StoreError::ReadOnlyNewer(format!(
        "This would change {name}, which was made with a newer version of ReqHarbor and is read-only here. {advice}"
    ))
}

/// Refuse to edit a project, collection, folder or environment whose
/// `minClientVersion` is newer than this build (AC-FWD-4).
fn ensure_entity_editable(editable: bool, name: &str, fallback: &str) -> Result<()> {
    if editable {
        return Ok(());
    }
    let name = if name.trim().is_empty() {
        fallback.to_string()
    } else {
        format!("\u{201c}{name}\u{201d}")
    };
    Err(StoreError::ReadOnlyNewer(format!(
        "{name} was made with a newer version of ReqHarbor and is read-only here. Update ReqHarbor to edit it."
    )))
}

/// Non-editable requests of a tree, by id.
pub fn read_only_requests(t: &ProjectTree) -> BTreeMap<String, Request> {
    t.requests
        .iter()
        .filter(|r| !r.is_editable())
        .map(|r| (r.id.clone(), r.clone()))
        .collect()
}

/// Every non-editable entity of a project tree, by id (AC-FWD-3/4).
#[derive(Debug, Clone, Default)]
pub struct ReadOnlyEntities {
    pub project: Option<Project>,
    pub collections: BTreeMap<String, Collection>,
    pub folders: BTreeMap<String, Folder>,
    pub requests: BTreeMap<String, Request>,
}

impl ReadOnlyEntities {
    pub fn of(t: &ProjectTree) -> Self {
        ReadOnlyEntities {
            project: (!t.project.is_editable()).then(|| t.project.clone()),
            collections: t
                .collections
                .iter()
                .filter(|c| !c.is_editable())
                .map(|c| (c.id.clone(), c.clone()))
                .collect(),
            folders: t
                .folders
                .iter()
                .filter(|f| !f.is_editable())
                .map(|f| (f.id.clone(), f.clone()))
                .collect(),
            requests: read_only_requests(t),
        }
    }

    /// Every read-only entity is still in `after`, byte for byte, and
    /// `after` gained no new read-only entity (no copies).
    fn check_kept(&self, after: &ProjectTree, verb: &str) -> Result<()> {
        let col = |c: &Collection| read_only_newer_named(&quoted(&c.name, "a collection"), verb);
        let fol = |f: &Folder| read_only_newer_named(&quoted(&f.name, "a folder"), verb);
        if let Some(p) = &self.project
            && &after.project != p
        {
            return Err(read_only_newer_named(&quoted(&p.name, "a project"), verb));
        }
        for (id, c) in &self.collections {
            if after.collections.iter().find(|x| &x.id == id) != Some(c) {
                return Err(col(c));
            }
        }
        for (id, f) in &self.folders {
            if after.folders.iter().find(|x| &x.id == id) != Some(f) {
                return Err(fol(f));
            }
        }
        for (id, r) in &self.requests {
            if after.requests.iter().find(|x| &x.id == id) != Some(r) {
                return Err(read_only_newer(r, verb));
            }
        }
        if let Some(c) = after
            .collections
            .iter()
            .find(|x| !x.is_editable() && !self.collections.contains_key(&x.id))
        {
            return Err(col(c));
        }
        if let Some(f) = after
            .folders
            .iter()
            .find(|x| !x.is_editable() && !self.folders.contains_key(&x.id))
        {
            return Err(fol(f));
        }
        if let Some(r) = after
            .requests
            .iter()
            .find(|x| !x.is_editable() && !self.requests.contains_key(&x.id))
        {
            return Err(read_only_newer(r, verb));
        }
        Ok(())
    }
}

/// The first read-only entity among the given folders and requests of a
/// tree (collections first, then folders, then requests), as a
/// `ReadOnlyNewer` error for `verb`. Used by cascading deletes and copies.
fn first_read_only(
    t: &ProjectTree,
    collection: impl Fn(&Collection) -> bool,
    folder: impl Fn(&Folder) -> bool,
    request: impl Fn(&Request) -> bool,
    verb: &str,
) -> Result<()> {
    if let Some(c) = t
        .collections
        .iter()
        .find(|c| !c.is_editable() && collection(c))
    {
        return Err(read_only_newer_named(
            &quoted(&c.name, "a collection"),
            verb,
        ));
    }
    if let Some(f) = t.folders.iter().find(|f| !f.is_editable() && folder(f)) {
        return Err(read_only_newer_named(&quoted(&f.name, "a folder"), verb));
    }
    if let Some(r) = t.requests.iter().find(|r| !r.is_editable() && request(r)) {
        return Err(read_only_newer(r, verb));
    }
    Ok(())
}

/// Sort orders for `order` (ids in their new sequence) that keep the values
/// of the `fixed` ids. Without fixed ids this is `0..n`. Otherwise the other
/// ids are numbered into the gaps around the fixed ones; `None` when a gap is
/// too small.
fn renumber_around(
    order: &[String],
    current: &HashMap<String, i64>,
    fixed: &HashSet<String>,
) -> Option<Vec<(String, i64)>> {
    if fixed.is_empty() {
        return Some(
            order
                .iter()
                .enumerate()
                .map(|(i, id)| (id.clone(), i as i64))
                .collect(),
        );
    }
    let mut out = Vec::with_capacity(order.len());
    let mut pending: Vec<&String> = Vec::new();
    let mut prev: Option<i64> = None;
    for id in order {
        if !fixed.contains(id) {
            pending.push(id);
            continue;
        }
        let v = *current.get(id)?;
        let k = pending.len() as i64;
        let start = match prev {
            // Before the first fixed id: count down from it.
            None => v.checked_sub(k)?,
            Some(p) => {
                if k > 0 && v.checked_sub(p)?.checked_sub(1)? < k {
                    return None;
                }
                p + 1
            }
        };
        for (i, pid) in pending.drain(..).enumerate() {
            out.push((pid.clone(), start + i as i64));
        }
        out.push((id.clone(), v));
        prev = Some(v);
    }
    let start = prev.map_or(Some(0), |p| p.checked_add(1))?;
    for (i, pid) in pending.into_iter().enumerate() {
        out.push((pid.clone(), start.checked_add(i as i64)?));
    }
    Some(out)
}

/// Refuse to modify a request this build cannot edit faithfully (AC-FWD-3/4).
pub fn ensure_editable(r: &Request) -> Result<()> {
    if r.is_editable() {
        Ok(())
    } else {
        let name = if r.name.trim().is_empty() {
            "This request".to_string()
        } else {
            format!("\u{201c}{}\u{201d}", r.name)
        };
        Err(StoreError::ReadOnlyNewer(format!(
            "{name} was made with a newer version of ReqHarbor and is read-only here. Update ReqHarbor to edit it."
        )))
    }
}

/// Copy keys of `stored` that `target` lacks into `target`.
pub fn merge_extra(target: &mut Extra, stored: &Extra) {
    for (k, v) in stored {
        if !target.contains_key(k) {
            target.insert(k.clone(), v.clone());
        }
    }
}

/// Merge `extra` of list items: each updated item takes the unknown keys of
/// the stored item with the same key (same index preferred, else the first
/// unused stored item with that key).
fn merge_list_extra<T>(
    target: &mut [T],
    stored: &[T],
    key: fn(&T) -> &str,
    extra_mut: fn(&mut T) -> &mut Extra,
    extra: fn(&T) -> &Extra,
) {
    let mut used = vec![false; stored.len()];
    for (i, item) in target.iter_mut().enumerate() {
        let k = key(item).to_string();
        let found = if stored.get(i).is_some_and(|s| key(s) == k) && !used[i] {
            Some(i)
        } else {
            stored
                .iter()
                .enumerate()
                .position(|(j, s)| !used[j] && key(s) == k)
        };
        if let Some(j) = found {
            used[j] = true;
            merge_extra(extra_mut(item), extra(&stored[j]));
        }
    }
}

fn merge_auth_extra(target: &mut Auth, stored: &Auth) {
    merge_extra(&mut target.extra, &stored.extra);
    if let (Some(t), Some(s)) = (target.api_key.as_mut(), stored.api_key.as_ref()) {
        merge_extra(&mut t.extra, &s.extra);
    }
}

fn kv_key(kv: &KeyValue) -> &str {
    &kv.key
}
fn kv_mut(kv: &mut KeyValue) -> &mut Extra {
    &mut kv.extra
}
fn kv_ref(kv: &KeyValue) -> &Extra {
    &kv.extra
}
fn ff_key(f: &FormField) -> &str {
    &f.key
}
fn ff_mut(f: &mut FormField) -> &mut Extra {
    &mut f.extra
}
fn ff_ref(f: &FormField) -> &Extra {
    &f.extra
}
fn var_key(v: &Variable) -> &str {
    &v.key
}
fn var_mut(v: &mut Variable) -> &mut Extra {
    &mut v.extra
}
fn var_ref(v: &Variable) -> &Extra {
    &v.extra
}

/// Keep every unknown key of `stored` (top level and nested) that `target`
/// lacks (tech design §9.1).
pub fn merge_request_extra(target: &mut Request, stored: &Request) {
    merge_extra(&mut target.extra, &stored.extra);
    if target.min_client_version.is_none() {
        target.min_client_version = stored.min_client_version.clone();
    }
    merge_auth_extra(&mut target.auth, &stored.auth);
    merge_extra(&mut target.settings.extra, &stored.settings.extra);
    merge_list_extra(&mut target.params, &stored.params, kv_key, kv_mut, kv_ref);
    merge_list_extra(&mut target.headers, &stored.headers, kv_key, kv_mut, kv_ref);
    let (tb, sb) = (&mut target.body, &stored.body);
    merge_extra(&mut tb.extra, &sb.extra);
    if let (Some(t), Some(s)) = (tb.graphql.as_mut(), sb.graphql.as_ref()) {
        merge_extra(&mut t.extra, &s.extra);
    }
    if let (Some(t), Some(s)) = (tb.form_encoded.as_mut(), sb.form_encoded.as_ref()) {
        merge_list_extra(t, s, kv_key, kv_mut, kv_ref);
    }
    if let (Some(t), Some(s)) = (tb.form.as_mut(), sb.form.as_ref()) {
        merge_list_extra(t, s, ff_key, ff_mut, ff_ref);
    }
}

/// New `updatedAt` for an edit: now, but always later than the previous
/// value so an edit never looks older than the version it was based on.
fn touch(prev: i64) -> i64 {
    now_ms().max(prev + 1)
}

fn non_empty(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        Err(StoreError::Invalid("Name cannot be empty".into()))
    } else {
        Ok(n.to_string())
    }
}

fn sorted_ids(items: impl Iterator<Item = (i64, String)>) -> Vec<String> {
    let mut v: Vec<(i64, String)> = items.collect();
    v.sort();
    v.into_iter().map(|(_, id)| id).collect()
}

/// Next sortOrder among folders+requests of a parent.
fn next_sort(t: &ProjectTree, collection_id: &str, folder_id: Option<&str>) -> i64 {
    let f = t
        .folders
        .iter()
        .filter(|f| f.collection_id == collection_id && f.parent_folder_id.as_deref() == folder_id)
        .map(|f| f.sort_order);
    let r = t
        .requests
        .iter()
        .filter(|r| r.collection_id == collection_id && r.folder_id.as_deref() == folder_id)
        .map(|r| r.sort_order);
    f.chain(r).max().unwrap_or(-1) + 1
}

/// The folder and all folders nested under it.
pub fn descendant_folders(t: &ProjectTree, id: &str) -> Vec<String> {
    let mut out = vec![id.to_string()];
    let mut seen: HashSet<String> = HashSet::from([id.to_string()]);
    let mut i = 0;
    while i < out.len() {
        let cur = out[i].clone();
        for f in &t.folders {
            if f.parent_folder_id.as_deref() == Some(cur.as_str()) && seen.insert(f.id.clone()) {
                out.push(f.id.clone());
            }
        }
        i += 1;
    }
    out
}

/// Deep-copy a collection with new ids.
fn copy_collection(
    src: &ProjectTree,
    c: &Collection,
    project_id: &str,
    now: i64,
    name: Option<String>,
) -> (Vec<Collection>, Vec<Folder>, Vec<Request>) {
    let new_cid = new_id();
    let mut fmap = std::collections::HashMap::new();
    for f in src.folders.iter().filter(|f| f.collection_id == c.id) {
        fmap.insert(f.id.clone(), new_id());
    }
    let collection = Collection {
        id: new_cid.clone(),
        project_id: project_id.to_string(),
        name: name.unwrap_or_else(|| c.name.clone()),
        created_at: now,
        updated_at: now,
        ..c.clone()
    };
    let folders = src
        .folders
        .iter()
        .filter(|f| f.collection_id == c.id)
        .map(|f| Folder {
            id: fmap[&f.id].clone(),
            collection_id: new_cid.clone(),
            parent_folder_id: f
                .parent_folder_id
                .as_ref()
                .and_then(|p| fmap.get(p).cloned()),
            created_at: now,
            updated_at: now,
            ..f.clone()
        })
        .collect();
    let requests = src
        .requests
        .iter()
        .filter(|r| r.collection_id == c.id)
        .map(|r| Request {
            id: new_id(),
            collection_id: new_cid.clone(),
            folder_id: r.folder_id.as_ref().and_then(|p| fmap.get(p).cloned()),
            created_at: now,
            updated_at: now,
            ..r.clone()
        })
        .collect();
    (vec![collection], folders, requests)
}

/// `{{$name}}` is reserved for dynamic variables (AC-ENV-8): an environment
/// variable may not start with `$`.
fn check_variable_names(vars: &[Variable]) -> Result<()> {
    match vars.iter().find(|v| v.key.trim_start().starts_with('$')) {
        Some(v) => Err(StoreError::Invalid(format!(
            "Variable names can't start with $ (\"{}\"): {{{{$…}}}} is reserved for dynamic variables",
            v.key.trim()
        ))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        (dir, s)
    }

    #[test]
    fn ac_col_1_first_launch_defaults() {
        let (dir, s) = store();
        assert_eq!(s.workspace.name, "My Workspace");
        let projects = s.project_list();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "Default");
        assert!(s.global_environment().is_some());
        assert!(dir.path().join("workspace.json").exists());
        assert!(
            dir.path()
                .join(format!("projects/{}.json", projects[0].id))
                .exists()
        );
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("workspace.json")).unwrap())
                .unwrap();
        assert_eq!(v["schemaVersion"], 1);
        // Reopening does not create a second default project.
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.project_list().len(), 1);
    }

    #[test]
    fn ac_col_2_crud_and_child_counts() {
        let (_d, mut s) = store();
        let pid = s.project_list()[0].id.clone();
        let c = s.create_collection(&pid, "Users API").unwrap();
        let f1 = s.create_folder(&c.id, None, "Admin").unwrap();
        let f2 = s.create_folder(&c.id, Some(&f1.id), "Nested").unwrap();
        let r = s
            .create_request(
                &c.id,
                Some(&f2.id),
                Request {
                    name: "List".into(),
                    url: "https://x".into(),
                    ..Request::default()
                },
            )
            .unwrap();
        s.create_request(&c.id, None, Request::default()).unwrap();
        assert_eq!(
            s.child_counts("collection", &c.id).unwrap(),
            ChildCounts {
                collections: 0,
                folders: 2,
                requests: 2
            }
        );
        assert_eq!(
            s.child_counts("folder", &f1.id).unwrap(),
            ChildCounts {
                collections: 0,
                folders: 1,
                requests: 1
            }
        );

        assert_eq!(s.rename_request(&r.id, "Renamed").unwrap().name, "Renamed");
        assert!(s.rename_folder(&f1.id, "  ").is_err());
        let dup = s.duplicate_request(&r.id).unwrap();
        assert_eq!(dup.name, "Renamed copy");
        assert_ne!(dup.id, r.id);
        let tree = s.duplicate_folder(&f1.id).unwrap();
        assert_eq!(tree.folders.len(), 4);
        assert_eq!(tree.requests.len(), 5);
        let tree = s.duplicate_collection(&c.id).unwrap();
        assert_eq!(tree.collections.len(), 2);
        assert_eq!(tree.collections[1].name, "Users API copy");

        let tree = s.delete_folder(&f1.id).unwrap();
        assert!(tree.folders.iter().all(|f| f.id != f2.id));
        assert!(tree.requests.iter().all(|x| x.id != r.id && x.id != dup.id));
        let tree = s.delete_collection(&c.id).unwrap();
        assert_eq!(tree.collections.len(), 1);

        let p2 = s.create_project("Second").unwrap();
        assert_eq!(s.rename_project(&p2.project.id, "Two").unwrap().name, "Two");
        let p3 = s.duplicate_project(&pid).unwrap();
        assert_eq!(p3.collections.len(), 1);
        s.delete_project(&p2.project.id).unwrap();
        assert_eq!(s.project_list().len(), 2);
    }

    #[test]
    fn ac_col_3_nested_move_and_order_persists() {
        let (dir, mut s) = store();
        let pid = s.project_list()[0].id.clone();
        let a = s.create_collection(&pid, "A").unwrap();
        let b = s.create_collection(&pid, "B").unwrap();
        let mut parent = None;
        for i in 0..6 {
            parent = Some(
                s.create_folder(
                    &a.id,
                    parent.as_ref().map(|f: &Folder| f.id.as_str()),
                    &format!("L{i}"),
                )
                .unwrap(),
            );
        }
        let deep = parent.unwrap();
        let r1 = s
            .create_request(
                &a.id,
                None,
                Request {
                    name: "r1".into(),
                    ..Request::default()
                },
            )
            .unwrap();
        let r2 = s
            .create_request(
                &a.id,
                None,
                Request {
                    name: "r2".into(),
                    ..Request::default()
                },
            )
            .unwrap();
        let r3 = s
            .create_request(
                &a.id,
                None,
                Request {
                    name: "r3".into(),
                    ..Request::default()
                },
            )
            .unwrap();
        // reorder r3 to the top
        s.move_node(NodeKind::Request, &r3.id, Some(&a.id), None, 0)
            .unwrap();
        // move r1 into the deepest folder of another collection? (B has no folders) -> into B root
        s.move_node(NodeKind::Request, &r1.id, Some(&b.id), None, 0)
            .unwrap();
        // move r2 into the deep folder
        s.move_node(NodeKind::Request, &r2.id, Some(&a.id), Some(&deep.id), 0)
            .unwrap();
        // a folder cannot move into its own descendant
        let top = s
            .project(&pid)
            .unwrap()
            .folders
            .iter()
            .find(|f| f.name == "L0")
            .unwrap()
            .clone();
        assert!(
            s.move_node(NodeKind::Folder, &top.id, Some(&a.id), Some(&deep.id), 0)
                .is_err()
        );
        // move the whole folder subtree to collection B
        s.move_node(NodeKind::Folder, &top.id, Some(&b.id), None, 0)
            .unwrap();
        s.move_node(NodeKind::Collection, &b.id, None, None, 0)
            .unwrap();

        let s2 = Store::open(dir.path()).unwrap();
        let t = s2.project(&pid).unwrap();
        let get = |id: &str| t.requests.iter().find(|r| r.id == id).unwrap().clone();
        assert_eq!(get(&r1.id).collection_id, b.id);
        assert_eq!(get(&r2.id).folder_id.as_deref(), Some(deep.id.as_str()));
        assert_eq!(get(&r2.id).collection_id, b.id, "subtree moved with folder");
        assert!(t.folders.iter().all(|f| f.collection_id == b.id));
        assert_eq!(get(&r3.id).sort_order, 0);
        let mut cols = t.collections.clone();
        cols.sort_by_key(|c| c.sort_order);
        assert_eq!(cols[0].id, b.id);
    }

    #[test]
    fn ac_col_6_atomic_writes_and_corrupt_file_quarantine() {
        let (dir, mut s) = store();
        let pid = s.project_list()[0].id.clone();
        s.create_collection(&pid, "Keep").unwrap();
        // no temp files left behind
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("projects"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        // corrupt a second project file
        let p2 = s.create_project("Broken").unwrap();
        let path = dir.path().join(format!("projects/{}.json", p2.project.id));
        std::fs::write(&path, b"{ not json").unwrap();
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.corrupt.len(), 1);
        assert!(s.corrupt[0].renamed_to.contains(".corrupt-"));
        assert!(std::path::Path::new(&s.corrupt[0].renamed_to).exists());
        assert!(!path.exists());
        assert_eq!(s.project_list().len(), 1);
        assert_eq!(s.project(&pid).unwrap().collections[0].name, "Keep");
    }

    #[test]
    fn ac_env_8_variable_names_may_not_start_with_dollar() {
        let (_dir, mut s) = store();
        let pid = s.project_list()[0].id.clone();
        let mut e = s.create_environment(Some(&pid), "Staging").unwrap();
        e.variables = vec![Variable::new("ok$", "1"), Variable::new(" $uuid", "2")];
        let err = s.update_environment(&e).unwrap_err();
        assert!(
            matches!(&err, StoreError::Invalid(m) if m.contains("$uuid")),
            "{err}"
        );
        assert!(s.environment(&e.id).unwrap().variables.is_empty());
        e.variables.truncate(1);
        s.update_environment(&e).unwrap();
    }

    #[test]
    fn ac_env_6_environment_crud_and_active_selection() {
        let (dir, mut s) = store();
        let pid = s.project_list()[0].id.clone();
        let mut e = s.create_environment(Some(&pid), "Staging").unwrap();
        e.variables = vec![
            Variable::new("host", "stg"),
            Variable::new("tok", "x").secret(),
        ];
        s.update_environment(&e).unwrap();
        let d = s.duplicate_environment(&e.id).unwrap();
        assert_eq!(d.name, "Staging copy");
        assert_eq!(d.variables.len(), 2);
        s.set_active_environment(&pid, Some(&e.id)).unwrap();
        // QA-R2-005: a duplicated project keeps (a copy of) its active env.
        let dup = s.duplicate_project(&pid).unwrap();
        let dup_active = dup.project.active_environment_id.clone().unwrap();
        assert_ne!(dup_active, e.id);
        let copied = s.environment(&dup_active).unwrap();
        assert_eq!(copied.name, "Staging");
        assert_eq!(copied.project_id.as_deref(), Some(dup.project.id.as_str()));
        assert!(s.create_environment(None, "Another global").is_err());
        let gid = s.global_environment().unwrap().id.clone();
        assert!(s.delete_environment(&gid).is_err());
        // AC-ENV-5: selection persists per project
        let s2 = Store::open(dir.path()).unwrap();
        assert_eq!(
            s2.project(&pid)
                .unwrap()
                .project
                .active_environment_id
                .as_deref(),
            Some(e.id.as_str())
        );
        assert_eq!(s2.var_scope(Some(&pid)).resolve_simple("{{host}}").0, "stg");
        s.delete_environment(&e.id).unwrap();
        assert_eq!(s.project(&pid).unwrap().project.active_environment_id, None);
    }

    #[test]
    fn ac_req_11_scratch_request_saved_into_collection_or_folder() {
        let (_d, mut s) = store();
        let pid = s.project_list()[0].id.clone();
        let c = s.create_collection(&pid, "C").unwrap();
        let f = s.create_folder(&c.id, None, "F").unwrap();
        let scratch = Request {
            name: String::new(),
            method: "QUERY".into(),
            url: "https://x.dev".into(),
            ..Request::default()
        };
        let saved = s
            .create_request(&c.id, Some(&f.id), scratch.clone())
            .unwrap();
        assert_ne!(saved.id, scratch.id);
        assert_eq!(saved.collection_id, c.id);
        assert_eq!(saved.folder_id.as_deref(), Some(f.id.as_str()));
        assert_eq!(saved.name, "Untitled request");
        // edits are persisted only by an explicit save (no auto-save on send)
        let mut edited = saved.clone();
        edited.url = "https://y.dev".into();
        let stored = s.save_request(&edited).unwrap();
        assert_eq!(stored.url, "https://y.dev");
        assert!(stored.updated_at > saved.updated_at);
        let bad = Request {
            method: "bad method".into(),
            ..stored
        };
        assert!(s.save_request(&bad).is_err());
    }

    #[test]
    fn tombstones_only_for_synced_entities() {
        let (_d, mut s) = store();
        let pid = s.project_list()[0].id.clone();
        let c = s.create_collection(&pid, "C").unwrap();
        let r = s.create_request(&c.id, None, Request::default()).unwrap();
        s.sync_state
            .synced
            .insert(sync_key(EntityType::Request, &r.id), r.updated_at);
        s.delete_collection(&c.id).unwrap();
        assert_eq!(s.sync_state.tombstones.len(), 1);
        assert_eq!(s.sync_state.tombstones[0].id, r.id);
    }
}
