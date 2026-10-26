// The browser side of the Condx SDK: catches errors, groups the ones that repeat, keeps a short
// trail of what happened before (routes, console errors, Convex calls by name) and sends it all, in
// batches, to your Condx Cloud. Private by default: no cookies, no function arguments, no input
// contents, URLs without query or hash; `beforeSend` can edit or drop anything.

export type CondxEvent = {
  type: "error" | "report";
  message: string;
  fingerprint: string;
  stack?: string;
  url?: string;
  release?: string;
  app?: string;
  function?: string;
  breadcrumbs?: string[];
  count: number;
  text?: string;
  email?: string;
  screenshot?: string;
};

export type CondxOptions = {
  /** The project's public ingest key (`ck_pub_…`), from Condx → Settings → Condx SDK. Without it, nothing is sent. */
  ingestKey?: string;
  /** Your Condx Cloud URL (the same one your phone uses), e.g. `https://condx.example.com`. */
  endpoint?: string;
  /** Your app's name and version, to tell events apart. */
  app?: string;
  release?: string;
  /** Edit or drop (return null) an event before it's sent. */
  beforeSend?: (event: CondxEvent) => CondxEvent | null;
  /** How often queued events go out, in ms (default 5000). */
  flushEvery?: number;
  /** At most this many events per minute from this tab (default 30). */
  perMinute?: number;
  /** For tests: the fetch to use. */
  fetch?: typeof fetch;
};

const MAX_CRUMBS = 20;

/** The same error with different ids or numbers is the same error. */
export function normalize(message: string): string {
  return message
    .replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi, "<id>")
    .replace(/\b[0-9a-z]{20,}\b/gi, "<id>")
    .replace(/\d+/g, "<n>")
    .slice(0, 200);
}

function topFrame(stack?: string): string {
  const line = stack?.split("\n").find((l) => /\bat\b|@/.test(l)) ?? "";
  return line.replace(/\?[^:)\s]*/g, "").replace(/:\d+:\d+/, "").trim().slice(0, 120);
}

/** `https://x.com/a/b?c=1#d` → `/a/b`. */
export function pathOf(url: string): string {
  try {
    return new URL(url, "https://x.invalid").pathname;
  } catch {
    return url.split(/[?#]/)[0];
  }
}

function errorParts(error: unknown): { message: string; stack?: string } {
  if (error instanceof Error) return { message: `${error.name}: ${error.message}`, stack: error.stack };
  if (typeof error === "string") return { message: error };
  try {
    return { message: JSON.stringify(error).slice(0, 500) };
  } catch {
    return { message: String(error) };
  }
}

export class CondxClient {
  readonly options: CondxOptions;
  private queue = new Map<string, CondxEvent>();
  private crumbs: string[] = [];
  private sentThisMinute = 0;
  private minute = 0;
  private timer: ReturnType<typeof setInterval> | undefined;
  private undo: (() => void)[] = [];

  constructor(options: CondxOptions) {
    this.options = options;
  }

  get enabled(): boolean {
    return Boolean(this.options.ingestKey && this.options.endpoint);
  }

  breadcrumb(text: string): void {
    this.crumbs.push(`${new Date().toISOString().slice(11, 19)} ${text.slice(0, 200)}`);
    if (this.crumbs.length > MAX_CRUMBS) this.crumbs.shift();
  }

  /** An error: grouped with the same one already queued. */
  captureError(error: unknown, extra: { function?: string; source?: string } = {}): void {
    const { message, stack } = errorParts(error);
    const fingerprint = `${extra.function ?? ""}|${normalize(message)}|${topFrame(stack)}`;
    const queued = this.queue.get(fingerprint);
    if (queued) {
      queued.count++;
      return;
    }
    this.enqueue(fingerprint, {
      type: "error",
      message,
      fingerprint,
      stack: stack?.split("\n").slice(0, 12).join("\n"),
      url: typeof location !== "undefined" ? pathOf(location.href) : undefined,
      function: extra.function,
      breadcrumbs: [...this.crumbs],
      count: 1,
    });
  }

  /** A user's report: what they say went wrong, with the same context. Sent right away. */
  async captureReport(report: { text: string; email?: string; screenshot?: string }): Promise<boolean> {
    const text = report.text.trim();
    if (!text) return false;
    const key = `report|${Date.now()}|${Math.random()}`;
    this.enqueue(key, {
      type: "report",
      message: `User report: ${text.slice(0, 120)}`,
      fingerprint: key,
      url: typeof location !== "undefined" ? pathOf(location.href) : undefined,
      breadcrumbs: [...this.crumbs],
      count: 1,
      text,
      email: report.email?.trim() || undefined,
      screenshot: report.screenshot,
    });
    return this.flush();
  }

  private enqueue(key: string, event: CondxEvent): void {
    if (!this.enabled) return;
    const e: CondxEvent = { ...event, app: this.options.app, release: this.options.release };
    const kept = this.options.beforeSend ? this.options.beforeSend(e) : e;
    if (kept) this.queue.set(key, kept);
  }

  /** Sends what's queued. Returns false if it couldn't (it stays queued for the next try). */
  async flush(): Promise<boolean> {
    if (!this.enabled || this.queue.size === 0) return true;
    const now = Math.floor(Date.now() / 60_000);
    if (now !== this.minute) {
      this.minute = now;
      this.sentThisMinute = 0;
    }
    const room = (this.options.perMinute ?? 30) - this.sentThisMinute;
    if (room <= 0) return false;
    const batch = [...this.queue.entries()].slice(0, Math.min(room, 50));
    const doFetch = this.options.fetch ?? fetch;
    try {
      const res = await doFetch(`${this.options.endpoint!.replace(/\/$/, "")}/ingest`, {
        method: "POST",
        headers: { "content-type": "application/json", "x-condx-key": this.options.ingestKey! },
        body: JSON.stringify({ events: batch.map(([, e]) => e) }),
        keepalive: true,
        credentials: "omit",
      });
      if (!res.ok && res.status !== 429) return false;
      for (const [k] of batch) this.queue.delete(k);
      this.sentThisMinute += batch.length;
      return res.ok;
    } catch {
      return false;
    }
  }

  /** Listens to the page: uncaught errors, rejected promises, console errors and route changes. */
  install(): void {
    if (typeof window === "undefined" || this.undo.length) return;
    const onError = (e: ErrorEvent) => this.captureError(e.error ?? e.message);
    const onRejection = (e: PromiseRejectionEvent) => this.captureError(e.reason);
    const onHide = () => {
      if (document.visibilityState === "hidden") void this.flush();
    };
    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onRejection);
    document.addEventListener("visibilitychange", onHide);
    const original = console.error;
    console.error = (...args: unknown[]) => {
      this.breadcrumb(`console.error ${args.map((a) => (a instanceof Error ? a.message : typeof a === "string" ? a : "…")).join(" ").slice(0, 160)}`);
      original.apply(console, args);
    };
    const push = history.pushState;
    history.pushState = function (this: History, ...args: Parameters<History["pushState"]>) {
      const r = push.apply(this, args);
      client.breadcrumb(`route ${pathOf(location.href)}`);
      return r;
    };
    const client = this;
    const onPop = () => this.breadcrumb(`route ${pathOf(location.href)}`);
    window.addEventListener("popstate", onPop);
    this.timer = setInterval(() => void this.flush(), this.options.flushEvery ?? 5000);
    this.undo.push(() => {
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onRejection);
      window.removeEventListener("popstate", onPop);
      document.removeEventListener("visibilitychange", onHide);
      console.error = original;
      history.pushState = push;
      clearInterval(this.timer);
    });
  }

  uninstall(): void {
    for (const u of this.undo.splice(0)) u();
  }
}
