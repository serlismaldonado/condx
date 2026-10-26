/// <reference types="vite/client" />
// Adjuntos de los hilos: el teléfono sube (cifrado para su Mac), solo su daemon baja y borra.

import { convexTest } from "convex-test";
import { expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import schema from "./schema";

const modules = import.meta.glob("./**/*.ts");
const A = { daemonId: "daemon-aaaa", token: "a".repeat(40) };
const B = { daemonId: "daemon-bbbb", token: "b".repeat(40) };
const PHONE = { publicKey: "1".repeat(64), token: "p".repeat(48), name: "Android" };

async function setup() {
  const t = convexTest(schema, modules);
  await t.mutation(internal.daemons.register, { ...A, name: "Mac A" });
  await t.mutation(internal.daemons.register, { ...B, name: "Mac B" });
  const { code } = await t.mutation(api.pairing.open, { token: A.token });
  const { deviceId } = await t.mutation(api.pairing.claim, { code, ...PHONE });
  await t.mutation(api.pairing.confirm, { token: A.token, deviceId });
  return t;
}
const auth = (token: string) => ({ authorization: `Bearer ${token}` });

test("el teléfono sube, su daemon baja y borra; nadie más", async () => {
  const t = await setup();
  const bytes = new Uint8Array([97, 103, 101, 1, 2, 3]);
  // Sin token o con el del daemon no se sube.
  expect((await t.fetch("/files", { method: "POST", body: bytes })).status).toBe(401);
  expect((await t.fetch("/files", { method: "POST", body: bytes, headers: auth(A.token) })).status).toBe(401);
  const up = await t.fetch("/files", { method: "POST", body: bytes, headers: auth(PHONE.token) });
  expect(up.status).toBe(200);
  const { id } = (await up.json()) as { id: string };
  // El teléfono no lo baja; otro daemon no lo ve.
  expect((await t.fetch(`/files/${id}`, { headers: auth(PHONE.token) })).status).toBe(401);
  expect((await t.fetch(`/files/${id}`, { headers: auth(B.token) })).status).toBe(404);
  const got = await t.fetch(`/files/${id}`, { headers: auth(A.token) });
  expect(got.status).toBe(200);
  expect(new Uint8Array(await got.arrayBuffer())).toEqual(bytes);
  expect((await t.fetch(`/files/${id}`, { method: "DELETE", headers: auth(B.token) })).status).toBe(404);
  expect((await t.fetch(`/files/${id}`, { method: "DELETE", headers: auth(A.token) })).status).toBe(200);
  expect((await t.fetch(`/files/${id}`, { headers: auth(A.token) })).status).toBe(404);
});

test("vacío o enorme no entra", async () => {
  const t = await setup();
  expect((await t.fetch("/files", { method: "POST", body: new Uint8Array(), headers: auth(PHONE.token) })).status).toBe(400);
  expect((await t.fetch("/files", { method: "POST", body: new Uint8Array(10 * 1024 * 1024 + 1), headers: auth(PHONE.token) })).status).toBe(413);
});
