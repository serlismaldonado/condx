// Adjuntos de los hilos (fase 3 de los chats): el teléfono sube el archivo YA CIFRADO con age para la
// Mac (la nube no puede leerlo), el daemon dueño lo baja y lo borra. Un teléfono solo sube para su
// daemon; solo ese daemon baja o borra. Lo que nadie bajó en un día se borra solo.

import { v } from "convex/values";
import { internalMutation, internalQuery } from "./_generated/server";
import { principalFromToken } from "./auth";

/** Tope por archivo (ya cifrado): una foto reducida o unos minutos de audio. */
export const MAX_BYTES = 10 * 1024 * 1024;
const KEEP_MS = 24 * 60 * 60 * 1000;

/** Quién es el token (un teléfono activo o el daemon): lo que la ruta HTTP necesita saber. */
export const whoIs = internalQuery({
  args: { token: v.string() },
  handler: async (ctx, { token }) => {
    try {
      const p = await principalFromToken(ctx, token);
      return p.kind === "device" ? { kind: "device" as const, daemonId: p.daemonId, deviceId: p.deviceId, site: p.site === true } : { kind: "daemon" as const, daemonId: p.daemonId, deviceId: null, site: false };
    } catch {
      return null;
    }
  },
});

export const record = internalMutation({
  args: { daemonId: v.string(), deviceId: v.string(), storageId: v.id("_storage"), size: v.number() },
  handler: async (ctx, a) => {
    const now = Date.now();
    // De paso, lo viejo que nadie bajó.
    for (const old of await ctx.db.query("files").withIndex("by_createdAt", (q) => q.lt("createdAt", now - KEEP_MS)).take(20)) {
      await ctx.storage.delete(old.storageId);
      await ctx.db.delete(old._id);
    }
    return await ctx.db.insert("files", { ...a, createdAt: now });
  },
});

/** El archivo si es de ese daemon (si no, null: no se distingue «no existe» de «no es tuyo»). */
export const forDaemon = internalQuery({
  args: { id: v.string(), daemonId: v.string() },
  handler: async (ctx, { id, daemonId }) => {
    const fid = ctx.db.normalizeId("files", id);
    const f = fid ? await ctx.db.get(fid) : null;
    return f && f.daemonId === daemonId ? f : null;
  },
});

export const remove = internalMutation({
  args: { id: v.id("files") },
  handler: async (ctx, { id }) => {
    const f = await ctx.db.get(id);
    if (f) {
      await ctx.storage.delete(f.storageId);
      await ctx.db.delete(id);
    }
  },
});
