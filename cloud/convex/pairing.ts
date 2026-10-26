// Vincular un teléfono a un daemon:
//   1. la Mac abre un código (`open`, token del daemon) y lo muestra con un QR; vence en 5 minutos;
//   2. el teléfono lo reclama (`claim`, público) con su llave pública, su token y un nombre: queda
//      PENDIENTE; el código ya no sirve para otro;
//   3. la Mac ve el reclamo (`status`) con el nombre y la huella de la llave, y la persona confirma
//      (`confirm`) o cancela. Solo entonces el teléfono queda activo.
// El daemon guarda además la llave confirmada en su lado: un backend comprometido no puede meter
// otra llave en lo que el daemon acepta.

import { ConvexError, v } from "convex/values";
import { mutation, query, type MutationCtx } from "./_generated/server";
import { daemonFromToken, sha256Hex } from "./auth";

export const PAIRING_TTL_MS = 5 * 60 * 1000;
/** Sin 0/O, 1/I/L: se lee y se escribe sin dudas. */
const ALPHABET = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const CODE_LENGTH = 12;

function invalid(message: string): never {
  throw new ConvexError({ code: "invalid", message });
}

/** Un código como `K7Q2-M9XD-4HWR` (~59 bits). */
function newCode(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(CODE_LENGTH));
  const chars = [...bytes].map((b) => ALPHABET[b % ALPHABET.length]).join("");
  return `${chars.slice(0, 4)}-${chars.slice(4, 8)}-${chars.slice(8, 12)}`;
}

/** El código como lo escribió la persona: sin guiones ni espacios, en mayúsculas. */
export function normalizeCode(code: string): string {
  return code.toUpperCase().replace(/[^0-9A-Z]/g, "");
}

export const open = mutation({
  args: { token: v.string() },
  returns: v.object({ code: v.string(), expiresAt: v.number() }),
  handler: async (ctx, { token }) => {
    const daemon = await daemonFromToken(ctx, token);
    // Un solo código abierto por daemon: abrir otro cancela el anterior (y su teléfono pendiente).
    for await (const p of ctx.db.query("pairings").withIndex("by_daemonId", (q) => q.eq("daemonId", daemon.daemonId))) {
      if (p.status === "open" || p.status === "claimed") {
        await ctx.db.patch("pairings", p._id, { status: "cancelled" });
        if (p.deviceId) await revokePending(ctx, p.deviceId);
      }
    }
    const code = newCode();
    const expiresAt = Date.now() + PAIRING_TTL_MS;
    await ctx.db.insert("pairings", { codeHash: await sha256Hex(normalizeCode(code)), daemonId: daemon.daemonId, expiresAt, status: "open" });
    return { code, expiresAt };
  },
});

async function revokePending(ctx: MutationCtx, deviceId: string) {
  const d = await ctx.db.query("devices").withIndex("by_deviceId", (q) => q.eq("deviceId", deviceId)).unique();
  if (d && d.status === "pending") await ctx.db.patch("devices", d._id, { status: "revoked" });
}

export const claim = mutation({
  args: { code: v.string(), publicKey: v.string(), token: v.string(), name: v.string(), encryptionKey: v.optional(v.string()), kind: v.optional(v.union(v.literal("phone"), v.literal("site"))) },
  returns: v.object({ deviceId: v.string(), daemonId: v.string() }),
  handler: async (ctx, { code, publicKey, token, name, encryptionKey, kind }) => {
    if (!/^[0-9a-f]{64}$/.test(publicKey)) invalid("publicKey must be an Ed25519 key in hex");
    // La identidad de cifrado (age X25519) con la que el daemon le cifra las claves de los proveedores.
    if (encryptionKey !== undefined && !/^age1[02-9ac-hj-np-z]{58}$/.test(encryptionKey)) invalid("encryptionKey must be an age X25519 recipient");
    if (token.length < 32 || token.length > 128) invalid("token must be 32–128 characters");
    const cleanName = name.trim().slice(0, 40) || "Phone";
    const codeHash = await sha256Hex(normalizeCode(code));
    const p = await ctx.db.query("pairings").withIndex("by_codeHash", (q) => q.eq("codeHash", codeHash)).unique();
    // Mismo mensaje para «no existe», «vencido» y «ya usado»: no se puede tantear qué códigos hay.
    if (!p || p.status !== "open" || p.expiresAt < Date.now()) invalid("That code isn't valid (or expired). Open a new one on the Mac.");
    const tokenHash = await sha256Hex(token);
    const taken = await ctx.db.query("devices").withIndex("by_tokenHash", (q) => q.eq("tokenHash", tokenHash)).unique();
    if (taken) invalid("Use a new token for each pairing");
    const deviceId = `phone-${[...crypto.getRandomValues(new Uint8Array(8))].map((b) => b.toString(16).padStart(2, "0")).join("")}`;
    await ctx.db.insert("devices", { deviceId, daemonId: p.daemonId, name: cleanName, publicKey, encryptionKey, kind, tokenHash, status: "pending", createdAt: Date.now() });
    await ctx.db.patch("pairings", p._id, { status: "claimed", deviceId });
    return { deviceId, daemonId: p.daemonId };
  },
});

const deviceView = v.object({ deviceId: v.string(), name: v.string(), publicKey: v.string(), encryptionKey: v.optional(v.string()), kind: v.optional(v.string()), status: v.string(), createdAt: v.number() });

/** Lo que ve la Mac del código abierto: si ya lo reclamó un teléfono, cuál. */
export const status = query({
  args: { token: v.string() },
  returns: v.union(
    v.null(),
    v.object({ status: v.string(), expiresAt: v.number(), device: v.union(v.null(), deviceView) }),
  ),
  handler: async (ctx, { token }) => {
    const daemon = await daemonFromToken(ctx, token);
    const p = await ctx.db.query("pairings").withIndex("by_daemonId", (q) => q.eq("daemonId", daemon.daemonId)).order("desc").first();
    if (!p) return null;
    const d = p.deviceId ? await ctx.db.query("devices").withIndex("by_deviceId", (q) => q.eq("deviceId", p.deviceId!)).unique() : null;
    const expired = p.status === "open" && p.expiresAt < Date.now();
    return {
      status: expired ? "expired" : p.status,
      expiresAt: p.expiresAt,
      device: d ? { deviceId: d.deviceId, name: d.name, publicKey: d.publicKey, encryptionKey: d.encryptionKey, kind: d.kind, status: d.status, createdAt: d.createdAt } : null,
    };
  },
});

export const confirm = mutation({
  args: { token: v.string(), deviceId: v.string() },
  returns: v.null(),
  handler: async (ctx, { token, deviceId }) => {
    const daemon = await daemonFromToken(ctx, token);
    const p = await ctx.db.query("pairings").withIndex("by_daemonId", (q) => q.eq("daemonId", daemon.daemonId)).order("desc").first();
    if (!p || p.status !== "claimed" || p.deviceId !== deviceId) invalid("There's no pending phone with that id");
    const d = await ctx.db.query("devices").withIndex("by_deviceId", (q) => q.eq("deviceId", deviceId)).unique();
    if (!d || d.daemonId !== daemon.daemonId || d.status !== "pending") invalid("There's no pending phone with that id");
    await ctx.db.patch("devices", d._id, { status: "active" });
    await ctx.db.patch("pairings", p._id, { status: "confirmed" });
    return null;
  },
});

export const cancel = mutation({
  args: { token: v.string() },
  returns: v.null(),
  handler: async (ctx, { token }) => {
    const daemon = await daemonFromToken(ctx, token);
    for await (const p of ctx.db.query("pairings").withIndex("by_daemonId", (q) => q.eq("daemonId", daemon.daemonId))) {
      if (p.status === "open" || p.status === "claimed") {
        await ctx.db.patch("pairings", p._id, { status: "cancelled" });
        if (p.deviceId) await revokePending(ctx, p.deviceId);
      }
    }
    return null;
  },
});

export const devices = query({
  args: { token: v.string() },
  returns: v.array(deviceView),
  handler: async (ctx, { token }) => {
    const daemon = await daemonFromToken(ctx, token);
    const list = await ctx.db.query("devices").withIndex("by_daemonId", (q) => q.eq("daemonId", daemon.daemonId)).take(50);
    return list.filter((d) => d.status !== "revoked").map((d) => ({ deviceId: d.deviceId, name: d.name, publicKey: d.publicKey, encryptionKey: d.encryptionKey, kind: d.kind, status: d.status, createdAt: d.createdAt }));
  },
});

export const revoke = mutation({
  args: { token: v.string(), deviceId: v.string() },
  returns: v.null(),
  handler: async (ctx, { token, deviceId }) => {
    const daemon = await daemonFromToken(ctx, token);
    const d = await ctx.db.query("devices").withIndex("by_deviceId", (q) => q.eq("deviceId", deviceId)).unique();
    if (d && d.daemonId === daemon.daemonId) await ctx.db.patch("devices", d._id, { status: "revoked" });
    return null;
  },
});

/** El teléfono pregunta por sí mismo (con su token, también estando pendiente). */
export const me = query({
  args: { token: v.string() },
  returns: v.union(v.null(), v.object({ deviceId: v.string(), daemonId: v.string(), name: v.string(), status: v.string() })),
  handler: async (ctx, { token }) => {
    if (token.length < 32) return null;
    const tokenHash = await sha256Hex(token);
    const d = await ctx.db.query("devices").withIndex("by_tokenHash", (q) => q.eq("tokenHash", tokenHash)).unique();
    return d ? { deviceId: d.deviceId, daemonId: d.daemonId, name: d.name, status: d.status } : null;
  },
});
