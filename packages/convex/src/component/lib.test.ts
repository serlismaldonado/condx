/// <reference types="vite/client" />
// The component: groups errors, keeps reports, forwards only with endpoint and secret.
import { convexTest } from "convex-test";
import { afterEach, describe, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import { normalize } from "./lib";
import schema from "./schema";
import { Condx } from "../client";

const modules = import.meta.glob("./**/*.ts");

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("condx component", () => {
  test("groups the same error and keeps reports", async () => {
    const t = convexTest(schema, modules);
    await t.mutation(api.lib.recordError, { message: "Error: Card declined for order 123", fn: "payments:charge" });
    await t.mutation(api.lib.recordError, { message: "Error: Card declined for order 456", fn: "payments:charge" });
    await t.mutation(api.lib.recordReport, { text: "  The pay button does nothing  ", app: "shop" });
    await t.mutation(api.lib.recordReport, { text: "   " });
    const { errors, reports } = await t.query(api.lib.recent, {});
    expect(errors).toHaveLength(1);
    expect(errors[0]).toMatchObject({ fn: "payments:charge", count: 2, sentCount: 0 });
    expect(reports.map((r: { text: string }) => r.text)).toEqual(["The pay button does nothing"]);
    // Without endpoint and secret, nothing is scheduled.
    expect(await t.run(async (ctx) => (await ctx.db.query("outbox").collect()).length)).toBe(0);
    expect(normalize("Order 12 for 0f8a9c1e-1234-4abc-9def-0123456789ab")).toBe("Order <n> for <id>");
  });

  test("forwards pending events with the secret and marks them sent", async () => {
    vi.useFakeTimers();
    const sent: { url: string; auth: string; events: unknown[] }[] = [];
    vi.stubGlobal("fetch", vi.fn(async (url: string, init: RequestInit) => {
      sent.push({ url, auth: (init.headers as Record<string, string>).authorization, events: JSON.parse(init.body as string).events });
      return new Response("{}", { status: 200 });
    }));
    const t = convexTest(schema, modules);
    const target = { endpoint: "https://condx.example.com/", secret: "ck_sec_" + "1".repeat(48) };
    await t.mutation(api.lib.recordError, { message: "TypeError: x", fn: "sync:run", ...target });
    await t.mutation(api.lib.recordError, { message: "TypeError: x", fn: "sync:run", ...target });
    await t.mutation(api.lib.recordReport, { text: "Broken", ...target });
    // One forward scheduled, not three.
    expect(await t.run(async (ctx) => (await ctx.db.query("outbox").collect()).length)).toBe(1);
    await t.finishAllScheduledFunctions(vi.runAllTimers);
    expect(sent).toHaveLength(1);
    expect(sent[0].url).toBe("https://condx.example.com/ingest");
    expect(sent[0].auth).toBe(`Bearer ${target.secret}`);
    expect(sent[0].events).toEqual([
      expect.objectContaining({ type: "error", function: "sync:run", count: 2 }),
      expect.objectContaining({ type: "report", text: "Broken" }),
    ]);
    const { errors, reports } = await t.query(api.lib.recent, {});
    expect(errors[0]).toMatchObject({ count: 2, sentCount: 2 });
    expect(reports[0].sent).toBe(true);
    expect(await t.query(internal.lib.pending, {})).toEqual({ errors: [], reports: [] });
  });

  test("retries later if Condx Cloud is down", async () => {
    vi.useFakeTimers();
    let calls = 0;
    vi.stubGlobal("fetch", vi.fn(async () => {
      calls++;
      return new Response("down", { status: calls < 2 ? 503 : 200 });
    }));
    const t = convexTest(schema, modules);
    await t.mutation(api.lib.recordError, { message: "Error: boom", endpoint: "https://x", secret: "s" });
    await t.finishAllScheduledFunctions(vi.runAllTimers);
    expect(calls).toBe(2);
    expect((await t.query(api.lib.recent, {})).errors[0].sentCount).toBe(1);
  });

  test("wrap records and rethrows; reportError carries the name", async () => {
    const calls: unknown[] = [];
    const condx = new Condx({ lib: { recordError: "recordError" as never, recordReport: "recordReport" as never } }, { endpoint: "https://x", secret: "s" });
    const ctx = { runMutation: async (_ref: unknown, args: unknown) => void calls.push(args) };
    const failing = condx.wrap("billing:sync", async () => {
      throw new Error("Stripe timeout");
    });
    await expect(failing(ctx, {})).rejects.toThrow("Stripe timeout");
    expect(calls[0]).toMatchObject({ message: "Error: Stripe timeout", fn: "billing:sync", endpoint: "https://x", secret: "s" });
    const ok = condx.wrap("billing:ok", async () => 42);
    expect(await ok(ctx, {})).toBe(42);
    expect(calls).toHaveLength(1);
  });
});
