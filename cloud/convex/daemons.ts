// Registro de daemons. Interno: lo llama el propio daemon al vincularse, con la deploy key de su
// deployment de Condx Cloud (`npx convex run daemons:register`), nunca un cliente cualquiera.

import { ConvexError, v } from "convex/values";
import { internalMutation } from "./_generated/server";
import { sha256Hex } from "./auth";

export const register = internalMutation({
  args: { daemonId: v.string(), token: v.string(), name: v.string() },
  returns: v.null(),
  handler: async (ctx, { daemonId, token, name }) => {
    if (!/^[a-z0-9-]{8,64}$/.test(daemonId)) throw new ConvexError({ code: "invalid", message: "daemonId: 8–64 lowercase letters, digits or hyphens" });
    if (token.length < 32) throw new ConvexError({ code: "invalid", message: "token too short" });
    const tokenHash = await sha256Hex(token);
    const existing = await ctx.db.query("daemons").withIndex("by_daemonId", (q) => q.eq("daemonId", daemonId)).unique();
    if (existing) await ctx.db.patch("daemons", existing._id, { tokenHash, name });
    else await ctx.db.insert("daemons", { daemonId, tokenHash, name, createdAt: Date.now() });
    return null;
  },
});

/** Desvincular: el token deja de valer. Los datos quedan hasta que se borren. */
export const unregister = internalMutation({
  args: { daemonId: v.string() },
  returns: v.null(),
  handler: async (ctx, { daemonId }) => {
    const existing = await ctx.db.query("daemons").withIndex("by_daemonId", (q) => q.eq("daemonId", daemonId)).unique();
    if (existing) await ctx.db.delete("daemons", existing._id);
    return null;
  },
});
