// Pruebas del host sin modelo real: el proveedor guionado de pi-ai responde lo que dice `faux`.
//   node --experimental-strip-types --test test/host.test.ts

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import assert from "node:assert/strict";
import { test } from "node:test";

import { clip, inside, MAX_OUTPUT } from "../src/tools.ts";

test("las rutas no salen del repo ni leen secretos", () => {
  const root = "/work/repo";
  assert.equal(inside(root, "src/a.ts"), "/work/repo/src/a.ts");
  assert.equal(inside(root, "./src/../b.ts"), "/work/repo/b.ts");
  for (const bad of ["../x", "/etc/passwd", "src/../../x", ".env", "app/.env.local", "certs/server.pem", ".git/config", "config/secrets.json", "id_rsa"]) {
    assert.throws(() => inside(root, bad), new RegExp("outside|secrets"), bad);
  }
});

test("la salida queda acotada", () => {
  const long = "x".repeat(MAX_OUTPUT * 2);
  assert.ok(clip(long).length < MAX_OUTPUT + 100);
  assert.match(clip(Array.from({ length: 1000 }, (_, i) => `l${i}`).join("\n")), /cut/);
});

const TOKEN = "host-token-for-tests";

function repo(): string {
  const dir = mkdtempSync(join(tmpdir(), "condx-agent-repo-"));
  mkdirSync(join(dir, "convex"));
  writeFileSync(join(dir, "convex", "seed.ts"), "export const demoData = mutation(async (ctx) => {\n  if ((await ctx.db.query('tasks').first()) !== null) throw new Error('seed:demoData only runs on an empty database');\n});\n");
  writeFileSync(join(dir, ".env.local"), "CONVEX_DEPLOY_KEY=prod:supersecret\n");
  const git = (...a: string[]) => execFileSync("git", a, { cwd: dir, stdio: "ignore" });
  git("init", "-q");
  git("-c", "user.email=t@t", "-c", "user.name=t", "add", "convex/seed.ts");
  git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "feat: seed demo data");
  return dir;
}

async function start(storage: string, faux: unknown[]): Promise<{ child: ChildProcess; url: string }> {
  const child = spawn(process.execPath, ["--experimental-strip-types", "--no-warnings", join(import.meta.dirname, "..", "src", "main.ts")], { stdio: ["pipe", "pipe", "inherit"] });
  child.stdin!.write(`${JSON.stringify({ hostToken: TOKEN, daemonUrl: "http://127.0.0.1:9", daemonToken: "x", storage, providers: [], faux })}\n`);
  const line = await new Promise<string>((ok) => createInterface({ input: child.stdout! }).once("line", ok));
  return { child, url: `http://127.0.0.1:${JSON.parse(line).listening}` };
}

async function call(url: string, method: string, path: string, body?: unknown, token = TOKEN): Promise<{ status: number; json: any }> {
  const res = await fetch(url + path, { method, headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: body ? JSON.stringify(body) : undefined });
  return { status: res.status, json: await res.json() };
}

async function until(f: () => Promise<boolean>, what: string): Promise<void> {
  for (let i = 0; i < 100; i++) {
    if (await f()) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`timed out waiting for ${what}`);
}

test("una corrida de diagnóstico lee el repo, responde y sobrevive a un reinicio", async () => {
  const dir = repo();
  const storage = join(mkdtempSync(join(tmpdir(), "condx-agent-store-")), "agents.sqlite");
  const faux = [
    { tool: "search_code", args: { pattern: "only runs on an empty" } },
    { tool: "read_file", args: { path: "convex/seed.ts" } },
    { tool: "read_file", args: { path: ".env.local" } },
    { text: "## Cause\nThe seed refuses to run on a non-empty database.\n\n## Confidence\nhigh" },
  ];
  let { child, url } = await start(storage, faux);
  try {
    assert.equal((await call(url, "GET", "/health", undefined, "wrong")).status, 401);
    const incident = { title: "Seed data failed on dev", evidence: { error: "Uncaught Error: seed:demoData only runs on an empty database" } };
    const run = { runId: "run-1", mode: "diagnose", providerId: "anthropic", model: "x", cwd: dir, incident };
    const first = await call(url, "POST", "/runs", run);
    assert.equal(first.status, 200, JSON.stringify(first.json));
    // El mismo runId no arranca otra.
    assert.equal((await call(url, "POST", "/runs", run)).json.conversationId, first.json.conversationId);
    let got: any;
    await until(async () => (got = (await call(url, "GET", "/runs/run-1")).json).status === "done", "the run to finish");
    assert.match(got.answer, /## Cause/);
    const tools = got.transcript.filter((t: any) => t.role === "tool");
    assert.equal(tools[0].tool, "search_code");
    assert.match(tools[0].text, /convex\/seed\.ts:2:/);
    assert.match(tools[1].text, /only runs on an empty database/);
    // El archivo de secretos no se leyó.
    assert.equal(tools.length, 3, JSON.stringify(tools));
    assert.equal(tools[2].isError, true);
    assert.match(tools[2].text, /may hold secrets/);
    assert.doesNotMatch(JSON.stringify(got), /supersecret/);
    assert.ok(got.usage.total > 0);
    // Un mensaje de la persona sigue la conversación.
    assert.equal((await call(url, "POST", "/runs/run-1/message", { text: "Are you sure?" })).status, 200);
    await until(async () => (got = (await call(url, "GET", "/runs/run-1")).json).status === "done" && got.transcript.some((t: any) => t.text === "Are you sure?"), "the follow-up");
  } finally {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
  // Otro proceso sobre el mismo almacén encuentra la corrida.
  ({ child, url } = await start(storage, faux));
  try {
    const again = (await call(url, "GET", "/runs/run-1")).json;
    assert.match(again.answer, /## Cause|Are you sure/);
    assert.ok(again.transcript.length >= 6);
    assert.equal((await call(url, "GET", "/runs/nope")).status, 404);
  } finally {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
});

function fixRepo(): string {
  const dir = mkdtempSync(join(tmpdir(), "condx-agent-fix-"));
  writeFileSync(join(dir, "sum.js"), "export const sum = (a, b) => a - b;\n");
  writeFileSync(join(dir, "test.js"), "import { sum } from './sum.js';\nif (sum(2, 3) !== 5) { console.log('FAIL sum(2,3)=' + sum(2, 3)); process.exit(1); }\nconsole.log('PASS');\n");
  writeFileSync(join(dir, "net.js"), "fetch('https://example.com').then(() => console.log('ONLINE')).catch(() => console.log('OFFLINE'));\n");
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: "fixme", type: "module", scripts: { test: "node test.js", net: "node net.js" } }));
  const git = (...a: string[]) => execFileSync("git", a, { cwd: dir, stdio: "ignore" });
  git("init", "-q");
  git("-c", "user.email=t@t", "-c", "user.name=t", "add", "-A");
  git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "sum");
  // Un script que el agente agregara después no cuenta: solo los del commit base.
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: "fixme", type: "module", scripts: { test: "node test.js", net: "node net.js", evil: "curl https://example.com" } }));
  return dir;
}

test("en modo arreglo edita su worktree, corre los chequeos sin red y solo los scripts del commit base", async () => {
  const dir = fixRepo();
  const storage = join(mkdtempSync(join(tmpdir(), "condx-agent-store-")), "agents.sqlite");
  const faux = [
    { tool: "run_check", args: { script: "test" } },
    { tool: "edit_file", args: { path: "sum.js", old_text: "a - b", new_text: "a + b" } },
    { tool: "run_check", args: { script: "test" } },
    { tool: "run_check", args: { script: "evil" } },
    { tool: "run_check", args: { script: "net" } },
    { tool: "edit_file", args: { path: "../outside.js", old_text: "x", new_text: "y" } },
    { text: "## Cause\nsum subtracted.\n\n## Change\nsum.js: add instead of subtract.\n\n## Checks\ntest passes.\n\n## Confidence\nhigh" },
  ];
  const { child, url } = await start(storage, faux);
  try {
    await call(url, "POST", "/runs", { runId: "fix-1", mode: "fix", providerId: "x", model: "x", cwd: dir, incident: { title: "Tests fail" } });
    let got: any;
    await until(async () => (got = (await call(url, "GET", "/runs/fix-1")).json).status === "done", "the fix run");
    const tools = got.transcript.filter((t: any) => t.role === "tool");
    assert.match(tools[0].text, /exit 1[\s\S]*FAIL sum\(2,3\)=-1/, tools[0].text);
    assert.equal(tools[1].text, "edited sum.js");
    assert.match(tools[2].text, /^exit 0[\s\S]*PASS/, tools[2].text);
    assert.match(tools[3].text, /no script “evil”/);
    assert.match(tools[4].text, /OFFLINE/, tools[4].text);
    assert.doesNotMatch(tools[4].text, /ONLINE/);
    assert.match(tools[5].text, /outside the repository/);
    assert.equal(execFileSync("git", ["diff", "--stat"], { cwd: dir, encoding: "utf8" }).includes("sum.js"), true);
  } finally {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
});

test("en modo diagnóstico no hay herramientas que escriban", async () => {
  const dir = fixRepo();
  const storage = join(mkdtempSync(join(tmpdir(), "condx-agent-store-")), "agents.sqlite");
  const faux = [{ tool: "edit_file", args: { path: "sum.js", old_text: "a - b", new_text: "a + b" } }, { text: "## Cause\nx" }];
  const { child, url } = await start(storage, faux);
  try {
    await call(url, "POST", "/runs", { runId: "diag-1", mode: "diagnose", providerId: "x", model: "x", cwd: dir, incident: { title: "x" } });
    let got: any;
    await until(async () => (got = (await call(url, "GET", "/runs/diag-1")).json).status === "done", "the run");
    const tool = got.transcript.find((t: any) => t.role === "tool");
    assert.equal(tool.isError, true, JSON.stringify(tool));
    assert.match(execFileSync("cat", ["sum.js"], { cwd: dir, encoding: "utf8" }), /a - b/);
  } finally {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
});
