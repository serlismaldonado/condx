// Releases de la app del teléfono (docs/design/cloud-site.md §9.3): el daemon sube la APK al
// storage (por una URL de subida: pesa más de lo que acepta una ruta HTTP) y la publica; cada versión
// tiene una llave al azar y el enlace de descarga la lleva (`/download/android?k=…`): sin la llave no
// se baja, y no se adivina. Quedan las tres últimas por daemon.

import { v } from "convex/values";
import { internalMutation, internalQuery } from "./_generated/server";

const KEEP = 3;

export const publish = internalMutation({
  args: { daemonId: v.string(), platform: v.string(), version: v.string(), versionCode: v.number(), storageId: v.id("_storage"), sha256: v.string(), size: v.number(), key: v.string() },
  handler: async (ctx, a) => {
    await ctx.db.insert("releases", { ...a, createdAt: Date.now() });
    const all = await ctx.db
      .query("releases")
      .withIndex("by_daemon_platform", (q) => q.eq("daemonId", a.daemonId).eq("platform", a.platform))
      .order("desc")
      .collect();
    for (const old of all.slice(KEEP)) {
      await ctx.storage.delete(old.storageId);
      await ctx.db.delete(old._id);
    }
  },
});

/** La versión con esa llave (si no, null: no se distingue «no existe» de «llave equivocada»). */
export const byKey = internalQuery({
  args: { key: v.string() },
  handler: async (ctx, { key }) => {
    const r = await ctx.db.query("releases").withIndex("by_key", (q) => q.eq("key", key)).unique();
    if (!r) return null;
    return { version: r.version, url: await ctx.storage.getUrl(r.storageId) };
  },
});
