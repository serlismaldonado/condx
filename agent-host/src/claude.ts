// Claude Code como proveedor de los agentes de Condx: en vez de llamar a una API con una clave, cada
// corrida es `claude -p` (modo no interactivo) en el repo, con la sesión de Claude Code de la persona
// (su plan). Se transmite su salida (`stream-json`) para que el teléfono vea qué hace y el texto a
// medida que llega; la conversación sigue con `--resume <sesión>`. Herramientas: las de lectura de
// Claude Code y las de Condx por su MCP (el token de agentes); escribir, solo en modo arreglo y en su
// worktree. Las corridas se guardan en un archivo por corrida (sobreviven a un reinicio del host).

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync, readdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import type { StartRun, TranscriptItem } from "./host.ts";

/** Lo que guarda una corrida de Claude Code. */
interface ClaudeRun {
  runId: string;
  mode: StartRun["mode"];
  /** El proveedor (su token, si tiene, se lee al lanzar: nunca se guarda en disco). */
  providerId?: string;
  /** Las herramientas del rol (vacío: todas las del modo). */
  tools?: string[];
  cwd: string;
  model: string;
  system: string;
  sessionId?: string;
  transcript: TranscriptItem[];
  status: "running" | "done" | "failed" | "aborted";
  reason?: string;
  /** `total`: tokens nuevos (entrada sin caché + salida: lo que cuenta para el presupuesto; el plan no
   * cobra por token). `context`: lo que ocupó la última llamada; `window`: la ventana del modelo. */
  usage: { input: number; output: number; total: number; cost: number; cacheRead?: number; context?: number; window?: number };
  createdAt: number;
}

/** Las herramientas de Condx que un agente puede usar (las mismas que la herramienta `condx` de pi). */
export function condxToolNames(tools: readonly string[]): string[] {
  return tools.map((t) => `mcp__condx__${t}`);
}

/** Git solo para leer (el historial y los cambios: el Reviewer lee el diff así). */
const GIT_READ = ["Bash(git log:*)", "Bash(git show:*)", "Bash(git diff:*)", "Bash(git blame:*)", "Bash(git status:*)"];
/** Los chequeos del proyecto (solo al arreglar, en su worktree): pruebas y tipos. */
const CHECKS = ["Bash(npm test:*)", "Bash(npm run test:*)", "Bash(npm run check:*)", "Bash(npm run typecheck:*)", "Bash(bun test:*)", "Bash(bun run test:*)", "Bash(bun run check:*)", "Bash(npx tsc:*)", "Bash(cargo check:*)", "Bash(cargo test:*)", "Bash(cargo clippy:*)"];

/** Las herramientas de pi que puede tener un rol, en las de Claude Code (lo que el rol no tiene, no lo usa). */
const PI_TO_CLAUDE: Record<string, string[]> = {
  read_file: ["Read"],
  list_files: ["Glob", "LS"],
  search_code: ["Grep"],
  git_log: ["Bash(git log:*)", "Bash(git blame:*)", "Bash(git status:*)"],
  git_show: ["Bash(git show:*)", "Bash(git diff:*)"],
  edit_file: ["Edit", "MultiEdit"],
  write_file: ["Write"],
  run_check: CHECKS,
};

/** Herramientas de Claude Code por modo: leer (y git de lectura) siempre; escribir y los chequeos solo
 * al arreglar (en su worktree). `roleTools`: las del rol (vacío: todas las del modo). */
export function claudeTools(mode: StartRun["mode"], condx: readonly string[], roleTools: readonly string[] = []): { allowed: string[]; disallowed: string[] } {
  const read = ["Read", "Grep", "Glob", "LS", ...GIT_READ, ...condxToolNames(condx)];
  const write = ["Edit", "MultiEdit", "Write", ...CHECKS];
  // Nunca: red ni tareas en segundo plano. Otros comandos: no están permitidos (en -p se niegan solos).
  const never = ["WebFetch", "WebSearch", "Task", "NotebookEdit", "KillShell", "BashOutput"];
  let allowed = mode === "fix" ? [...read, ...write] : read;
  if (roleTools.length > 0) {
    const fromRole = new Set(roleTools.flatMap((t) => (t === "condx" ? condxToolNames(condx) : PI_TO_CLAUDE[t] ?? [])));
    allowed = allowed.filter((t) => fromRole.has(t));
  }
  return { allowed, disallowed: mode === "fix" ? never : [...never, "Edit", "MultiEdit", "Write"] };
}

/** Los argumentos de `claude` para una vuelta de la corrida. */
export function claudeArgs(run: Pick<ClaudeRun, "mode" | "model" | "system" | "sessionId" | "tools">, text: string, mcpConfig: string, condx: readonly string[], addDirs: readonly string[] = []): string[] {
  const { allowed, disallowed } = claudeTools(run.mode, condx, run.tools ?? []);
  const args = ["-p", text, "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--append-system-prompt", run.system, "--mcp-config", mcpConfig, "--strict-mcp-config", "--allowedTools", allowed.join(","), "--disallowedTools", disallowed.join(","), "--permission-mode", run.mode === "fix" ? "acceptEdits" : "default", "--thinking-display", "summarized"];
  if (run.model) args.push("--model", run.model);
  if (run.sessionId) args.push("--resume", run.sessionId);
  // Carpetas fuera del repo que puede leer (las imágenes que adjuntó la persona).
  for (const d of addDirs) args.push("--add-dir", d);
  return args;
}

/** Dónde está `claude`: el servicio de launchd no tiene ~/.local/bin en el PATH. */
export function claudeBin(): string {
  if (process.env.CONDX_CLAUDE_BIN) return process.env.CONDX_CLAUDE_BIN;
  const home = homedir();
  const known = [join(home, ".local/bin/claude"), join(home, ".claude/local/claude"), "/opt/homebrew/bin/claude", "/usr/local/bin/claude", join(home, ".npm-global/bin/claude")];
  return known.find((p) => existsSync(p)) ?? "claude";
}

/** Una herramienta de Claude Code como se ve en el teléfono («Reading the code…»). */
function toolLine(name: string, input: unknown): string {
  return `→ ${name.replace(/^mcp__condx__/, "condx.")}(${JSON.stringify(input ?? {}).slice(0, 300)})`;
}

export class ClaudeCode {
  #dir: string;
  #runs = new Map<string, ClaudeRun>();
  #procs = new Map<string, ChildProcess>();
  /** Lo que escribe ahora y las herramientas en curso, por corrida (solo en memoria). */
  #live = new Map<string, { text: string; thinking: string; tools: string[] }>();
  #mcpConfig: string;
  #condx: readonly string[];
  #bin: string;

  /** El token de `claude setup-token` de un proveedor (en el sitio en la nube no hay sesión de Claude Code). */
  #token: (providerId: string | undefined) => string | undefined;

  constructor(dir: string, daemonUrl: string, daemonToken: string, condxTools: readonly string[], token: (providerId: string | undefined) => string | undefined = () => undefined) {
    this.#dir = dir;
    this.#token = token;
    this.#condx = condxTools;
    this.#bin = claudeBin();
    mkdirSync(dir, { recursive: true });
    // Las herramientas de Condx por su MCP, con el token de agentes (solo lo que pueden usar).
    this.#mcpConfig = JSON.stringify({ mcpServers: { condx: { type: "http", url: `${daemonUrl}/mcp`, headers: { Authorization: `Bearer ${daemonToken}` } } } });
    for (const f of readdirSync(dir).filter((f) => f.endsWith(".json"))) {
      try {
        const r = JSON.parse(readFileSync(join(dir, f), "utf8")) as ClaudeRun;
        // Una vuelta cortada por un reinicio no sigue sola: queda como fallida (se puede seguir escribiendo).
        if (r.status === "running") Object.assign(r, { status: "failed", reason: "the agent host restarted" });
        this.#runs.set(r.runId, r);
      } catch {
        // Un archivo dañado: se ignora.
      }
    }
  }

  has(runId: string): boolean {
    return this.#runs.has(runId);
  }

  #save(r: ClaudeRun): void {
    writeFileSync(join(this.#dir, `${r.runId.replace(/[^\w-]/g, "_")}.json`), JSON.stringify(r));
  }

  start(req: StartRun, system: string, first: string): { conversationId: number } {
    if (!this.#runs.has(req.runId)) {
      const r: ClaudeRun = { runId: req.runId, mode: req.mode, providerId: req.providerId, tools: req.tools ?? [], cwd: req.cwd, model: req.model, system, transcript: [], status: "running", usage: { input: 0, output: 0, total: 0, cost: 0 }, createdAt: Date.now() };
      this.#runs.set(req.runId, r);
      this.#turn(r, first);
    }
    return { conversationId: 0 };
  }

  /** Las imágenes de un hilo: `claude -p` no las recibe en el mensaje, así que van a un archivo fuera
   * del repo (con `--add-dir`) y el agente las abre con Read, que ve imágenes. */
  #images(runId: string, images: readonly { data: string; mimeType: string }[]): string[] {
    const dir = join(this.#dir, "attachments", runId.replace(/[^\w-]/g, "_"));
    mkdirSync(dir, { recursive: true });
    return images.map((img, i) => {
      const ext = img.mimeType.split("/")[1]?.replace("jpeg", "jpg").replace(/[^a-z0-9]/g, "") || "img";
      const path = join(dir, `image-${Date.now()}-${i + 1}.${ext}`);
      writeFileSync(path, Buffer.from(img.data, "base64"));
      return path;
    });
  }

  message(runId: string, text: string, images: readonly { data: string; mimeType: string }[] = []): void {
    const r = this.#runs.get(runId);
    if (!r) throw new Error(`there's no run ${runId}`);
    if (images.length) {
      const paths = this.#images(runId, images);
      text = `${text}\n\n(Condx note: the developer attached ${paths.length === 1 ? "an image" : `${paths.length} images`}; open ${paths.length === 1 ? "it" : "them"} with the Read tool: ${paths.join(", ")})`;
    }
    // Trabajando: se corta la vuelta y se sigue con el mensaje nuevo (como «steer»).
    this.#procs.get(runId)?.kill("SIGTERM");
    this.#turn(r, text);
  }

  abort(runId: string): void {
    const r = this.#runs.get(runId);
    if (!r) throw new Error(`there's no run ${runId}`);
    this.#procs.get(runId)?.kill("SIGTERM");
    Object.assign(r, { status: "aborted", reason: "aborted by the person" });
    this.#save(r);
  }

  #turn(r: ClaudeRun, text: string): void {
    r.status = "running";
    r.reason = undefined;
    r.transcript.push({ role: "user", text, at: Date.now() });
    this.#save(r);
    this.#live.set(r.runId, { text: "", thinking: "", tools: [] });
    // Con token (el sitio en la nube), solo para este proceso; sin él, la sesión de Claude Code de la Mac.
    const token = this.#token(r.providerId);
    const env: NodeJS.ProcessEnv = { ...process.env, CLAUDE_CODE_ENTRYPOINT: "condx" };
    if (token) env.CLAUDE_CODE_OAUTH_TOKEN = token;
    const child = spawn(this.#bin, claudeArgs(r, text, this.#mcpConfig, this.#condx, [join(this.#dir, "attachments")]), { cwd: r.cwd, stdio: ["ignore", "pipe", "pipe"], env });
    this.#procs.set(r.runId, child);
    let buf = "";
    let err = "";
    child.stdout!.on("data", (chunk: Buffer) => {
      buf += chunk.toString();
      let nl: number;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (line) this.#event(r, line);
      }
    });
    child.stderr!.on("data", (chunk: Buffer) => {
      err = (err + chunk.toString()).slice(-2_000);
    });
    child.on("error", (e) => {
      Object.assign(r, { status: "failed", reason: `couldn't run Claude Code (${this.#bin}): ${e.message}` });
      this.#save(r);
    });
    child.on("close", (code, signal) => {
      if (this.#procs.get(r.runId) === child) this.#procs.delete(r.runId);
      this.#live.delete(r.runId);
      if (r.status === "running") {
        // Sin el evento final: se cortó (otro mensaje lo reemplazó, o falló).
        if (signal) return;
        Object.assign(r, { status: code === 0 ? "done" : "failed", reason: code === 0 ? undefined : err.trim().split("\n").slice(-3).join(" ") || `claude exited with ${code}` });
      }
      this.#save(r);
    });
  }

  /** Un evento de `stream-json`: la sesión, el texto parcial, los mensajes completos y el final. */
  #event(r: ClaudeRun, line: string): void {
    let ev: any;
    try {
      ev = JSON.parse(line);
    } catch {
      return;
    }
    const live = this.#live.get(r.runId) ?? { text: "", thinking: "", tools: [] };
    if (ev.session_id && !r.sessionId) r.sessionId = ev.session_id;
    if (ev.type === "stream_event") {
      const e = ev.event;
      if (e?.type === "content_block_delta" && e.delta?.type === "text_delta") live.text += e.delta.text ?? "";
      if (e?.type === "content_block_delta" && e.delta?.type === "thinking_delta") live.thinking += e.delta.thinking ?? "";
      if (e?.type === "content_block_start" && e.content_block?.type === "tool_use") live.tools = [String(e.content_block.name ?? "").replace(/^mcp__condx__/, "condx.")];
      if (e?.type === "message_start") Object.assign(live, { text: "", thinking: "" });
    } else if (ev.type === "assistant") {
      // El contexto en uso: lo que ocupó esta llamada (entrada, con lo que se leyó y escribió en caché).
      const u = ev.message?.usage;
      if (u) r.usage.context = (u.input_tokens ?? 0) + (u.cache_read_input_tokens ?? 0) + (u.cache_creation_input_tokens ?? 0) + (u.output_tokens ?? 0);
      const parts = (ev.message?.content ?? []) as { type: string; text?: string; thinking?: string; name?: string; input?: unknown }[];
      const said = parts.map((p) => (p.type === "text" ? p.text ?? "" : p.type === "tool_use" ? toolLine(p.name ?? "", p.input) : "")).filter(Boolean).join("\n");
      // El resumen de lo que pensó (con `--thinking-display summarized`): la app lo muestra plegado.
      const thinking = parts.filter((p) => p.type === "thinking").map((p) => p.thinking ?? "").join("\n\n").trim();
      if (said || thinking) r.transcript.push({ role: "assistant", text: said, ...(thinking ? { thinking } : {}), at: Date.now() });
      if (said) live.text = "";
      if (thinking) live.thinking = "";
    } else if (ev.type === "user") {
      for (const p of (ev.message?.content ?? []) as { type: string; content?: unknown; is_error?: boolean }[]) {
        if (p.type !== "tool_result") continue;
        const out = typeof p.content === "string" ? p.content : Array.isArray(p.content) ? (p.content as { text?: string }[]).map((c) => c.text ?? "").join("\n") : "";
        r.transcript.push({ role: "tool", text: out.slice(0, 2_000), isError: Boolean(p.is_error), at: Date.now() });
      }
      live.tools = [];
    } else if (ev.type === "result") {
      const u = ev.usage ?? {};
      // Nuevos: la entrada sin caché y la que se escribió en caché, más la salida. Lo que se relee de la
      // caché (el prompt de Claude Code y la conversación) no se suma: si no, cada vuelta «gasta» 100k.
      r.usage.input += (u.input_tokens ?? 0) + (u.cache_creation_input_tokens ?? 0);
      r.usage.cacheRead = (r.usage.cacheRead ?? 0) + (u.cache_read_input_tokens ?? 0);
      r.usage.output += u.output_tokens ?? 0;
      r.usage.total = r.usage.input + r.usage.output;
      const windows = Object.values((ev.modelUsage ?? {}) as Record<string, { contextWindow?: number }>).map((m) => m.contextWindow ?? 0).filter((w) => w > 0);
      if (windows.length) r.usage.window = Math.max(...windows);
      // Con el plan no hay cobro por corrida: el costo que informa Claude Code es una estimación.
      r.usage.cost += Number(ev.total_cost_usd ?? 0);
      const failed = ev.is_error || (ev.subtype && ev.subtype !== "success");
      Object.assign(r, { status: failed ? "failed" : "done", reason: failed ? String(ev.result ?? ev.subtype) : undefined });
      if (!failed && typeof ev.result === "string" && !r.transcript.some((t) => t.role === "assistant" && t.text.includes(ev.result))) r.transcript.push({ role: "assistant", text: ev.result, at: Date.now() });
      this.#save(r);
    }
    this.#live.set(r.runId, live);
  }

  get(runId: string): Record<string, unknown> | undefined {
    const r = this.#runs.get(runId);
    if (!r) return undefined;
    const prose = (t: TranscriptItem) => t.text.split("\n").filter((l) => !l.startsWith("→ ")).join("\n").trim();
    const last = [...r.transcript].reverse().find((t) => t.role === "assistant" && prose(t));
    const live = r.status === "running" ? this.#live.get(runId) ?? { text: "", thinking: "", tools: [] } : null;
    return { runId, status: r.status, reason: r.reason ?? null, answer: last ? prose(last) : null, transcript: r.transcript, live, usage: r.usage, conversationId: 0 };
  }

  close(): void {
    for (const p of this.#procs.values()) p.kill("SIGTERM");
  }
}
