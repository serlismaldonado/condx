import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { CondxClient, CondxProvider, ReportProblem, functionName, normalize, pathOf, wrapConvexClient } from "../src";

type Sent = { url: string; headers: Record<string, string>; body: { events: Record<string, unknown>[] } };

function fakeFetch(status = 200) {
  const sent: Sent[] = [];
  const f = vi.fn(async (url: string, init: RequestInit) => {
    sent.push({ url, headers: init.headers as Record<string, string>, body: JSON.parse(init.body as string) });
    return new Response("{}", { status });
  });
  return { f: f as unknown as typeof fetch, sent };
}

const KEY = "ck_pub_" + "a".repeat(48);

afterEach(() => vi.restoreAllMocks());

describe("client", () => {
  test("groups the same error, keeps the path only and sends with the key", async () => {
    const { f, sent } = fakeFetch();
    const c = new CondxClient({ ingestKey: KEY, endpoint: "https://condx.example.com/", app: "shop", release: "1.2.0", fetch: f });
    history.replaceState(null, "", "/checkout?card=4242#pay");
    c.breadcrumb("route /cart");
    for (let i = 0; i < 3; i++) c.captureError(new TypeError(`Cannot read 'total' of undefined (order ${i})`));
    expect(await c.flush()).toBe(true);
    expect(sent).toHaveLength(1);
    expect(sent[0].url).toBe("https://condx.example.com/ingest");
    expect(sent[0].headers["x-condx-key"]).toBe(KEY);
    const [e] = sent[0].body.events;
    expect(e).toMatchObject({ type: "error", count: 3, url: "/checkout", app: "shop", release: "1.2.0" });
    expect(JSON.stringify(e)).not.toContain("4242");
    expect((e.breadcrumbs as string[])[0]).toContain("route /cart");
    // Nothing queued: no request.
    expect(await c.flush()).toBe(true);
    expect(sent).toHaveLength(1);
  });

  test("does nothing without a key, and beforeSend can drop events", async () => {
    const { f, sent } = fakeFetch();
    const off = new CondxClient({ endpoint: "https://x", fetch: f });
    off.captureError(new Error("x"));
    await off.flush();
    const c = new CondxClient({ ingestKey: KEY, endpoint: "https://x", fetch: f, beforeSend: (e) => (e.message.includes("ignore") ? null : e) });
    c.captureError(new Error("please ignore"));
    c.captureError(new Error("keep"));
    await c.flush();
    expect(sent).toHaveLength(1);
    expect(sent[0].body.events.map((e) => e.message)).toEqual(["Error: keep"]);
  });

  test("keeps events if the cloud is down and respects the per-minute limit", async () => {
    const down = fakeFetch(500);
    const c = new CondxClient({ ingestKey: KEY, endpoint: "https://x", fetch: down.f, perMinute: 2 });
    c.captureError(new Error("a"));
    expect(await c.flush()).toBe(false);
    const up = fakeFetch();
    (c.options as { fetch?: typeof fetch }).fetch = up.f;
    c.captureError(new Error("b"));
    c.captureError(new Error("c"));
    await c.flush();
    expect(up.sent[0].body.events).toHaveLength(2);
    expect(await c.flush()).toBe(false);
  });

  test("helpers", () => {
    expect(normalize("Order 123 failed for 0f8a9c1e-1234-4abc-9def-0123456789ab")).toBe("Order <n> failed for <id>");
    expect(pathOf("https://a.com/x/y?z=1#w")).toBe("/x/y");
    expect(functionName({ [Symbol.for("functionName")]: "tasks:list" })).toBe("tasks:list");
    expect(functionName("payments:charge")).toBe("payments:charge");
  });
});

describe("convex", () => {
  test("a failed mutation is reported with its name, not its arguments", async () => {
    const { f, sent } = fakeFetch();
    const c = new CondxClient({ ingestKey: KEY, endpoint: "https://x", fetch: f });
    const convex = { mutation: vi.fn(async () => Promise.reject(new Error("[CONVEX M(payments:charge)] Uncaught Error: card declined"))), action: vi.fn(async () => "ok") };
    wrapConvexClient(convex, c);
    const ref = { [Symbol.for("functionName")]: "payments:charge" };
    await expect((convex.mutation as (r: unknown, a: unknown) => Promise<unknown>)(ref, { card: "4242424242424242" })).rejects.toThrow("card declined");
    expect(await (convex.action as (r: unknown) => Promise<unknown>)({ [Symbol.for("functionName")]: "mail:send" })).toBe("ok");
    await c.flush();
    const [e] = sent[0].body.events;
    expect(e).toMatchObject({ function: "payments:charge" });
    expect(JSON.stringify(sent)).not.toContain("4242424242424242");
    expect((e.breadcrumbs as string[]).some((b) => b.includes("convex mutation payments:charge"))).toBe(true);
    // Wrapping twice doesn't wrap twice.
    expect(wrapConvexClient(convex, c)).toBe(convex);
  });
});

describe("react", () => {
  test("the boundary reports a render error and shows the fallback", async () => {
    const { f, sent } = fakeFetch();
    vi.spyOn(console, "error").mockImplementation(() => {});
    function Broken(): never {
      throw new Error("Checkout exploded");
    }
    render(
      <CondxProvider ingestKey={KEY} endpoint="https://x" fetch={f} fallback={<p>Something went wrong</p>}>
        <Broken />
      </CondxProvider>,
    );
    expect(screen.getByText("Something went wrong")).toBeTruthy();
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(sent.flatMap((s) => s.body.events).some((e) => String(e.message).includes("Checkout exploded"))).toBe(true);
  });

  test("ReportProblem sends what the person writes, and hides itself without a key", async () => {
    const { f, sent } = fakeFetch();
    const { unmount } = render(
      <CondxProvider ingestKey={KEY} endpoint="https://x" fetch={f}>
        <ReportProblem askEmail captureScreenshot={async () => "data:image/png;base64,AAAA"} labels={{ button: "Reportar un problema", send: "Enviar" }} />
      </CondxProvider>,
    );
    fireEvent.click(screen.getByText("Reportar un problema"));
    fireEvent.change(screen.getByRole("textbox", { name: "What went wrong?" }), { target: { value: "El botón de pagar no hace nada" } });
    fireEvent.click(screen.getByRole("checkbox"));
    await act(async () => {
      fireEvent.click(screen.getByText("Enviar"));
      await new Promise((r) => setTimeout(r, 10));
    });
    const report = sent.flatMap((s) => s.body.events).find((e) => e.type === "report")!;
    expect(report).toMatchObject({ text: "El botón de pagar no hace nada", screenshot: "data:image/png;base64,AAAA" });
    expect(screen.getByText("Thanks, we got it.")).toBeTruthy();
    unmount();
    const { container } = render(
      <CondxProvider>
        <ReportProblem />
      </CondxProvider>,
    );
    expect(container.textContent).toBe("");
  });
});
