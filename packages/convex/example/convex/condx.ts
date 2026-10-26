// Una app de ejemplo (no es heskala): usa el componente de Condx como lo haría cualquier app.
import { v } from "convex/values";
import { Condx } from "../../src/client";
import { components } from "./_generated/api";
import { action, mutation, query } from "./_generated/server";

export const condx = new Condx(components.condx);

/** Una acción que falla: el error queda en el componente y se reenvía a Condx Cloud. */
export const chargeCard = action({
  args: { orderId: v.string() },
  handler: condx.wrap("payments:chargeCard", async (_ctx, { orderId }: { orderId: string }) => {
    throw new Error(`Card declined for order ${orderId}`);
  }),
});

/** Un error que la app maneja y decide reportar. */
export const placeOrder = mutation({
  args: { total: v.number() },
  handler: async (ctx, { total }) => {
    try {
      if (total <= 0) throw new Error("Invalid total");
      return await ctx.db.insert("orders", { total });
    } catch (error) {
      await condx.reportError(ctx, error, { function: "orders:placeOrder" });
      return null;
    }
  },
});

/** Un reporte de un usuario que llega por el backend de la app. */
export const feedback = mutation({
  args: { text: v.string() },
  handler: async (ctx, { text }) => {
    await condx.reportProblem(ctx, { text, app: "shop" });
  },
});

export const recent = query({ args: {}, handler: async (ctx) => await ctx.runQuery(components.condx.lib.recent, {}) });
