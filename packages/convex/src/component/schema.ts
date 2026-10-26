// Las tablas del componente (viven en el deployment de la app, aparte de las suyas).
import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";

export default defineSchema({
  /** Un error del backend, agrupado por huella: `count` sube; `sentCount` es lo ya reenviado. */
  errors: defineTable({
    fingerprint: v.string(),
    message: v.string(),
    stack: v.optional(v.string()),
    fn: v.optional(v.string()),
    count: v.number(),
    sentCount: v.number(),
    firstAt: v.number(),
    lastAt: v.number(),
  })
    .index("by_fingerprint", ["fingerprint"])
    .index("by_lastAt", ["lastAt"]),

  /** Un reporte de una persona que usa la app, guardado primero aquí para que no se pierda. */
  reports: defineTable({
    text: v.string(),
    email: v.optional(v.string()),
    url: v.optional(v.string()),
    app: v.optional(v.string()),
    at: v.number(),
    sent: v.boolean(),
  }).index("by_sent", ["sent"]),

  /** Si ya hay un reenvío programado (para no programar uno por error). */
  outbox: defineTable({ scheduledAt: v.number(), attempts: v.number() }),
});
