import { test } from "node:test";
import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ClaudeCode, claudeArgs, claudeTools } from "../src/claude.ts";

test("Claude Code: leer y git de lectura siempre; escribir y chequeos solo al arreglar; las del rol limitan", () => {
  const chat = claudeTools("chat", ["get_overview"]);
  assert.ok(chat.allowed.includes("Read") && chat.allowed.includes("mcp__condx__get_overview") && chat.allowed.includes("Bash(git show:*)"));
  assert.ok(!chat.allowed.includes("Edit") && chat.disallowed.includes("Edit") && !chat.allowed.includes("Bash(cargo test:*)"));
  assert.ok(!chat.allowed.includes("Bash"), "never a free shell");
  const fix = claudeTools("fix", []);
  assert.ok(fix.allowed.includes("Edit") && fix.allowed.includes("Bash(cargo test:*)") && !fix.disallowed.includes("Edit"));
  // Un rol con solo leer archivos y el diff: nada más.
  const reviewer = claudeTools("task", ["get_overview"], ["read_file", "git_show"]);
  assert.deepEqual(reviewer.allowed, ["Read", "Bash(git show:*)", "Bash(git diff:*)"]);
  const args = claudeArgs({ mode: "chat", model: "sonnet", system: "S", sessionId: "abc" }, "hola", "{}", []);
  assert.deepEqual(args.slice(0, 2), ["-p", "hola"]);
  assert.ok(args.includes("--resume") && args[args.indexOf("--resume") + 1] === "abc");
  assert.equal(args[args.indexOf("--permission-mode") + 1], "default");
});

test("Claude Code: transmite el texto, guarda la sesión y la respuesta, y sigue la conversación", async () => {
  const dir = mkdtempSync(join(tmpdir(), "condx-claude-"));
  // Un `claude` falso: escribe stream-json como el de verdad (y repite el --resume que recibe).
  const bin = join(dir, "claude");
  writeFileSync(
    bin,
    `#!/usr/bin/env node
const a = process.argv.slice(2);
const resumed = a.includes("--resume") ? a[a.indexOf("--resume") + 1] : null;
const out = (o) => process.stdout.write(JSON.stringify(o) + "\\n");
out({ type: "system", subtype: "init", session_id: "s-1" });
out({ type: "stream_event", event: { type: "content_block_delta", delta: { type: "text_delta", text: "Hol" } } });
out({ type: "assistant", message: { content: [{ type: "tool_use", name: "mcp__condx__get_overview", input: {} }] } });
out({ type: "user", message: { content: [{ type: "tool_result", content: "ok" }] } });
out({ type: "assistant", message: { content: [{ type: "text", text: resumed ? "Seguimos (" + resumed + ")" : "Hola, todo bien." }] } });
out({ type: "result", subtype: "success", result: resumed ? "Seguimos (" + resumed + ")" : "Hola, todo bien.", usage: { input_tokens: 10, output_tokens: 5 }, total_cost_usd: 0.01, session_id: "s-1" });
`,
  );
  chmodSync(bin, 0o755);
  process.env.CONDX_CLAUDE_BIN = bin;
  const cc = new ClaudeCode(join(dir, "runs"), "http://127.0.0.1:1", "t", ["get_overview"]);
  const wait = async (status: string) => {
    for (let i = 0; i < 100; i++) {
      if ((cc.get("r1") as { status: string }).status === status) return;
      await new Promise((r) => setTimeout(r, 30));
    }
    throw new Error(`never ${status}: ${JSON.stringify(cc.get("r1"))}`);
  };
  cc.start({ runId: "r1", mode: "chat", providerId: "claude-code", model: "sonnet", cwd: dir, text: "hola" }, "S", "hola");
  await wait("done");
  let r = cc.get("r1") as { answer: string; transcript: { role: string; text: string }[]; usage: { total: number } };
  assert.equal(r.answer, "Hola, todo bien.");
  assert.ok(r.transcript.some((t) => t.role === "assistant" && t.text.startsWith("→ condx.get_overview")));
  assert.equal(r.usage.total, 15);
  cc.message("r1", "¿y ahora?");
  await new Promise((res) => setTimeout(res, 50));
  await wait("done");
  r = cc.get("r1") as typeof r;
  assert.equal(r.answer, "Seguimos (s-1)");
  // Otra instancia (un reinicio del host) la encuentra en disco.
  const again = new ClaudeCode(join(dir, "runs"), "http://127.0.0.1:1", "t", []);
  assert.equal((again.get("r1") as { answer: string }).answer, "Seguimos (s-1)");
});
