/// <reference types="vite/client" />
// Releases de la app del teléfono: solo el daemon publica; se baja con la llave de esa versión.

import { convexTest } from "convex-test";
import { expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import schema from "./schema";

const modules = import.meta.glob("./**/*.ts");
const A = { daemonId: "daemon-aaaa", token: "a".repeat(40) };
const PHONE = { publicKey: "1".repeat(64), token: "p".repeat(48), name: "Android" };
const auth = (token: string) => ({ authorization: `Bearer ${token}`, "content-type": "application/json" });

async function setup() {
  const t = convexTest(schema, modules);
  await t.mutation(internal.daemons.register, { ...A, name: "Mac A" });
  const { code } = await t.mutation(api.pairing.open, { token: A.token });
  const { deviceId } = await t.mutation(api.pairing.claim, { code, ...PHONE });
  await t.mutation(api.pairing.confirm, { token: A.token, deviceId });
  return t;
}

async function publish(t: Awaited<ReturnType<typeof setup>>, version: string, code: number) {
  const storageId = await t.run(async (ctx) => ctx.storage.store(new Blob([`apk ${version}`], { type: "application/vnd.android.package-archive" })));
  const r = await t.fetch("/releases/publish", { method: "POST", headers: auth(A.token), body: JSON.stringify({ platform: "android", version, versionCode: code, storageId, sha256: "ab", size: 5 }) });
  expect(r.status).toBe(200);
  return (await r.json()) as { key: string; path: string };
}

test("solo el daemon publica y la descarga pide la llave de esa versión", async () => {
  const t = await setup();
  // Un teléfono (o nadie) no pide URLs de subida ni publica.
  expect((await t.fetch("/releases/upload-url", { method: "POST", headers: auth(PHONE.token) })).status).toBe(401);
  expect((await t.fetch("/releases/upload-url", { method: "POST" })).status).toBe(401);
  expect((await t.fetch("/releases/upload-url", { method: "POST", headers: auth(A.token) })).status).toBe(200);
  const { key, path } = await publish(t, "0.3.0", 300);
  expect(key).toMatch(/^[0-9a-f]{48}$/);
  const ok = await t.fetch(path);
  expect(ok.status).toBe(302);
  // Al archivo con su nombre en la ruta (así lo nombra el teléfono); sin el proxy, al almacenamiento.
  const location = ok.headers.get("location") ?? "";
  expect(location).toMatch(/^\/apk\/[A-Za-z0-9+=_-]{16,}\/condx-0\.3\.0\.apk$/);
  const file = await t.fetch(location);
  expect(file.status).toBe(302);
  expect(file.headers.get("location")).toMatch(/\/api\/storage\/[A-Za-z0-9+=_-]{16,}$/);
  expect((await t.fetch("/apk/nope/condx.apk")).status).toBe(404);
  // Sin llave, con una mal formada o con una que no existe: 404.
  expect((await t.fetch("/download/android")).status).toBe(404);
  expect((await t.fetch("/download/android?k=xyz")).status).toBe(404);
  expect((await t.fetch(`/download/android?k=${"0".repeat(48)}`)).status).toBe(404);
});

test("quedan las tres últimas versiones", async () => {
  const t = await setup();
  const keys = [];
  for (const [i, v] of ["0.3.0", "0.3.1", "0.3.2", "0.3.3"].entries()) keys.push((await publish(t, v, 300 + i)).key);
  expect((await t.fetch(`/download/android?k=${keys[0]}`)).status).toBe(404);
  for (const k of keys.slice(1)) expect((await t.fetch(`/download/android?k=${k}`)).status).toBe(302);
  expect(await t.run(async (ctx) => (await ctx.db.query("releases").collect()).length)).toBe(3);
});
