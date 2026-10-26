import { test } from "node:test";
import assert from "node:assert/strict";
import { renderEmail } from "../src/email.ts";

test("la respuesta de un agente sale como correo con formato y texto", async () => {
  const md = "El último job fue **Deploy to cloud dev** (`j1`).\n\n| ID | Estado |\n|---|---|\n| `j1` | expirado |\n\n```rust\nfn main() {}\n```";
  const { html, text } = await renderEmail({ markdown: md, title: "Agent · Why did the last job fail?", footer: "Responde este correo para seguir." });
  assert.match(html, /<html/);
  assert.match(html, /<strong[^>]*>Deploy to cloud dev<\/strong>/);
  assert.match(html, /<table/);
  assert.match(html, /<code/);
  assert.match(html, /Responde este correo para seguir/);
  assert.match(html, /<img[^>]+condx-mark\.png/);
  assert.doesNotMatch(html, /\*\*Deploy/);
  assert.match(text, /Deploy to cloud dev/);
  assert.doesNotMatch(text, /\*\*/);
});

test("un correo vacío no se arma", async () => {
  await assert.rejects(() => renderEmail({ markdown: "  " }));
});
