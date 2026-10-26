// Failed Convex calls: wraps a ConvexReactClient (or ConvexHttpClient) so a mutation, action or
// one-off query that fails is reported with the function's name and the error, never its arguments.
// Queries used through hooks throw into React: the provider's error boundary reports those.

import type { CondxClient } from "./client.js";

type Callable = (ref: unknown, ...rest: unknown[]) => Promise<unknown>;

/** The name Convex keeps in a function reference (`api.tasks.list` → `tasks:list`). */
export function functionName(ref: unknown): string | undefined {
  if (typeof ref === "string") return ref;
  if (ref && typeof ref === "object") {
    const name = (ref as Record<symbol, unknown>)[Symbol.for("functionName")];
    if (typeof name === "string") return name;
  }
  return undefined;
}

const WRAPPED = Symbol.for("condx.wrapped");

export function wrapConvexClient<T extends object>(client: T, condx: CondxClient): T {
  const c = client as unknown as Record<string | symbol, unknown>;
  if (c[WRAPPED]) return client;
  for (const method of ["mutation", "action", "query"] as const) {
    const original = c[method] as Callable | undefined;
    if (typeof original !== "function") continue;
    c[method] = async function (this: unknown, ref: unknown, ...rest: unknown[]) {
      const name = functionName(ref);
      condx.breadcrumb(`convex ${method} ${name ?? "?"}`);
      try {
        return await original.call(client, ref, ...rest);
      } catch (error) {
        condx.captureError(error, { function: name });
        throw error;
      }
    };
  }
  c[WRAPPED] = true;
  return client;
}
