// Quién llama y qué puede tocar. Dos tipos de llamante, cada uno con su token (como una API key; se
// compara su SHA-256):
// - el daemon: lee y escribe los alcances con su id;
// - un teléfono activo de ese daemon: lee entornos, trabajos, aprobaciones e incidentes, y solo
//   escribe su decisión firmada en una aprobación o su pedido firmado en un incidente
//   (diagnosticar, arreglar, ignorar…). Nunca hace nada por sí mismo: el daemon verifica la firma
//   con la llave que confirmó en la Mac y decide.

import { ConvexError } from "convex/values";
import type { QueryCtx } from "./_generated/server";
import type { Op } from "localfirst/server";

export const SCOPE_KINDS = ["env", "jobs", "approvals", "runs", "workflows", "incidents", "keys", "events", "threads"] as const;
export type ScopeKind = (typeof SCOPE_KINDS)[number];

/** Lo que un teléfono puede leer. */
/** `keys`: las claves de los proveedores, cifradas por el daemon para cada teléfono (solo se leen). */
const DEVICE_READS: readonly ScopeKind[] = ["env", "jobs", "approvals", "incidents", "keys", "threads"];
/** Los únicos campos que un teléfono escribe en `approvals`. */
export const DECISION_FIELDS = ["decision", "decidedAt", "deviceKey", "signature", "confirmText"] as const;
/** Los únicos campos que un teléfono escribe en `incidents`: un pedido firmado. */
export const REQUEST_FIELDS = ["request", "requestArg", "requestId", "requestedAt", "deviceKey", "signature"] as const;
export const REQUESTS = ["diagnose", "fix", "ignore", "resolve", "deploy", "discard", "message", "thread", "run"] as const;

export type Principal =
  | { kind: "daemon"; daemonId: string; key: string }
  | { kind: "device"; daemonId: string; deviceId: string; publicKey: string; key: string; site?: boolean };

export async function sha256Hex(text: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function unauthorized(): never {
  throw new ConvexError({ code: "unauthorized", message: "Invalid token" });
}

/** El daemon dueño del token, o un error sin pistas (no dice si el daemon existe). */
export async function daemonFromToken(ctx: QueryCtx, token: string): Promise<Extract<Principal, { kind: "daemon" }>> {
  if (token.length < 32) unauthorized();
  const tokenHash = await sha256Hex(token);
  const daemon = await ctx.db.query("daemons").withIndex("by_tokenHash", (q) => q.eq("tokenHash", tokenHash)).unique();
  if (!daemon) unauthorized();
  return { kind: "daemon", daemonId: daemon.daemonId, key: `daemon:${daemon.daemonId}` };
}

/** El daemon o un teléfono ACTIVO dueño del token (un teléfono pendiente o revocado no entra). */
export async function principalFromToken(ctx: QueryCtx, token: string): Promise<Principal> {
  if (token.length < 32) unauthorized();
  const tokenHash = await sha256Hex(token);
  const daemon = await ctx.db.query("daemons").withIndex("by_tokenHash", (q) => q.eq("tokenHash", tokenHash)).unique();
  if (daemon) return { kind: "daemon", daemonId: daemon.daemonId, key: `daemon:${daemon.daemonId}` };
  const device = await ctx.db.query("devices").withIndex("by_tokenHash", (q) => q.eq("tokenHash", tokenHash)).unique();
  if (!device || device.status !== "active") unauthorized();
  // El daemon tiene que seguir vinculado.
  const owner = await ctx.db.query("daemons").withIndex("by_daemonId", (q) => q.eq("daemonId", device.daemonId)).unique();
  if (!owner) unauthorized();
  return { kind: "device", daemonId: device.daemonId, deviceId: device.deviceId, publicKey: device.publicKey, key: `device:${device.deviceId}`, site: device.kind === "site" };
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

/** Lectura (timbre, pull). */
export function canAccess(principal: Principal, scope: string): boolean {
  const parsed = parseScope(scope);
  if (!parsed || parsed.daemonId !== principal.daemonId) return false;
  return principal.kind === "daemon" || DEVICE_READS.includes(parsed.kind);
}

/** Escritura de una operación: el motivo si no se permite, o null. */
export function writeDenied(principal: Principal, op: Op): string | null {
  const parsed = parseScope(op.scope);
  if (!parsed || parsed.daemonId !== principal.daemonId) return "No access to this scope";
  if (principal.kind === "daemon") return null;
  if (parsed.kind === "incidents") return requestDenied(principal, op);
  // En un hilo, el teléfono solo escribe mensajes (firmados; el daemon verifica y decide a quién van).
  if (parsed.kind === "threads") {
    // Los adjuntos (ids de `/files`, firmados con el texto) solo van con un mensaje en un hilo.
    const { requestFiles, ...rest } = op.set ?? {};
    if (requestFiles !== undefined && requestFiles !== null && (typeof requestFiles !== "string" || requestFiles.length > 8000)) return "requestFiles must be text (at most 8000 characters)";
    // En `_run`, pedir que corra una receta o un flujo (la Mac verifica la firma y aplica las aprobaciones).
    const allowed = op.rowId === "_run" ? "run" : "message";
    return requestDenied(principal, { ...op, set: rest }) ?? (op.set?.request === allowed ? null : op.rowId === "_run" ? "Only run requests go in _run" : "A phone can only send messages in a thread");
  }
  if (parsed.kind !== "approvals" || op.kind !== "patch") return "A phone can only decide on approvals";
  if (op.sets && Object.keys(op.sets).length > 0) return "A phone can only decide on approvals";
  const set = op.set ?? {};
  for (const key of Object.keys(set)) {
    if (!(DECISION_FIELDS as readonly string[]).includes(key)) return `A phone can't write “${key}”`;
  }
  if (set.decision !== "approve" && set.decision !== "reject") return "decision must be approve or reject";
  if (set.deviceKey !== principal.publicKey) return "The decision must be signed with this phone's key";
  if (typeof set.signature !== "string" || !/^[0-9a-f]{128}$/.test(set.signature)) return "Missing signature";
  if (typeof set.decidedAt !== "number") return "Missing decidedAt";
  return null;
}

/** Lo que el sitio en la nube escribe en sus propios hilos (`site-…`): como la Mac en los suyos. */
const SITE_OWN_FIELDS = ["title", "kind", "status", "updatedAt", "items", "participants", "waitingAnswer", "project", "result", "resultFor", "overview"];
/** En un hilo de la Mac (o en `_new`), aparte de lo de la Mac: sus respuestas y a qué pedido contestó. */
const SITE_MAC_FIELDS = ["siteItems", "siteFor", "siteResult"];
/** Lo que escribe el sitio en una aprobación suya (los mismos campos que la Mac en las suyas). */
const SITE_APPROVAL_FIELDS = ["recipe", "title", "env", "envKind", "status", "approval", "requestedBy", "createdAt", "phoneResult"];
/** Un latido de la Mac más viejo que esto: la Mac está apagada (late cada minuto; al irse en orden avisa con 0). */
export const MAC_GONE_MS = 3 * 60_000;

/**
 * El sitio en la nube (S5): contesta en los hilos cuando la Mac no está. En los suyos (`site-…`)
 * escribe como la Mac; en los de la Mac solo sus campos aparte, y solo si la Mac no late.
 */
export async function siteWriteDenied(ctx: QueryCtx, principal: Principal, op: Op): Promise<string | null> {
  const parsed = parseScope(op.scope);
  if (!parsed || parsed.daemonId !== principal.daemonId) return "No access to this scope";
  // Las aprobaciones de sus propios trabajos (`site-<trabajo>`): las crea, las actualiza y las quita.
  if (parsed.kind === "approvals") {
    if (!op.rowId.startsWith("site-")) return "A cloud site only writes its own approvals";
    if (op.kind === "delete") return null;
    const bad = Object.keys(op.set ?? {}).find((k) => !SITE_APPROVAL_FIELDS.includes(k));
    return bad ? `A cloud site can't write “${bad}” in an approval` : null;
  }
  if (parsed.kind !== "threads") return "A cloud site only writes threads";
  if (op.sets && Object.keys(op.sets).length > 0) return "A cloud site doesn't write sets";
  const keys = Object.keys(op.set ?? {});
  if (op.rowId.startsWith("site-")) {
    if (op.kind === "delete") return null;
    const bad = keys.find((k) => !SITE_OWN_FIELDS.includes(k));
    return bad ? `A cloud site can't write “${bad}”` : null;
  }
  // `_mcp-…`: una llamada del MCP remoto (mcp.ts) que el sitio contesta si la Mac no late.
  const mcp = op.rowId.startsWith("_mcp-");
  if (op.kind !== "patch" || (op.rowId.startsWith("_") && op.rowId !== "_new" && op.rowId !== "_run" && !mcp)) return "A cloud site only answers in threads";
  // En `_run` y en las llamadas solo contesta, nunca escribe sus respuestas de hilo.
  const fields = op.rowId === "_run" || mcp ? SITE_MAC_FIELDS.filter((k) => k !== "siteItems") : SITE_MAC_FIELDS;
  const bad = keys.find((k) => !fields.includes(k));
  if (bad) return `A cloud site can't write “${bad}” in the Mac's threads`;
  const mac = await ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", op.scope).eq("rowId", "_mac")).unique();
  const macValues = mac?.values as { aliveAt?: unknown; handover?: unknown } | undefined;
  const aliveAt = macValues?.aliveAt;
  // Con el interruptor del teléfono («que conteste la nube»), la Mac se hace a un lado aunque lata.
  if (typeof aliveAt === "number" && Date.now() - aliveAt < MAC_GONE_MS && macValues?.handover !== true) return "The Mac is answering this thread";
  return null;
}

/** Un pedido de un teléfono sobre un incidente: solo sus campos, firmado con su llave. */
function requestDenied(principal: Extract<Principal, { kind: "device" }>, op: Op): string | null {
  if (op.kind !== "patch" || (op.sets && Object.keys(op.sets).length > 0)) return "A phone can only send requests on incidents";
  const set = op.set ?? {};
  for (const key of Object.keys(set)) {
    if (!(REQUEST_FIELDS as readonly string[]).includes(key)) return `A phone can't write “${key}”`;
  }
  if (!(REQUESTS as readonly unknown[]).includes(set.request)) return "unknown request";
  if (set.requestArg !== undefined && set.requestArg !== null && (typeof set.requestArg !== "string" || set.requestArg.length > 4000)) return "requestArg must be text (at most 4000 characters)";
  if (typeof set.requestId !== "string" || !/^[0-9a-f]{16,64}$/.test(set.requestId)) return "Missing requestId";
  if (typeof set.requestedAt !== "number") return "Missing requestedAt";
  if (set.deviceKey !== principal.publicKey) return "The request must be signed with this phone's key";
  if (typeof set.signature !== "string" || !/^[0-9a-f]{128}$/.test(set.signature)) return "Missing signature";
  return null;
}

/**
 * Campos que van a revisión ante un conflicto (no «último gana»): en las aprobaciones, la decisión
 * y el estado. Si el daemon cambió el estado después de lo que vio el teléfono, la decisión no se
 * aplica a ciegas.
 */
export function reviewFields(scope: string): Set<string> {
  return parseScope(scope)?.kind === "approvals" ? new Set(["status", "decision"]) : new Set();
}
