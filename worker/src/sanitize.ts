// Server-side secret stripping (AC-ENV-7, AC-SHARE-2). The client already sanitizes;
// this is defense in depth so a buggy or old client can never persist secret values.

import { isPlainObject } from "./http";

const isSecretVar = (v: unknown): boolean => isPlainObject(v) && v.secret === true;

/**
 * Sync: secret variables keep their key/flags (so other devices know the variable exists)
 * but their value is never stored. Clients must keep their local secret values on pull.
 */
export function stripSecretValuesForSync(env: Record<string, unknown>): Record<string, unknown> {
  if (!Array.isArray(env.variables)) return env;
  return {
    ...env,
    variables: env.variables.map((v) => (isSecretVar(v) ? { ...(v as object), value: "" } : v)),
  };
}

/** Share: secret variables are removed entirely from an attached environment. */
export function removeSecretVariables(env: Record<string, unknown>): Record<string, unknown> {
  if (!Array.isArray(env.variables)) return env;
  return { ...env, variables: env.variables.filter((v) => !isSecretVar(v)) };
}
