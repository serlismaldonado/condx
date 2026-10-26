# @condx/convex

The backend half of the Condx SDK: a Convex component. It records the errors of your actions (and
the ones you report) in its own tables in your deployment and forwards them to Condx Cloud in
batches, retrying if it's down.

```ts
// convex/convex.config.ts
import condx from "@condx/convex/convex.config";
const app = defineApp();
app.use(condx);
export default app;

// convex/condx.ts
import { Condx } from "@condx/convex";
import { components } from "./_generated/api";
export const condx = new Condx(components.condx);

export const sync = action({ args: {}, handler: condx.wrap("billing:sync", async (ctx) => { /* … */ }) });
```

Set `CONDX_ENDPOINT` and `CONDX_SECRET` (`ck_sec_…`, from Condx → Settings → Condx SDK) on the
deployment. An uncaught error in a mutation or query rolls back with its transaction (including
anything recorded during it): those reach Condx from `@condx/react` (the failed call). Wrap actions,
and use `condx.reportError(ctx, error)` for errors you handle.
