// Host de agentes de Condx. Lo arranca el daemon (agent_host.rs), que le manda su configuración en
// una línea JSON por stdin — con las API keys descifradas, que así no quedan en el entorno del
// proceso ni en disco — y lee el puerto en la primera línea de stdout. Escucha solo en 127.0.0.1 y
// cada pedido lleva el token que le dio el daemon.
//
//   POST /providers            { providers: [...] }        reemplaza los proveedores (y sus claves)
//   POST /runs                 StartRun                    arranca una corrida (idempotente por runId)
//   GET  /runs/:id                                         estado, respuesta, transcripción, gasto
//   POST /runs/:id/message     { text }                    redirige o sigue la conversación
//   POST /runs/:id/abort
//   GET  /health

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { createInterface } from "node:readline";

import { fauxAssistantMessage, fauxProvider, fauxText, fauxToolCall } from "@earendil-works/pi-ai";

import { Host, type StartRun } from "./host.ts";
import { type ProviderConfig, Providers } from "./providers.ts";

interface Config {
  hostToken: string;
  daemonUrl: string;
  daemonToken: string;
  storage: string;
  providers: ProviderConfig[];
  /** Solo pruebas: respuestas guionadas en vez de un modelo real. */
  faux?: FauxStep[];
}

type FauxStep = { text: string } | { tool: string; args: Parameters<typeof fauxToolCall>[1] };

function same(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let d = 0;
  for (let i = 0; i < a.length; i++) d |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return d === 0;
}

async function body(req: IncomingMessage): Promise<any> {
  let raw = "";
  for await (const chunk of req) {
    raw += chunk;
    if (raw.length > 1_000_000) throw new Error("body too large");
  }
  return raw ? JSON.parse(raw) : {};
}

function send(res: ServerResponse, status: number, value: unknown): void {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(value));
}

async function main(): Promise<void> {
  const line = await new Promise<string>((ok) => createInterface({ input: process.stdin }).once("line", ok));
  const config = JSON.parse(line) as Config;
  const providers = new Providers();

  // Modo de prueba: un proveedor guionado que responde con las mismas respuestas en cada corrida.
  let fauxModel: { provider: string; id: string } | undefined;
  let resetFaux = () => {};
  if (config.faux) {
    const faux = fauxProvider();
    providers.models.setProvider(faux.provider);
    fauxModel = faux.getModel();
    const script = config.faux;
    resetFaux = () =>
      faux.setResponses(script.map((s) => ("text" in s ? fauxAssistantMessage([fauxText(s.text)]) : fauxAssistantMessage([fauxToolCall(s.tool, s.args)], { stopReason: "toolUse" }))));
    await providers.set([{ id: "faux", kind: "faux", model: fauxModel.id }]);
  } else {
    await providers.set(config.providers);
  }

  const host = await Host.open(config.storage, providers, config.daemonUrl, config.daemonToken);
  if (fauxModel) {
    // Las corridas de prueba usan el modelo guionado, sea cual sea el proveedor pedido.
    const start = host.start.bind(host);
    host.start = async (req: StartRun) => {
      if (!(await host.get(req.runId))) resetFaux();
      return start({ ...req, providerId: "faux", model: fauxModel!.id });
    };
  }

  const server = createServer(async (req, res) => {
    try {
      const auth = req.headers.authorization ?? "";
      if (!same(auth, `Bearer ${config.hostToken}`)) return send(res, 401, { error: "unauthorized" });
      const url = new URL(req.url ?? "/", "http://127.0.0.1");
      const parts = url.pathname.split("/").filter(Boolean);
      if (req.method === "GET" && url.pathname === "/health") return send(res, 200, { ok: true });
      if (req.method === "POST" && url.pathname === "/providers") {
        if (!fauxModel) await providers.set((await body(req)).providers ?? []);
        return send(res, 200, { ok: true });
      }
      if (parts[0] === "runs") {
        if (req.method === "POST" && parts.length === 1) return send(res, 200, await host.start((await body(req)) as StartRun));
        const id = parts[1];
        if (!id) return send(res, 404, { error: "not found" });
        if (req.method === "GET" && parts.length === 2) {
          const run = await host.get(id);
          return run ? send(res, 200, run) : send(res, 404, { error: `there's no run ${id}` });
        }
        if (req.method === "POST" && parts[2] === "message") {
          const text = String((await body(req)).text ?? "").slice(0, 4_000);
          if (!text.trim()) return send(res, 400, { error: "empty message" });
          if (fauxModel) resetFaux();
          await host.message(id, text);
          return send(res, 200, { ok: true });
        }
        if (req.method === "POST" && parts[2] === "abort") {
          await host.abort(id);
          return send(res, 200, { ok: true });
        }
      }
      send(res, 404, { error: "not found" });
    } catch (e) {
      send(res, 400, { error: e instanceof Error ? e.message : String(e) });
    }
  });
  server.listen(0, "127.0.0.1", () => {
    const address = server.address();
    process.stdout.write(`${JSON.stringify({ listening: typeof address === "object" && address ? address.port : 0 })}\n`);
  });
  const stop = async () => {
    server.close();
    await host.close().catch(() => {});
    process.exit(0);
  };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
  // Si el daemon se va (cierra el stdin), el host también.
  process.stdin.on("end", stop);
}

main().catch((e) => {
  process.stderr.write(`agent host failed: ${e instanceof Error ? e.stack : String(e)}\n`);
  process.exit(1);
});
