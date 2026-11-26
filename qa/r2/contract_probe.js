// QA round 2 — dynamic IPC contract probe. The SAME function body is executed
//   (a) inside the real Tauri webview: invoke = window.__TAURI_INTERNALS__.invoke   (qa/r2/real_app_smoke.mjs)
//   (b) in Chromium against the browser mock: invoke = getBackend().invoke         (ui/e2e/qa_contract_shapes.spec.ts)
// It calls every command with representative arguments and returns { [label]: value | {__error} }.
// The two outputs are compared shape-by-shape by qa/r2/shape_diff.mjs.
// ctx: { echo: "http://127.0.0.1:8099", worker: "http://127.0.0.1:8787" | null, tmp: "/tmp/x" | null, spec: "<openapi text>" }
// eslint-disable-next-line no-unused-vars
async function contractProbe(invoke, ctx) {
  const out = {};
  const norm = (v) => {
    if (v instanceof ArrayBuffer) return { __type: "ArrayBuffer", byteLength: v.byteLength };
    if (ArrayBuffer.isView(v)) return { __type: Object.prototype.toString.call(v), byteLength: v.byteLength };
    if (Array.isArray(v) && v.length > 0 && v.every((x) => typeof x === "number")) return { __type: "number[]", length: v.length };
    return v;
  };
  const run = async (label, cmd, args) => {
    try {
      const v = norm(await invoke(cmd, args));
      out[label] = v;
      return v;
    } catch (e) {
      let err = e;
      if (typeof e === "string") err = { __string: e };
      else if (e && typeof e === "object") err = { code: e.code, message: typeof e.message, details: e.details ?? undefined, keys: Object.keys(e).sort() };
      out[label] = { __error: err };
      return null;
    }
  };
  const boot = await run("bootstrap", "bootstrap", {});
  const settings = await run("get_settings", "get_settings", {});
  await run("save_settings", "save_settings", { settings: { ...settings, qaExtraKey: "kept" } });
  await run("save_settings.fractionalSidebarWidth", "save_settings", { settings: { ...settings, sidebarWidth: 312.5 } });
  await run("list_projects", "list_projects", {});
  const tree = await run("create_project", "create_project", { name: "QA contract" });
  const pid = tree.project.id;
  await run("rename_project", "rename_project", { projectId: pid, name: "QA contract 2" });
  await run("get_project_tree", "get_project_tree", { projectId: pid });
  const col = await run("create_collection", "create_collection", { projectId: pid, name: "Col" });
  await run("update_collection", "update_collection", {
    collection: { ...col, description: "d", auth: { type: "bearer", token: "{{token}}" } },
  });
  const folder = await run("create_folder", "create_folder", { collectionId: col.id, parentFolderId: null, name: "F" });
  await run("rename_folder", "rename_folder", { folderId: folder.id, name: "F2" });
  const req = await run("create_request", "create_request", { collectionId: col.id, folderId: folder.id });
  const edited = {
    ...req,
    method: "QUERY",
    url: "{{baseUrl}}/query?x=1",
    params: [{ key: "x", value: "1", enabled: true }, { key: "off", value: "2", enabled: false }],
    headers: [{ key: "X-Qa", value: "1", enabled: true }],
    auth: { type: "inherit" },
    body: { type: "json", text: '{"q":"{{secretTok}}"}' },
  };
  await run("save_request", "save_request", { request: edited });
  await run("rename_request", "rename_request", { requestId: req.id, name: "Renamed" });
  const dup = await run("duplicate_request", "duplicate_request", { requestId: req.id });
  await run("child_counts", "child_counts", { kind: "collection", id: col.id });
  await run("duplicate_folder", "duplicate_folder", { folderId: folder.id });
  await run("duplicate_collection", "duplicate_collection", { collectionId: col.id });
  await run("move_node", "move_node", { kind: "request", id: dup.id, targetCollectionId: col.id, targetFolderId: null, index: 0 });
  await run("move_node.intoSelf", "move_node", { kind: "folder", id: folder.id, targetCollectionId: col.id, targetFolderId: folder.id, index: 0 });
  await run("list_environments", "list_environments", {});
  const env = await run("create_environment", "create_environment", { projectId: pid, name: "QA env" });
  await run("update_environment", "update_environment", {
    environment: {
      ...env,
      variables: [
        { key: "baseUrl", value: ctx.echo, enabled: true, secret: false },
        { key: "secretTok", value: "S3CRET-VALUE", enabled: true, secret: true },
      ],
    },
  });
  await run("duplicate_environment", "duplicate_environment", { environmentId: env.id });
  await run("set_active_environment", "set_active_environment", { projectId: pid, environmentId: env.id });
  await run("get_variables", "get_variables", { projectId: pid });

  const saved = (await invoke("get_project_tree", { projectId: pid })).requests.find((r) => r.id === req.id);
  const sent = await run("send_request", "send_request", { executionId: "qa-exec-1", request: saved, projectId: pid });
  if (sent && sent.response) {
    await run("get_response_body", "get_response_body", { responseId: sent.response.responseId });
    await run("get_response_body.limit", "get_response_body", { responseId: sent.response.responseId, limit: 10 });
    await run("save_response_body", "save_response_body", { responseId: sent.response.responseId, path: ctx.tmp || "qa-body.json" });
  }
  await run("get_response_body.missing", "get_response_body", { responseId: "nope" });
  await run("send_request.connectError", "send_request", {
    executionId: "qa-exec-2",
    request: { ...saved, url: "http://127.0.0.1:9/x", method: "GET", body: { type: "none" } },
    projectId: pid,
  });
  await run("send_request.invalidMethod", "send_request", { executionId: "qa-exec-3", request: { ...saved, method: "bad method" }, projectId: pid });
  await run("send_request.emptyUrl", "send_request", { executionId: "qa-exec-4", request: { ...saved, url: "" }, projectId: pid });
  await run("cancel_request", "cancel_request", { executionId: "not-in-flight" });
  const hist = await run("list_history", "list_history", {});
  if (hist && hist[0]) {
    await run("open_history_entry", "open_history_entry", { historyId: hist[0].id });
    const ok = hist.find((h) => h.status !== null);
    if (ok) await run("open_history_entry.ok", "open_history_entry", { historyId: ok.id });
    await run("delete_history_entry", "delete_history_entry", { historyId: hist[0].id });
  }
  await run("open_history_entry.missing", "open_history_entry", { historyId: "nope" });
  await run("format_body", "format_body", { kind: "json", text: '{"a":1,"b":[1,2]}' });
  await run("format_body.xml", "format_body", { kind: "xml", text: "<a><b>1</b></a>" });
  await run("format_body.error", "format_body", { kind: "json", text: '{"a":1,,}' });
  await run("url_to_params", "url_to_params", { url: "https://x.dev/a?b=1&b=2&c", params: [{ key: "z", value: "9", enabled: false }] });
  await run("params_to_url", "params_to_url", { url: "https://x.dev/a?old=1", params: [{ key: "q", value: "a b", enabled: true }] });
  await run("import_curl", "import_curl", { text: "curl -X QUERY 'https://api.x.dev/s' -H 'Content-Type: application/json' --data-raw '{\"a\":1}' --bogus" });
  await run("import_curl.notCurl", "import_curl", { text: "wget https://x" });
  await run("import_curl.syntax", "import_curl", { text: "curl 'https://x" });
  await run("import_curl.missingUrl", "import_curl", { text: "curl -X GET" });
  await run("looks_like_curl", "looks_like_curl", { text: "curl https://x" });
  await run("export_curl", "export_curl", { request: saved, projectId: pid, keepVariables: false });
  await run("export_curl.keep", "export_curl", { request: saved, projectId: pid, keepVariables: true });
  await run("preview_openapi", "preview_openapi", { text: ctx.spec });
  await run("preview_openapi.unsupported", "preview_openapi", { text: '{"asyncapi":"2.0.0","info":{"title":"x"}}' });
  await run("preview_openapi.parse", "preview_openapi", { text: "{{{" });
  await run("import_openapi", "import_openapi", { projectId: pid, text: ctx.spec });
  await run("get_sync_status", "get_sync_status", {});
  await run("share_preview", "share_preview", { kind: "collection", entityId: col.id, includeCredentials: false, environmentId: env.id });

  if (ctx.worker) {
    await run("sync_create_workspace", "sync_create_workspace", { workerUrl: ctx.worker, name: "QA R2" });
    await run("sync_workspace_info", "sync_workspace_info", {});
    await run("sync_now", "sync_now", {});
    const share = await run("share_create", "share_create", { kind: "collection", entityId: col.id, includeCredentials: false, environmentId: env.id });
    await run("share_list", "share_list", {});
    if (share) {
      await run("share_update", "share_update", { shareId: share.shareId, kind: "collection", entityId: col.id, includeCredentials: true, environmentId: null });
      await run("preview_share", "preview_share", { url: share.url });
      await run("import_share", "import_share", { url: share.url, projectId: pid });
      await run("share_revoke", "share_revoke", { shareId: share.shareId });
      await run("preview_share.gone", "preview_share", { url: share.url });
    }
    await run("sync_rotate_token", "sync_rotate_token", {});
    await run("sync_disconnect", "sync_disconnect", {});
  }
  await run("sync_now.notConnected", "sync_now", {});
  await run("share_list.notConnected", "share_list", {});

  await run("move_project", "move_project", { projectId: pid, index: 0 });
  const dupP = await run("duplicate_project", "duplicate_project", { projectId: pid });
  await run("delete_request", "delete_request", { requestId: dup.id });
  await run("delete_folder", "delete_folder", { folderId: folder.id });
  await run("delete_collection", "delete_collection", { collectionId: col.id });
  await run("delete_environment", "delete_environment", { environmentId: env.id });
  const globalEnv = (boot.environments || []).find((e) => e.projectId === null);
  if (globalEnv) await run("delete_environment.global", "delete_environment", { environmentId: globalEnv.id });
  if (dupP) await run("delete_project", "delete_project", { projectId: dupP.project.id });
  await run("delete_project.qa", "delete_project", { projectId: pid });
  await run("get_project_tree.missing", "get_project_tree", { projectId: "nope" });
  await run("clear_history", "clear_history", {});
  await run("unknown_command", "definitely_not_a_command", {});
  await run("create_project.missingArg", "create_project", {});
  await run("move_node.negativeIndex", "move_node", { kind: "collection", id: "x", index: -1 });
  return out;
}
