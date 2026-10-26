/// <reference types="vite/client" />
// El sitio en la nube contesta en los hilos cuando la Mac no está (S5): qué puede escribir y cuándo.

import { convexTest } from "convex-test";
import { expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import schema from "./schema";
import type { Op, OpGroup } from "localfirst/core";

const modules = import.meta.glob("./**/*.ts");

const A = { daemonId: "daemon-aaaa", token: "a".repeat(40) };
const SITE = { publicKey: "2".repeat(64), token: "s".repeat(48), name: "Cloud site", kind: "site" as const };
const THREADS = `threads:${A.daemonId}`;
const PV = 1;

let seq = 0;
const op = (partial: Partial<Op> & Pick<Op, "kind" | "rowId">, scope = THREADS): Op => ({ opId: `op_${++seq}`, scope, fieldsVersion: 0, madeAt: Date.now(), ...partial });
const group = (...ops: Op[]): OpGroup => ({ groupId: `g_${++seq}`, seq, ops, createdAt: Date.now(), attempts: 0 });

async function setup(macAliveAt: number) {
  const t = convexTest(schema, modules);
  await t.mutation(internal.daemons.register, { ...A, name: "Mac A" });
  const { code } = await t.mutation(api.pairing.open, { token: A.token });
  const { deviceId } = await t.mutation(api.pairing.claim, { code, ...SITE });
  await t.mutation(api.pairing.confirm, { token: A.token, deviceId });
  const mac = op({ kind: "create", rowId: "_mac", set: { aliveAt: macAliveAt } });
  const thread = op({ kind: "create", rowId: "chat-1", set: { title: "Hola", items: "[]" } });
  const fresh = op({ kind: "create", rowId: "_new", set: { title: "new" } });
  await t.mutation(api.sync.push, { token: A.token, group: group(mac, thread, fresh), protocolVersion: PV });
  return t;
}

const site = (t: Awaited<ReturnType<typeof setup>>, ...ops: Op[]) => t.mutation(api.sync.push, { token: SITE.token, group: group(...ops), protocolVersion: PV });

test("con la Mac apagada contesta en sus campos y crea sus hilos; nada más", async () => {
  const t = await setup(Date.now() - 60 * 60_000);
  await expect(site(t, op({ kind: "patch", rowId: "chat-1", set: { siteItems: "[]", siteFor: "r1" } }))).resolves.toMatchObject({ status: "ok" });
  await expect(site(t, op({ kind: "patch", rowId: "_new", set: { siteFor: "r2", siteResult: "thread site-1" } }))).resolves.toMatchObject({ status: "ok" });
  await expect(site(t, op({ kind: "create", rowId: "site-1", set: { title: "Nuevo", items: "[]", status: "done" } }))).resolves.toMatchObject({ status: "ok" });
  // Lo de la Mac no se toca: sus campos, sus filas especiales ni otros alcances.
  await expect(site(t, op({ kind: "patch", rowId: "chat-1", set: { items: "[]" } }))).rejects.toThrow(/can't write “items”/);
  await expect(site(t, op({ kind: "patch", rowId: "_mac", set: { siteFor: "x" } }))).rejects.toThrow(/only answers in threads/);
  await expect(site(t, op({ kind: "create", rowId: "j1", set: { status: "x" } }, `approvals:${A.daemonId}`))).rejects.toThrow(/only writes threads/);
});

test("con la Mac latiendo no contesta en los hilos de la Mac (los suyos sí)", async () => {
  const t = await setup(Date.now() - 60_000);
  await expect(site(t, op({ kind: "patch", rowId: "chat-1", set: { siteFor: "r1" } }))).rejects.toThrow(/The Mac is answering/);
  await expect(site(t, op({ kind: "create", rowId: "site-2", set: { title: "Mío" } }))).resolves.toMatchObject({ status: "ok" });
});

test("la Mac se despide: el sitio contesta enseguida, sin esperar a que venza el latido", async () => {
  const t = await setup(Date.now() - 30_000);
  await expect(site(t, op({ kind: "patch", rowId: "chat-1", set: { siteFor: "r1" } }))).rejects.toThrow(/The Mac is answering/);
  await t.mutation(api.sync.goodbye, { token: A.token });
  await expect(site(t, op({ kind: "patch", rowId: "chat-1", set: { siteFor: "r1" } }))).resolves.toMatchObject({ status: "ok" });
  await expect(t.mutation(api.sync.goodbye, { token: SITE.token })).rejects.toThrow(/Invalid token/);
});


test("con la Mac apagada contesta los pedidos de servicios en _run y publica su resumen", async () => {
  const t = await setup(Date.now() - 60 * 60_000);
  await t.mutation(api.sync.push, { token: A.token, group: group(op({ kind: "create", rowId: "_run", set: { title: "run" } })), protocolVersion: PV });
  await expect(site(t, op({ kind: "patch", rowId: "_run", set: { siteFor: "r9", siteResult: "start all: on its way" } }))).resolves.toMatchObject({ status: "ok" });
  await expect(site(t, op({ kind: "patch", rowId: "_run", set: { siteItems: "[]" } }))).rejects.toThrow(/can't write “siteItems”/);
  await expect(site(t, op({ kind: "patch", rowId: "_run", set: { result: "x" } }))).rejects.toThrow(/can't write “result”/);
  await expect(site(t, op({ kind: "create", rowId: "site-overview", set: { title: "overview", overview: "{}" } }))).resolves.toMatchObject({ status: "ok" });
});
