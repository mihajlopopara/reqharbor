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
use std::collections::{BTreeMap, BTreeSet, HashSet};

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
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    /// Stable ULID of this installation; LWW tie-breaker (contract C3).
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub tombstones: Vec<Tombstone>,
    /// `"type:id"` -> `updatedAt` last known to be on the server.
    #[serde(default)]
    pub synced: BTreeMap<String, i64>,
    /// `"type:id"` -> deviceId that produced the local version, when it came
    /// from another device (absent = this device).
    #[serde(default)]
    pub origins: BTreeMap<String, String>,
    /// Entities not synced because they are too large (contract C2).
    #[serde(default)]
    pub sync_skipped: BTreeMap<String, SyncSkipped>,
    #[serde(default)]
    pub last_sync_at: Option<i64>,
}

/// An entity excluded from sync until it changes again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSkipped {
    /// `updatedAt` of the version that was skipped; a newer edit retries.
    pub updated_at: i64,
    /// Currently always `"too_large"`.
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
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsFile {
    schema_version: u32,
    settings: Settings,
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
        let workspace = match ws_file {
            Some(f) => f.workspace,
            None => {
                let now = now_ms();
                Workspace {
                    id: new_id(),
                    name: "My Workspace".into(),
                    created_at: now,
                    updated_at: now,
                    sync: None,
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
                .map(|f| f.environments)
                .unwrap_or_default();
        let settings = read_json::<SettingsFile>(&dir.join("settings.json"), &mut corrupt)?
            .map(|f| f.settings)
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
            },
        )
    }

    pub fn save_settings(&self) -> Result<()> {
        write_json(
            &self.dir.join("settings.json"),
            &SettingsFile {
                schema_version: SCHEMA_VERSION,
                settings: self.settings.clone(),
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
        };
        let tree = ProjectTree {
            project,
            collections: vec![],
            folders: vec![],
            requests: vec![],
        };
        let id = tree.project.id.clone();
        self.projects.insert(id.clone(), tree.clone());
        self.save_project(&id)?;
        Ok(tree)
    }

    pub fn rename_project(&mut self, id: &str, name: &str) -> Result<Project> {
        let name = non_empty(name)?;
        let t = self.project_mut(id)?;
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
        t.project.active_environment_id = env_id.map(str::to_string);
        t.project.updated_at = touch(t.project.updated_at);
        let p = t.project.clone();
        self.save_project(project_id)?;
        Ok(p)
    }

    pub fn duplicate_project(&mut self, id: &str) -> Result<ProjectTree> {
        let src = self.project(id)?.clone();
        let now = now_ms();
        let new_pid = new_id();
        let sort_order = self
            .projects
            .values()
            .map(|t| t.project.sort_order)
            .max()
            .unwrap_or(0)
            + 1;
        let mut tree = ProjectTree {
            project: Project {
                id: new_pid.clone(),
                name: format!("{} copy", src.project.name),
                sort_order,
                active_environment_id: None,
                created_at: now,
                updated_at: now,
                ..src.project.clone()
            },
            collections: vec![],
            folders: vec![],
            requests: vec![],
        };
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
        list.insert(index.min(list.len()), p);
        let now = now_ms();
        for (i, p) in list.iter().enumerate() {
            let t = self.project_mut(&p.id)?;
            if t.project.sort_order != i as i64 {
                t.project.sort_order = i as i64;
                t.project.updated_at = now;
                let pid = p.id.clone();
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
        c.name = name;
        c.description = update.description.clone();
        c.auth = update.auth.clone();
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
        let t = self.project_mut(&pid)?;
        let ids = descendant_folders(t, id);
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
    pub fn save_request(&mut self, update: &Request) -> Result<Request> {
        if !is_valid_method(&update.method) {
            return Err(StoreError::Invalid(format!(
                "Invalid method {}",
                update.method
            )));
        }
        let pid = self.project_of(NodeKind::Request, &update.id)?;
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
        *r = update.clone();
        (r.collection_id, r.folder_id, r.sort_order, r.created_at) = keep;
        r.updated_at = touch(r.updated_at);
        let out = r.clone();
        self.save_project(&pid)?;
        Ok(out)
    }

    pub fn rename_request(&mut self, id: &str, name: &str) -> Result<Request> {
        let name = non_empty(name)?;
        let pid = self.project_of(NodeKind::Request, id)?;
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
        let now = now_ms();
        let t = self.project_mut(&pid)?;
        match kind {
            NodeKind::Collection => {
                let mut order: Vec<String> =
                    sorted_ids(t.collections.iter().map(|c| (c.sort_order, c.id.clone())));
                order.retain(|c| c != id);
                order.insert(index.min(order.len()), id.to_string());
                for (i, cid) in order.iter().enumerate() {
                    if let Some(c) = t.collections.iter_mut().find(|c| &c.id == cid)
                        && c.sort_order != i as i64
                    {
                        c.sort_order = i as i64;
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
                    let moved = descendant_folders(t, id);
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
                    for (i, fid) in order.iter().enumerate() {
                        if let Some(f) = t.folders.iter_mut().find(|f| &f.id == fid)
                            && (f.sort_order != i as i64 || f.id == id)
                        {
                            f.sort_order = i as i64;
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
                    for (i, rid) in order.iter().enumerate() {
                        if let Some(r) = t.requests.iter_mut().find(|r| &r.id == rid)
                            && (r.sort_order != i as i64 || r.id == id)
                        {
                            r.sort_order = i as i64;
                            r.updated_at = now;
                        }
                    }
                }
            }
        }
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
        };
        self.environments.push(env.clone());
        self.save_environments()?;
        Ok(env)
    }

    /// Update name and variables (AC-ENV-6).
    pub fn update_environment(&mut self, update: &Environment) -> Result<Environment> {
        let name = non_empty(&update.name)?;
        let e = self
            .environments
            .iter_mut()
            .find(|e| e.id == update.id)
            .ok_or_else(|| StoreError::NotFound(format!("Environment {}", update.id)))?;
        e.name = name;
        e.variables = update.variables.clone();
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
        let pid = src.project_id.clone().ok_or_else(|| {
            StoreError::Invalid("The Global environment cannot be duplicated".into())
        })?;
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
        let affected: Vec<String> = self
            .projects
            .values()
            .filter(|t| t.project.active_environment_id.as_deref() == Some(id))
            .map(|t| t.project.id.clone())
            .collect();
        for pid in affected {
            self.set_active_environment(&pid, None)?;
        }
        self.save_environments()?;
        self.persist_tombstones()
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
