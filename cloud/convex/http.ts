// La página a la que lleva el QR de vinculación (`https://<sitio>/pair#d=<deployment>&c=<código>`).
// La cámara de cualquier teléfono abre un enlace; un QR con texto la deja sin saber qué hacer. La
// página no consulta nada: lee el fragmento (que no viaja al servidor), muestra el código y copia el
// texto que la app pega con «Paste the QR's text». La app también lee este enlace al escanear.
import { httpRouter } from "convex/server";
import { httpAction } from "./_generated/server";
import { internal } from "./_generated/api";
import { MAX_BYTES } from "./files";

const http = httpRouter();

const PAGE = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>Pair with Condx</title>
<style>
  :root { color-scheme: dark; --bg:#1b1b1b; --card:#222; --line:#333; --text:#eee; --muted:#9a9a9a; --brand:#f2b01e; }
  * { box-sizing: border-box; }
  body { margin:0; background:var(--bg); color:var(--text); font:16px/1.45 system-ui, -apple-system, sans-serif; }
  main { max-width:440px; margin:0 auto; padding:28px 16px; }
  h1 { font-size:20px; font-weight:500; display:flex; gap:10px; align-items:center; margin:0 0 20px; }
  h1::before { content:""; width:14px; height:14px; background:var(--brand); }
  .card { background:var(--card); border:1px solid var(--line); padding:18px; }
  .code { font:600 28px/1.2 ui-monospace, Menlo, monospace; letter-spacing:2px; margin:6px 0 2px; }
  .muted { color:var(--muted); font-size:14px; word-break:break-all; }
  button { width:100%; margin-top:18px; padding:14px; border:0; background:var(--brand); color:#1b1b1b; font:500 16px system-ui, sans-serif; }
  ol { color:var(--muted); font-size:14px; padding-left:20px; margin:18px 0 0; }
  .bad { color:#f87171; }
</style>
</head>
<body>
<main>
  <h1>Condx</h1>
  <div class="card" id="card">
    <div class="muted">Pairing code</div>
    <div class="code" id="code">…</div>
    <div class="muted" id="url"></div>
    <button id="copy">Copy for the Condx app</button>
    <ol>
      <li>Tap the button to copy.</li>
      <li>Open Condx on this phone and tap <b>Paste the QR's text</b>.</li>
      <li>Check that the Mac shows the same key and confirm there.</li>
    </ol>
  </div>
</main>
<script>
  const p = new URLSearchParams(location.hash.slice(1));
  const url = p.get("d"), code = p.get("c");
  const card = document.getElementById("card");
  if (!url || !code) {
    card.innerHTML = '<p class="bad">This link is incomplete. On the Mac, open Settings → Condx Cloud → Pair a phone and scan the new QR.</p>';
  } else {
    document.getElementById("code").textContent = code;
    document.getElementById("url").textContent = url;
    const text = JSON.stringify({ condx: 1, url, code });
    const btn = document.getElementById("copy");
    btn.onclick = async () => {
      try { await navigator.clipboard.writeText(text); }
      catch { const t = document.createElement("textarea"); t.value = text; document.body.append(t); t.select(); document.execCommand("copy"); t.remove(); }
      btn.textContent = "Copied. Now paste it in Condx";
    };
  }
</script>
</body>
</html>`;

http.route({
  path: "/pair",
  method: "GET",
  handler: httpAction(async () => {
    return new Response(PAGE, {
      status: 200,
      headers: {
        "content-type": "text/html; charset=utf-8",
        "cache-control": "no-store",
        "referrer-policy": "no-referrer",
        "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'",
      },
    });
  }),
});

// La ingesta del SDK (ingest.ts): el navegador manda con `x-condx-key: ck_pub_…` (CORS abierto,
// sin cookies) y el componente de Convex con `authorization: Bearer ck_sec_…`.
const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "POST, OPTIONS",
  "access-control-allow-headers": "content-type, x-condx-key",
  "access-control-max-age": "86400",
};

function reply(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...CORS } });
}

http.route({ path: "/ingest", method: "OPTIONS", handler: httpAction(async () => new Response(null, { status: 204, headers: CORS })) });

http.route({
  path: "/ingest",
  method: "POST",
  handler: httpAction(async (ctx, req) => {
    const bearer = req.headers.get("authorization")?.match(/^Bearer\s+(ck_sec_[a-f0-9]{48})$/)?.[1];
    const publicKey = req.headers.get("x-condx-key")?.match(/^ck_pub_[a-f0-9]{48}$/)?.[0];
    if (!bearer && !publicKey) return reply(401, { error: "missing ingest key" });
    const text = await req.text();
    if (text.length > 1_000_000) return reply(413, { error: "too large" });
    let body: { events?: unknown[] };
    try {
      body = JSON.parse(text);
    } catch {
      return reply(400, { error: "invalid JSON" });
    }
    const events = Array.isArray(body.events) ? body.events : [];
    if (events.length === 0) return reply(400, { error: "no events" });
    const key = await ctx.runQuery(internal.ingest.keyFor, { publicKey, secret: bearer });
    if (!key) return reply(401, { error: "unknown or revoked ingest key" });
    const res = await ctx.runMutation(internal.ingest.record, { daemonId: key.daemonId, project: key.project, source: key.source, events, key: key.key });
    return reply(res.stored === 0 && res.dropped > 0 ? 429 : 200, res);
  }),
});

// ── Adjuntos de los hilos (files.ts) ──
const bearerOf = (req: Request) => req.headers.get("authorization")?.match(/^Bearer\s+(\S{32,})$/)?.[1];

/** Un teléfono sube un adjunto (cifrado para su Mac); devuelve su id. */
http.route({
  path: "/files",
  method: "POST",
  handler: httpAction(async (ctx, req) => {
    const token = bearerOf(req);
    const who = token ? await ctx.runQuery(internal.files.whoIs, { token }) : null;
    if (!who || who.kind !== "device") return reply(401, { error: "only a paired phone uploads" });
    const blob = await req.blob();
    if (blob.size === 0) return reply(400, { error: "empty file" });
    if (blob.size > MAX_BYTES) return reply(413, { error: "too large" });
    const storageId = await ctx.storage.store(blob);
    const id = await ctx.runMutation(internal.files.record, { daemonId: who.daemonId, deviceId: who.deviceId!, storageId, size: blob.size });
    return reply(200, { id });
  }),
});

/** El daemon baja (GET) o borra (DELETE) un adjunto suyo: `/files/<id>`. */
const daemonFile = (method: "GET" | "DELETE") =>
  httpAction(async (ctx, req) => {
    const token = bearerOf(req);
    const who = token ? await ctx.runQuery(internal.files.whoIs, { token }) : null;
    if (!who || who.kind !== "daemon") return reply(401, { error: "only the daemon reads files" });
    const id = new URL(req.url).pathname.split("/").pop() ?? "";
    const f = await ctx.runQuery(internal.files.forDaemon, { id, daemonId: who.daemonId });
    if (!f) return reply(404, { error: "no such file" });
    if (method === "DELETE") {
      await ctx.runMutation(internal.files.remove, { id: f._id });
      return reply(200, { deleted: true });
    }
    const blob = await ctx.storage.get(f.storageId);
    return blob ? new Response(blob, { status: 200, headers: { "content-type": "application/octet-stream" } }) : reply(404, { error: "gone" });
  });
http.route({ pathPrefix: "/files/", method: "GET", handler: daemonFile("GET") });
http.route({ pathPrefix: "/files/", method: "DELETE", handler: daemonFile("DELETE") });

/** Solo el daemon sube y publica releases. */
async function daemonOf(ctx: Parameters<Parameters<typeof httpAction>[0]>[0], req: Request) {
  const token = bearerOf(req);
  const who = token ? await ctx.runQuery(internal.files.whoIs, { token }) : null;
  return who && who.kind === "daemon" ? who : null;
}

/** Una URL para subir la APK directo al storage (sin el tope de las rutas HTTP). */
http.route({
  path: "/releases/upload-url",
  method: "POST",
  handler: httpAction(async (ctx, req) => {
    if (!(await daemonOf(ctx, req))) return reply(401, { error: "only the daemon publishes releases" });
    return reply(200, { url: await ctx.storage.generateUploadUrl() });
  }),
});

/** Publica lo subido: devuelve el enlace de descarga (con su llave). */
http.route({
  path: "/releases/publish",
  method: "POST",
  handler: httpAction(async (ctx, req) => {
    const who = await daemonOf(ctx, req);
    if (!who) return reply(401, { error: "only the daemon publishes releases" });
    const b = (await req.json()) as { platform?: string; version?: string; versionCode?: number; storageId?: string; sha256?: string; size?: number };
    if (b.platform !== "android" || !b.version || typeof b.versionCode !== "number" || !b.storageId || !b.sha256) return reply(400, { error: "platform, version, versionCode, storageId and sha256" });
    const bytes = new Uint8Array(24);
    crypto.getRandomValues(bytes);
    const key = [...bytes].map((x) => x.toString(16).padStart(2, "0")).join("");
    await ctx.runMutation(internal.releases.publish, { daemonId: who.daemonId, platform: b.platform, version: b.version, versionCode: b.versionCode, storageId: b.storageId as never, sha256: b.sha256, size: b.size ?? 0, key });
    return reply(200, { key, path: `/download/android?k=${key}` });
  }),
});

/** La descarga: con la llave de esa versión, al archivo (con su tipo, así el teléfono lo instala). Va a
 * `/apk/<id>/condx-<versión>.apk`, que el proxy de Vercel sirve desde el almacenamiento: el teléfono
 * nombra la descarga por el último tramo (la respuesta de una acción HTTP no alcanza para la APK). */
http.route({
  path: "/download/android",
  method: "GET",
  handler: httpAction(async (ctx, req) => {
    const key = new URL(req.url).searchParams.get("k") ?? "";
    const r = /^[0-9a-f]{48}$/.test(key) ? await ctx.runQuery(internal.releases.byKey, { key }) : null;
    if (!r?.url) return new Response("Not found", { status: 404 });
    const id = new URL(r.url).pathname.split("/").pop();
    return new Response(null, { status: 302, headers: { location: `/apk/${id}/condx-${r.version}.apk`, "cache-control": "no-store" } });
  }),
});

/** Sin el proxy (el sitio del deployment directo): al archivo, con su nombre de almacenamiento. */
http.route({
  pathPrefix: "/apk/",
  method: "GET",
  handler: httpAction(async (_ctx, req) => {
    const id = new URL(req.url).pathname.split("/")[2] ?? "";
    if (!/^[A-Za-z0-9+=_-]{16,}$/.test(id)) return new Response("Not found", { status: 404 });
    const cloud = (process.env.CONVEX_CLOUD_URL ?? "").replace(/\/$/, "");
    return new Response(null, { status: 302, headers: { location: `${cloud}/api/storage/${id}` } });
  }),
});

export default http;
