/// <reference types="vite/client" />
// La ingesta del SDK: llaves por proyecto, límites, recorte y agrupación de lo que se repite.

import { convexTest } from "convex-test";
import { describe, expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import { clean, redact } from "./ingest";
import schema from "./schema";

const modules = import.meta.glob("./**/*.ts");
const A = { daemonId: "daemon-aaaa", token: "a".repeat(40) };
const B = { daemonId: "daemon-bbbb", token: "b".repeat(40) };
const PV = 1;

async function setup() {
  const t = convexTest(schema, modules);
  await t.mutation(internal.daemons.register, { ...A, name: "Mac A" });
  await t.mutation(internal.daemons.register, { ...B, name: "Mac B" });
  return t;
}

const pull = (t: Awaited<ReturnType<typeof setup>>, token: string, scope: string) => t.query(api.sync.pull, { token, scope, cursor: null, protocolVersion: PV });

describe("ingest", () => {
  test("recorta, quita secretos y deja solo campos conocidos", () => {
    const e = clean(
      {
        type: "error",
        message: "Failed with token sk_live_abcdefghijklmnop for ana@example.com",
        url: "https://app.example.com/checkout?card=4242&email=x#top",
        stack: "at charge (payments.ts:40)",
        breadcrumbs: ["nav /cart", { console: "boom" }],
        cookie: "session=secret",
        args: { card: "4242" },
      },
      "browser",
      1,
    )!;
    expect(e.message).toBe("Failed with token [redacted] for [email]");
    expect(e.url).toBe("/checkout");
    expect(e.breadcrumbs).toContain("nav /cart");
    expect(JSON.stringify(e)).not.toContain("session=secret");
    expect(JSON.stringify(e)).not.toContain("4242");
    expect(clean({ type: "error" }, "browser", 1)).toBeNull();
    expect(clean({ type: "report", text: "The pay button does nothing", screenshot: "data:image/png;base64,AAAA" }, "browser", 1)?.screenshot).toContain("data:image/png");
    expect(clean({ type: "error", message: "x", screenshot: "data:image/png;base64,AAAA" }, "browser", 1)?.screenshot).toBeUndefined();
    expect(redact("Bearer abcdefghijklmnopqrstuv and prod:abcdefghijklmnop|qrstuv")).toBe("Bearer [redacted] and [redacted]");
  });

  test("solo el daemon crea llaves; lo que llega va a su alcance y se agrupa", async () => {
    const t = await setup();
    const { publicKey, secret } = await t.mutation(api.ingest.createKey, { token: A.token, project: "shop" });
    expect(publicKey).toMatch(/^ck_pub_[a-f0-9]{48}$/);
    expect(secret).toMatch(/^ck_sec_[a-f0-9]{48}$/);
    const browser = await t.query(internal.ingest.keyFor, { publicKey });
    expect(browser).toEqual({ daemonId: A.daemonId, project: "shop", source: "browser", key: publicKey });
    expect((await t.query(internal.ingest.keyFor, { secret }))?.source).toBe("server");
    expect(await t.query(internal.ingest.keyFor, { publicKey: "ck_pub_" + "0".repeat(48) })).toBeNull();

    const err = { type: "error", message: "TypeError: x is undefined", fingerprint: "te-x", url: "/checkout" };
    await t.mutation(internal.ingest.record, { ...browser!, events: [err, err] });
    await t.mutation(internal.ingest.record, { ...browser!, events: [{ type: "report", text: "Checkout hangs" }, { type: "report", text: "Checkout hangs" }] });
    const rows = (await pull(t, A.token, `events:${A.daemonId}`)) as { status: string; rows?: { values: Record<string, unknown> }[]; changes?: unknown };
    const values = JSON.stringify(rows);
    expect(values).toContain("TypeError: x is undefined");
    expect(values).toContain("Checkout hangs");
    // El otro daemon no ve el alcance de A.
    expect(((await pull(t, B.token, `events:${A.daemonId}`)) as { status: string }).status).toBe("denied");
    // Revocar la llave la deja sin efecto; otro daemon no puede revocarla.
    expect(await t.mutation(api.ingest.revokeKey, { token: B.token, publicKey })).toBe(false);
    expect(await t.mutation(api.ingest.revokeKey, { token: A.token, publicKey })).toBe(true);
    expect(await t.query(internal.ingest.keyFor, { publicKey })).toBeNull();
    expect((await t.query(api.ingest.listKeys, { token: A.token }))[0]).toMatchObject({ project: "shop", revoked: true });
  });

  test("el mismo error suma; hay un límite por minuto", async () => {
    const t = await setup();
    const { publicKey } = await t.mutation(api.ingest.createKey, { token: A.token, project: "shop" });
    const key = (await t.query(internal.ingest.keyFor, { publicKey }))!;
    const err = { type: "error", message: "boom", fingerprint: "boom", count: 3 };
    await t.mutation(internal.ingest.record, { ...key, events: [err] });
    await t.mutation(internal.ingest.record, { ...key, events: [err] });
    const counts = await t.run(async (ctx) => (await ctx.db.query("lf_rows").collect()).map((r) => r.values.count));
    expect(counts).toEqual([6]);
    const many = Array.from({ length: 50 }, (_, i) => ({ type: "error", message: `e${i}`, fingerprint: `e${i}` }));
    await t.mutation(internal.ingest.record, { ...key, events: many });
    await t.mutation(internal.ingest.record, { ...key, events: many });
    const last = await t.mutation(internal.ingest.record, { ...key, events: many });
    expect(last.stored).toBeLessThan(50);
  });
});
