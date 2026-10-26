// Los correos de Condx con react-email (docs/design/cloud-site.md §9.1): la respuesta de un agente
// (markdown) o un aviso, con el estilo de la app. Sin JSX (el host corre TS sin compilar):
// `createElement`. El daemon pide el HTML a `POST /email` y lo manda por AgentMail con el texto.

import { createElement as h } from "react";
import { Body, Column, Container, Head, Hr, Html, Img, Markdown, Preview, Row, Section, Text, render, toPlainText } from "@react-email/components";

export type EmailRequest = {
  /** Lo que dijo el agente o el aviso, en markdown. */
  markdown: string;
  /** Quién habla (el agente) o de qué es el aviso. */
  title?: string;
  /** Una línea chica al pie (de dónde viene, qué se puede hacer). */
  footer?: string;
  /** El logo de la cabecera (PNG público: los clientes de correo no muestran SVG). */
  logo?: string;
};

/** La marca de Condx en el sitio de Condx Cloud (archivo estático del proyecto de Vercel). */
const LOGO = "https://condx.heskala.com/brand/condx-mark.png";

const ink = "#1f1f1f";
const muted = "#666666";
const accent = "#f3b01c";
const line = "#e6e6e6";
const font = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif";
const mono = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";

const markdownStyles = {
  p: { fontSize: "15px", lineHeight: "23px", color: ink, margin: "0 0 12px" },
  h1: { fontSize: "20px", lineHeight: "28px", color: ink, margin: "8px 0 10px" },
  h2: { fontSize: "17px", lineHeight: "25px", color: ink, margin: "8px 0 8px" },
  h3: { fontSize: "15px", lineHeight: "23px", color: ink, margin: "8px 0 6px" },
  li: { fontSize: "15px", lineHeight: "23px", color: ink },
  bold: { fontWeight: 600 },
  link: { color: "#b7770a" },
  codeInline: { fontFamily: mono, fontSize: "13px", background: "#f4f4f4", borderRadius: "4px", padding: "1px 5px", color: "#c4314b" },
  codeBlock: { fontFamily: mono, fontSize: "12.5px", lineHeight: "19px", background: "#f6f6f6", border: `1px solid ${line}`, borderRadius: "8px", padding: "12px", overflowX: "auto" as const, whiteSpace: "pre" as const },
  blockQuote: { borderLeft: `3px solid ${line}`, margin: "0 0 12px", padding: "0 0 0 12px", color: muted },
  table: { borderCollapse: "collapse" as const, width: "100%", margin: "0 0 12px" },
  th: { fontSize: "13px", textAlign: "left" as const, padding: "7px 10px", background: "#f4f4f4", borderBottom: `1px solid ${line}` },
  td: { fontSize: "13px", padding: "7px 10px", borderBottom: `1px solid ${line}`, verticalAlign: "top" as const },
  hr: { borderColor: line, margin: "16px 0" },
};

/** La primera línea con texto, sin marcas: la vista previa del cliente de correo. */
function preview(markdown: string): string {
  const first = markdown.split("\n").map((l) => l.trim()).find((l) => l && !l.startsWith("```") && !/^\|?[\s:-]+\|/.test(l)) ?? "";
  return first.replace(/[*_`#>|]/g, "").trim().slice(0, 140);
}

function Email({ markdown, title, footer, logo }: EmailRequest) {
  return h(
    Html,
    { lang: "es" },
    h(Head, null),
    h(Preview, null, preview(markdown)),
    h(
      Body,
      { style: { background: "#f6f6f6", fontFamily: font, margin: 0, padding: "24px 0" } },
      h(
        Container,
        { style: { background: "#ffffff", border: `1px solid ${line}`, borderRadius: "12px", maxWidth: "600px", padding: "24px 28px" } },
        h(
          Section,
          null,
          h(
            Row,
            null,
            h(Column, { style: { width: "30px", verticalAlign: "middle" } }, h(Img, { src: logo || LOGO, width: "22", height: "22", alt: "Condx", style: { display: "block" } })),
            h(Column, { style: { verticalAlign: "middle" } }, h(Text, { style: { margin: 0, fontSize: "15px", fontWeight: 600, color: ink } }, "Condx")),
          ),
          title ? h(Text, { style: { margin: "6px 0 0", fontSize: "13px", color: muted } }, title) : null,
        ),
        h(Hr, { style: { borderColor: line, margin: "14px 0 18px" } }),
        h(Markdown, { children: markdown, markdownCustomStyles: markdownStyles, markdownContainerStyles: { color: ink } }),
        footer ? h(Hr, { style: { borderColor: line, margin: "18px 0 12px" } }) : null,
        footer ? h(Text, { style: { margin: 0, fontSize: "12px", lineHeight: "18px", color: muted } }, footer) : null,
      ),
    ),
  );
}

/** El HTML del correo y su versión en texto (para los clientes que no muestran HTML). */
export async function renderEmail(req: EmailRequest): Promise<{ html: string; text: string }> {
  const markdown = String(req.markdown ?? "").trim();
  if (!markdown) throw new Error("empty email");
  const html = await render(h(Email, { ...req, markdown }));
  return { html, text: toPlainText(html) };
}
