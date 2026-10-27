// Configuración → Conectar un agente → Agentes en los hilos (connections.rs del daemon): cada agente
// externo (Claude Code, Codex, Cursor…) con su token y su nivel. Leer; leer y contestar donde lo
// mencionan; orquestar (llamar a los agentes de Condx, arrancar chats, tareas y flujos, y contestar
// las preguntas de los flujos si se deja). Aprobar, producción y claves quedan siempre aquí.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::json;

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

const KINDS: &[(&str, &str, &str)] = &[("claude-code", "Claude Code", "claude"), ("codex", "Codex", "openai"), ("cursor", "Cursor", "other"), ("gemini-cli", "Gemini CLI", "gemini"), ("other", "Other", "other")];

impl DevtoolApp {
    pub fn load_connections(&mut self, cx: &mut Context<Self>) {
        self.get("/api/connections".into(), |app, res| app.connections = Some(res), cx);
    }

    fn create_connection(&mut self, cx: &mut Context<Self>) {
        let name = self.inputs.get("conn-name").map(|s| s.read(cx).value().trim().to_string()).unwrap_or_default();
        let body = json!({ "name": name, "kind": self.conn_kind, "level": self.conn_level, "answerQuestions": self.conn_answer, "editFlows": self.conn_edit });
        self.conn_error = None;
        self.post("/api/connections".into(), body, |app, res, cx| match res {
            Load::Ready(v) => {
                app.conn_new = Some(v);
                app.load_connections(cx);
            }
            Load::Failed(e) => app.conn_error = Some(e),
            Load::Loading => {}
        }, cx);
    }

    fn revoke_connection(&mut self, id: String, cx: &mut Context<Self>) {
        self.delete(format!("/api/connections/{id}"), |app, res, cx| match res {
            Load::Failed(e) => app.conn_error = Some(e),
            _ => app.load_connections(cx),
        }, cx);
    }
}

fn level_label(level: &str) -> &'static str {
    match level {
        "read" => t!("Read"),
        "reply" => t!("Read and reply"),
        _ => t!("Orchestrate"),
    }
}

fn snippet(id: &str, label: &'static str, text: String, cx: &mut Context<DevtoolApp>) -> Div {
    v_flex()
        .gap_1()
        .child(h_flex().items_center().child(div().flex_1().child(ui::muted(label, cx).text_xs())).child(Clipboard::new(SharedString::from(format!("conn-copy-{id}"))).value(text.clone()).tooltip(t!("Copy"))))
        .child(ui::contained().px_2().py_1().overflow_hidden().child(ui::mono(text).text_xs().whitespace_normal()))
}

pub fn card(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    let card = ui::card(cx).child(ui::section(t!("Agents in the threads"), cx)).child(
        ui::muted(t!("Give each external agent its own token: it joins the threads with its name and logo, and you mention it with @its-name. Read: it reads the threads. Read and reply: it also answers where you mention it. Orchestrate: it also calls Condx's agents, starts chats, tasks and flows, cancels and retries them. Approving, production and keys stay in this app."), cx).text_xs(),
    );
    let data = match app.connections.clone() {
        Some(Load::Ready(v)) => v,
        Some(Load::Failed(e)) => return card.child(ui::status_tag(Health::Warn, e)),
        _ => {
            if app.daemon.is_some() {
                app.load_connections(cx);
            }
            return card.child(ui::muted(t!("Loading…"), cx));
        }
    };
    let mut col = v_flex().gap_1p5();
    let list = data["connections"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        col = col.child(ui::muted(t!("No agent has its own connection yet."), cx).text_xs());
    }
    for c in list {
        let id = c["id"].as_str().unwrap_or_default().to_string();
        let online = c["online"] == true;
        let seen = c["lastSeen"].as_i64().filter(|t| *t > 0).map(ui::ago).unwrap_or_else(|| t!("never connected").to_string());
        let mut flags = vec![level_label(c["level"].as_str().unwrap_or("")).to_string()];
        if c["answerQuestions"] == true {
            flags.push(t!("answers flow questions").to_string());
        }
        if c["editFlows"] == true {
            flags.push(t!("edits flows and agents").to_string());
        }
        col = col.child(
            h_flex()
                .gap_2()
                .items_center()
                .children(crate::logos::badge(c["logo"].as_str().unwrap_or(""), 22., cx))
                .child(v_flex().flex_1().min_w_0().child(h_flex().gap_1p5().child(div().text_size(px(12.5)).child(ui::txt(&c["name"]).to_string())).child(ui::muted(format!("@{}", ui::txt(&c["handle"])), cx).text_xs())).child(ui::muted(flags.join(" · "), cx).text_xs().truncate()))
                .child(ui::status_tag(if online { Health::Ok } else { Health::Unknown }, if online { t!("connected").to_string() } else { seen }))
                .child(Button::new(SharedString::from(format!("conn-revoke-{id}"))).xsmall().ghost().label(t!("Revoke")).on_click(cx.listener(move |this, _, _, cx| this.revoke_connection(id.clone(), cx)))),
        );
    }
    // Formulario: nombre, qué agente es, nivel y opciones.
    let input = app.inputs.entry("conn-name".into()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(t!("Name (e.g. Claude Code)")))).clone();
    let chip = |label: String, id: String, on: bool, cx: &mut Context<DevtoolApp>, f: Box<dyn Fn(&mut DevtoolApp)>| ui::brand_if(on, Button::new(SharedString::from(id)).xsmall().label(label).on_click(cx.listener(move |this, _, _, cx| {
        f(this);
        cx.notify();
    })), cx);
    let mut kinds = h_flex().gap_1().flex_wrap().items_center();
    for (k, label, logo) in KINDS {
        let k2 = k.to_string();
        kinds = kinds.child(h_flex().gap_1().items_center().children(crate::logos::badge(logo, 16., cx)).child(chip(label.to_string(), format!("conn-kind-{k}"), app.conn_kind == *k, cx, Box::new(move |this| this.conn_kind = k2.clone()))));
    }
    let mut levels = h_flex().gap_1();
    for l in ["read", "reply", "orchestrate"] {
        levels = levels.child(chip(level_label(l).to_string(), format!("conn-level-{l}"), app.conn_level == l, cx, Box::new(move |this| this.conn_level = l.to_string())));
    }
    let orchestrate = app.conn_level == "orchestrate";
    col = col.child(
        v_flex()
            .gap_1p5()
            .pt_2()
            .child(div().w(px(260.)).child(Input::new(&input).small()))
            .child(kinds)
            .child(levels)
            .when(orchestrate, |d| {
                d.child(h_flex().gap_2().items_center().child(ui::toggle("conn-answer", app.conn_answer, cx).on_click(cx.listener(|this, _, _, cx| {
                    this.conn_answer = !this.conn_answer;
                    cx.notify();
                }))).child(ui::muted(t!("Can answer the flows' questions (e.g. “Fix it now?”)"), cx).text_xs()))
                .child(h_flex().gap_2().items_center().child(ui::toggle("conn-edit", app.conn_edit, cx).on_click(cx.listener(|this, _, _, cx| {
                    this.conn_edit = !this.conn_edit;
                    cx.notify();
                }))).child(ui::muted(t!("Can edit flows and agents of the catalog"), cx).text_xs()))
            })
            .child(h_flex().child(ui::brand(Button::new("conn-create").small().icon(IconName::Plus).label(t!("Connect")), cx).on_click(cx.listener(|this, _, _, cx| this.create_connection(cx))))),
    );
    // La recién creada: el token se ve solo ahora.
    if let Some(new) = app.conn_new.clone() {
        col = col.child(
            v_flex()
                .gap_2()
                .mt_1()
                .p_2()
                .border_1()
                .border_color(crate::theme::Tokens::current(cx).accent_ink)
                .child(ui::status_tag(Health::Warn, t!("Copy it now: the token isn't shown again.").to_string()))
                .child(snippet("claude", t!("Claude Code"), ui::txt(&new["claudeCode"]).to_string(), cx))
                .child(snippet("desktop", t!("Claude Desktop (claude_desktop_config.json, then restart it)"), serde_json::to_string_pretty(&new["claudeDesktop"]).unwrap_or_default(), cx))
                .child(snippet("json", t!("Other clients (mcp.json)"), serde_json::to_string_pretty(&new["mcpJson"]).unwrap_or_default(), cx))
                .child(ui::muted(t!("Mention it in a thread with @{h}; it sees it with the wait_for_mention tool.", h = ui::txt(&new["handle"])), cx).text_xs())
                .child(h_flex().child(Button::new("conn-done").xsmall().ghost().label(t!("Done, I copied it")).on_click(cx.listener(|this, _, _, cx| {
                    this.conn_new = None;
                    cx.notify();
                })))),
        );
    }
    card.child(col).when_some(app.conn_error.clone(), |c, e| c.child(ui::status_tag(Health::Bad, e)))
}
