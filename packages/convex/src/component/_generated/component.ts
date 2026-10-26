/* eslint-disable */
/**
 * Generated `ComponentApi` utility.
 *
 * THIS CODE IS AUTOMATICALLY GENERATED.
 *
 * To regenerate, run `npx convex dev`.
 * @module
 */

import type { FunctionReference } from "convex/server";

/**
 * A utility for referencing a Convex component's exposed API.
 *
 * Useful when expecting a parameter like `components.myComponent`.
 * Usage:
 * ```ts
 * async function myFunction(ctx: QueryCtx, component: ComponentApi) {
 *   return ctx.runQuery(component.someFile.someQuery, { ...args });
 * }
 * ```
 */
export type ComponentApi<Name extends string | undefined = string | undefined> =
  {
    lib: {
      recent: FunctionReference<
        "query",
        "internal",
        { limit?: number },
        any,
        Name
      >;
      recordError: FunctionReference<
        "mutation",
        "internal",
        {
          endpoint?: string;
          fn?: string;
          message: string;
          secret?: string;
          stack?: string;
        },
        null,
        Name
      >;
      recordReport: FunctionReference<
        "mutation",
        "internal",
        {
          app?: string;
          email?: string;
          endpoint?: string;
          secret?: string;
          text: string;
          url?: string;
        },
        null,
        Name
      >;
    };
  };
