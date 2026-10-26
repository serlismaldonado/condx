// El protocolo local-first (§5 del diseño de la librería) para los alcances de Condx: timbre,
// pull (foto inicial paginada + incremental) y push (un grupo atómico por llamada, idempotente por
// `opId`, conflictos por campo con `planOp`). Mismas decisiones que el servidor falso de las
// pruebas y que el adaptador del ERP: la lógica viene de la librería.
//
// Un rechazo permanente lanza `ConvexError` con `{ code, message, status }`: Convex deshace todo el
// grupo y el cliente lo clasifica como rechazo (no se reintenta).

import { ConvexError, v } from "convex/values";
import { mutation, query, type MutationCtx, type QueryCtx } from "./_generated/server";
import type { Doc } from "./_generated/dataModel";
import { applySetDelta, planOp, PROTOCOL_VERSION, type Op, type OpResult, type Row } from "localfirst/server";
import { canAccess, principalFromToken, reviewFields, siteWriteDenied, writeDenied } from "./auth";
import { wakeIfMacGone } from "./boat";

/** Versión mínima del protocolo que se acepta (§5.3 bis). */
export const MIN_PROTOCOL_VERSION = PROTOCOL_VERSION;
/** Filas o cambios por página de pull. */
export const PAGE_SIZE = 100;
/** Cambios retenidos por alcance; un cursor más viejo recibe `resetRequired`. */
export const RETAIN_CHANGES = 5000;
/** Grupos más grandes se rechazan (una transacción tiene límites). */
const MAX_OPS_PER_GROUP = 100;
const FIELD_KEY = /^[A-Za-z][A-Za-z0-9]{0,63}$/;

const setDelta = v.object({
  add: v.optional(v.array(v.any())),
  remove: v.optional(v.array(v.any())),
  encoding: v.optional(v.union(v.literal("array"), v.literal("json"))),
});

const opValidator = v.object({
  opId: v.string(),
  scope: v.string(),
  kind: v.union(v.literal("create"), v.literal("patch"), v.literal("delete")),
  rowId: v.string(),
  set: v.optional(v.record(v.string(), v.any())),
  sets: v.optional(v.record(v.string(), setDelta)),
  base: v.optional(v.record(v.string(), v.number())),
  fieldsVersion: v.number(),
  madeAt: v.number(),
});

const groupValidator = v.object({
  groupId: v.string(),
  seq: v.number(),
  ops: v.array(opValidator),
  createdAt: v.number(),
  confirmAt: v.optional(v.record(v.string(), v.number())),
  attempts: v.number(),
});

function reject(code: string, message: string, status: "rejected" | "staleSchema" = "rejected"): never {
  throw new ConvexError({ code, message, status });
}

async function scopeDoc(ctx: QueryCtx, scope: string): Promise<Doc<"lf_scopes"> | null> {
  return await ctx.db.query("lf_scopes").withIndex("by_scope", (q) => q.eq("scope", scope)).unique();
}

function toRow(doc: Doc<"lf_rows">): Row {
  return { id: doc.rowId, docId: doc._id, values: doc.values, fieldVersions: doc.fieldVersions };
}

// ── Timbre ───────────────────────────────────────────────────────────────

/** Versión de cada alcance (o `denied`). Diminuta: se invalida con cada cambio de esos alcances. */
export const doorbell = query({
  args: { token: v.string(), scopes: v.array(v.string()), protocolVersion: v.number() },
  returns: v.union(v.object({ status: v.literal("upgradeRequired") }), v.object({ status: v.literal("ok"), versions: v.record(v.string(), v.union(v.number(), v.literal("denied"))) })),
  handler: async (ctx, { token, scopes, protocolVersion }) => {
    if (protocolVersion < MIN_PROTOCOL_VERSION) return { status: "upgradeRequired" as const };
    const principal = await principalFromToken(ctx, token);
    const versions: Record<string, number | "denied"> = {};
    for (const scope of scopes.slice(0, 32)) {
      versions[scope] = canAccess(principal, scope) ? ((await scopeDoc(ctx, scope))?.version ?? 0) : "denied";
    }
    return { status: "ok" as const, versions };
  },
});

// ── Pull ─────────────────────────────────────────────────────────────────

export const pull = query({
  args: { token: v.string(), scope: v.string(), cursor: v.union(v.number(), v.null()), bootstrap: v.optional(v.string()), protocolVersion: v.number() },
  returns: v.any(),
  handler: async (ctx, { token, scope, cursor, bootstrap, protocolVersion }) => {
    if (protocolVersion < MIN_PROTOCOL_VERSION) return { status: "upgradeRequired" };
    const principal = await principalFromToken(ctx, token);
    if (!canAccess(principal, scope)) return { status: "denied" };
    const s = await scopeDoc(ctx, scope);
    const version = s?.version ?? 0;
    const fieldsVersion = s?.fieldsVersion ?? 1;

    if (cursor === null) {
      // Foto inicial: el cursor se fija al empezar (`at`); lo que cambie mientras se pagina llega
      // después por el incremental.
      const token = bootstrap ? (JSON.parse(bootstrap) as { c: string | null; at: number }) : { c: null, at: version };
      const page = await ctx.db
        .query("lf_rows")
        .withIndex("by_scope_and_deleted_and_rowId", (q) => q.eq("scope", scope).eq("deleted", false))
        .paginate({ numItems: PAGE_SIZE, cursor: token.c });
      return {
        status: "ok",
        rows: page.page.map(toRow),
        deletes: [],
        cursor: token.at,
        ...(page.isDone ? {} : { bootstrap: JSON.stringify({ c: page.continueCursor, at: token.at }) }),
        hasMore: !page.isDone,
        fieldsVersion,
      };
    }

    if (s && cursor < s.minVersion) return { status: "resetRequired" };
    const changes = await ctx.db
      .query("lf_changes")
      .withIndex("by_scope_and_version", (q) => q.eq("scope", scope).gt("version", cursor))
      .take(PAGE_SIZE + 1);
    const page = changes.slice(0, PAGE_SIZE);
    const ids = [...new Set(page.map((c) => c.rowId))];
    const rows: Row[] = [];
    const deletes: string[] = [];
    for (const rowId of ids) {
      const doc = await ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", scope).eq("rowId", rowId)).unique();
      if (!doc || doc.deleted) deletes.push(rowId);
      else rows.push(toRow(doc));
    }
    return {
      status: "ok",
      rows,
      deletes,
      cursor: page.length ? page[page.length - 1]!.version : cursor,
      hasMore: changes.length > PAGE_SIZE,
      fieldsVersion,
    };
  },
});

// ── Push ─────────────────────────────────────────────────────────────────

export const push = mutation({
  args: { token: v.string(), group: groupValidator, protocolVersion: v.number() },
  returns: v.any(),
  handler: async (ctx, { token, group, protocolVersion }) => {
    if (protocolVersion < MIN_PROTOCOL_VERSION) return { status: "upgradeRequired" };
    const principal = await principalFromToken(ctx, token);
    if (group.ops.length === 0 || group.ops.length > MAX_OPS_PER_GROUP) reject("invalid", `a group has 1–${MAX_OPS_PER_GROUP} operations`);
    const results: OpResult[] = [];
    for (const op of group.ops as Op[]) {
      const done = await ctx.db.query("lf_ops").withIndex("by_principal_and_opId", (q) => q.eq("principal", principal.key).eq("opId", op.opId)).unique();
      if (done) {
        results.push(done.result as OpResult);
        continue;
      }
      const denied = principal.kind === "device" && principal.site ? await siteWriteDenied(ctx, principal, op) : writeDenied(principal, op);
      if (denied) reject("denied", denied);
      results.push(await applyOp(ctx, principal.key, op));
    }
    // Un teléfono dejó trabajo: si la Mac no está, se despierta al sitio en la nube (`boat.ts`).
    if (principal.kind === "device" && !principal.site) await wakeIfMacGone(ctx, principal.daemonId);
    return { status: "ok", results };
  },
});

export async function applyOp(ctx: MutationCtx, principalKey: string, op: Op): Promise<OpResult> {
  for (const key of [...Object.keys(op.set ?? {}), ...Object.keys(op.sets ?? {})]) {
    if (!FIELD_KEY.test(key)) reject("invalid", `invalid field name “${key}”`);
  }
  let s = await scopeDoc(ctx, op.scope);
  if (!s) {
    const id = await ctx.db.insert("lf_scopes", { scope: op.scope, version: 0, fieldsVersion: 1, minVersion: 0 });
    s = (await ctx.db.get("lf_scopes", id))!;
  }
  if (op.fieldsVersion !== 0 && op.fieldsVersion !== s.fieldsVersion) reject("schema_changed", "The schema changed", "staleSchema");
  const existing = await ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", op.scope).eq("rowId", op.rowId)).unique();
  const live = existing && !existing.deleted ? existing : null;
  if (op.kind !== "create" && !live) reject("not_found", "Record not found");
  if (op.kind === "create" && live) reject("duplicate", "Already exists");

  const version = s.version + 1;
  const now = Date.now();
  let conflicts: OpResult["conflicts"] = [];
  let overwrote: string[] = [];
  let docId: string;
  if (op.kind === "delete") {
    await ctx.db.patch("lf_rows", live!._id, { deleted: true, updatedAt: now });
    docId = live!._id;
  } else {
    const values: Record<string, unknown> = live ? { ...live.values } : {};
    const fieldVersions: Record<string, number> = live ? { ...live.fieldVersions } : {};
    const plan = planOp({ op, fieldVersions, reviewFields: reviewFields(op.scope), scopeVersion: s.version, now });
    conflicts = plan.conflicts.map((field) => ({ field, local: op.set?.[field] ?? null, server: values[field] ?? null }));
    overwrote = plan.overwrote;
    for (const [key, value] of Object.entries(plan.set)) {
      if (value === null || value === undefined) delete values[key];
      else values[key] = value;
      fieldVersions[key] = version;
    }
    for (const [key, delta] of Object.entries(plan.sets)) {
      const next = applySetDelta(values[key], delta);
      if (next === undefined) delete values[key];
      else values[key] = next;
      fieldVersions[key] = version;
    }
    if (existing) {
      // Un borrado que vuelve a crearse reutiliza su documento (el id por fila es estable).
      await ctx.db.patch("lf_rows", existing._id, { values, fieldVersions, deleted: false, updatedAt: now });
      docId = existing._id;
    } else {
      docId = await ctx.db.insert("lf_rows", { scope: op.scope, rowId: op.rowId, values, fieldVersions, deleted: false, updatedAt: now });
    }
  }
  await logChange(ctx, s, version, op.rowId);
  const result: OpResult = { opId: op.opId, docId, version, ...(conflicts.length ? { conflicts } : {}), ...(overwrote.length ? { overwrote } : {}) };
  await ctx.db.insert("lf_ops", { principal: principalKey, opId: op.opId, result, appliedAt: now });
  return result;
}

/** Sube la versión del alcance, anota el cambio y poda lo más viejo que lo retenido. */
async function logChange(ctx: MutationCtx, s: Doc<"lf_scopes">, version: number, rowId: string): Promise<void> {
  await ctx.db.insert("lf_changes", { scope: s.scope, version, rowId });
  let minVersion = s.minVersion;
  if (version - minVersion > RETAIN_CHANGES) {
    const old = await ctx.db
      .query("lf_changes")
      .withIndex("by_scope_and_version", (q) => q.eq("scope", s.scope).lte("version", version - RETAIN_CHANGES))
      .take(16);
    for (const c of old) {
      await ctx.db.delete("lf_changes", c._id);
      minVersion = Math.max(minVersion, c.version);
    }
  }
  await ctx.db.patch("lf_scopes", s._id, { version, minVersion });
}
