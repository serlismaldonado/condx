# Condx Cloud backend (Convex)

What a Condx daemon publishes (and the phone requests, from C4), as scopes of the
[local-first protocol](https://github.com/serlismaldonado/localfirst/blob/main/docs/protocol.md):
`env:<daemonId>`, `jobs:`, `approvals:`, `runs:`, `workflows:`. Design: [`docs/design/cloud.md`](../docs/design/cloud.md).

## Set up (your own Convex project)

```bash
cd cloud && bun install
npx convex deploy                 # into a new Convex project of yours
```

Then in Condx: Settings → **Condx Cloud** → paste the deployment URL and a deploy key (used once to
register this daemon; not stored). Or from the terminal:
`condxd cloud link --url https://<deployment>.convex.cloud` (reads the key from stdin).

| File | What |
| --- | --- |
| `convex/schema.ts` | `daemons` (token as SHA-256), `lf_scopes`, `lf_rows`, `lf_changes`, `lf_ops` |
| `convex/auth.ts` | daemon from its token; a daemon only reaches scopes with its id; review fields per scope kind |
| `convex/daemons.ts` | `register` / `unregister` — **internal**: only with the deployment's admin or deploy key |
| `convex/sync.ts` | `doorbell`, `pull` (paginated snapshot + incremental), `push` (atomic group, idempotent `opId`, per-field conflicts with the library's `planOp`) |

The daemon side lives in `crates/daemon/src/cloud.rs` (engine and protocol from
[`localfirst-core`](https://github.com/serlismaldonado/localfirst/tree/main/rust)).

A permanent rejection throws `ConvexError({ code, message, status })`: Convex rolls back the whole
group and clients classify it as a rejection (`localfirst-core`'s HTTP transport does).

## Tests

```bash
bun install && bun run test        # convex-test: protocol, security, differential vs FakeServer
LF_SEEDS=300 bun run test -t diferencial
```

The differential test runs the same random groups against this backend and the library's
`FakeServer`: same results, same rows. Two things differ by nature and are normalized:

- Convex stores object keys sorted, so `overwrote` (a set) can come in another order. Set *items*
  that are objects get their keys sorted too: keep set items scalar, or objects with sorted keys.
- `undefined` doesn't exist in Convex: a field missing on the server is `null` in `conflicts`.

The Rust engine runs against a real backend with `localfirst`'s `rust/localfirst-core/tests/live_backend.rs`
(throwaway local backend, `npx convex dev --once` against it, a daemon registered with
`npx convex run daemons:register`).
