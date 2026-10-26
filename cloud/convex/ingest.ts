// Ingesta de eventos (docs/design/agent-flows.md, §2.8): el SDK de Condx de cualquier app de Convex
// manda aquí los errores de su frontend (con la llave pública, desde el navegador), los de su
// backend (con la secreta, desde el componente de Convex) y los reportes de sus usuarios. Se guardan
// recortados en el alcance `events:<daemonId>`, agrupados por huella (un error que se repite suma),
// y el daemon los recoge por sincronización y los convierte en eventos que disparan flujos.
//
// Límites: 50 eventos por pedido, 120 por minuto por llave, textos recortados, capturas de pantalla
// de hasta 300 KB solo en reportes. Nada aquí son secretos de la app: el SDK no manda cookies, ni
// argumentos de funciones, ni lo que la persona escribe en los campos.

import { v } from "convex/values";
import { internalMutation, internalQuery, mutation, query, type MutationCtx } from "./_generated/server";
import { principalFromToken, sha256Hex } from "./auth";
import { applyOp } from "./sync";

const MAX_EVENTS = 50;
const PER_MINUTE = 120;
const MAX_SCREENSHOT = 300_000;

function randomKey(prefix: string): string {
  const bytes = new Uint8Array(24);
  crypto.getRandomValues(bytes);
  return prefix + Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function cap(value: unknown, max: number): string | undefined {
  if (typeof value !== "string") return undefined;
  const t = value.trim();
  if (!t) return undefined;
  return t.length > max ? `${t.slice(0, max)}…` : t;
}

/** Una ruta sin dominio, query ni fragmento (pueden llevar datos de la persona). */
function pathOnly(url: unknown): string | undefined {
  const t = cap(url, 600);
  if (!t) return undefined;
  try {
    return cap(new URL(t, "https://x.invalid").pathname, 300);
  } catch {
    return cap(t.split(/[?#]/)[0], 300);
  }
}

/** Tokens, llaves y correos fuera del texto antes de guardarlo. */
export function redact(text: string): string {
  return text
    .replace(/\b(sk|pk|rk|ck)_(live|test|pub|sec)?_?[A-Za-z0-9]{12,}\b/g, "[redacted]")
    .replace(/\b(prod|dev|preview):[A-Za-z0-9|_-]{16,}\b/g, "[redacted]")
    .replace(/\bBearer\s+[A-Za-z0-9._-]{12,}/gi, "Bearer [redacted]")
    .replace(/eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}/g, "[redacted]")
    .replace(/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g, "[email]");
}

export type CleanEvent = {
  type: "error" | "report";
  source: "browser" | "server";
  fingerprint: string;
  message: string;
  stack?: string;
  url?: string;
  release?: string;
  app?: string;
  fn?: string;
  breadcrumbs?: string;
  count: number;
  at: number;
  reportText?: string;
  reportEmail?: string;
  screenshot?: string;
};

/** Lo que se acepta de un evento: solo campos conocidos, recortados y sin secretos. */
export function clean(raw: unknown, source: "browser" | "server", now: number): CleanEvent | null {
  if (!raw || typeof raw !== "object") return null;
  const e = raw as Record<string, unknown>;
  const type = e.type === "report" ? "report" : "error";
  const message = cap(e.message, 1000);
  const reportText = type === "report" ? cap(e.text ?? (e.report as Record<string, unknown> | undefined)?.text, 4000) : undefined;
  if (!message && !reportText) return null;
  const crumbs = Array.isArray(e.breadcrumbs) ? e.breadcrumbs.slice(-20).map((b) => cap(typeof b === "string" ? b : JSON.stringify(b), 200)).filter(Boolean) : [];
  const shot = type === "report" && typeof e.screenshot === "string" && e.screenshot.startsWith("data:image/") && e.screenshot.length <= MAX_SCREENSHOT ? e.screenshot : undefined;
  const fp = cap(e.fingerprint, 200) ?? cap(`${type}:${message ?? ""}`, 200)!;
  const count = typeof e.count === "number" && e.count > 0 ? Math.min(Math.floor(e.count), 10_000) : 1;
  return {
    type,
    source,
    fingerprint: fp,
    message: redact(message ?? "User report"),
    stack: cap(e.stack, 4000) && redact(cap(e.stack, 4000)!),
    url: pathOnly(e.url),
    release: cap(e.release, 80),
    app: cap(e.app, 80),
    fn: cap(e.function ?? e.fn, 200),
    breadcrumbs: crumbs.length ? redact(crumbs.join("\n")) : undefined,
    count,
    at: now,
    reportText: reportText && redact(reportText),
    reportEmail: type === "report" ? cap((e.email as string) ?? (e.report as Record<string, unknown> | undefined)?.email, 200) : undefined,
    screenshot: shot,
  };
}

async function rowIdFor(project: string, ev: CleanEvent, now: number): Promise<string> {
  // Un error que se repite es la misma fila; cada reporte de una persona, una nueva.
  const basis = ev.type === "report" ? `${project}|report|${now}|${Math.random()}` : `${project}|${ev.source}|${ev.fingerprint}`;
  return "ev-" + (await sha256Hex(basis)).slice(0, 24);
}

async function opId(): Promise<string> {
  return "ingest-" + randomKey("");
}

/** Guarda los eventos en el alcance del daemon (agrupando los que se repiten). */
export const record = internalMutation({
  args: { daemonId: v.string(), project: v.string(), source: v.union(v.literal("browser"), v.literal("server")), events: v.array(v.any()), key: v.string() },
  returns: v.object({ stored: v.number(), dropped: v.number() }),
  handler: async (ctx, { daemonId, project, source, events, key }) => {
    const now = Date.now();
    const minute = Math.floor(now / 60_000);
    const rate = await ctx.db.query("ingestRate").withIndex("by_key_and_minute", (q) => q.eq("key", key).eq("minute", minute)).unique();
    const used = rate?.count ?? 0;
    const room = Math.max(0, PER_MINUTE - used);
    const accepted = events.slice(0, Math.min(MAX_EVENTS, room));
    let stored = 0;
    for (const raw of accepted) {
      const ev = clean(raw, source, now);
      if (!ev) continue;
      await upsert(ctx, `events:${daemonId}`, await rowIdFor(project, ev, now), project, ev);
      stored++;
    }
    if (rate) await ctx.db.patch("ingestRate", rate._id, { count: used + accepted.length });
    else await ctx.db.insert("ingestRate", { key, minute, count: accepted.length });
    return { stored, dropped: events.length - stored };
  },
});

async function upsert(ctx: MutationCtx, scope: string, rowId: string, project: string, ev: CleanEvent): Promise<void> {
  const existing = await ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", scope).eq("rowId", rowId)).unique();
  const live = existing && !existing.deleted ? existing : null;
  const set: Record<string, unknown> = {
    project,
    type: ev.type,
    source: ev.source,
    fingerprint: ev.fingerprint,
    message: ev.message,
    stack: ev.stack ?? null,
    url: ev.url ?? null,
    release: ev.release ?? null,
    app: ev.app ?? null,
    fn: ev.fn ?? null,
    breadcrumbs: ev.breadcrumbs ?? null,
    reportText: ev.reportText ?? null,
    reportEmail: ev.reportEmail ?? null,
    screenshot: ev.screenshot ?? null,
    lastAt: ev.at,
    count: ((live?.values.count as number | undefined) ?? 0) + ev.count,
  };
  if (!live) set.firstAt = ev.at;
  await applyOp(ctx, "ingest", { opId: await opId(), scope, kind: live ? "patch" : "create", rowId, set, fieldsVersion: 0, madeAt: ev.at } as never);
}

/** La llave de un pedido: la pública (navegador) o la secreta (componente). */
export const keyFor = internalQuery({
  args: { publicKey: v.optional(v.string()), secret: v.optional(v.string()) },
  returns: v.union(v.null(), v.object({ daemonId: v.string(), project: v.string(), source: v.union(v.literal("browser"), v.literal("server")), key: v.string() })),
  handler: async (ctx, { publicKey, secret }) => {
    if (secret) {
      const hash = await sha256Hex(secret);
      const k = await ctx.db.query("ingestKeys").withIndex("by_secretHash", (q) => q.eq("secretHash", hash)).unique();
      return k && !k.revokedAt ? { daemonId: k.daemonId, project: k.project, source: "server" as const, key: k.publicKey } : null;
    }
    if (publicKey) {
      const k = await ctx.db.query("ingestKeys").withIndex("by_publicKey", (q) => q.eq("publicKey", publicKey)).unique();
      return k && !k.revokedAt ? { daemonId: k.daemonId, project: k.project, source: "browser" as const, key: k.publicKey } : null;
    }
    return null;
  },
});

/** El daemon crea una llave para un proyecto: la secreta se ve una sola vez. */
export const createKey = mutation({
  args: { token: v.string(), project: v.string() },
  returns: v.object({ publicKey: v.string(), secret: v.string() }),
  handler: async (ctx, { token, project }) => {
    const p = await principalFromToken(ctx, token);
    if (p.kind !== "daemon") throw new Error("only the daemon creates ingest keys");
    if (!/^[A-Za-z0-9._-]{1,80}$/.test(project)) throw new Error("invalid project name");
    const publicKey = randomKey("ck_pub_");
    const secret = randomKey("ck_sec_");
    await ctx.db.insert("ingestKeys", { daemonId: p.daemonId, project, publicKey, secretHash: await sha256Hex(secret), createdAt: Date.now() });
    return { publicKey, secret };
  },
});

export const listKeys = query({
  args: { token: v.string() },
  returns: v.array(v.object({ project: v.string(), publicKey: v.string(), createdAt: v.number(), revoked: v.boolean() })),
  handler: async (ctx, { token }) => {
    const p = await principalFromToken(ctx, token);
    if (p.kind !== "daemon") return [];
    const keys = await ctx.db.query("ingestKeys").withIndex("by_daemonId", (q) => q.eq("daemonId", p.daemonId)).take(100);
    return keys.map((k) => ({ project: k.project, publicKey: k.publicKey, createdAt: k.createdAt, revoked: k.revokedAt !== undefined }));
  },
});

export const revokeKey = mutation({
  args: { token: v.string(), publicKey: v.string() },
  returns: v.boolean(),
  handler: async (ctx, { token, publicKey }) => {
    const p = await principalFromToken(ctx, token);
    const k = await ctx.db.query("ingestKeys").withIndex("by_publicKey", (q) => q.eq("publicKey", publicKey)).unique();
    if (p.kind !== "daemon" || !k || k.daemonId !== p.daemonId) return false;
    await ctx.db.patch("ingestKeys", k._id, { revokedAt: Date.now() });
    return true;
  },
});
