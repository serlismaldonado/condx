// El componente de Condx en el deployment de la app: guarda los errores de su backend (agrupados) y
// los reportes de sus usuarios, y los reenvía en lotes a Condx Cloud con la llave secreta. Los
// componentes no ven las variables de entorno de la app: la app pasa `endpoint` y `secret` (las
// lee de las suyas, CONDX_ENDPOINT y CONDX_SECRET, en la clase `Condx`).

import { v } from "convex/values";
import { internal } from "./_generated/api";
import { internalAction, internalMutation, internalQuery, mutation, query } from "./_generated/server";

const target = { endpoint: v.optional(v.string()), secret: v.optional(v.string()) };
const FIRST_DELAY = 10_000;
const MAX_ATTEMPTS = 6;

function cap(text: string | undefined, max: number): string | undefined {
  if (!text) return undefined;
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

/** The same message with different ids or numbers is the same error. */
export function normalize(message: string): string {
  return message
    .replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi, "<id>")
    .replace(/\b[0-9a-z]{20,}\b/gi, "<id>")
    .replace(/\d+/g, "<n>")
    .slice(0, 200);
}

export const recordError = mutation({
  args: { message: v.string(), stack: v.optional(v.string()), fn: v.optional(v.string()), ...target },
  returns: v.null(),
  handler: async (ctx, { message, stack, fn, endpoint, secret }) => {
    const now = Date.now();
    const fingerprint = `${fn ?? ""}|${normalize(message)}`;
    const existing = await ctx.db.query("errors").withIndex("by_fingerprint", (q) => q.eq("fingerprint", fingerprint)).unique();
    if (existing) await ctx.db.patch(existing._id, { count: existing.count + 1, lastAt: now, message: cap(message, 1000)!, stack: cap(stack, 4000) });
    else await ctx.db.insert("errors", { fingerprint, message: cap(message, 1000)!, stack: cap(stack, 4000), fn, count: 1, sentCount: 0, firstAt: now, lastAt: now });
    await schedule(ctx, endpoint, secret);
    return null;
  },
});

export const recordReport = mutation({
  args: { text: v.string(), email: v.optional(v.string()), url: v.optional(v.string()), app: v.optional(v.string()), ...target },
  returns: v.null(),
  handler: async (ctx, { text, email, url, app, endpoint, secret }) => {
    const t = text.trim();
    if (!t) return null;
    await ctx.db.insert("reports", { text: cap(t, 4000)!, email: cap(email, 200), url: cap(url, 300), app: cap(app, 80), at: Date.now(), sent: false });
    await schedule(ctx, endpoint, secret);
    return null;
  },
});

async function schedule(ctx: { db: any; scheduler: any }, endpoint?: string, secret?: string): Promise<void> {
  if (!endpoint || !secret) return;
  const pending = await ctx.db.query("outbox").first();
  if (pending) return;
  await ctx.db.insert("outbox", { scheduledAt: Date.now(), attempts: 0 });
  await ctx.scheduler.runAfter(FIRST_DELAY, internal.lib.forward, { endpoint, secret });
}

/** Lo que falta reenviar. */
export const pending = internalQuery({
  args: {},
  returns: v.any(),
  handler: async (ctx) => {
    const errors = (await ctx.db.query("errors").withIndex("by_lastAt").order("desc").take(200)).filter((e) => e.count > e.sentCount).slice(0, 50);
    const reports = await ctx.db.query("reports").withIndex("by_sent", (q) => q.eq("sent", false)).take(20);
    return { errors, reports };
  },
});

export const markSent = internalMutation({
  args: { errors: v.array(v.object({ id: v.id("errors"), count: v.number() })), reports: v.array(v.id("reports")), done: v.boolean() },
  returns: v.null(),
  handler: async (ctx, { errors, reports, done }) => {
    for (const e of errors) {
      const row = await ctx.db.get(e.id);
      if (row) await ctx.db.patch(e.id, { sentCount: Math.max(row.sentCount, e.count) });
    }
    for (const r of reports) await ctx.db.patch(r, { sent: true });
    if (done) for (const o of await ctx.db.query("outbox").collect()) await ctx.db.delete(o._id);
    return null;
  },
});

export const retryLater = internalMutation({
  args: { endpoint: v.string(), secret: v.string() },
  returns: v.null(),
  handler: async (ctx, { endpoint, secret }) => {
    const o = await ctx.db.query("outbox").first();
    const attempts = (o?.attempts ?? 0) + 1;
    if (attempts > MAX_ATTEMPTS) {
      if (o) await ctx.db.delete(o._id);
      return null;
    }
    if (o) await ctx.db.patch(o._id, { attempts });
    else await ctx.db.insert("outbox", { scheduledAt: Date.now(), attempts });
    await ctx.scheduler.runAfter(FIRST_DELAY * 2 ** attempts, internal.lib.forward, { endpoint, secret });
    return null;
  },
});

/** Reenvía lo pendiente a Condx Cloud (`POST <endpoint>/ingest`, con la secreta). */
export const forward = internalAction({
  args: { endpoint: v.string(), secret: v.string() },
  returns: v.null(),
  handler: async (ctx, { endpoint, secret }) => {
    const { errors, reports } = await ctx.runQuery(internal.lib.pending, {});
    if (errors.length === 0 && reports.length === 0) {
      await ctx.runMutation(internal.lib.markSent, { errors: [], reports: [], done: true });
      return null;
    }
    const events = [
      ...errors.map((e: any) => ({ type: "error", message: e.message, stack: e.stack, function: e.fn, fingerprint: e.fingerprint, count: e.count - e.sentCount })),
      ...reports.map((r: any) => ({ type: "report", text: r.text, email: r.email, url: r.url, app: r.app })),
    ];
    let ok = false;
    try {
      const res = await fetch(`${endpoint.replace(/\/$/, "")}/ingest`, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${secret}` }, body: JSON.stringify({ events }) });
      ok = res.ok;
    } catch {
      ok = false;
    }
    if (!ok) {
      await ctx.runMutation(internal.lib.retryLater, { endpoint, secret });
      return null;
    }
    await ctx.runMutation(internal.lib.markSent, { errors: errors.map((e: any) => ({ id: e._id, count: e.count })), reports: reports.map((r: any) => r._id), done: true });
    return null;
  },
});

/** Los últimos errores y reportes (para una pantalla de administración en la app). */
export const recent = query({
  args: { limit: v.optional(v.number()) },
  returns: v.any(),
  handler: async (ctx, { limit }) => {
    const n = Math.min(limit ?? 20, 100);
    return {
      errors: await ctx.db.query("errors").withIndex("by_lastAt").order("desc").take(n),
      reports: await ctx.db.query("reports").order("desc").take(n),
    };
  },
});
