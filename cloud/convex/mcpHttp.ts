// Las rutas del MCP remoto (mcp.ts): descubrimiento y autorización OAuth, `/mcp` y los permisos que crea
// el teléfono. La URL pública sale de `CONDX_PUBLIC_URL` o del host que reenvía el proxy (Vercel).

import type { HttpRouter } from "convex/server";
import { httpAction } from "./_generated/server";
import { internal } from "./_generated/api";
import { LEVELS } from "./mcp";

const PROTOCOL = "2025-06-18";

const json = (status: number, body: unknown, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store", ...headers } });

function base(req: Request): string {
  const env = (process.env.CONDX_PUBLIC_URL ?? "").replace(/\/$/, "");
  if (env) return env;
  const host = req.headers.get("x-forwarded-host");
  return host ? `${req.headers.get("x-forwarded-proto") ?? "https"}://${host.split(",")[0]!.trim()}` : new URL(req.url).origin;
}

async function sha(text: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** PKCE S256: base64url(SHA-256(verificador)). */
async function s256(verifier: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)));
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** El código del teléfono como se firma: sin guiones ni espacios, en mayúsculas. */
export const normalizeCode = (code: string) => code.toUpperCase().replace(/[^A-Z0-9]/g, "");

const bearer = (req: Request) => req.headers.get("authorization")?.match(/^Bearer\s+(\S{16,})$/)?.[1];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const okRedirect = (uri: string) => /^https:\/\//.test(uri) || /^http:\/\/(localhost|127\.0\.0\.1)(:\d+)?\//.test(uri);

// ── La página de autorización ──

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

const TEXT = {
  en: { title: "Connect to Condx", wants: "wants to work with your Condx projects.", how: "On your phone, open Condx → Settings → Connect an agent, and type the code it shows.", code: "Code", go: "Connect", never: "It can't approve deploys, touch production or read keys: those stay on your phone and your Mac." },
  es: { title: "Conectar con Condx", wants: "quiere trabajar con tus proyectos de Condx.", how: "En el teléfono, abre Condx → Ajustes → Conectar un agente, y escribe el código que muestra.", code: "Código", go: "Conectar", never: "No puede aprobar despliegues, tocar producción ni leer claves: eso queda en tu teléfono y tu Mac." },
};

function page(req: Request, client: string, fields: Record<string, string>, error?: string): Response {
  const t = /^es\b/i.test(req.headers.get("accept-language") ?? "") ? TEXT.es : TEXT.en;
  const hidden = Object.entries(fields).map(([k, v]) => `<input type="hidden" name="${esc(k)}" value="${esc(v)}">`).join("");
  const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="robots" content="noindex"><title>${t.title}</title>
<style>:root{color-scheme:dark;--bg:#1b1b1b;--card:#222;--line:#333;--text:#eee;--muted:#9a9a9a;--brand:#f2b01e}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:16px/1.45 system-ui,-apple-system,sans-serif}main{max-width:440px;margin:0 auto;padding:28px 16px}h1{font-size:20px;font-weight:500;display:flex;gap:10px;align-items:center;margin:0 0 20px}h1::before{content:"";width:14px;height:14px;background:var(--brand)}.card{background:var(--card);border:1px solid var(--line);padding:18px}.muted{color:var(--muted);font-size:14px}input[name=code]{width:100%;margin-top:6px;padding:12px;background:#1b1b1b;border:1px solid var(--line);color:var(--text);font:600 22px ui-monospace,Menlo,monospace;letter-spacing:3px;text-transform:uppercase}button{width:100%;margin-top:16px;padding:14px;border:0;background:var(--brand);color:#1b1b1b;font:500 16px system-ui,sans-serif}.bad{color:#f87171;font-size:14px;margin-top:10px}</style></head>
<body><main><h1>Condx</h1><div class="card"><p><b>${esc(client)}</b> ${t.wants}</p><p class="muted">${t.how}</p>
<form method="post" action="/oauth/authorize">${hidden}<label class="muted">${t.code}<input name="code" autocomplete="one-time-code" autofocus required maxlength="16"></label>${error ? `<div class="bad">${esc(error)}</div>` : ""}<button>${t.go}</button></form>
<p class="muted">${t.never}</p></div></main></body></html>`;
  return new Response(html, { status: error ? 400 : 200, headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store", "x-frame-options": "DENY" } });
}

export function addMcpRoutes(http: HttpRouter) {
  // Descubrimiento (RFC 9728 y RFC 8414).
  const resource = httpAction(async (_ctx, req) => {
    const b = base(req);
    return json(200, { resource: `${b}/mcp`, authorization_servers: [b], bearer_methods_supported: ["header"], resource_name: "Condx" });
  });
  http.route({ path: "/.well-known/oauth-protected-resource", method: "GET", handler: resource });
  http.route({ path: "/.well-known/oauth-protected-resource/mcp", method: "GET", handler: resource });
  http.route({
    path: "/.well-known/oauth-authorization-server",
    method: "GET",
    handler: httpAction(async (_ctx, req) => {
      const b = base(req);
      return json(200, {
        issuer: b,
        authorization_endpoint: `${b}/oauth/authorize`,
        token_endpoint: `${b}/oauth/token`,
        registration_endpoint: `${b}/oauth/register`,
        response_types_supported: ["code"],
        grant_types_supported: ["authorization_code", "refresh_token"],
        code_challenge_methods_supported: ["S256"],
        token_endpoint_auth_methods_supported: ["none"],
      });
    }),
  });

  // Registro dinámico (RFC 7591): clientes públicos (sin secreto), con PKCE.
  http.route({
    path: "/oauth/register",
    method: "POST",
    handler: httpAction(async (ctx, req) => {
      const b = (await req.json().catch(() => ({}))) as { client_name?: string; redirect_uris?: unknown };
      const uris = Array.isArray(b.redirect_uris) ? b.redirect_uris.filter((u): u is string => typeof u === "string") : [];
      if (uris.length === 0 || !uris.every(okRedirect)) return json(400, { error: "invalid_redirect_uri" });
      const name = typeof b.client_name === "string" && b.client_name.trim() ? b.client_name.trim() : "An agent";
      const clientId = await ctx.runMutation(internal.mcp.registerClient, { name, redirectUris: uris });
      return json(201, { client_id: clientId, client_name: name, redirect_uris: uris, token_endpoint_auth_method: "none", grant_types: ["authorization_code", "refresh_token"], response_types: ["code"] });
    }),
  });

  const authFields = (p: URLSearchParams) => ({
    client_id: p.get("client_id") ?? "",
    redirect_uri: p.get("redirect_uri") ?? "",
    state: p.get("state") ?? "",
    code_challenge: p.get("code_challenge") ?? "",
    code_challenge_method: p.get("code_challenge_method") ?? "",
  });
  async function checkAuth(ctx: Parameters<Parameters<typeof httpAction>[0]>[0], f: ReturnType<typeof authFields>) {
    const client = f.client_id ? await ctx.runQuery(internal.mcp.client, { clientId: f.client_id }) : null;
    if (!client) return { error: "unknown client" };
    if (!client.redirectUris.includes(f.redirect_uri)) return { error: "redirect_uri not registered" };
    if (f.code_challenge_method !== "S256" || f.code_challenge.length < 43) return { error: "PKCE (S256) is required" };
    return { client };
  }
  http.route({
    path: "/oauth/authorize",
    method: "GET",
    handler: httpAction(async (ctx, req) => {
      const p = new URL(req.url).searchParams;
      if (p.get("response_type") !== "code") return json(400, { error: "unsupported_response_type" });
      const f = authFields(p);
      const c = await checkAuth(ctx, f);
      if ("error" in c) return json(400, { error: "invalid_request", error_description: c.error });
      return page(req, c.client.name, f);
    }),
  });
  http.route({
    path: "/oauth/authorize",
    method: "POST",
    handler: httpAction(async (ctx, req) => {
      const form = new URLSearchParams(await req.text());
      const f = authFields(form);
      const c = await checkAuth(ctx, f);
      if ("error" in c) return json(400, { error: "invalid_request", error_description: c.error });
      const code = normalizeCode(form.get("code") ?? "");
      const r = await ctx.runMutation(internal.mcp.authorize, { codeHash: await sha(code), clientId: f.client_id, redirectUri: f.redirect_uri, challenge: f.code_challenge });
      if (!r.ok) return page(req, c.client.name, f, r.error);
      const to = new URL(f.redirect_uri);
      to.searchParams.set("code", r.code);
      if (f.state) to.searchParams.set("state", f.state);
      return new Response(null, { status: 302, headers: { location: to.toString(), "cache-control": "no-store" } });
    }),
  });
  http.route({
    path: "/oauth/token",
    method: "POST",
    handler: httpAction(async (ctx, req) => {
      const raw = await req.text();
      const p = (req.headers.get("content-type") ?? "").includes("json") ? new URLSearchParams(Object.entries(JSON.parse(raw || "{}") as Record<string, string>)) : new URLSearchParams(raw);
      const clientId = p.get("client_id") ?? "";
      let r: { error: string } | Record<string, unknown>;
      if (p.get("grant_type") === "authorization_code") {
        const verifier = p.get("code_verifier") ?? "";
        if (!verifier) return json(400, { error: "invalid_request" });
        r = await ctx.runMutation(internal.mcp.exchange, { codeHash: await sha(p.get("code") ?? ""), clientId, redirectUri: p.get("redirect_uri") ?? "", challenge: await s256(verifier) });
      } else if (p.get("grant_type") === "refresh_token") {
        r = await ctx.runMutation(internal.mcp.refresh, { refreshHash: await sha(p.get("refresh_token") ?? ""), clientId });
      } else return json(400, { error: "unsupported_grant_type" });
      return "error" in r ? json(400, r) : json(200, r);
    }),
  });

  // ── MCP (Streamable HTTP, sin sesión: cada POST es un mensaje) ──
  http.route({ path: "/mcp", method: "GET", handler: httpAction(async () => new Response(null, { status: 405, headers: { allow: "POST" } })) });
  http.route({
    path: "/mcp",
    method: "POST",
    handler: httpAction(async (ctx, req) => {
      const token = bearer(req);
      const grant = token ? await ctx.runQuery(internal.mcp.byToken, { tokenHash: await sha(token) }) : null;
      if (!grant) return json(401, { error: "invalid_token" }, { "www-authenticate": `Bearer resource_metadata="${base(req)}/.well-known/oauth-protected-resource"` });
      const msg = (await req.json().catch(() => null)) as { jsonrpc?: string; id?: unknown; method?: string; params?: Record<string, unknown> } | null;
      if (!msg || typeof msg !== "object" || Array.isArray(msg) || typeof msg.method !== "string") return json(400, { jsonrpc: "2.0", id: null, error: { code: -32600, message: "one JSON-RPC message per request" } });
      const id = msg.id ?? null;
      if (msg.id === undefined) return new Response(null, { status: 202 });
      const reply = (result: unknown) => json(200, { jsonrpc: "2.0", id, result });
      const fail = (message: string, code = -32001) => json(200, { jsonrpc: "2.0", id, error: { code, message } });
      if (msg.method === "initialize") {
        const asked = typeof msg.params?.protocolVersion === "string" ? (msg.params.protocolVersion as string) : PROTOCOL;
        return reply({
          protocolVersion: asked,
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: "condx", version: "cloud" },
          instructions: `Condx, a devtool for Convex projects, reached through Condx Cloud as “${grant.name}”. The Mac or, when it's off, the cloud site does the work. Start with list_projects and get_overview; threads (list_threads, read_thread), Condx's agents (list_agents, start_chat with a project, give_task) and flows. To try a project from anywhere (the developer is usually on a phone): the cloud site's preview, which runs a branch against the project's cloud dev — list_services gives its URL (cloudSite.services[].openUrl, or publicUrl when the cloud site answers); service_action starts it (with a branch). get_vercel is production, not a place to try changes. A task (give_task) ends with that preview. Approving deploys, production and keys stay with the developer.`,
        });
      }
      if (msg.method === "ping") return reply({});
      // `wait_for_mention` espera menos que el proxy.
      if (msg.method === "tools/call" && (msg.params?.name as string) === "wait_for_mention") {
        const args = (msg.params?.arguments ?? {}) as Record<string, unknown>;
        msg.params = { ...msg.params, arguments: { ...args, timeoutSeconds: Math.min(Number(args.timeoutSeconds) || 20, 20) } };
      }
      const call = await ctx.runMutation(internal.mcp.enqueue, { grantId: grant.grantId, msg: JSON.stringify(msg) });
      const listing = msg.method === "tools/list";
      const deadline = Date.now() + (listing ? 8_000 : 26_000);
      let macAlive = false;
      while (Date.now() < deadline) {
        await sleep(300);
        const st = await ctx.runQuery(internal.mcp.callState, { daemonId: grant.daemonId, rowId: call.rowId, callId: call.callId });
        macAlive = st.macAlive;
        if (st.result !== null) {
          let answer: { result?: unknown; error?: unknown } = {};
          try {
            answer = JSON.parse(st.result);
          } catch {
            answer = { error: { code: -32603, message: "the daemon's answer wasn't JSON" } };
          }
          await ctx.runMutation(internal.mcp.finish, { daemonId: grant.daemonId, rowId: call.rowId, grantId: grant.grantId, ...(listing && answer.result ? { tools: JSON.stringify(answer.result) } : {}) });
          return json(200, { jsonrpc: "2.0", id, ...(answer.error ? { error: answer.error } : { result: answer.result ?? {} }) });
        }
      }
      await ctx.runMutation(internal.mcp.finish, { daemonId: grant.daemonId, rowId: call.rowId, grantId: grant.grantId });
      if (listing && grant.tools) return reply(JSON.parse(grant.tools));
      return fail(macAlive ? "The Mac didn't answer in time. If it started the work it keeps going: check the thread in a moment." : "Neither the Mac nor the cloud site answered in time (the site may be waking up): try again in a minute.");
    }),
  });

  // ── Los permisos (los crea y revoca un teléfono de confianza) ──
  http.route({
    path: "/mcp/grants",
    method: "POST",
    handler: httpAction(async (ctx, req) => {
      const token = bearer(req);
      const phone = token ? await ctx.runQuery(internal.mcp.phoneOf, { token }) : null;
      if (!phone) return json(401, { error: "only a paired phone creates connections" });
      const b = (await req.json().catch(() => ({}))) as Record<string, unknown>;
      const str = (k: string, re: RegExp) => (typeof b[k] === "string" && re.test(b[k] as string) ? (b[k] as string) : null);
      const grantId = str("grantId", /^[0-9a-f]{16,40}$/), codeHash = str("codeHash", /^[0-9a-f]{64}$/), signature = str("signature", /^[0-9a-f]{128}$/);
      const name = typeof b.name === "string" ? b.name.trim().slice(0, 60) : "";
      const level = typeof b.level === "string" && (LEVELS as readonly string[]).includes(b.level) ? b.level : null;
      const signedAt = typeof b.signedAt === "number" ? b.signedAt : null;
      if (!grantId || !codeHash || !signature || !name || !level || !signedAt || Math.abs(Date.now() - signedAt) > 10 * 60_000) return json(400, { error: "grantId, name, level, codeHash, signedAt and signature" });
      await ctx.runMutation(internal.mcp.createGrant, { daemonId: phone.daemonId, deviceId: phone.deviceId, deviceKey: phone.publicKey, grantId, name, level, codeHash, signedAt, signature });
      return json(200, { grantId, url: `${base(req)}/mcp` });
    }),
  });
  http.route({
    path: "/mcp/grants",
    method: "GET",
    handler: httpAction(async (ctx, req) => {
      const token = bearer(req);
      const phone = token ? await ctx.runQuery(internal.mcp.phoneOf, { token }) : null;
      if (!phone) return json(401, { error: "only a paired phone" });
      return json(200, { url: `${base(req)}/mcp`, grants: await ctx.runQuery(internal.mcp.grantsOf, { daemonId: phone.daemonId }) });
    }),
  });
  http.route({
    pathPrefix: "/mcp/grants/",
    method: "DELETE",
    handler: httpAction(async (ctx, req) => {
      const token = bearer(req);
      const phone = token ? await ctx.runQuery(internal.mcp.phoneOf, { token }) : null;
      if (!phone) return json(401, { error: "only a paired phone" });
      const grantId = new URL(req.url).pathname.split("/").pop() ?? "";
      return json(200, { revoked: await ctx.runMutation(internal.mcp.revokeGrant, { daemonId: phone.daemonId, grantId }) });
    }),
  });
}
