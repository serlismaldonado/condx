// Agentes: conversar con el agente del proyecto (agent_host.rs del daemon). A la izquierda, los
// chats y las corridas sobre incidentes; a la derecha, la conversación y dónde escribir. Un chat
// nuevo solo lee (el código del proyecto y Condx por el MCP); para arreglar algo, el agente dice
// qué y dónde, y los arreglos se piden desde un incidente. El daemon pone el modelo y los límites.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{h_flex, v_flex, Disableable, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load, Page};
use crate::ui::{self, Health};

impl DevtoolApp {
    pub fn load_chats(&mut self, cx: &mut Context<Self>) {
        self.chats_at = ui::now_ms();
        self.get("/api/agent-runs".into(), |app, res| {
            if let Load::Ready(v) = res {
                app.chats = v["runs"].as_array().cloned().unwrap_or_default();
            }
        }, cx);
    }

    fn open_chat(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.chat_open = id;
        self.chat = None;
        self.chat_error = None;
        self.load_chat(cx);
        cx.notify();
    }

    fn load_chat(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.chat_open.clone() else { return };
        self.chat_at = ui::now_ms();
        self.get(format!("/api/agent-runs/{id}"), move |app, res| {
            if app.chat_open.as_deref() == Some(id.as_str()) {
                app.chat = Some(res);
            }
        }, cx);
    }

    /// Enviar lo escrito: sin conversación abierta, empieza un chat; si no, sigue (o redirige) la abierta.
    fn send_chat(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.chat_input.clone() else { return };
        let text = input.read(cx).value().trim().to_string();
        if text.is_empty() || self.chat_sending {
            return;
        }
        self.chat_sending = true;
        self.chat_error = None;
        let (path, body) = match self.chat_open.clone() {
            Some(id) => (format!("/api/agent-runs/{id}/message"), json!({ "text": text })),
            None => ("/api/agent-chats".to_string(), json!({ "text": text, "project": self.project })),
        };
        self.post(path, body, |app, res, cx| {
            app.chat_sending = false;
            match res {
                Load::Ready(run) => {
                    app.chat_clear = true;
                    if app.chat_open.is_none() {
                        app.chat_open = run["id"].as_str().map(str::to_string);
                    }
                    app.load_chat(cx);
                    app.load_chats(cx);
                }
                Load::Failed(e) => app.chat_error = Some(e),
                Load::Loading => {}
            }
        }, cx);
    }

    fn stop_chat(&mut self, id: String, cx: &mut Context<Self>) {
        self.post(format!("/api/agent-runs/{id}/abort"), Value::Null, |app, res, cx| match res {
            Load::Failed(e) => app.chat_error = Some(e),
            _ => {
                app.load_chat(cx);
                app.load_chats(cx);
            }
        }, cx);
    }
}

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let t = crate::theme::Tokens::current(cx);
    // La lista se refresca cada 5 s; la conversación abierta, cada 2 s mientras el agente trabaja.
    if app.daemon.is_some() && ui::now_ms() - app.chats_at > 5_000 {
        app.load_chats(cx);
    }
    let running = matches!(&app.chat, Some(Load::Ready(r)) if r["status"] == "running");
    // También si la lista ve otra cosa (un mensaje que llegó desde el teléfono o por otra vía).
    let loaded = match &app.chat {
        Some(Load::Ready(r)) => (r["status"].as_str().map(str::to_string), r["usage"]["total"].as_u64()),
        _ => (None, None),
    };
    let listed = app.chat_open.as_ref().and_then(|id| app.chats.iter().find(|c| c["id"] == id.as_str())).map(|c| (c["status"].as_str().map(str::to_string), c["usage"].as_u64()));
    let stale = listed.as_ref().is_some_and(|l| l.0.as_deref() == Some("running") || (l.0 != loaded.0 && loaded.0.is_some()));
    if (running || stale) && ui::now_ms() - app.chat_at > 2_000 {
        app.load_chat(cx);
    }
    // (El sondeo general de la app vuelve a pintar cada 3 s: ahí se piden de nuevo.)
    h_flex()
        .size_full()
        .items_stretch()
        .child(list(app, cx).w(px(260.)).flex_none().border_r_1().border_color(t.card_border))
        .child(conversation(app, window, cx).flex_1().min_w_0())
}

fn list(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) -> Div {
    let t = crate::theme::Tokens::current(cx);
    let new_selected = app.chat_open.is_none();
    let new = Button::new("chat-new").small().w_full().icon(IconName::Plus).label(t!("New chat")).on_click(cx.listener(|this, _, _, cx| this.open_chat(None, cx)));
    let mut items = v_flex().id("chat-list").flex_1().min_h_0().overflow_y_scroll().gap_0p5().p_2();
    if app.chats.is_empty() {
        items = items.child(div().p_2().child(ui::muted(t!("Your conversations with the agent show up here, and the agents that worked on incidents."), cx).text_xs()));
    }
    for r in app.chats.clone() {
        let id = r["id"].as_str().unwrap_or_default().to_string();
        let selected = app.chat_open.as_deref() == Some(id.as_str());
        let health = match r["status"].as_str() {
            Some("running") => Health::Warn,
            Some("done") => Health::Ok,
            Some("failed") => Health::Bad,
            _ => Health::Unknown,
        };
        let sub = [
            (r["kind"] == "incident").then(|| t!("incident").to_string()),
            r["model"].as_str().map(str::to_string),
            r["startedAt"].as_i64().map(ui::ago),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let open_id = id.clone();
        items = items.child(
            v_flex()
                .id(SharedString::from(format!("chat-{id}")))
                .gap_0p5()
                .px_2()
                .py_1p5()
                .cursor_pointer()
                .border_l_2()
                .border_color(if selected { t.accent_ink } else { gpui_kit::transparent_black() })
                .when(selected, |d| d.bg(t.active))
                .hover(|d| d.bg(t.hover))
                .on_click(cx.listener(move |this, _, _, cx| this.open_chat(Some(open_id.clone()), cx)))
                .child(h_flex().gap_1p5().items_center().child(ui::dot(health, cx)).child(div().flex_1().min_w_0().text_size(px(12.5)).text_color(t.fg).truncate().child(ui::txt(&r["title"]).to_string())))
                .child(div().pl(px(14.)).text_size(px(11.)).text_color(t.subtle).truncate().child(sub)),
        );
    }
    v_flex()
        .child(div().p_2().border_b_1().border_color(t.card_border).child(if new_selected { ui::brand(new, cx).into_any_element() } else { new.outline().into_any_element() }))
        .child(items)
}

fn conversation(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    let t = crate::theme::Tokens::current(cx);
    let has_provider = app.agents.as_ref().is_some_and(|a| a["settings"]["defaultProvider"].is_string());
    if app.agents.is_none() && app.daemon.is_some() && app.agents_error.is_none() {
        app.load_agents(cx);
    }
    let run = match (&app.chat_open, &app.chat) {
        (None, _) => None,
        (Some(_), Some(Load::Ready(r))) => Some(r.clone()),
        (Some(_), Some(Load::Failed(e))) => return v_flex().p_4().child(ui::status_tag(Health::Bad, e.clone())),
        (Some(_), _) => return v_flex().p_4().child(ui::muted(t!("Loading…"), cx)),
    };
    let status = run.as_ref().and_then(|r| r["status"].as_str()).unwrap_or("").to_string();
    let running = status == "running";

    // Encabezado: de qué es, con qué modelo, cuánto lleva y detener.
    let header = run.as_ref().map(|r| {
        let title = r["title"].as_str().map(str::to_string).or_else(|| app.chats.iter().find(|c| c["id"] == r["id"]).and_then(|c| c["title"].as_str().map(str::to_string))).unwrap_or_default();
        let state = match status.as_str() {
            "running" => (Health::Warn, t!("Working…").to_string()),
            "done" => (Health::Ok, t!("Waiting for you").to_string()),
            "aborted" => (Health::Unknown, t!("Stopped").to_string()),
            "stopped" => (Health::Unknown, t!("The agent stopped: {why}", why = r["error"].as_str().unwrap_or("it reached a limit")).to_string()),
            _ => (Health::Bad, r["error"].as_str().map(|e| t!("Failed: {e}", e = e).to_string()).unwrap_or_else(|| t!("The agent failed").to_string())),
        };
        let id = r["id"].as_str().unwrap_or_default().to_string();
        let mut h = h_flex()
            .gap_3()
            .items_center()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(t.card_border)
            .child(v_flex().flex_1().min_w_0().child(div().text_size(px(13.)).text_color(t.fg).truncate().child(title)).child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(ui::status_tag(state.0, state.1))
                    .child(ui::muted(format!("{} · {}", ui::txt(&r["model"]), t!("{n} tokens", n = r["usage"]["total"].as_u64().unwrap_or(0))), cx).text_xs()),
            ));
        if let Some(inc) = r["incident"].as_str().filter(|i| !i.is_empty()) {
            let _ = inc;
            h = h.child(Button::new("chat-incident").xsmall().ghost().icon(IconName::TriangleAlert).label(t!("Open incident")).on_click(cx.listener(|this, _, window, cx| this.open_page(Page::Incidents, window, cx))));
        }
        if running {
            h = h.child(Button::new("chat-stop").xsmall().danger().outline().label(t!("Stop")).on_click(cx.listener(move |this, _, _, cx| this.stop_chat(id.clone(), cx))));
        }
        h
    });

    // Los mensajes. En una corrida de incidente, el primero es el incidente entero: se omite.
    let mut msgs = v_flex().gap_3().px_4().py_3().max_w(px(820.));
    match &run {
        None => {
            msgs = msgs.child(
                v_flex()
                    .gap_2()
                    .pt(px(48.))
                    .child(div().text_size(px(18.)).text_color(t.fg).child(t!("Ask the agent about your project")))
                    .child(ui::muted(t!("It reads your code and what Condx knows: environments, jobs and their logs, incidents, workflows. It only reads; it can't change files, deploy or approve."), cx))
                    .child(examples(cx)),
            );
            if !has_provider && app.agents.is_some() {
                msgs = msgs.child(
                    h_flex().gap_2().items_center().pt_2().child(ui::status_tag(Health::Warn, t!("Choose a default provider first").to_string())).child(Button::new("chat-settings").xsmall().outline().label(t!("Open Settings → Agents")).on_click(cx.listener(|this, _, window, cx| {
                        this.settings_tab = crate::app::SettingsTab::Agents;
                        this.open_page(Page::Settings, window, cx);
                    }))),
                );
            }
        }
        Some(r) => {
            // Llegó algo nuevo (o se abrió otra): abajo, a lo último.
            // (Cuenta el texto también: el último mensaje crece mientras el agente escribe.)
            let len = r["transcript"].as_array().map(|t| t.iter().map(|i| i["text"].as_str().map_or(0, str::len) + 1).sum::<usize>()).unwrap_or(0) + usize::from(running);
            if len != app.chat_len {
                app.chat_len = len;
                app.chat_scroll.scroll_to_bottom();
            }
            let skip = usize::from(r["kind"] != "chat" && r["incident"].as_str().is_some_and(|i| !i.is_empty()));
            for item in r["transcript"].as_array().cloned().unwrap_or_default().iter().skip(skip) {
                msgs = msgs.child(message(item, cx));
            }
            if running {
                msgs = msgs.child(h_flex().gap_2().items_center().child(ui::dot(Health::Warn, cx)).child(ui::muted(t!("The agent is working…"), cx).text_xs()));
            }
        }
    }

    let input = app
        .chat_input
        .get_or_insert_with(|| {
            let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 8).submit_on_enter(true).placeholder(t!("Message the agent…  (Enter sends, Shift+Enter adds a line)")));
            cx.subscribe(&state, |this: &mut DevtoolApp, _, ev: &InputEvent, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    this.send_chat(cx);
                }
            })
            .detach();
            state
        })
        .clone();
    // Enviado: se vacía (aquí, que hay ventana).
    if std::mem::take(&mut app.chat_clear) {
        input.update(cx, |s, cx| s.set_value("", window, cx));
    }
    let can_send = has_provider && !app.chat_sending;
    let send = Button::new("chat-send").small().icon(IconName::ArrowUp).label(if running { t!("Redirect") } else { t!("Send") }).disabled(!can_send).on_click(cx.listener(|this, _, _, cx| this.send_chat(cx)));
    let composer = v_flex()
        .gap_1()
        .px_4()
        .py_3()
        .border_t_1()
        .border_color(t.card_border)
        .when_some(app.chat_error.clone(), |d, e| d.child(ui::status_tag(Health::Bad, e)))
        .child(h_flex().gap_2().items_end().child(div().flex_1().min_w_0().child(Textarea::new(&input))).child(ui::brand(send, cx)))
        .child(ui::muted(if running { t!("While it works, what you write redirects it.") } else { t!("It only reads. Today's token budget and the agents-at-once limit apply (Settings → Agents).") }, cx).text_xs());

    v_flex()
        .size_full()
        .children(header)
        .child(div().id("chat-messages").flex_1().min_h_0().overflow_y_scroll().track_scroll(&app.chat_scroll).child(msgs))
        .child(composer)
}

/// Preguntas de ejemplo: un clic las deja escritas.
fn examples(cx: &mut Context<DevtoolApp>) -> Div {
    let mut row = h_flex().gap_2().flex_wrap().pt_2();
    for (i, q) in [t!("Why did the last job fail?"), t!("What changed in the schema this week?"), t!("Which functions does the mobile app call?"), t!("Is production behind dev?")].into_iter().enumerate() {
        row = row.child(Button::new(SharedString::from(format!("chat-ex-{i}"))).xsmall().outline().label(q).on_click(cx.listener(move |this, _, window, cx| {
            if let Some(input) = this.chat_input.clone() {
                input.update(cx, |s, cx| s.set_value(q, window, cx));
            }
        })));
    }
    row
}

/// Un mensaje: lo de la persona en un recuadro a la derecha; las llamadas del agente a sus
/// herramientas, en una línea tenue; su respuesta, como texto.
fn message(item: &Value, cx: &mut Context<DevtoolApp>) -> AnyElement {
    let t = crate::theme::Tokens::current(cx);
    let text = item["text"].as_str().unwrap_or_default();
    match item["role"].as_str() {
        Some("user") => h_flex()
            .child(div().flex_1())
            .child(div().max_w(px(560.)).px_3().py_2().bg(t.card).border_1().border_color(t.card_border).text_size(px(12.5)).text_color(t.fg).child(text.to_string()))
            .into_any_element(),
        Some("tool") => {
            // Una línea: el resultado con los espacios y saltos juntados (un JSON se lee así).
            let first = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect::<String>();
            let color: Hsla = if item["isError"] == true { rgb(0xe5756e).into() } else { t.subtle };
            ui::mono(format!("  {} → {first}", ui::txt(&item["tool"]))).text_size(px(11.)).text_color(color).truncate().into_any_element()
        }
        _ => {
            // Las llamadas («→ herramienta(args)») van tenues; el resto es la respuesta.
            let mut col = v_flex().gap_0p5();
            let mut prose = String::new();
            for line in text.lines() {
                if line.starts_with("→ ") {
                    col = col.child(ui::mono(line.to_string()).text_size(px(11.)).text_color(t.muted).truncate());
                } else {
                    prose.push_str(line);
                    prose.push('\n');
                }
            }
            if !prose.trim().is_empty() {
                col = col.child(super::incidents::answer_view(prose.trim(), cx));
            }
            col.into_any_element()
        }
    }
}
