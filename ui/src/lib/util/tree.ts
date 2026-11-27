import type { Collection, Folder, ProjectTree, Request } from "../api/types";

export interface TreeRow {
  kind: "collection" | "folder" | "request";
  id: string;
  depth: number;
  name: string;
  collectionId: string;
  /** For folders: parent folder; for requests: containing folder. */
  parentFolderId: string | null;
  /** HTTP method, or "WS" / "SIO" for WebSocket requests (then `ws` is true). */
  method?: string;
  ws?: boolean;
  url?: string;
  count?: number;
  hasChildren: boolean;
  expanded: boolean;
}

export interface TreeIndex {
  collections: Collection[];
  foldersByParent: Map<string, Folder[]>;
  requestsByParent: Map<string, Request[]>;
  folderById: Map<string, Folder>;
  requestById: Map<string, Request>;
  collectionById: Map<string, Collection>;
}

const parentKey = (collectionId: string, folderId: string | null): string => `${collectionId}/${folderId ?? ""}`;
const bySort = (a: { sortOrder: number }, b: { sortOrder: number }) => a.sortOrder - b.sortOrder;

export function indexTree(tree: ProjectTree): TreeIndex {
  const foldersByParent = new Map<string, Folder[]>();
  const requestsByParent = new Map<string, Request[]>();
  for (const f of tree.folders) {
    const k = parentKey(f.collectionId, f.parentFolderId);
    (foldersByParent.get(k) ?? foldersByParent.set(k, []).get(k)!).push(f);
  }
  for (const r of tree.requests) {
    const k = parentKey(r.collectionId, r.folderId);
    (requestsByParent.get(k) ?? requestsByParent.set(k, []).get(k)!).push(r);
  }
  for (const l of foldersByParent.values()) l.sort(bySort);
  for (const l of requestsByParent.values()) l.sort(bySort);
  return {
    collections: [...tree.collections].sort(bySort),
    foldersByParent,
    requestsByParent,
    folderById: new Map(tree.folders.map((f) => [f.id, f])),
    requestById: new Map(tree.requests.map((r) => [r.id, r])),
    collectionById: new Map(tree.collections.map((c) => [c.id, c])),
  };
}

/** Ids of the requests a delete of this node removes (the request itself, or every request below). */
export function requestsInSubtree(ix: TreeIndex, kind: "collection" | "folder" | "request", id: string): string[] {
  if (kind === "request") return ix.requestById.has(id) ? [id] : [];
  const out: string[] = [];
  const walk = (collectionId: string, folderId: string | null) => {
    for (const r of childRequests(ix, collectionId, folderId)) out.push(r.id);
    for (const f of childFolders(ix, collectionId, folderId)) walk(collectionId, f.id);
  };
  if (kind === "collection") walk(id, null);
  else {
    const f = ix.folderById.get(id);
    if (f) walk(f.collectionId, f.id);
  }
  return out;
}

export function childFolders(ix: TreeIndex, collectionId: string, folderId: string | null): Folder[] {
  return ix.foldersByParent.get(parentKey(collectionId, folderId)) ?? [];
}
export function childRequests(ix: TreeIndex, collectionId: string, folderId: string | null): Request[] {
  return ix.requestsByParent.get(parentKey(collectionId, folderId)) ?? [];
}

/** Count of requests below a container (for the collection row count). */
export function countRequests(ix: TreeIndex, collectionId: string, folderId: string | null): number {
  let n = childRequests(ix, collectionId, folderId).length;
  for (const f of childFolders(ix, collectionId, folderId)) n += countRequests(ix, collectionId, f.id);
  return n;
}

/**
 * Flatten the tree into visible rows. With a filter, only requests whose name
 * or URL match (case-insensitive) are shown, with their ancestors forced open
 * (AC-COL-4); containers whose own name matches are shown too.
 */
export function flattenTree(ix: TreeIndex, expanded: ReadonlySet<string>, filter: string): TreeRow[] {
  const q = filter.trim().toLowerCase();
  const rows: TreeRow[] = [];
  const match = (s: string | undefined) => !!s && s.toLowerCase().includes(q);

  // Returns rows for a container's children; empty when nothing matches under a filter.
  const walk = (collectionId: string, folderId: string | null, depth: number): TreeRow[] => {
    const out: TreeRow[] = [];
    for (const f of childFolders(ix, collectionId, folderId)) {
      const kids = walk(collectionId, f.id, depth + 1);
      const hasChildren =
        childFolders(ix, collectionId, f.id).length + childRequests(ix, collectionId, f.id).length > 0;
      if (q && kids.length === 0 && !match(f.name)) continue;
      const open = q ? kids.length > 0 : expanded.has(f.id);
      out.push({
        kind: "folder",
        id: f.id,
        depth,
        name: f.name,
        collectionId,
        parentFolderId: folderId,
        hasChildren,
        expanded: open,
      });
      if (open) out.push(...kids);
    }
    for (const r of childRequests(ix, collectionId, folderId)) {
      if (q && !match(r.name) && !match(r.url)) continue;
      out.push({
        kind: "request",
        id: r.id,
        depth,
        name: r.name,
        collectionId,
        parentFolderId: folderId,
        method: r.kind === "ws" ? (r.ws?.socketIo ? "SIO" : "WS") : r.method,
        ws: r.kind === "ws",
        url: r.url,
        hasChildren: false,
        expanded: false,
      });
    }
    return out;
  };

  for (const c of ix.collections) {
    const kids = walk(c.id, null, 1);
    if (q && kids.length === 0 && !match(c.name)) continue;
    const open = q ? kids.length > 0 : expanded.has(c.id);
    rows.push({
      kind: "collection",
      id: c.id,
      depth: 0,
      name: c.name,
      collectionId: c.id,
      parentFolderId: null,
      count: countRequests(ix, c.id, null),
      hasChildren: childFolders(ix, c.id, null).length + childRequests(ix, c.id, null).length > 0,
      expanded: open,
    });
    if (open) rows.push(...kids);
  }
  return rows;
}

/** Ancestor ids (collection + folders) of a request, for revealing it. */
export function ancestorsOf(ix: TreeIndex, requestId: string): string[] {
  const r = ix.requestById.get(requestId);
  if (!r) return [];
  const out = [r.collectionId];
  let f = r.folderId ? ix.folderById.get(r.folderId) : undefined;
  while (f) {
    out.push(f.id);
    f = f.parentFolderId ? ix.folderById.get(f.parentFolderId) : undefined;
  }
  return out;
}

/** Is `folderId` equal to or inside `ancestorId`? */
export function isInside(ix: TreeIndex, folderId: string | null, ancestorId: string): boolean {
  let f = folderId ? ix.folderById.get(folderId) : undefined;
  while (f) {
    if (f.id === ancestorId) return true;
    f = f.parentFolderId ? ix.folderById.get(f.parentFolderId) : undefined;
  }
  return false;
}
