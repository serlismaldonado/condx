// El sitio en la nube en boat (docs/design/cloud-site.md, S1): detenido no cobra, y lo despierta
// Condx Cloud cuando un teléfono deja trabajo y la Mac no está (su latido, `aliveAt` en la fila
// `_mac` de los hilos, tiene más de 3 min o es 0). Va por eventos: una llamada solo cuando llega algo.
//
// Se configura con dos variables del deployment: `BOAT_API_KEY` (una key de boat solo de esa
// máquina: `sandbox.read,sandbox.stop,sandbox.resume`) y `BOAT_SANDBOX_ID`. Sin ellas no hace nada.

import { v } from "convex/values";
import { internalAction, internalMutation, type MutationCtx } from "./_generated/server";
import { internal } from "./_generated/api";

/** Un latido más viejo que esto: la Mac está apagada (late cada minuto; al irse en orden avisa con 0). */
const MAC_GONE_MS = 3 * 60_000;
/** Entre dos intentos de despertarlo, al menos esto (una ráfaga de mensajes, un intento). */
const WAKE_EVERY_MS = 2 * 60_000;

function configured(): { key: string; sandbox: string } | null {
  const key = process.env.BOAT_API_KEY;
  const sandbox = process.env.BOAT_SANDBOX_ID;
  return key && sandbox ? { key, sandbox } : null;
}

/** Lo que escribió un teléfono: si la Mac no está, despertar al sitio (si no se intentó recién). */
export async function wakeIfMacGone(ctx: MutationCtx, daemonId: string): Promise<void> {
  if (!configured()) return;
  const mac = await ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", `threads:${daemonId}`).eq("rowId", "_mac")).unique();
  const values = mac?.values as { aliveAt?: unknown; handover?: unknown } | undefined;
  const now = Date.now();
  // Con la Mac viva no hace falta, salvo que el teléfono haya elegido que conteste la nube.
  if (typeof values?.aliveAt === "number" && now - values.aliveAt < MAC_GONE_MS && values.handover !== true) return;
  const last = await ctx.db.query("siteWakes").withIndex("by_daemonId", (q) => q.eq("daemonId", daemonId)).unique();
  if (last && now - last.at < WAKE_EVERY_MS) return;
  if (last) await ctx.db.patch("siteWakes", last._id, { at: now, result: "pending" });
  else await ctx.db.insert("siteWakes", { daemonId, at: now, result: "pending" });
  await ctx.scheduler.runAfter(0, internal.boat.wake, { daemonId });
}

/** Mira la máquina en boat y, si está detenida, la reanuda. */
export const wake = internalAction({
  args: { daemonId: v.string() },
  returns: v.null(),
  handler: async (ctx, { daemonId }) => {
    const c = configured();
    if (!c) return null;
    const api = process.env.CONDX_BOAT_API ?? "https://boat.dev/api/v1";
    const headers = { Authorization: `Bearer ${c.key}`, "Content-Type": "application/json" };
    let result: string;
    try {
      const r = await fetch(`${api}/sandboxes/${encodeURIComponent(c.sandbox)}`, { headers });
      const state = r.ok ? ((await r.json()) as { sandbox?: { state?: string } }).sandbox?.state : undefined;
      if (!r.ok) result = `boat answered ${r.status}`;
      else if (state === "archived") {
        const w = await fetch(`${api}/sandboxes/${encodeURIComponent(c.sandbox)}/resume`, { method: "POST", headers, body: "{}" });
        result = w.ok ? "resumed" : `resume answered ${w.status}`;
      } else result = `already ${state ?? "unknown"}`;
    } catch (e) {
      result = `couldn't reach boat: ${e instanceof Error ? e.message : String(e)}`.slice(0, 200);
    }
    await ctx.runMutation(internal.boat.record, { daemonId, result });
    return null;
  },
});

export const record = internalMutation({
  args: { daemonId: v.string(), result: v.string() },
  returns: v.null(),
  handler: async (ctx, { daemonId, result }) => {
    const last = await ctx.db.query("siteWakes").withIndex("by_daemonId", (q) => q.eq("daemonId", daemonId)).unique();
    if (last) await ctx.db.patch("siteWakes", last._id, { result });
    return null;
  },
});
