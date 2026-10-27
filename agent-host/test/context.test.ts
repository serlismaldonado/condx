import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { MAX_FILE_CHARS, projectContext, repoRoot } from "../src/context.ts";

function repo(): string {
  const dir = mkdtempSync(join(tmpdir(), "condx-ctx-"));
  mkdirSync(join(dir, ".git"));
  mkdirSync(join(dir, "src", "deep"), { recursive: true });
  return dir;
}

test("finds the repo root from a subfolder, and nothing outside a repo", () => {
  const dir = repo();
  assert.equal(repoRoot(join(dir, "src", "deep")), dir);
  assert.equal(projectContext(mkdtempSync(join(tmpdir(), "condx-none-")), { claudeCode: false }), "");
});

test("other providers get AGENTS.md and CLAUDE.md; Claude Code only AGENTS.md", () => {
  const dir = repo();
  writeFileSync(join(dir, "AGENTS.md"), "Use pnpm.");
  writeFileSync(join(dir, "CLAUDE.md"), "Read the Convex guidelines first.");
  const others = projectContext(join(dir, "src"), { claudeCode: false });
  assert.match(others, /name="AGENTS.md"[\s\S]*Use pnpm\./);
  assert.match(others, /name="CLAUDE.md"[\s\S]*Convex guidelines/);
  const claude = projectContext(dir, { claudeCode: true });
  assert.match(claude, /Use pnpm\./);
  assert.doesNotMatch(claude, /Convex guidelines/);
});

test("a CLAUDE.md linked to AGENTS.md goes once, and a huge file is cut", () => {
  const dir = repo();
  writeFileSync(join(dir, "AGENTS.md"), "x".repeat(MAX_FILE_CHARS + 500));
  symlinkSync(join(dir, "AGENTS.md"), join(dir, "CLAUDE.md"));
  const out = projectContext(dir, { claudeCode: false });
  assert.equal(out.match(/<project-file/g)?.length, 1);
  assert.match(out, /cut: the file is longer/);
});
