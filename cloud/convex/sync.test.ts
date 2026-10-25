/// <reference types="vite/client" />
// Condx Cloud: semántica del protocolo, seguridad, y una prueba diferencial contra el servidor falso
// de la librería (la misma secuencia de grupos tiene que dar lo mismo en los dos).

import { convexTest } from "convex-test";
import { describe, expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import schema from "./schema";
import { PAGE_SIZE } from "./sync";
import { FakeServer, rng } from "localfirst/testing";
import type { Op, OpGroup, PullResult } from "localfirst/core";

const modules = import.meta.glob("./**/*.ts");

const A = { daemonId: "daemon-aaaa", token: "a".repeat(40) };
const B = { daemonId: "daemon-bbbb", token: "b".repeat(40) };
const ENV = `env:${A.daemonId}`;
const APPROVALS = `approvals:${A.daemonId}`;
const PV = 1;

async function setup() {
  const t = convexTest(schema, modules);
  await t.mutation(internal.daemons.register, { ...A, name: "Mac A" });
  await t.mutation(internal.daemons.register, { ...B, name: "Mac B" });
  return t;
}

let seq = 0;
const op = (partial: Partial<Op> & Pick<Op, "kind" | "rowId">, scope = ENV): Op => ({ opId: `op_${++seq}`, scope, fieldsVersion: 0, madeAt: Date.now(), ...partial });
const group = (...ops: Op[]): OpGroup => ({ groupId: `g_${++seq}`, seq, ops, createdAt: Date.now(), attempts: 0 });

type T = Awaited<ReturnType<typeof setup>>;
const push = (t: T, g: OpGroup, token = A.token) => t.mutation(api.sync.push, { token, group: g, protocolVersion: PV });
const pull = (t: T, scope: string, cursor: number | null, bootstrap?: string, token = A.token) =>
  t.query(api.sync.pull, { token, scope, cursor, bootstrap, protocolVersion: PV }) as Promise<PullResult>;

/** Foto inicial completa (todas las páginas). */
async function bootstrapAll(t: T, scope: string, token = A.token) {
  const rows = new Map<string, Record<string, unknown>>();
  let bootstrap: string | undefined;
  for (;;) {
    const r = await pull(t, scope, null, bootstrap, token);
    if (r.status !== "ok") throw new Error(r.status);
    for (const row of r.rows) rows.set(row.id, row.values);
    if (!r.hasMore) return { rows, cursor: r.cursor };
    bootstrap = r.bootstrap;
  }
}

describe("protocolo", () => {
  test("crear, cambiar, borrar; foto inicial e incremental", async () => {
    const t = await setup();
    const created = await push(t, group(op({ kind: "create", rowId: "e1", set: { name: "local", health: "up" } })));
    expect(created).toMatchObject({ status: "ok", results: [{ version: 1 }] });
    expect(created.results![0].docId).toBeTruthy();
    const first = await bootstrapAll(t, ENV);
    expect(first.cursor).toBe(1);
    expect(first.rows.get("e1")).toEqual({ name: "local", health: "up" });

    await push(t, group(op({ kind: "patch", rowId: "e1", set: { health: null, commit: "abc" }, base: { health: 1 } })));
    await push(t, group(op({ kind: "create", rowId: "e2", set: { name: "prod" } })));
    await push(t, group(op({ kind: "delete", rowId: "e2" })));
    const inc = await pull(t, ENV, 1);
    expect(inc).toMatchObject({ status: "ok", cursor: 4, hasMore: false, deletes: ["e2"] });
    if (inc.status === "ok") expect(inc.rows.map((r) => [r.id, r.values])).toEqual([["e1", { name: "local", commit: "abc" }]]);
  });

  test("idempotente: el mismo grupo dos veces aplica una vez", async () => {
    const t = await setup();
    const g = group(op({ kind: "create", rowId: "j1", set: { status: "queued" } }, `jobs:${A.daemonId}`));
    const first = await push(t, g);
    const second = await push(t, g);
    expect(second).toEqual(first);
    const doorbell = await t.query(api.sync.doorbell, { token: A.token, scopes: [`jobs:${A.daemonId}`], protocolVersion: PV });
    expect(doorbell).toEqual({ status: "ok", versions: { [`jobs:${A.daemonId}`]: 1 } });
  });

  test("grupo atómico: si una operación se rechaza no queda nada", async () => {
    const t = await setup();
    await expect(push(t, group(op({ kind: "create", rowId: "x", set: { a: 1 } }), op({ kind: "patch", rowId: "missing", set: { a: 2 } })))).rejects.toThrow(/not_found|Record not found/);
    expect((await bootstrapAll(t, ENV)).rows.size).toBe(0);
    const bell = await t.query(api.sync.doorbell, { token: A.token, scopes: [ENV], protocolVersion: PV });
    expect(bell).toEqual({ status: "ok", versions: { [ENV]: 0 } });
  });

  test("conflicto por campo: revisión en las aprobaciones, último gana en lo demás", async () => {
    const t = await setup();
    await push(t, group(op({ kind: "create", rowId: "job1", set: { status: "waiting-approval", note: "x" } }, APPROVALS)));
    // El daemon cambia el estado (el trabajo se canceló) después de lo que vio el teléfono.
    await push(t, group(op({ kind: "patch", rowId: "job1", set: { status: "cancelled" }, base: { status: 1 } }, APPROVALS)));
    // El teléfono decide sobre lo viejo: `status` es de revisión → conflicto, no se aplica.
    const r = await push(t, group(op({ kind: "patch", rowId: "job1", set: { status: "approved", note: "ok" }, base: { status: 1, note: 1 } }, APPROVALS)));
    expect(r.results![0]).toMatchObject({ conflicts: [{ field: "status", local: "approved", server: "cancelled" }] });
    expect((await bootstrapAll(t, APPROVALS)).rows.get("job1")).toEqual({ status: "cancelled", note: "ok" });
    // En un alcance sin campos de revisión: último gana y queda marcado.
    await push(t, group(op({ kind: "create", rowId: "e", set: { health: "up" } })));
    await push(t, group(op({ kind: "patch", rowId: "e", set: { health: "down" }, base: { health: 1 } })));
    const lw = await push(t, group(op({ kind: "patch", rowId: "e", set: { health: "slow" }, base: { health: 1 } })));
    expect(lw.results![0]).toMatchObject({ overwrote: ["health"] });
  });

  test("conjuntos que convergen", async () => {
    const t = await setup();
    await push(t, group(op({ kind: "create", rowId: "w", set: { tags: ["z"] } }, `workflows:${A.daemonId}`)));
    await push(t, group(op({ kind: "patch", rowId: "w", sets: { tags: { add: ["x"] } } }, `workflows:${A.daemonId}`)));
    await push(t, group(op({ kind: "patch", rowId: "w", sets: { tags: { add: ["y"], remove: ["z"] } } }, `workflows:${A.daemonId}`)));
    expect((await bootstrapAll(t, `workflows:${A.daemonId}`)).rows.get("w")).toEqual({ tags: ["x", "y"] });
  });

  test("foto inicial paginada con el cursor fijo; lo que cambia mientras llega por el incremental", async () => {
    const t = await setup();
    const n = PAGE_SIZE + 37;
    for (let i = 0; i < n; i += 50) {
      await push(t, group(...Array.from({ length: Math.min(50, n - i) }, (_, k) => op({ kind: "create", rowId: `r${String(i + k).padStart(4, "0")}`, set: { n: i + k } }))));
    }
    const p1 = await pull(t, ENV, null);
    expect(p1).toMatchObject({ status: "ok", hasMore: true, cursor: n });
    // Cambio en medio de la foto.
    await push(t, group(op({ kind: "patch", rowId: "r0000", set: { n: -1 } })));
    if (p1.status !== "ok") throw new Error();
    const p2 = await pull(t, ENV, null, p1.bootstrap);
    expect(p2).toMatchObject({ status: "ok", hasMore: false, cursor: n });
    if (p2.status !== "ok") throw new Error();
    expect(p1.rows.length + p2.rows.length).toBe(n);
    const inc = await pull(t, ENV, n);
    if (inc.status !== "ok") throw new Error();
    expect(inc.rows.map((r) => [r.id, r.values.n])).toEqual([["r0000", -1]]);
  });

  test("cursor más viejo que lo retenido: resetRequired", async () => {
    const t = await setup();
    await push(t, group(op({ kind: "create", rowId: "a", set: { a: 1 } })));
    await t.run(async (ctx) => {
      const s = await ctx.db.query("lf_scopes").withIndex("by_scope", (q) => q.eq("scope", ENV)).unique();
      await ctx.db.patch("lf_scopes", s!._id, { minVersion: 50 });
    });
    expect(await pull(t, ENV, 10)).toEqual({ status: "resetRequired" });
  });

  test("versión de protocolo vieja: upgradeRequired en todo", async () => {
    const t = await setup();
    expect(await t.query(api.sync.doorbell, { token: A.token, scopes: [ENV], protocolVersion: 0 })).toEqual({ status: "upgradeRequired" });
    expect(await t.query(api.sync.pull, { token: A.token, scope: ENV, cursor: null, protocolVersion: 0 })).toEqual({ status: "upgradeRequired" });
    expect(await t.mutation(api.sync.push, { token: A.token, group: group(op({ kind: "create", rowId: "a" })), protocolVersion: 0 })).toEqual({ status: "upgradeRequired" });
  });
});

describe("seguridad", () => {
  test("sin token válido no hay nada", async () => {
    const t = await setup();
    for (const token of ["", "short", "c".repeat(40)]) {
      await expect(t.query(api.sync.doorbell, { token, scopes: [ENV], protocolVersion: PV })).rejects.toThrow(/Invalid token/);
      await expect(pull(t, ENV, null, undefined, token)).rejects.toThrow(/Invalid token/);
      await expect(push(t, group(op({ kind: "create", rowId: "a" })), token)).rejects.toThrow(/Invalid token/);
    }
  });

  test("un daemon no ve ni escribe los alcances de otro", async () => {
    const t = await setup();
    await push(t, group(op({ kind: "create", rowId: "secret", set: { name: "A's env" } })));
    expect(await pull(t, ENV, null, undefined, B.token)).toEqual({ status: "denied" });
    expect(await t.query(api.sync.doorbell, { token: B.token, scopes: [ENV, `env:${B.daemonId}`], protocolVersion: PV })).toEqual({ status: "ok", versions: { [ENV]: "denied", [`env:${B.daemonId}`]: 0 } });
    await expect(push(t, group(op({ kind: "patch", rowId: "secret", set: { name: "pwned" } })), B.token)).rejects.toThrow(/denied|No access/);
    // Ni con un alcance inventado o de otro tipo.
    for (const scope of ["env:", "env:daemon-aaaa:x", "secrets:daemon-bbbb", "env:DAEMON-BBBB"]) {
      expect(await pull(t, scope, null, undefined, B.token)).toEqual({ status: "denied" });
    }
    expect((await bootstrapAll(t, ENV)).rows.get("secret")).toEqual({ name: "A's env" });
  });

  test("los opId de un daemon no chocan con los de otro", async () => {
    const t = await setup();
    const shared = "same-op-id";
    const ra = await push(t, group({ ...op({ kind: "create", rowId: "x", set: { by: "a" } }), opId: shared }));
    const rb = await push(t, group({ ...op({ kind: "create", rowId: "x", set: { by: "b" } }, `env:${B.daemonId}`), opId: shared }), B.token);
    expect(ra.results![0].docId).not.toBe(rb.results![0].docId);
    expect((await bootstrapAll(t, `env:${B.daemonId}`, B.token)).rows.get("x")).toEqual({ by: "b" });
  });

  test("nombres de campo inválidos y grupos enormes se rechazan", async () => {
    const t = await setup();
    for (const key of ["_id", "$x", "", "a b", "ñ"]) {
      await expect(push(t, group(op({ kind: "create", rowId: `k${seq}`, set: { [key]: 1 } })))).rejects.toThrow();
    }
    await expect(push(t, group(...Array.from({ length: 101 }, (_, i) => op({ kind: "create", rowId: `big${i}`, set: { a: i } }))))).rejects.toThrow(/1–100/);
  });

  test("desvincular invalida el token", async () => {
    const t = await setup();
    await t.mutation(internal.daemons.unregister, { daemonId: A.daemonId });
    await expect(pull(t, ENV, null)).rejects.toThrow(/Invalid token/);
  });

  test("el token no se guarda en claro", async () => {
    const t = await setup();
    const daemons = await t.run((ctx) => ctx.db.query("daemons").collect());
    for (const d of daemons) {
      expect(JSON.stringify(d)).not.toContain(A.token);
      expect(JSON.stringify(d)).not.toContain(B.token);
      expect(d.tokenHash).toMatch(/^[0-9a-f]{64}$/);
    }
  });
});

describe("diferencial con el servidor falso de la librería", () => {
  // Misma secuencia de grupos aleatorios (crear, cambiar, conjuntos, borrar, con bases viejas y
  // grupos de varias operaciones) en Convex y en FakeServer: mismos resultados y mismas filas.
  test("30 semillas × 30 grupos dan lo mismo", async () => {
    for (let seed = 1; seed <= Number((globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env.LF_SEEDS ?? 30); seed++) {
      const t = await setup();
      const fake = new FakeServer({ pageSize: 1000 });
      fake.setReviewFields(APPROVALS, ["status", "decision"]);
      const random = rng(seed);
      const pick = <X>(xs: readonly X[]): X => xs[Math.floor(random() * xs.length)]!;
      const ids = ["r1", "r2", "r3", "r4"];
      for (let i = 0; i < 30; i++) {
        const scope = pick([ENV, APPROVALS]);
        const ops: Op[] = Array.from({ length: 1 + Math.floor(random() * 3) }, () => {
          const kind = pick(["create", "patch", "patch", "delete"] as const);
          const o = op({ kind, rowId: pick(ids) }, scope);
          if (kind !== "delete") {
            o.set = Object.fromEntries(["status", "note", "n"].filter(() => random() < 0.5).map((f) => [f, pick([null, 1, 2, "x", "approved", ["l"]])]));
            if (random() < 0.4) o.sets = { tags: random() < 0.6 ? { add: [pick(["t1", "t2", "t3"])] } : { remove: [pick(["t1", "t2"])] } };
            if (kind === "patch" && random() < 0.8) o.base = Object.fromEntries(Object.keys(o.set).map((f) => [f, Math.floor(random() * (i + 1))]));
          }
          return o;
        });
        const g = group(...ops);
        const expected = fake.push(structuredClone(g), PV);
        let got: unknown;
        try {
          got = await push(t, g);
        } catch (error) {
          // Con el cliente real `data` es un objeto; convex-test lo entrega como JSON.
          const raw = (error as { data?: unknown }).data;
          const data = (typeof raw === "string" ? JSON.parse(raw) : raw) as { status: string; code: string } | undefined;
          got = data ? { status: data.status, error: { code: data.code } } : String(error);
        }
        const normalize = (r: unknown) => {
          const x = r as { status: string; results?: Array<Record<string, unknown>>; error?: { code: string } };
          // `overwrote` es un conjunto: Convex ordena las claves de los objetos (y con ellas los campos de `set`).
          // Y `undefined` no existe en Convex: un campo que falta en el servidor llega como `null`.
          const sorted = (r: Record<string, unknown>) => ({
            ...r,
            ...(Array.isArray(r.overwrote) ? { overwrote: [...(r.overwrote as string[])].sort() } : {}),
            ...(Array.isArray(r.conflicts) ? { conflicts: (r.conflicts as Array<{ field: string; local: unknown; server: unknown }>).map((c) => ({ field: c.field, local: c.local ?? null, server: c.server ?? null })) } : {}),
          });
          return x.status === "ok" ? { status: "ok", results: x.results!.map(({ docId: _d, ...rest }) => sorted(rest)) } : { status: x.status, code: x.error?.code };
        };
        expect(normalize(got), `seed ${seed}, group ${i}: ${JSON.stringify(g.ops)}`).toEqual(normalize(expected));
      }
      for (const scope of [ENV, APPROVALS]) {
        const rows = Object.fromEntries([...(await bootstrapAll(t, scope)).rows].sort(([a], [b]) => a.localeCompare(b)));
        const fakeRows = Object.fromEntries(fake.visibleRows(scope).map((r) => [r.id, r.values]).sort(([a], [b]) => String(a).localeCompare(String(b))));
        expect(rows, `seed ${seed}, ${scope}`).toEqual(fakeRows);
      }
    }
  }, 120_000);
});
