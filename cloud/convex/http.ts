// La página a la que lleva el QR de vinculación (`https://<sitio>/pair#d=<deployment>&c=<código>`).
// La cámara de cualquier teléfono abre un enlace; un QR con texto la deja sin saber qué hacer. La
// página no consulta nada: lee el fragmento (que no viaja al servidor), muestra el código y copia el
// texto que la app pega con «Paste the QR's text». La app también lee este enlace al escanear.
import { httpRouter } from "convex/server";
import { httpAction } from "./_generated/server";

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

export default http;
