/// <reference types="vite/client" />
// MCP remoto: el teléfono crea el permiso, Claude se autoriza (OAuth con PKCE) con su código, y cada
// llamada es una fila `_mcp-…` que contesta la Mac (o el sitio, si la Mac no late).

import { convexTest } from "convex-test";
import { expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import schema from "./schema";
import type { Op, OpGroup } from "localfirst/core";

const modules = import.meta.glob("./**/*.ts");

const A = { daemonId: "daemon-aaaa", token: "a".repeat(40) };
const PHONE = { publicKey: "1".repeat(64), token: "p".repeat(48), name: "Pixel" };
const SITE = { publicKey: "2".repeat(64), token: "s".repeat(48), name: "Cloud site", kind: "site" as const };
const THREADS = `threads:${A.daemonId}`;
const PV = 1;
let seq = 0;
const op = (partial: Partial<Op> & Pick<Op, "kind" | "rowId">): Op => ({ opId: `op_${++seq}`, scope: THREADS, fieldsVersion: 0, madeAt: Date.now(), ...partial });
const group = (...ops: Op[]): OpGroup => ({ groupId: `g_${++seq}`, seq, ops, createdAt: Date.now(), attempts: 0 });

async function sha(text: string) {
  const b = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}
async function s256(v: string) {
  const b = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(v)));
  return btoa(String.fromCharCode(...b)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function setup(macAliveAt: number) {
  const t = convexTest(schema, modules);
  await t.mutation(internal.daemons.register, { ...A, name: "Mac A" });
  for (const d of [PHONE, SITE]) {
    const { code } = await t.mutation(api.pairing.open, { token: A.token });
    const { deviceId } = await t.mutation(api.pairing.claim, { code, ...d });
    await t.mutation(api.pairing.confirm, { token: A.token, deviceId });
  }
  await t.mutation(api.sync.push, { token: A.token, group: group(op({ kind: "create", rowId: "_mac", set: { aliveAt: macAliveAt } })), protocolVersion: PV });
  return t;
}

/** El teléfono crea un permiso con el código `K7QF-2M9P`; Claude se registra y se autoriza. */
async function connect(t: Awaited<ReturnType<typeof setup>>) {
  const grant = { grantId: "ab".repeat(10), name: "Claude", level: "orchestrate", codeHash: await sha("K7QF2M9P"), signedAt: Date.now(), signature: "cd".repeat(64) };
  const auth = { authorization: `Bearer ${PHONE.token}`, "content-type": "application/json" };
  expect((await t.fetch("/mcp/grants", { method: "POST", headers: { authorization: `Bearer ${SITE.token}` }, body: JSON.stringify(grant) })).status).toBe(401);
  expect((await t.fetch("/mcp/grants", { method: "POST", headers: auth, body: JSON.stringify(grant) })).status).toBe(200);

  const meta = await (await t.fetch("/.well-known/oauth-authorization-server", { headers: { "x-forwarded-host": "condx.example.com" } })).json();
  expect(meta.token_endpoint).toBe("https://condx.example.com/oauth/token");
  const redirect = "https://claude.ai/api/mcp/auth_callback";
  const reg = await (await t.fetch("/oauth/register", { method: "POST", body: JSON.stringify({ client_name: "Claude", redirect_uris: [redirect] }) })).json();
  const verifier = "v".repeat(50);
  const q = new URLSearchParams({ response_type: "code", client_id: reg.client_id, redirect_uri: redirect, state: "st", code_challenge: await s256(verifier), code_challenge_method: "S256" });
  const pageRes = await t.fetch(`/oauth/authorize?${q}`, { headers: { "accept-language": "es-ES" } });
  expect(await pageRes.text()).toContain("Conectar un agente");
  // Un código equivocado vuelve a la página; el bueno (con guion y minúsculas) redirige con el código.
  const form = (code: string) => ({ method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams({ ...Object.fromEntries(q), code }).toString() });
  expect((await t.fetch("/oauth/authorize", form("XXXX-0000"))).status).toBe(400);
  const ok = await t.fetch("/oauth/authorize", form("k7qf-2m9p"));
  expect(ok.status).toBe(302);
  const back = new URL(ok.headers.get("location")!);
  expect(back.searchParams.get("state")).toBe("st");
  // El código del teléfono sirve una vez.
  expect((await t.fetch("/oauth/authorize", form("K7QF2M9P"))).status).toBe(400);
  const tokenBody = (verifierUsed: string) => ({ method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams({ grant_type: "authorization_code", code: back.searchParams.get("code")!, code_verifier: verifierUsed, client_id: reg.client_id, redirect_uri: redirect }).toString() });
  expect((await t.fetch("/oauth/token", tokenBody("w".repeat(50)))).status).toBe(400);
  const tok = await (await t.fetch("/oauth/token", tokenBody(verifier))).json();
  expect(tok.access_token).toMatch(/^cx_mcp_/);
  return { tok, reg, grant };
}

test("el teléfono conecta a Claude: OAuth con PKCE, initialize sin pasar por la Mac, renovar el token", async () => {
  const t = await setup(Date.now());
  const { tok, reg } = await connect(t);
  expect((await t.fetch("/mcp", { method: "POST", body: "{}" })).headers.get("www-authenticate")).toContain("oauth-protected-resource");
  const init = await (await t.fetch("/mcp", { method: "POST", headers: { authorization: `Bearer ${tok.access_token}` }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18" } }) })).json();
  expect(init.result.serverInfo.name).toBe("condx");
  const renewed = await (await t.fetch("/oauth/token", { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams({ grant_type: "refresh_token", refresh_token: tok.refresh_token, client_id: reg.client_id }).toString() })).json();
  expect(renewed.access_token).toMatch(/^cx_mcp_/);
  // El de renovación se usa una vez.
  expect((await t.fetch("/oauth/token", { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams({ grant_type: "refresh_token", refresh_token: tok.refresh_token, client_id: reg.client_id }).toString() })).status).toBe(400);
});

test("una llamada es una fila _mcp- que contesta la Mac; el sitio solo si la Mac no late", async () => {
  const t = await setup(Date.now());
  const { grant } = await connect(t);
  const call = await t.mutation(internal.mcp.enqueue, { grantId: grant.grantId, msg: JSON.stringify({ jsonrpc: "2.0", id: 7, method: "tools/list" }) });
  expect(call.rowId).toBe(`_mcp-${call.callId}`);
  const row = await t.run((ctx) => ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", THREADS).eq("rowId", call.rowId)).unique());
  expect(JSON.parse(row!.values.mcpGrant as string)).toMatchObject({ grantId: grant.grantId, signature: grant.signature });
  // Con la Mac latiendo, el sitio no contesta.
  await expect(t.mutation(api.sync.push, { token: SITE.token, group: group(op({ kind: "patch", rowId: call.rowId, set: { siteFor: call.callId, siteResult: "{}" } })), protocolVersion: PV })).rejects.toThrow(/The Mac is answering/);
  await t.mutation(api.sync.push, { token: A.token, group: group(op({ kind: "patch", rowId: call.rowId, set: { resultFor: call.callId, result: JSON.stringify({ result: { tools: [{ name: "list_threads" }] } }) } })), protocolVersion: PV });
  const st = await t.query(internal.mcp.callState, { daemonId: A.daemonId, rowId: call.rowId, callId: call.callId });
  expect(JSON.parse(st.result!).result.tools[0].name).toBe("list_threads");
  await t.mutation(internal.mcp.finish, { daemonId: A.daemonId, rowId: call.rowId, grantId: grant.grantId, tools: JSON.stringify({ tools: [] }) });
  const gone = await t.run((ctx) => ctx.db.query("lf_rows").withIndex("by_scope_and_rowId", (q) => q.eq("scope", THREADS).eq("rowId", call.rowId)).unique());
  expect(gone!.deleted).toBe(true);
});

test("con la Mac apagada contesta el sitio (solo sus campos)", async () => {
  const t = await setup(Date.now() - 60 * 60_000);
  const { grant } = await connect(t);
  const call = await t.mutation(internal.mcp.enqueue, { grantId: grant.grantId, msg: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "list_threads", arguments: {} } }) });
  await expect(t.mutation(api.sync.push, { token: SITE.token, group: group(op({ kind: "patch", rowId: call.rowId, set: { result: "{}" } })), protocolVersion: PV })).rejects.toThrow(/can't write “result”/);
  await t.mutation(api.sync.push, { token: SITE.token, group: group(op({ kind: "patch", rowId: call.rowId, set: { siteFor: call.callId, siteResult: JSON.stringify({ result: { content: [] } }) } })), protocolVersion: PV });
  const st = await t.query(internal.mcp.callState, { daemonId: A.daemonId, rowId: call.rowId, callId: call.callId });
  expect(st.macAlive).toBe(false);
  expect(JSON.parse(st.result!).result.content).toEqual([]);
});
