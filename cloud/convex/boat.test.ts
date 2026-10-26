/// <reference types="vite/client" />
// Despertar al sitio en la nube en boat cuando un teléfono deja trabajo y la Mac no está.

import { convexTest } from "convex-test";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import schema from "./schema";
import type { Op, OpGroup } from "localfirst/core";

const modules = import.meta.glob("./**/*.ts");

const A = { daemonId: "daemon-aaaa", token: "a".repeat(40) };
const PHONE = { publicKey: "1".repeat(64), token: "p".repeat(48), name: "Android" };
const INCIDENTS = `incidents:${A.daemonId}`;
const THREADS = `threads:${A.daemonId}`;
const PV = 1;

let seq = 0;
const op = (partial: Partial<Op> & Pick<Op, "kind" | "rowId">, scope: string): Op => ({ opId: `op_${++seq}`, scope, fieldsVersion: 0, madeAt: Date.now(), ...partial });
const group = (...ops: Op[]): OpGroup => ({ groupId: `g_${++seq}`, seq, ops, createdAt: Date.now(), attempts: 0 });

let calls: string[] = [];
let state = "archived";

beforeEach(() => {
  vi.useFakeTimers();
  calls = [];
  state = "archived";
  process.env.BOAT_API_KEY = "boat_test";
  process.env.BOAT_SANDBOX_ID = "sb-1";
  // boat falso: el estado de la máquina y la reanudación.
  vi.stubGlobal("fetch", async (url: string, init?: { method?: string }) => {
    calls.push(`${init?.method ?? "GET"} ${new URL(url).pathname}`);
    if (url.endsWith("/resume")) return new Response("{}", { status: 200 });
    return new Response(JSON.stringify({ sandbox: { state } }), { status: 200 });
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  delete process.env.BOAT_API_KEY;
  delete process.env.BOAT_SANDBOX_ID;
});

async function setup(macAliveAt?: number) {
  const t = convexTest(schema, modules);
  await t.mutation(internal.daemons.register, { ...A, name: "Mac A" });
  const { code } = await t.mutation(api.pairing.open, { token: A.token });
  const { deviceId } = await t.mutation(api.pairing.claim, { code, ...PHONE });
  await t.mutation(api.pairing.confirm, { token: A.token, deviceId });
  // El daemon publica un incidente (y su latido, si se pide).
  const ops = [op({ kind: "create", rowId: "i1", set: { title: "Convex down" } }, INCIDENTS)];
  await t.mutation(api.sync.push, { token: A.token, group: group(...ops), protocolVersion: PV });
  if (macAliveAt !== undefined) await t.mutation(api.sync.push, { token: A.token, group: group(op({ kind: "create", rowId: "_mac", set: { aliveAt: macAliveAt } }, THREADS)), protocolVersion: PV });
  return t;
}

/** El teléfono pide diagnosticar el incidente (un pedido firmado, como los de verdad). */
const ask = (t: Awaited<ReturnType<typeof setup>>) =>
  t.mutation(api.sync.push, {
    token: PHONE.token,
    group: group(op({ kind: "patch", rowId: "i1", set: { request: "diagnose", requestId: `${++seq}`.padStart(16, "0"), requestedAt: Date.now(), deviceKey: PHONE.publicKey, signature: "f".repeat(128) } }, INCIDENTS)),
    protocolVersion: PV,
  });

test("con la Mac apagada, un pedido del teléfono reanuda la máquina en boat (una vez por ráfaga)", async () => {
  const t = await setup(Date.now() - 60 * 60_000);
  await ask(t);
  await t.finishAllScheduledFunctions(vi.runAllTimers);
  expect(calls).toEqual(["GET /api/v1/sandboxes/sb-1", "POST /api/v1/sandboxes/sb-1/resume"]);
  expect(await t.run((ctx) => ctx.db.query("siteWakes").first())).toMatchObject({ daemonId: A.daemonId, result: "resumed" });
  // Otro pedido enseguida: no se vuelve a intentar.
  await ask(t);
  await t.finishAllScheduledFunctions(vi.runAllTimers);
  expect(calls).toHaveLength(2);
});

test("ya encendida no se reanuda; con la Mac latiendo ni se mira", async () => {
  state = "running";
  const t = await setup();
  await ask(t);
  await t.finishAllScheduledFunctions(vi.runAllTimers);
  expect(calls).toEqual(["GET /api/v1/sandboxes/sb-1"]);
  expect(await t.run((ctx) => ctx.db.query("siteWakes").first())).toMatchObject({ result: "already running" });

  calls = [];
  const t2 = await setup(Date.now() - 60_000);
  await ask(t2);
  await t2.finishAllScheduledFunctions(vi.runAllTimers);
  expect(calls).toEqual([]);
});
