// MCP remoto: Claude (Desktop en cualquier computadora, la web o el teléfono) y otros agentes llegan a
// Condx sin estar en la Mac. Lo que hace cada herramienta lo sigue haciendo la Mac, o el sitio en la
// nube si la Mac no late: aquí solo se autoriza y se pasa el mensaje.
//
// Autorizar (OAuth 2.1, registro dinámico y PKCE, como piden los conectores de Claude): en el teléfono,
// Ajustes → «Conectar un agente» crea un permiso firmado con su llave (nombre, nivel y el SHA-256 de un
// código); la persona escribe ese código en la página de autorización. La Mac y el sitio verifican la
// firma en cada llamada (un teléfono desvinculado deja de valer), así que Condx Cloud no puede
// inventar permisos.
//
// Cada llamada es una fila `_mcp-<id>` en el alcance de hilos del daemon: la Mac y el sitio ya escuchan
// ese alcance (el timbre en vivo), contestan en la misma fila (`result`, o `siteResult` el sitio) y la
// ruta HTTP espera la respuesta y borra la fila.

import { v } from "convex/values";
import { internalMutation, internalQuery, type MutationCtx } from "./_generated/server";
import type { Op } from "localfirst/server";
import { MAC_GONE_MS, principalFromToken } from "./auth";
import { applyOp } from "./sync";
import { wakeIfMacGone } from "./boat";

export const LEVELS = ["read", "reply", "orchestrate"] as const;
/** Cuánto vale el código del teléfono para autorizar. */
export const GRANT_TTL_MS = 10 * 60_000;
const AUTH_CODE_TTL_MS = 2 * 60_000;
const ACCESS_TTL_MS = 7 * 24 * 3600_000;
const REFRESH_TTL_MS = 90 * 24 * 3600_000;
/** Quién escribe las filas de las llamadas en el registro de operaciones. */
const PRINCIPAL = "cloud:mcp";

export function randomHex(bytes: number): string {
  return [...crypto.getRandomValues(new Uint8Array(bytes))].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Lo que firma el teléfono al crear un permiso (la Mac y el sitio lo rearman igual para verificar). */
export function grantMessage(g: { daemonId: string; grantId: string; name: string; level: string; codeHash: string; signedAt: number }): string {
  return `condx-mcp-grant:v1|${g.daemonId}|${g.grantId}|${g.name}|${g.level}|${g.codeHash}|${g.signedAt}`;
}

// ── El permiso (lo crea el teléfono) ──

/** El teléfono del token (activo, no el sitio), con su llave: solo un teléfono crea permisos. */
export const phoneOf = internalQuery({
  args: { token: v.string() },
  returns: v.union(v.null(), v.object({ daemonId: v.string(), deviceId: v.string(), publicKey: v.string() })),
  handler: async (ctx, { token }) => {
    try {
      const p = await principalFromToken(ctx, token);
      return p.kind === "device" && !p.site ? { daemonId: p.daemonId, deviceId: p.deviceId, publicKey: p.publicKey } : null;
    } catch {
      return null;
    }
  },
});

export const createGrant = internalMutation({
  args: { daemonId: v.string(), deviceId: v.string(), deviceKey: v.string(), grantId: v.string(), name: v.string(), level: v.string(), codeHash: v.string(), signedAt: v.number(), signature: v.string() },
  returns: v.null(),
  handler: async (ctx, a) => {
    if (await ctx.db.query("mcpGrants").withIndex("by_grantId", (q) => q.eq("grantId", a.grantId)).unique()) return null;
    const now = Date.now();
    await ctx.db.insert("mcpGrants", { ...a, status: "open", expiresAt: now + GRANT_TTL_MS, createdAt: now });
    return null;
  },
});

/** Los permisos de un daemon (para el teléfono: quién está conectado). */
export const grantsOf = internalQuery({
  args: { daemonId: v.string() },
  returns: v.array(v.object({ grantId: v.string(), name: v.string(), level: v.string(), status: v.string(), lastUsedAt: v.union(v.number(), v.null()), createdAt: v.number() })),
  handler: async (ctx, { daemonId }) => {
    const all = await ctx.db.query("mcpGrants").withIndex("by_daemonId", (q) => q.eq("daemonId", daemonId)).take(50);
    return all.filter((g) => g.status === "active").map((g) => ({ grantId: g.grantId, name: g.name, level: g.level, status: g.status, lastUsedAt: g.lastUsedAt ?? null, createdAt: g.createdAt }));
  },
});

export const revokeGrant = internalMutation({
  args: { daemonId: v.string(), grantId: v.string() },
  returns: v.boolean(),
  handler: async (ctx, { daemonId, grantId }) => {
    const g = await ctx.db.query("mcpGrants").withIndex("by_grantId", (q) => q.eq("grantId", grantId)).unique();
    if (!g || g.daemonId !== daemonId) return false;
    await ctx.db.patch("mcpGrants", g._id, { status: "revoked" });
    for (const t of await ctx.db.query("mcpTokens").withIndex("by_grantId", (q) => q.eq("grantId", grantId)).take(50)) await ctx.db.delete("mcpTokens", t._id);
    return true;
  },
});

// ── OAuth ──

export const registerClient = internalMutation({
  args: { name: v.string(), redirectUris: v.array(v.string()) },
  returns: v.string(),
  handler: async (ctx, { name, redirectUris }) => {
    const clientId = `cx_client_${randomHex(12)}`;
    await ctx.db.insert("mcpClients", { clientId, name: name.slice(0, 80), redirectUris: redirectUris.slice(0, 10), createdAt: Date.now() });
    return clientId;
  },
});

export const client = internalQuery({
  args: { clientId: v.string() },
  returns: v.union(v.null(), v.object({ name: v.string(), redirectUris: v.array(v.string()) })),
  handler: async (ctx, { clientId }) => {
    const c = await ctx.db.query("mcpClients").withIndex("by_clientId", (q) => q.eq("clientId", clientId)).unique();
    return c ? { name: c.name, redirectUris: c.redirectUris } : null;
  },
});

/** El código del teléfono vale: el permiso queda de este cliente y sale un código de autorización. */
export const authorize = internalMutation({
  args: { codeHash: v.string(), clientId: v.string(), redirectUri: v.string(), challenge: v.string() },
  returns: v.union(v.object({ ok: v.literal(true), code: v.string(), name: v.string() }), v.object({ ok: v.literal(false), error: v.string() })),
  handler: async (ctx, a) => {
    const g = await ctx.db.query("mcpGrants").withIndex("by_codeHash", (q) => q.eq("codeHash", a.codeHash)).unique();
    if (!g || g.status !== "open" || g.expiresAt < Date.now()) return { ok: false as const, error: "That code isn't valid or expired. Create a new one on the phone." };
    await ctx.db.patch("mcpGrants", g._id, { status: "active", clientId: a.clientId });
    const code = randomHex(24);
    await ctx.db.insert("mcpCodes", { codeHash: await sha(code), grantId: g.grantId, clientId: a.clientId, redirectUri: a.redirectUri, challenge: a.challenge, expiresAt: Date.now() + AUTH_CODE_TTL_MS, used: false });
    return { ok: true as const, code, name: g.name };
  },
});

async function sha(text: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function issue(ctx: MutationCtx, grantId: string, clientId: string) {
  const access = `cx_mcp_${randomHex(24)}`;
  const refresh = `cx_mcpr_${randomHex(24)}`;
  const now = Date.now();
  await ctx.db.insert("mcpTokens", { tokenHash: await sha(access), refreshHash: await sha(refresh), grantId, clientId, expiresAt: now + ACCESS_TTL_MS, refreshExpiresAt: now + REFRESH_TTL_MS });
  return { access_token: access, refresh_token: refresh, token_type: "Bearer", expires_in: Math.floor(ACCESS_TTL_MS / 1000) };
}

const tokenReply = v.union(v.object({ access_token: v.string(), refresh_token: v.string(), token_type: v.string(), expires_in: v.number() }), v.object({ error: v.string() }));

/** El código de autorización por los tokens (con PKCE: `challenge` es el S256 del verificador). */
export const exchange = internalMutation({
  args: { codeHash: v.string(), clientId: v.string(), redirectUri: v.string(), challenge: v.string() },
  returns: tokenReply,
  handler: async (ctx, a) => {
    const c = await ctx.db.query("mcpCodes").withIndex("by_codeHash", (q) => q.eq("codeHash", a.codeHash)).unique();
    if (!c || c.used || c.expiresAt < Date.now() || c.clientId !== a.clientId || c.redirectUri !== a.redirectUri || c.challenge !== a.challenge) return { error: "invalid_grant" };
    await ctx.db.patch("mcpCodes", c._id, { used: true });
    return await issue(ctx, c.grantId, c.clientId);
  },
});

/** Renovar: el token de renovación se usa una vez (sale otro par). */
export const refresh = internalMutation({
  args: { refreshHash: v.string(), clientId: v.string() },
  returns: tokenReply,
  handler: async (ctx, a) => {
    const t = await ctx.db.query("mcpTokens").withIndex("by_refreshHash", (q) => q.eq("refreshHash", a.refreshHash)).unique();
    if (!t || t.clientId !== a.clientId || t.refreshExpiresAt < Date.now()) return { error: "invalid_grant" };
    const g = await ctx.db.query("mcpGrants").withIndex("by_grantId", (q) => q.eq("grantId", t.grantId)).unique();
    if (!g || g.status !== "active") return { error: "invalid_grant" };
    await ctx.db.delete("mcpTokens", t._id);
    return await issue(ctx, t.grantId, t.clientId);
  },
});

const grantView = v.object({ grantId: v.string(), daemonId: v.string(), name: v.string(), level: v.string(), deviceKey: v.string(), codeHash: v.string(), signedAt: v.number(), signature: v.string(), tools: v.union(v.string(), v.null()) });

/** El permiso de un token de acceso (vigente y activo), o null. */
export const byToken = internalQuery({
  args: { tokenHash: v.string() },
  returns: v.union(v.null(), grantView),
  handler: async (ctx, { tokenHash }) => {
    const t = await ctx.db.query("mcpTokens").withIndex("by_tokenHash", (q) => q.eq("tokenHash", tokenHash)).unique();
    if (!t || t.expiresAt < Date.now()) return null;
    const g = await ctx.db.query("mcpGrants").withIndex("by_grantId", (q) => q.eq("grantId", t.grantId)).unique();
    if (!g || g.status !== "active") return null;
    return { grantId: g.grantId, daemonId: g.daemonId, name: g.name, level: g.level, deviceKey: g.deviceKey, codeHash: g.codeHash, signedAt: g.signedAt, signature: g.signature, tools: g.tools ?? null };
  },
});

// ── Las llamadas ──

const threadsOf = (daemonId: string) => `threads:${daemonId}`;
const op = (scope: string, partial: Pick<Op, "kind" | "rowId"> & Partial<Op>): Op => ({ opId: `mcp-${randomHex(8)}`, scope, fieldsVersion: 0, madeAt: Date.now(), ...partial });

/** Una llamada: la fila `_mcp-<id>` con el mensaje y el permiso firmado. Si la Mac no late, despierta al sitio. */
export const enqueue = internalMutation({
  args: { grantId: v.string(), msg: v.string() },
  returns: v.object({ rowId: v.string(), callId: v.string() }),
  handler: async (ctx, { grantId, msg }) => {
    const g = await ctx.db.query("mcpGrants").withIndex("by_grantId", (q) => q.eq("grantId", grantId)).unique();
    if (!g || g.status !== "active") throw new Error("no grant");
    const callId = randomHex(10);
    const rowId = `_mcp-${callId}`;
    const grant = JSON.stringify({ grantId: g.grantId, name: g.name, level: g.level, deviceKey: g.deviceKey, codeHash: g.codeHash, signedAt: g.signedAt, signature: g.signature });
    await applyOp(ctx, PRINCIPAL, op(threadsOf(g.daemonId), { kind: "create", rowId, set: { mcpId: callId, mcpMsg: msg, mcpGrant: grant, requestedAt: Date.now() } }));
    await ctx.db.patch("mcpGrants", g._id, { lastUsedAt: Date.now() });
    await wakeIfMacGone(ctx, g.daemonId);
    return { rowId, callId };
  },
});

/** La respuesta de una llamada (de la Mac o del sitio), si ya llegó; y si alguien la puede contestar. */
export const callState = internalQuery({
  args: { daemonId: v.string(), rowId: v.string(), callId: v.string() },
  returns: v.object({ result: v.union(v.string(), v.null()), macAlive: v.boolean() }),
  handler: async (ctx, { daemonId, rowId, callId }) => {
    const scope = threadsOf(daemonId);
    const row = await ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", scope).eq("rowId", rowId)).unique();
    const vals = (row?.values ?? {}) as Record<string, unknown>;
    const result = vals.resultFor === callId ? (vals.result as string) : vals.siteFor === callId ? (vals.siteResult as string) : null;
    const mac = await ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", scope).eq("rowId", "_mac")).unique();
    const aliveAt = (mac?.values as { aliveAt?: unknown } | undefined)?.aliveAt;
    return { result: typeof result === "string" ? result : null, macAlive: typeof aliveAt === "number" && Date.now() - aliveAt < MAC_GONE_MS };
  },
});

/** Contestada (o vencida): se borra la fila; una lista de herramientas queda para cuando nadie esté. */
export const finish = internalMutation({
  args: { daemonId: v.string(), rowId: v.string(), grantId: v.string(), tools: v.optional(v.string()) },
  returns: v.null(),
  handler: async (ctx, { daemonId, rowId, grantId, tools }) => {
    try {
      await applyOp(ctx, PRINCIPAL, op(threadsOf(daemonId), { kind: "delete", rowId }));
    } catch {
      // Ya no estaba.
    }
    if (tools) {
      const g = await ctx.db.query("mcpGrants").withIndex("by_grantId", (q) => q.eq("grantId", grantId)).unique();
      if (g) await ctx.db.patch("mcpGrants", g._id, { tools });
    }
    return null;
  },
});
