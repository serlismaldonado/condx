// Herramientas del agente de diagnóstico: solo leen, y solo dentro del repo del incidente (el `cwd`
// de su conversación). Nada de shell: cada una corre un programa fijo con argumentos (`git`), con
// tiempo y salida acotados. Los archivos de secretos no se leen aunque estén en el repo.

import { execFile } from "node:child_process";
import { readFile, stat } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";

import type { Context } from "@earendil-works/chord/context";
import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ToolExecutionApi } from "@earendil-works/pi-durable";

/** Lo que una herramienta devuelve al modelo como mucho. */
export const MAX_OUTPUT = 40_000;
const MAX_LINES = 400;

/** Archivos que nunca se leen: variables de entorno, llaves, credenciales. */
const SECRET = [/(^|\/)\.env(\.|$)/i, /\.(pem|key|p12|pfx|keystore|jks)$/i, /(^|\/)id_(rsa|ed25519|ecdsa)/i, /(^|\/)\.(npmrc|netrc|pgpass)$/i, /(^|\/)\.condx(\/|$)/, /(^|\/)\.git\//, /secrets?\.(json|ya?ml|toml)$/i];

export class ToolError extends Error {}

/** La ruta `path` dentro de `root`, o un error si sale de él o es un archivo de secretos. */
export function inside(root: string, path: string): string {
  const full = resolve(root, path);
  const rel = relative(root, full);
  if (rel === "" ) return full;
  if (rel.startsWith("..") || isAbsolute(rel) || rel.split(sep)[0] === "..") throw new ToolError(`${path} is outside the repository`);
  if (SECRET.some((re) => re.test(rel.split(sep).join("/")))) throw new ToolError(`${path} may hold secrets: not readable`);
  return full;
}

export function clip(text: string): string {
  let lines = text.split("\n");
  let cut = false;
  if (lines.length > MAX_LINES) {
    lines = lines.slice(0, MAX_LINES);
    cut = true;
  }
  let out = lines.join("\n");
  if (out.length > MAX_OUTPUT) {
    out = out.slice(0, MAX_OUTPUT);
    cut = true;
  }
  return cut ? `${out}\n[… cut: ask for a narrower range]` : out;
}

function run(cmd: string, args: string[], cwd: string, timeoutMs = 15_000): Promise<string> {
  return new Promise((ok, fail) => {
    execFile(cmd, args, { cwd, timeout: timeoutMs, maxBuffer: 4 * 1024 * 1024, env: { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: process.env.HOME ?? "", GIT_PAGER: "cat", GIT_TERMINAL_PROMPT: "0" } }, (err, stdout, stderr) => {
      // `git grep` sin coincidencias sale con 1: no es un error.
      if (err && !(cmd === "git" && args[0] === "grep" && (err as { code?: number }).code === 1)) {
        fail(new ToolError((stderr || err.message).trim().split("\n").slice(-3).join("\n")));
      } else ok(stdout);
    });
  });
}

async function repo(api: ToolExecutionApi, context: Context): Promise<string> {
  const cwd = (await api.agent(context)).cwd;
  if (!cwd) throw new ToolError("this conversation has no repository");
  return cwd;
}

const text = (t: string) => ({ output: [{ type: "text" as const, text: t }] });

export const readFileTool = defineTool({
  name: "read_file",
  description: "Read a text file of the repository (path relative to its root). Optionally a range of lines.",
  parameters: Type.Object({ path: Type.String(), from_line: Type.Optional(Type.Number()), to_line: Type.Optional(Type.Number()) }),
  replay: "safe",
  execute: async (args, api, context) => {
    const full = inside(await repo(api, context), args.path);
    const info = await stat(full).catch(() => undefined);
    if (!info?.isFile()) throw new ToolError(`${args.path} isn't a file`);
    if (info.size > 2 * 1024 * 1024) throw new ToolError(`${args.path} is too big (${info.size} bytes)`);
    const lines = (await readFile(full, "utf8")).split("\n");
    const from = Math.max(1, Math.floor(args.from_line ?? 1));
    const to = Math.min(lines.length, Math.floor(args.to_line ?? lines.length));
    return text(clip(lines.slice(from - 1, to).map((l, i) => `${from + i}\t${l}`).join("\n")));
  },
});

export const listFilesTool = defineTool({
  name: "list_files",
  description: "List the repository's tracked files, optionally under a directory or matching a substring.",
  parameters: Type.Object({ under: Type.Optional(Type.String()), contains: Type.Optional(Type.String()) }),
  replay: "safe",
  execute: async (args, api, context) => {
    const root = await repo(api, context);
    const under = args.under ? relative(root, inside(root, args.under)) : "";
    const files = (await run("git", ["ls-files", "--", under || "."], root)).split("\n").filter((f) => f && (!args.contains || f.includes(args.contains)));
    return text(clip(files.join("\n") || "(no files)"));
  },
});

export const searchCodeTool = defineTool({
  name: "search_code",
  description: "Search the repository's tracked files for a regular expression (git grep). Returns path:line:text.",
  parameters: Type.Object({ pattern: Type.String(), under: Type.Optional(Type.String()), ignore_case: Type.Optional(Type.Boolean()) }),
  replay: "safe",
  execute: async (args, api, context) => {
    const root = await repo(api, context);
    const under = args.under ? relative(root, inside(root, args.under)) : ".";
    const flags = ["grep", "-n", "-I", "--full-name", "-E", ...(args.ignore_case ? ["-i"] : []), "-e", args.pattern, "--", under];
    const out = await run("git", flags, root);
    // Sin líneas de archivos de secretos.
    const lines = out.split("\n").filter((l) => l && !SECRET.some((re) => re.test(l.split(":")[0] ?? "")));
    return text(clip(lines.join("\n") || "(no matches)"));
  },
});

export const gitLogTool = defineTool({
  name: "git_log",
  description: "Recent commits (hash, date, author, subject), optionally only those touching a path.",
  parameters: Type.Object({ path: Type.Optional(Type.String()), limit: Type.Optional(Type.Number()) }),
  replay: "safe",
  execute: async (args, api, context) => {
    const root = await repo(api, context);
    const n = String(Math.min(50, Math.max(1, Math.floor(args.limit ?? 15))));
    const path = args.path ? ["--", relative(root, inside(root, args.path))] : [];
    return text(clip(await run("git", ["log", "-n", n, "--date=short", "--format=%h %ad %an %s", ...path], root)));
  },
});

export const gitShowTool = defineTool({
  name: "git_show",
  description: "A commit's message and diff (or only the files it changed with stat_only).",
  parameters: Type.Object({ commit: Type.String(), stat_only: Type.Optional(Type.Boolean()) }),
  replay: "safe",
  execute: async (args, api, context) => {
    if (!/^[0-9a-zA-Z._\/~^-]{1,80}$/.test(args.commit) || args.commit.startsWith("-")) throw new ToolError("not a commit");
    const root = await repo(api, context);
    const out = await run("git", ["show", "--no-color", ...(args.stat_only ? ["--stat"] : ["--stat", "--patch"]), args.commit, "--", ".", ":(exclude).env*"], root);
    return text(clip(out));
  },
});

/** Las herramientas de lectura de Condx que el agente puede usar (el daemon también lo limita). */
export const CONDX_TOOLS = ["get_overview", "list_environments", "get_environment", "get_env_var_names", "get_git", "get_deployer", "get_vercel", "get_frontend", "list_incidents", "list_recipes", "list_jobs", "get_job", "get_job_log", "list_workflows", "get_workflow_run", "get_errors"] as const;

export function condxTool(daemonUrl: string, token: string) {
  return defineTool({
    name: "condx",
    description: `Read from Condx (the developer daemon watching this project): ${CONDX_TOOLS.join(", ")}. Pass the tool name and its arguments.`,
    parameters: Type.Object({ tool: Type.String(), args: Type.Optional(Type.Record(Type.String(), Type.Unknown())) }),
    replay: "safe",
    execute: async (args) => {
      if (!(CONDX_TOOLS as readonly string[]).includes(args.tool)) throw new ToolError(`${args.tool} isn't available to agents`);
      const res = await fetch(`${daemonUrl}/mcp`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: args.tool, arguments: args.args ?? {} } }),
        signal: AbortSignal.timeout(30_000),
      });
      const body = (await res.json()) as { result?: { content?: { text?: string }[]; isError?: boolean }; error?: { message?: string } };
      if (body.error) throw new ToolError(body.error.message ?? "Condx error");
      const out = (body.result?.content ?? []).map((c) => c.text ?? "").join("\n");
      if (body.result?.isError) throw new ToolError(out || "Condx error");
      return text(clip(out));
    },
  });
}

export const READ_TOOLS = [readFileTool, listFilesTool, searchCodeTool, gitLogTool, gitShowTool];
