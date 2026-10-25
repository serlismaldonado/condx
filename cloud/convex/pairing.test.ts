/// <reference types="vite/client" />
// Vincular teléfonos y lo que un teléfono puede (y no puede) hacer.

import { convexTest } from "convex-test";
import { describe, expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import schema from "./schema";
import type { Op, OpGroup } from "localfirst/core";

const modules = import.meta.glob("./**/*.ts");

const A = { daemonId: "daemon-aaaa", token: "a".repeat(40) };
const B = { daemonId: "daemon-bbbb", token: "b".repeat(40) };
const PHONE = { publicKey: "1".repeat(64), token: "p".repeat(48), name: "iPhone de Serlis" };
const APPROVALS = `approvals:${A.daemonId}`;
const SIG = "f".repeat(128);
const PV = 1;

async function setup() {
  const t = convexTest(schema, modules);
  await t.mutation(internal.daemons.register, { ...A, name: "Mac A" });
  await t.mutation(internal.daemons.register, { ...B, name: "Mac B" });
  return t;
}
type T = Awaited<ReturnType<typeof setup>>;

/** Abre, reclama y confirma: un teléfono activo del daemon A. */
async function pairPhone(t: T, phone = PHONE) {
  const { code } = await t.mutation(api.pairing.open, { token: A.token });
  const { deviceId } = await t.mutation(api.pairing.claim, { code, ...phone });
  await t.mutation(api.pairing.confirm, { token: A.token, deviceId });
  return deviceId;
}

let seq = 0;
const op = (partial: Partial<Op> & Pick<Op, "kind" | "rowId">, scope = APPROVALS): Op => ({ opId: `op_${++seq}`, scope, fieldsVersion: 0, madeAt: Date.now(), ...partial });
const group = (...ops: Op[]): OpGroup => ({ groupId: `g_${++seq}`, seq, ops, createdAt: Date.now(), attempts: 0 });
const push = (t: T, g: OpGroup, token: string) => t.mutation(api.sync.push, { token, group: g, protocolVersion: PV });
const decision = (extra: Record<string, unknown> = {}) => ({ decision: "approve", decidedAt: Date.now(), deviceKey: PHONE.publicKey, signature: SIG, ...extra });

describe("vincular", () => {
  test("abrir, reclamar con el código (con o sin guiones), confirmar", async () => {
    const t = await setup();
    const { code, expiresAt } = await t.mutation(api.pairing.open, { token: A.token });
    expect(code).toMatch(/^[2-9A-HJKMNP-Z]{4}-[2-9A-HJKMNP-Z]{4}-[2-9A-HJKMNP-Z]{4}$/);
    expect(expiresAt).toBeGreaterThan(Date.now());
    const { deviceId, daemonId } = await t.mutation(api.pairing.claim, { code: code.toLowerCase().replace(/-/g, " "), ...PHONE });
    expect(daemonId).toBe(A.daemonId);
    // Pendiente: la Mac lo ve, el teléfono todavía no entra.
    expect(await t.query(api.pairing.status, { token: A.token })).toMatchObject({ status: "claimed", device: { deviceId, name: PHONE.name, publicKey: PHONE.publicKey, status: "pending" } });
    expect(await t.query(api.pairing.me, { token: PHONE.token })).toMatchObject({ status: "pending" });
    await expect(t.query(api.sync.pull, { token: PHONE.token, scope: APPROVALS, cursor: null, protocolVersion: PV })).rejects.toThrow(/Invalid token/);
    await t.mutation(api.pairing.confirm, { token: A.token, deviceId });
    expect(await t.query(api.pairing.me, { token: PHONE.token })).toMatchObject({ status: "active", daemonId: A.daemonId });
    expect(await t.query(api.sync.pull, { token: PHONE.token, scope: APPROVALS, cursor: null, protocolVersion: PV })).toMatchObject({ status: "ok" });
  });

  test("un código sirve una vez, vence, y abrir otro cancela el anterior", async () => {
    const t = await setup();
    const { code } = await t.mutation(api.pairing.open, { token: A.token });
    await t.mutation(api.pairing.claim, { code, ...PHONE });
    await expect(t.mutation(api.pairing.claim, { code, ...PHONE, token: "q".repeat(48) })).rejects.toThrow(/isn't valid/);
    // Vencido.
    const second = await t.mutation(api.pairing.open, { token: A.token });
    await t.run(async (ctx) => {
      for await (const p of ctx.db.query("pairings")) if (p.status === "open") await ctx.db.patch("pairings", p._id, { expiresAt: Date.now() - 1 });
    });
    await expect(t.mutation(api.pairing.claim, { code: second.code, ...PHONE, token: "r".repeat(48) })).rejects.toThrow(/isn't valid/);
    // El teléfono pendiente del primer código quedó revocado al abrir el segundo.
    const devices = await t.run((ctx) => ctx.db.query("devices").collect());
    expect(devices.every((d) => d.status === "revoked")).toBe(true);
  });

  test("solo el daemon dueño confirma; cancelar revoca el pendiente", async () => {
    const t = await setup();
    const { code } = await t.mutation(api.pairing.open, { token: A.token });
    const { deviceId } = await t.mutation(api.pairing.claim, { code, ...PHONE });
    await expect(t.mutation(api.pairing.confirm, { token: B.token, deviceId })).rejects.toThrow(/no pending phone/);
    await t.mutation(api.pairing.cancel, { token: A.token });
    expect(await t.query(api.pairing.me, { token: PHONE.token })).toMatchObject({ status: "revoked" });
    await expect(t.mutation(api.pairing.confirm, { token: A.token, deviceId })).rejects.toThrow(/no pending phone/);
  });

  test("datos inválidos al reclamar", async () => {
    const t = await setup();
    const { code } = await t.mutation(api.pairing.open, { token: A.token });
    await expect(t.mutation(api.pairing.claim, { code, ...PHONE, publicKey: "zz" })).rejects.toThrow(/Ed25519/);
    await expect(t.mutation(api.pairing.claim, { code, ...PHONE, token: "short" })).rejects.toThrow(/32–128/);
    await expect(t.mutation(api.pairing.claim, { code: "AAAA-BBBB-CCCC", ...PHONE })).rejects.toThrow(/isn't valid/);
  });

  test("revocar saca al teléfono", async () => {
    const t = await setup();
    const deviceId = await pairPhone(t);
    expect((await t.query(api.pairing.devices, { token: A.token })).map((d) => d.deviceId)).toEqual([deviceId]);
    await t.mutation(api.pairing.revoke, { token: A.token, deviceId });
    await expect(t.query(api.sync.pull, { token: PHONE.token, scope: APPROVALS, cursor: null, protocolVersion: PV })).rejects.toThrow(/Invalid token/);
    expect(await t.query(api.pairing.devices, { token: A.token })).toEqual([]);
  });
});

describe("lo que puede un teléfono", () => {
  async function withApproval() {
    const t = await setup();
    await pairPhone(t);
    await push(t, group(op({ kind: "create", rowId: "job1", set: { status: "waiting-approval", recipe: "deploy-cloud" } })), A.token);
    return t;
  }

  test("decide con su firma en una aprobación de su daemon", async () => {
    const t = await withApproval();
    const r = await push(t, group(op({ kind: "patch", rowId: "job1", set: decision(), base: { decision: 0 } })), PHONE.token);
    expect(r).toMatchObject({ status: "ok" });
    const pull = await t.query(api.sync.pull, { token: A.token, scope: APPROVALS, cursor: null, protocolVersion: PV });
    expect(pull.rows![0].values).toMatchObject({ status: "waiting-approval", decision: "approve", deviceKey: PHONE.publicKey });
  });

  test("lee entornos, trabajos, aprobaciones e incidentes; nada más", async () => {
    const t = await withApproval();
    for (const kind of ["env", "jobs", "approvals", "incidents", "keys"]) {
      expect(await t.query(api.sync.pull, { token: PHONE.token, scope: `${kind}:${A.daemonId}`, cursor: null, protocolVersion: PV })).toMatchObject({ status: "ok" });
    }
    for (const scope of [`runs:${A.daemonId}`, `workflows:${A.daemonId}`, `approvals:${B.daemonId}`, `env:${B.daemonId}`]) {
      expect(await t.query(api.sync.pull, { token: PHONE.token, scope, cursor: null, protocolVersion: PV })).toEqual({ status: "denied" });
    }
  });

  test("no puede cambiar el estado, crear, borrar, ni firmar con otra llave", async () => {
    const t = await withApproval();
    const cases: Array<[string, Op]> = [
      ["status", op({ kind: "patch", rowId: "job1", set: { ...decision(), status: "approved" } })],
      ["create", op({ kind: "create", rowId: "job2", set: decision() })],
      ["delete", op({ kind: "delete", rowId: "job1" })],
      ["other key", op({ kind: "patch", rowId: "job1", set: decision({ deviceKey: "2".repeat(64) }) })],
      ["no signature", op({ kind: "patch", rowId: "job1", set: decision({ signature: "nope" }) })],
      ["bad decision", op({ kind: "patch", rowId: "job1", set: decision({ decision: "maybe" }) })],
      ["sets", op({ kind: "patch", rowId: "job1", set: decision(), sets: { tags: { add: ["x"] } } })],
      ["other scope", op({ kind: "patch", rowId: "e", set: decision() }, `env:${A.daemonId}`)],
      ["other daemon", op({ kind: "patch", rowId: "job1", set: decision() }, `approvals:${B.daemonId}`)],
    ];
    for (const [name, o] of cases) {
      await expect(push(t, group(o), PHONE.token), name).rejects.toThrow(/denied|phone|signature|decision|access/i);
    }
    const pull = await t.query(api.sync.pull, { token: A.token, scope: APPROVALS, cursor: null, protocolVersion: PV });
    expect(pull.rows!.map((r: { id: string; values: Record<string, unknown> }) => [r.id, r.values.status, r.values.decision ?? null])).toEqual([["job1", "waiting-approval", null]]);
  });

  test("las funciones de vincular del daemon no aceptan el token de un teléfono", async () => {
    const t = await withApproval();
    await expect(t.mutation(api.pairing.open, { token: PHONE.token })).rejects.toThrow(/Invalid token/);
    await expect(t.query(api.pairing.devices, { token: PHONE.token })).rejects.toThrow(/Invalid token/);
  });

  test("el token del teléfono no se guarda en claro", async () => {
    const t = await withApproval();
    const devices = await t.run((ctx) => ctx.db.query("devices").collect());
    expect(JSON.stringify(devices)).not.toContain(PHONE.token);
  });
});

describe("pedidos sobre incidentes", () => {
  const INCIDENTS = `incidents:${A.daemonId}`;
  const request = (extra: Record<string, unknown> = {}) => ({ request: "diagnose", requestId: "a1b2c3d4e5f60718", requestedAt: Date.now(), deviceKey: PHONE.publicKey, signature: SIG, ...extra });

  async function withIncident() {
    const t = await setup();
    await pairPhone(t);
    await push(t, group(op({ kind: "create", rowId: "inc-1", set: { title: "Unit tests failed on dev", status: "open" } }, INCIDENTS)), A.token);
    return t;
  }

  test("un teléfono pide algo firmado sobre un incidente de su daemon", async () => {
    const t = await withIncident();
    const r = await push(t, group(op({ kind: "patch", rowId: "inc-1", set: request({ request: "message", requestArg: "Look at lib/dates.js" }) }, INCIDENTS)), PHONE.token);
    expect(r).toMatchObject({ status: "ok" });
    const pull = await t.query(api.sync.pull, { token: A.token, scope: INCIDENTS, cursor: null, protocolVersion: PV });
    expect(pull.rows![0].values).toMatchObject({ status: "open", request: "message", requestArg: "Look at lib/dates.js", deviceKey: PHONE.publicKey });
  });

  test("no puede cambiar el incidente, pedir algo desconocido ni firmar con otra llave", async () => {
    const t = await withIncident();
    const cases: Array<[string, Op]> = [
      ["status", op({ kind: "patch", rowId: "inc-1", set: { ...request(), status: "resolved" } }, INCIDENTS)],
      ["agent", op({ kind: "patch", rowId: "inc-1", set: { ...request(), agent: { status: "done" } } }, INCIDENTS)],
      ["create", op({ kind: "create", rowId: "inc-2", set: request() }, INCIDENTS)],
      ["delete", op({ kind: "delete", rowId: "inc-1" }, INCIDENTS)],
      ["unknown request", op({ kind: "patch", rowId: "inc-1", set: request({ request: "approve" }) }, INCIDENTS)],
      ["other key", op({ kind: "patch", rowId: "inc-1", set: request({ deviceKey: "2".repeat(64) }) }, INCIDENTS)],
      ["no signature", op({ kind: "patch", rowId: "inc-1", set: request({ signature: "x" }) }, INCIDENTS)],
      ["no request id", op({ kind: "patch", rowId: "inc-1", set: request({ requestId: "?" }) }, INCIDENTS)],
      ["long text", op({ kind: "patch", rowId: "inc-1", set: request({ request: "message", requestArg: "x".repeat(5000) }) }, INCIDENTS)],
      ["other daemon", op({ kind: "patch", rowId: "inc-1", set: request() }, `incidents:${B.daemonId}`)],
    ];
    for (const [name, o] of cases) {
      await expect(push(t, group(o), PHONE.token), name).rejects.toThrow(/denied|phone|signature|request|access|write/i);
    }
    const pull = await t.query(api.sync.pull, { token: A.token, scope: INCIDENTS, cursor: null, protocolVersion: PV });
    expect(pull.rows!.map((r: { id: string; values: Record<string, unknown> }) => [r.id, r.values.status, r.values.request ?? null])).toEqual([["inc-1", "open", null]]);
  });
});

describe("claves cifradas para el teléfono", () => {
  const AGE = "age1" + "q".repeat(58);

  test("el teléfono manda su identidad de cifrado y la Mac la ve al confirmar", async () => {
    const t = await setup();
    const { code } = await t.mutation(api.pairing.open, { token: A.token });
    const { deviceId } = await t.mutation(api.pairing.claim, { code, ...PHONE, encryptionKey: AGE });
    const status = await t.query(api.pairing.status, { token: A.token });
    expect(status!.device).toMatchObject({ deviceId, encryptionKey: AGE });
    await t.mutation(api.pairing.confirm, { token: A.token, deviceId });
    expect((await t.query(api.pairing.devices, { token: A.token }))[0]).toMatchObject({ encryptionKey: AGE });
  });

  test("una identidad que no es de age no se acepta", async () => {
    const t = await setup();
    const { code } = await t.mutation(api.pairing.open, { token: A.token });
    for (const bad of ["age1short", "AGE1" + "q".repeat(58), "age1" + "b".repeat(58), "ssh-ed25519 AAAA"]) {
      await expect(t.mutation(api.pairing.claim, { code, ...PHONE, encryptionKey: bad }), bad).rejects.toThrow(/encryptionKey/);
    }
  });

  test("el teléfono lee las claves cifradas pero no las escribe", async () => {
    const t = await setup();
    await pairPhone(t);
    const KEYS = `keys:${A.daemonId}`;
    await push(t, group(op({ kind: "create", rowId: "anthropic", set: { kind: "anthropic", sealed: "-----BEGIN AGE ENCRYPTED FILE-----" } }, KEYS)), A.token);
    const pull = await t.query(api.sync.pull, { token: PHONE.token, scope: KEYS, cursor: null, protocolVersion: PV });
    expect(pull.rows![0].values).toMatchObject({ kind: "anthropic" });
    await expect(push(t, group(op({ kind: "patch", rowId: "anthropic", set: { sealed: "x" } }, KEYS)), PHONE.token)).rejects.toThrow(/denied|phone|approvals/i);
  });
});

