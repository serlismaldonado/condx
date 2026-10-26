// The Condx component, from your app's code.
//
//   // convex/convex.config.ts
//   import condx from "@condx/convex/convex.config";
//   const app = defineApp(); app.use(condx); export default app;
//
//   // convex/condx.ts
//   import { Condx } from "@condx/convex";
//   import { components } from "./_generated/api";
//   export const condx = new Condx(components.condx);
//
//   // an action whose failures should reach Condx
//   export const sync = action({ args: {}, handler: condx.wrap("billing:sync", async (ctx) => { … }) });
//
// Set CONDX_ENDPOINT (your Condx Cloud URL) and CONDX_SECRET (`ck_sec_…`, from Condx → Settings →
// Condx SDK) as environment variables of the deployment. Without them errors are still recorded in
// the component's tables, just not forwarded.
//
// Note: an uncaught error in a mutation or query rolls back the whole transaction, including
// anything recorded during it. Those reach Condx from the browser SDK (the failed call) instead;
// wrap actions (and scheduled actions), and use `reportError` for errors you handle.

import type { FunctionReference } from "convex/server";

type RecordError = FunctionReference<"mutation", "internal", { message: string; stack?: string; fn?: string; endpoint?: string; secret?: string }, null>;
type RecordReport = FunctionReference<"mutation", "internal", { text: string; email?: string; url?: string; app?: string; endpoint?: string; secret?: string }, null>;

export type CondxComponent = { lib: { recordError: RecordError; recordReport: RecordReport } };
type RunMutationCtx = { runMutation: (ref: any, args: any) => Promise<any> };

export type CondxOptions = { endpoint?: string; secret?: string };

export class Condx {
  constructor(
    private component: CondxComponent,
    private options: CondxOptions = {},
  ) {}

  private target() {
    const env = (typeof process !== "undefined" ? process.env : {}) as Record<string, string | undefined>;
    return { endpoint: this.options.endpoint ?? env.CONDX_ENDPOINT, secret: this.options.secret ?? env.CONDX_SECRET };
  }

  /** Record an error (from an action, or one you caught in a mutation and didn't rethrow). */
  async reportError(ctx: RunMutationCtx, error: unknown, info: { function?: string } = {}): Promise<void> {
    const e = error instanceof Error ? error : new Error(typeof error === "string" ? error : JSON.stringify(error));
    await ctx.runMutation(this.component.lib.recordError, { message: `${e.name}: ${e.message}`, stack: e.stack, fn: info.function, ...this.target() });
  }

  /** A report from a user of the app, received by your own backend (e.g. your own feedback form). */
  async reportProblem(ctx: RunMutationCtx, report: { text: string; email?: string; url?: string; app?: string }): Promise<void> {
    await ctx.runMutation(this.component.lib.recordReport, { ...report, ...this.target() });
  }

  /** Wraps an action handler: if it throws, the error is recorded (with `name`) and rethrown. */
  wrap<Ctx extends RunMutationCtx, Args, R>(name: string, handler: (ctx: Ctx, args: Args) => Promise<R>): (ctx: Ctx, args: Args) => Promise<R> {
    return async (ctx, args) => {
      try {
        return await handler(ctx, args);
      } catch (error) {
        await this.reportError(ctx, error, { function: name }).catch(() => {});
        throw error;
      }
    };
  }
}
