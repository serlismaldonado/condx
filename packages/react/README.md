# @condx/react

The frontend half of the Condx SDK, for any Convex app. It captures errors (uncaught, render errors
through its error boundary, failed Convex mutations/actions/queries with the function's name) and
lets the people using your app report a problem. Everything goes to your Condx Cloud, where Condx
groups it, opens a thread and runs the flows you set up (triage, risk, fix, notify).

```tsx
import { CondxProvider, ReportProblem } from "@condx/react";

<CondxProvider ingestKey="ck_pub_…" endpoint="https://your-condx-cloud" convex={convex} app="shop" release="1.4.0">
  <App />
  <ReportProblem />
</CondxProvider>
```

- Get the key in Condx → Settings → Condx SDK. Without `ingestKey`/`endpoint` the SDK does nothing.
- Private by default: no cookies, no function arguments, no input contents, URLs without query or
  hash. `beforeSend(event)` edits or drops anything.
- `useReportProblem()` sends a report from your own UI; `<ReportProblem labels={…} askEmail
  captureScreenshot={…} />` is the ready-made button and form.
