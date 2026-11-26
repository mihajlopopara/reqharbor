// ReqHarbor Worker entry: tiny router, auth, consistent errors, security headers.

import { ApiError, type AppEnv, errorResponse, notFound, withSecurityHeaders } from "./http";
import { createShare, listShares, publicShareJson, publicSharePage, revokeShare, updateShare } from "./shares";
import { pull, push } from "./sync";
import { type Workspace, authenticate, createWorkspace, getWorkspace, rotateToken } from "./workspaces";

type Handler = (req: Request, env: AppEnv, params: string[]) => Promise<Response>;
type Authed = (req: Request, env: AppEnv, ws: Workspace, params: string[]) => Promise<Response>;

const authed =
  (h: Authed): Handler =>
  async (req, env, params) =>
    h(req, env, await authenticate(req, env), params);

interface Route {
  pattern: RegExp;
  methods: Partial<Record<string, Handler>>;
}

const SHARE_PATH = "([A-Za-z0-9]{1,64})";

const routes: Route[] = [
  { pattern: /^\/v1\/workspaces$/, methods: { POST: (req, env) => createWorkspace(req, env) } },
  { pattern: /^\/v1\/workspace$/, methods: { GET: authed((_r, env, ws) => getWorkspace(ws, env)) } },
  { pattern: /^\/v1\/workspace\/token\/rotate$/, methods: { POST: authed((_r, env, ws) => rotateToken(ws, env)) } },
  {
    pattern: /^\/v1\/sync$/,
    methods: { GET: authed((req, env, ws) => pull(req, ws, env)), POST: authed((req, env, ws) => push(req, ws, env)) },
  },
  {
    pattern: /^\/v1\/shares$/,
    methods: {
      GET: authed((req, env, ws) => listShares(req, ws, env)),
      POST: authed((req, env, ws) => createShare(req, ws, env)),
    },
  },
  {
    pattern: new RegExp(`^/v1/shares/${SHARE_PATH}$`),
    methods: {
      PUT: authed((req, env, ws, [id]) => updateShare(req, id!, ws, env)),
      DELETE: authed((_r, env, ws, [id]) => revokeShare(id!, ws, env)),
    },
  },
  {
    pattern: new RegExp(`^/s/${SHARE_PATH}\\.json$`),
    methods: {
      GET: (_r, env, [id]) => publicShareJson(id!, env),
      OPTIONS: async () =>
        new Response(null, {
          status: 204,
          headers: {
            "Access-Control-Allow-Origin": "*",
            "Access-Control-Allow-Methods": "GET, OPTIONS",
            "Access-Control-Max-Age": "86400",
          },
        }),
    },
  },
  { pattern: new RegExp(`^/s/${SHARE_PATH}$`), methods: { GET: (_r, env, [id]) => publicSharePage(id!, env) } },
];

async function route(req: Request, env: AppEnv): Promise<Response> {
  const { pathname } = new URL(req.url);
  for (const r of routes) {
    const m = r.pattern.exec(pathname);
    if (!m) continue;
    const handler = r.methods[req.method];
    if (!handler) {
      const allow = Object.keys(r.methods).join(", ");
      throw new ApiError(405, "method_not_allowed", `Method ${req.method} not allowed; use ${allow}`, { Allow: allow });
    }
    return handler(req, env, m.slice(1));
  }
  throw notFound(`No route for ${req.method} ${pathname}`);
}

export default {
  async fetch(req: Request, env: AppEnv): Promise<Response> {
    let res: Response;
    try {
      res = await route(req, env);
    } catch (e) {
      if (e instanceof ApiError) {
        res = errorResponse(e);
      } else {
        console.error("unhandled error", e instanceof Error ? e.stack : e);
        res = errorResponse(new ApiError(500, "internal_error", "Internal server error"));
      }
    }
    return withSecurityHeaders(res);
  },
} satisfies ExportedHandler<AppEnv>;
