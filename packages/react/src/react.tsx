// React for the Condx SDK: the provider (captures errors in the app and in Convex calls, with an
// error boundary), `useReportProblem()` and `<ReportProblem />`, a small button and form so the
// people using your app can say something went wrong.

import { Component, createContext, useContext, useEffect, useMemo, useRef, useState, type CSSProperties, type ErrorInfo, type ReactNode } from "react";

import { CondxClient, type CondxOptions } from "./client.js";
import { wrapConvexClient } from "./convex.js";

const Ctx = createContext<CondxClient | null>(null);

export type CondxProviderProps = CondxOptions & {
  /** Your ConvexReactClient: failed mutations, actions and one-off queries get reported. */
  convex?: object;
  /** What to show when a render error reaches the boundary (default: nothing changes, it's rethrown). */
  fallback?: ReactNode | ((error: Error) => ReactNode);
  children?: ReactNode;
};

class Boundary extends Component<{ client: CondxClient; fallback?: CondxProviderProps["fallback"]; children?: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  componentDidCatch(error: Error, info: ErrorInfo) {
    this.props.client.breadcrumb(`render ${info.componentStack?.split("\n").find((l) => l.trim())?.trim() ?? ""}`);
    this.props.client.captureError(error, { source: "render" });
    void this.props.client.flush();
  }
  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const f = this.props.fallback;
    if (f === undefined) throw error;
    return typeof f === "function" ? f(error) : f;
  }
}

/** Put it next to ConvexProvider. Without `ingestKey`/`endpoint` it does nothing. */
export function CondxProvider({ convex, fallback, children, ...options }: CondxProviderProps) {
  const ref = useRef<CondxClient | null>(null);
  if (!ref.current) ref.current = new CondxClient(options);
  const client = ref.current;
  useEffect(() => {
    client.install();
    return () => client.uninstall();
  }, [client]);
  useEffect(() => {
    if (convex) wrapConvexClient(convex, client);
  }, [convex, client]);
  return (
    <Ctx.Provider value={client}>
      <Boundary client={client} fallback={fallback}>
        {children}
      </Boundary>
    </Ctx.Provider>
  );
}

export function useCondx(): CondxClient | null {
  return useContext(Ctx);
}

/** Send a report from your own UI. */
export function useReportProblem() {
  const client = useCondx();
  const [state, setState] = useState<"idle" | "sending" | "sent" | "failed">("idle");
  const report = async (text: string, extra: { email?: string; screenshot?: string } = {}) => {
    if (!client) return false;
    setState("sending");
    const ok = await client.captureReport({ text, ...extra });
    setState(ok ? "sent" : "failed");
    return ok;
  };
  return { report, state, enabled: Boolean(client?.enabled) };
}

export type ReportProblemProps = {
  /** Texts, to translate them. */
  labels?: Partial<typeof LABELS>;
  /** Offer a screenshot of the page (your function makes it, e.g. with html-to-image). Only sent if the person ticks it. */
  captureScreenshot?: () => Promise<string>;
  /** Ask for an email to get back to the person (optional for them). */
  askEmail?: boolean;
  style?: CSSProperties;
  className?: string;
};

const LABELS = {
  button: "Report a problem",
  title: "What went wrong?",
  placeholder: "Tell us what you were doing and what happened.",
  email: "Your email, if you want us to reply (optional)",
  screenshot: "Include a screenshot of this page",
  send: "Send",
  cancel: "Cancel",
  sent: "Thanks, we got it.",
  failed: "It couldn't be sent. Try again in a moment.",
};

/** A button that opens a small form. It renders nothing if the SDK isn't configured. */
export function ReportProblem({ labels, captureScreenshot, askEmail, style, className }: ReportProblemProps) {
  const l = useMemo(() => ({ ...LABELS, ...labels }), [labels]);
  const { report, state, enabled } = useReportProblem();
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const [email, setEmail] = useState("");
  const [shot, setShot] = useState(false);
  if (!enabled) return null;
  const send = async () => {
    const screenshot = shot && captureScreenshot ? await captureScreenshot().catch(() => undefined) : undefined;
    if (await report(text, { email: email || undefined, screenshot })) {
      setText("");
      setEmail("");
      setShot(false);
      setTimeout(() => setOpen(false), 1200);
    }
  };
  const box: CSSProperties = { position: "fixed", right: 16, bottom: 16, zIndex: 2147483000, fontFamily: "system-ui, sans-serif", fontSize: 14, ...style };
  if (!open) {
    return (
      <div style={box} className={className}>
        <button type="button" onClick={() => setOpen(true)} style={btn(true)}>
          {l.button}
        </button>
      </div>
    );
  }
  return (
    <div style={box} className={className} role="dialog" aria-label={l.title}>
      <div style={{ width: 320, background: "#fff", color: "#111", border: "1px solid #ddd", borderRadius: 8, padding: 14, boxShadow: "0 8px 24px rgba(0,0,0,.18)", display: "flex", flexDirection: "column", gap: 8 }}>
        <strong>{l.title}</strong>
        <textarea aria-label={l.title} value={text} onChange={(e) => setText(e.target.value)} placeholder={l.placeholder} rows={4} style={{ resize: "vertical", font: "inherit", padding: 8, border: "1px solid #ccc", borderRadius: 6 }} />
        {askEmail && <input aria-label={l.email} type="email" value={email} onChange={(e) => setEmail(e.target.value)} placeholder={l.email} style={{ font: "inherit", padding: 8, border: "1px solid #ccc", borderRadius: 6 }} />}
        {captureScreenshot && (
          <label style={{ display: "flex", gap: 6, alignItems: "center", fontSize: 13 }}>
            <input type="checkbox" checked={shot} onChange={(e) => setShot(e.target.checked)} /> {l.screenshot}
          </label>
        )}
        {state === "sent" && <span style={{ color: "#15803d", fontSize: 13 }}>{l.sent}</span>}
        {state === "failed" && <span style={{ color: "#b91c1c", fontSize: 13 }}>{l.failed}</span>}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <button type="button" onClick={() => setOpen(false)} style={btn(false)}>
            {l.cancel}
          </button>
          <button type="button" disabled={!text.trim() || state === "sending"} onClick={send} style={btn(true)}>
            {l.send}
          </button>
        </div>
      </div>
    </div>
  );
}

function btn(primary: boolean): CSSProperties {
  return { font: "inherit", padding: "8px 12px", borderRadius: 6, border: primary ? "1px solid #111" : "1px solid #ccc", background: primary ? "#111" : "#fff", color: primary ? "#fff" : "#111", cursor: "pointer" };
}
