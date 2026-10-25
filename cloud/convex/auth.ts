// Quién llama y qué alcances puede tocar. Por ahora solo daemons (token en el argumento, como
// una API key; se compara su SHA-256). Los usuarios del teléfono llegan en C4 con Convex Auth.

import { ConvexError } from "convex/values";
import type { QueryCtx } from "./_generated/server";

export const SCOPE_KINDS = ["env", "jobs", "approvals", "runs", "workflows"] as const;
export type ScopeKind = (typeof SCOPE_KINDS)[number];

export interface Principal {
  kind: "daemon";
  daemonId: string;
  /** Clave del registro de operaciones: un `opId` de un daemon no choca con el de otro. */
  key: string;
}

export async function sha256Hex(text: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** El daemon dueño del token, o un error sin pistas (no dice si el daemon existe). */
export async function daemonFromToken(ctx: QueryCtx, token: string): Promise<Principal> {
  if (token.length < 32) throw new ConvexError({ code: "unauthorized", message: "Invalid token" });
  const tokenHash = await sha256Hex(token);
  const daemon = await ctx.db.query("daemons").withIndex("by_tokenHash", (q) => q.eq("tokenHash", tokenHash)).unique();
  if (!daemon) throw new ConvexError({ code: "unauthorized", message: "Invalid token" });
  return { kind: "daemon", daemonId: daemon.daemonId, key: `daemon:${daemon.daemonId}` };
}

/** `<tipo>:<daemonId>` → sus partes, o null si no es un alcance de Condx. */
export function parseScope(scope: string): { kind: ScopeKind; daemonId: string } | null {
  const ix = scope.indexOf(":");
  if (ix < 0) return null;
  const kind = scope.slice(0, ix) as ScopeKind;
  const daemonId = scope.slice(ix + 1);
  if (!SCOPE_KINDS.includes(kind) || !/^[a-z0-9-]{8,64}$/.test(daemonId)) return null;
  return { kind, daemonId };
}

/** Un daemon lee y escribe solo los alcances con su id. */
export function canAccess(principal: Principal, scope: string): boolean {
  const parsed = parseScope(scope);
  return parsed !== null && parsed.daemonId === principal.daemonId;
}

/**
 * Campos que van a revisión ante un conflicto (no «último gana»): en las aprobaciones, la decisión
 * y el estado. Si el daemon cambió el estado después de lo que vio el teléfono, la decisión no se
 * aplica a ciegas.
 */
export function reviewFields(scope: string): Set<string> {
  return parseScope(scope)?.kind === "approvals" ? new Set(["status", "decision"]) : new Set();
}
