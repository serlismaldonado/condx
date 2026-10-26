// Condx Cloud: lo que un daemon de Condx publica (y lo que el teléfono pide), en alcances del
// protocolo local-first. Un alcance es `<tipo>:<daemonId>`; cada daemon solo ve los suyos.
//
// Nada de esto son secretos: el daemon nunca sube keys, tokens ni valores de variables.

import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";

export default defineSchema({
  /** Un daemon vinculado. El token se guarda como SHA-256 (hex); el token en claro vive en su Llavero. */
  daemons: defineTable({
    daemonId: v.string(),
    tokenHash: v.string(),
    name: v.string(),
    createdAt: v.number(),
  })
    .index("by_daemonId", ["daemonId"])
    .index("by_tokenHash", ["tokenHash"]),

  /**
   * Un teléfono vinculado a un daemon. Lo crea el teléfono al reclamar un código (pendiente) y lo
   * activa la Mac al confirmar. `publicKey`: su llave Ed25519 (hex), con la que firma decisiones;
   * `tokenHash`: SHA-256 de su token (el token y la llave privada viven en su Keychain/Keystore).
   */
  devices: defineTable({
    deviceId: v.string(),
    daemonId: v.string(),
    name: v.string(),
    publicKey: v.string(),
    /// Identidad de cifrado (age X25519) del teléfono: el daemon le cifra las claves de los proveedores.
    encryptionKey: v.optional(v.string()),
    tokenHash: v.string(),
    status: v.union(v.literal("pending"), v.literal("active"), v.literal("revoked")),
    createdAt: v.number(),
  })
    .index("by_tokenHash", ["tokenHash"])
    .index("by_deviceId", ["deviceId"])
    .index("by_daemonId", ["daemonId"]),

  /** Un código de vinculación que muestra la Mac (en el QR): vence a los 5 minutos y sirve una vez. */
  pairings: defineTable({
    codeHash: v.string(),
    daemonId: v.string(),
    expiresAt: v.number(),
    status: v.union(v.literal("open"), v.literal("claimed"), v.literal("confirmed"), v.literal("cancelled")),
    deviceId: v.optional(v.string()),
  })
    .index("by_codeHash", ["codeHash"])
    .index("by_daemonId", ["daemonId"]),

  /**
   * Llaves de ingesta (ingest.ts): con ellas el SDK de Condx de una app manda errores y reportes de
   * sus usuarios al alcance `events:<daemonId>`. La pública va en el navegador (se puede rotar); la
   * secreta, en el componente de Convex de la app (aquí solo su SHA-256).
   */
  /** Un adjunto de un hilo, cifrado para la Mac, hasta que el daemon lo baja (`files.ts`). */
  files: defineTable({
    daemonId: v.string(),
    deviceId: v.string(),
    storageId: v.id("_storage"),
    size: v.number(),
    createdAt: v.number(),
  }).index("by_createdAt", ["createdAt"]),

  ingestKeys: defineTable({
    daemonId: v.string(),
    project: v.string(),
    publicKey: v.string(),
    secretHash: v.string(),
    createdAt: v.number(),
    revokedAt: v.optional(v.number()),
  })
    .index("by_publicKey", ["publicKey"])
    .index("by_secretHash", ["secretHash"])
    .index("by_daemonId", ["daemonId"]),

  /** Cuántos eventos mandó cada llave en cada minuto (límite de ingesta). */
  ingestRate: defineTable({
    key: v.string(),
    minute: v.number(),
    count: v.number(),
  }).index("by_key_and_minute", ["key", "minute"]),

  /** Versión monotónica de cada alcance (sube con cada cambio). `minVersion`: lo más viejo retenido. */
  lf_scopes: defineTable({
    scope: v.string(),
    version: v.number(),
    fieldsVersion: v.number(),
    minVersion: v.number(),
  }).index("by_scope", ["scope"]),

  /** Filas de cada alcance. `values` y `fieldVersions` tienen claves de campo ASCII. */
  lf_rows: defineTable({
    scope: v.string(),
    rowId: v.string(),
    values: v.record(v.string(), v.any()),
    fieldVersions: v.record(v.string(), v.number()),
    deleted: v.boolean(),
    updatedAt: v.number(),
  })
    .index("by_scope_and_rowId", ["scope", "rowId"])
    .index("by_scope_and_deleted_and_rowId", ["scope", "deleted", "rowId"]),

  /** Registro de cambios: una entrada por escritura de una fila (sin el documento). */
  lf_changes: defineTable({
    scope: v.string(),
    version: v.number(),
    rowId: v.string(),
  }).index("by_scope_and_version", ["scope", "version"]),

  /** Operaciones aplicadas (idempotencia): re-enviar un `opId` devuelve el mismo resultado. */
  lf_ops: defineTable({
    principal: v.string(),
    opId: v.string(),
    result: v.any(),
    appliedAt: v.number(),
  }).index("by_principal_and_opId", ["principal", "opId"]),
});
