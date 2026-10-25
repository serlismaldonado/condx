// Incidentes (incidents.rs del daemon): lo que falló — un trabajo, un despliegue de Vercel, el CI,
// Convex que no responde — con su evidencia, cuántas veces pasó y desde cuándo. La misma falla es
// el mismo incidente; se resuelve solo cuando la señal desaparece. Ignorar o resolver se hace aquí
// (un agente por el MCP solo los lee).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

impl DevtoolApp {
    pub fn load_resolved_incidents(&mut self, cx: &mut Context<Self>) {
        self.incidents_resolved = Some(Load::Loading);
        self.get("/api/incidents?status=resolved&limit=50".into(), |app, res| app.incidents_resolved = Some(res), cx);
    }

    /// Arranca un agente de diagnóstico (lo decide y limita el daemon).
    fn diagnose(&mut self, id: String, cx: &mut Context<Self>) {
        self.post(format!("/api/incidents/{id}/diagnose"), Value::Null, |app, res, cx| match res {
            Load::Ready(run) => {
                app.agent_run_open = run["id"].as_str().map(str::to_string);
                app.load_agent_run(cx);
            }
            other => app.action = Some(other),
        }, cx);
    }

    pub fn load_agent_run(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.agent_run_open.clone() else { return };
        self.agent_run_at = ui::now_ms();
        if self.agent_run.is_none() {
            self.agent_run = Some(Load::Loading);
        }
        self.get(format!("/api/agent-runs/{id}"), move |app, res| {
            if app.agent_run_open.as_deref() == Some(id.as_str()) {
                app.agent_run = Some(res);
            }
        }, cx);
    }

    fn send_to_agent(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(input) = self.inputs.get("agent-message").cloned() else { return };
        let text = input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.post(format!("/api/agent-runs/{id}/message"), json!({ "text": text }), |app, res, cx| match res {
            Load::Ready(_) => {
                app.inputs.remove("agent-message");
                app.load_agent_run(cx);
            }
            other => app.action = Some(other),
        }, cx);
    }

    fn stop_agent(&mut self, id: String, cx: &mut Context<Self>) {
        self.post(format!("/api/agent-runs/{id}/abort"), Value::Null, |app, res, cx| match res {
            Load::Ready(_) => app.load_agent_run(cx),
            other => app.action = Some(other),
        }, cx);
    }

    fn incident_action(&mut self, id: String, action: &'static str, cx: &mut Context<Self>) {
        self.post(format!("/api/incidents/{id}/{action}"), Value::Null, move |app, res, cx| {
            match res {
                Load::Ready(v) => {
                    // Al momento, sin esperar al próximo sondeo.
                    app.incidents.retain(|i| i["id"] != v["id"]);
                    if v["status"] != "resolved" {
                        app.incidents.insert(0, v);
                    }
                    if app.incidents_resolved.is_some() {
                        app.load_resolved_incidents(cx);
                    }
                }
                other => app.action = Some(other),
            }
        }, cx);
    }
}

fn source_label(source: &str) -> &'static str {
    match source {
        "job" => t!("job"),
        "vercel" => "Vercel",
        "ci" => "CI",
        "convex" => "Convex",
        _ => t!("other"),
    }
}

/// El agente de un incidente: su estado, la respuesta y, abierta, la conversación.
fn agent_block(app: &mut DevtoolApp, inc: &Value, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Option<Div> {
    let t = crate::theme::Tokens::current(cx);
    let agent = &inc["agent"];
    let run_id = agent["runId"].as_str()?.to_string();
    let status = agent["status"].as_str().unwrap_or("running");
    let (health, label) = match status {
        "running" => (Health::Warn, t!("An agent is diagnosing it ({m})", m = ui::txt(&agent["model"])).to_string()),
        "done" => (Health::Ok, t!("Diagnosis by {m} · {w}", m = ui::txt(&agent["model"]), w = agent["at"].as_i64().map(ui::ago).unwrap_or_default()).to_string()),
        "aborted" => (Health::Unknown, t!("The agent was stopped").to_string()),
        "stopped" => (Health::Unknown, t!("The agent stopped: today's token budget ran out").to_string()),
        _ => (Health::Bad, t!("The agent failed").to_string()),
    };
    let open = app.agent_run_open.as_deref() == Some(run_id.as_str());
    let toggle_id = run_id.clone();
    let mut block = v_flex().gap_1p5().pt_1().border_t_1().border_color(t.card_border).child(
        h_flex()
            .gap_2()
            .items_center()
            .child(ui::status_tag(health, label))
            .child(div().flex_1())
            .child(Button::new(SharedString::from(format!("agent-open-{run_id}"))).xsmall().ghost().label(if open { t!("Hide conversation") } else { t!("Conversation") }).on_click(cx.listener(move |this, _, _, cx| {
                if this.agent_run_open.as_deref() == Some(toggle_id.as_str()) {
                    this.agent_run_open = None;
                } else {
                    this.agent_run_open = Some(toggle_id.clone());
                    this.agent_run = None;
                    this.load_agent_run(cx);
                }
                cx.notify();
            }))),
    );
    // La respuesta, con sus secciones (## Cause, ## Fix…) como títulos pequeños.
    if let Some(answer) = agent["answer"].as_str().filter(|a| !a.trim().is_empty()).filter(|_| !open) {
        block = block.child(answer_view(answer, cx));
    }
    if !open {
        return Some(block);
    }
    // Mientras corre, se vuelve a pedir cada 2 s.
    let running = matches!(&app.agent_run, Some(Load::Ready(r)) if r["status"] == "running") || status == "running";
    if running && ui::now_ms() - app.agent_run_at > 2_000 {
        app.load_agent_run(cx);
    }
    let run = match &app.agent_run {
        Some(Load::Ready(r)) => r.clone(),
        Some(Load::Failed(e)) => return Some(block.child(ui::muted(e.clone(), cx))),
        _ => return Some(block.child(ui::muted(t!("Loading…"), cx))),
    };
    let mut convo = v_flex().gap_1().p_2().bg(t.ground).border_1().border_color(t.card_border);
    for item in run["transcript"].as_array().cloned().unwrap_or_default().iter().skip(1) {
        let text = item["text"].as_str().unwrap_or_default();
        convo = convo.child(match item["role"].as_str() {
            Some("user") => div().text_size(px(12.)).text_color(t.fg).child(format!("› {text}")),
            Some("tool") => {
                let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(140).collect::<String>();
                let color = if item["isError"] == true { rgb(0xe5756e).into() } else { t.subtle };
                ui::mono(format!("  {} → {first}", ui::txt(&item["tool"]))).text_size(px(11.)).text_color(color).truncate()
            }
            _ if text.starts_with('→') => ui::mono(text.to_string()).text_size(px(11.)).text_color(t.muted).truncate(),
            _ => answer_view(text, cx),
        });
    }
    let usage = run["usage"]["total"].as_u64().unwrap_or(0);
    let input = app.inputs.entry("agent-message".into()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(t!("Ask or redirect the agent…")))).clone();
    let (send_id, stop_id) = (run_id.clone(), run_id.clone());
    let controls = h_flex()
        .gap_2()
        .items_center()
        .child(div().flex_1().child(Input::new(&input).small()))
        .child(Button::new(SharedString::from(format!("agent-send-{run_id}"))).xsmall().outline().label(t!("Send")).on_click(cx.listener(move |this, _, _, cx| this.send_to_agent(send_id.clone(), cx))))
        .when(run["status"] == "running", |d| {
            d.child(Button::new(SharedString::from(format!("agent-stop-{run_id}"))).xsmall().danger().outline().label(t!("Stop")).on_click(cx.listener(move |this, _, _, cx| this.stop_agent(stop_id.clone(), cx))))
        });
    Some(
        block
            .child(div().id(SharedString::from(format!("agent-convo-{run_id}"))).max_h(px(420.)).overflow_y_scroll().child(convo))
            .child(controls)
            .child(ui::muted(t!("{n} tokens · it only reads; it can't change files, deploy or approve", n = usage), cx).text_xs())
            .when_some(run["error"].as_str(), |b, e| b.child(ui::status_tag(Health::Bad, e.to_string()))),
    )
}

/// Texto del agente: las líneas `## …` como etiquetas de sección, el resto tal cual.
fn answer_view(text: &str, cx: &App) -> Div {
    let t = crate::theme::Tokens::current(cx);
    let mut col = v_flex().gap_0p5();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            col = col.child(div().pt_1().child(ui::section(h.to_string(), cx)));
        } else if !line.trim().is_empty() {
            col = col.child(div().text_size(px(12.5)).text_color(t.fg).child(line.to_string()));
        }
    }
    col
}

fn card(app: &mut DevtoolApp, inc: &Value, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    let t = crate::theme::Tokens::current(cx);
    let id = inc["id"].as_str().unwrap_or_default().to_string();
    let status = inc["status"].as_str().unwrap_or("open");
    let health = match status {
        "open" => Health::Bad,
        "ignored" => Health::Unknown,
        _ => Health::Ok,
    };
    let count = inc["count"].as_i64().unwrap_or(1);
    let when = match (inc["firstSeen"].as_i64(), inc["lastSeen"].as_i64()) {
        (Some(f), Some(l)) if count > 1 => t!("{n} times · first {f} · last {l}", n = count, f = ui::ago(f), l = ui::ago(l)).to_string(),
        (_, Some(l)) => ui::ago(l),
        _ => String::new(),
    };
    let ev = &inc["evidence"];
    let mut c = ui::card(cx)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::dot(health, cx))
                .child(div().flex_1().min_w_0().truncate().text_color(t.fg).child(ui::txt(&inc["title"])))
                .child(div().px_1p5().h(px(18.)).flex().items_center().border_1().border_color(t.card_border).text_size(px(11.)).text_color(t.muted).child(source_label(inc["source"].as_str().unwrap_or(""))))
                .when(status == "ignored", |d| d.child(ui::muted(t!("ignored"), cx)))
                .when(status == "resolved", |d| {
                    let by = if inc["resolvedBy"] == "auto" { t!("resolved on its own") } else { t!("resolved") };
                    d.child(ui::muted(format!("{by} {}", inc["resolvedAt"].as_i64().map(ui::ago).unwrap_or_default()), cx))
                }),
        )
        .child(ui::muted(
            [inc["env"].as_str().map(|e| t!("on {e}", e = e).to_string()), ev["commit"].as_str().map(|c| format!("commit {}", &c[..c.len().min(8)])), Some(when)]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
            cx,
        ));
    // La evidencia: el error (o el título de la corrida) y, si hay, el final del log.
    if let Some(err) = ev["error"].as_str().filter(|e| !e.is_empty()) {
        c = c.child(ui::mono(err.lines().next().unwrap_or("").to_string()).text_size(px(11.5)).text_color(rgb(0xe5756e)).truncate());
    } else if let Some(title) = ev["title"].as_str() {
        c = c.child(ui::muted(title.to_string(), cx));
    }
    if let Some(log) = ev["log"].as_str().filter(|l| !l.is_empty()) {
        let lines: Vec<String> = log.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().map(str::to_string).collect();
        c = c.child(div().p_2().bg(t.ground).border_1().border_color(t.card_border).children(lines.into_iter().map(|l| ui::mono(l).text_size(px(11.)))));
    }
    let mut actions = h_flex().gap_1();
    if let Some(job) = ev["jobId"].as_str().map(str::to_string) {
        actions = actions.child(Button::new(SharedString::from(format!("inc-job-{id}"))).xsmall().ghost().icon(IconName::SquareTerminal).label(t!("Open job")).on_click(cx.listener(move |this, _, _, cx| {
            this.show_jobs(cx);
            this.open_job(job.clone(), cx);
        })));
    }
    if let Some(url) = ev["url"].as_str().filter(|u| u.starts_with("http")).map(str::to_string) {
        actions = actions.child(Button::new(SharedString::from(format!("inc-open-{id}"))).xsmall().ghost().icon(IconName::ExternalLink).label(t!("Open")).on_click(move |_, _, cx| cx.open_url(&url)));
    }
    actions = actions.child(div().flex_1());
    let action = |label: &'static str, verb: &'static str, id: String, cx: &mut Context<DevtoolApp>| {
        Button::new(SharedString::from(format!("inc-{verb}-{id}"))).xsmall().outline().label(label).on_click(cx.listener(move |this, _, _, cx| this.incident_action(id.clone(), verb, cx)))
    };
    let agent_running = inc["agent"]["status"] == "running";
    let has_provider = app.agents.as_ref().is_some_and(|a| a["providers"].as_array().is_some_and(|l| !l.is_empty()));
    if status != "resolved" && !agent_running && has_provider {
        let did = id.clone();
        let label = if inc["agent"].is_object() { t!("Diagnose again") } else { t!("Diagnose") };
        actions = actions.child(ui::brand(Button::new(SharedString::from(format!("inc-diagnose-{id}"))).xsmall().label(label), cx).on_click(cx.listener(move |this, _, _, cx| this.diagnose(did.clone(), cx))));
    }
    actions = match status {
        "open" => actions.child(action(t!("Ignore"), "ignore", id.clone(), cx)).child(action(t!("Mark resolved"), "resolve", id.clone(), cx)),
        "ignored" => actions.child(action(t!("Reopen"), "reopen", id.clone(), cx)).child(action(t!("Mark resolved"), "resolve", id.clone(), cx)),
        _ => actions.child(action(t!("Reopen"), "reopen", id.clone(), cx)),
    };
    let agent = agent_block(app, inc, window, cx);
    c.child(actions).children(agent)
}

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    // Para saber si hay proveedores (el botón Diagnosticar).
    if app.agents.is_none() && app.agents_error.is_none() {
        app.load_agents(cx);
    }
    let resolved_tab = app.incidents_tab_resolved;
    let mut tabs = h_flex().gap_1();
    for (resolved, label) in [(false, t!("Open")), (true, t!("Resolved"))] {
        let b = Button::new(SharedString::from(format!("inc-tab-{resolved}"))).xsmall().label(label).on_click(cx.listener(move |this, _, _, cx| {
            this.incidents_tab_resolved = resolved;
            if resolved {
                this.load_resolved_incidents(cx);
            }
            cx.notify();
        }));
        tabs = tabs.child(ui::brand_if(resolved_tab == resolved, b, cx));
    }
    let open = app.incidents.iter().filter(|i| i["status"] == "open").count();
    let mut col = v_flex()
        .gap_3()
        .p_4()
        .child(h_flex().gap_2().items_center().child(tabs).child(div().flex_1()).child(ui::muted(t!("{n} open", n = open), cx)))
        .child(ui::muted(t!("Failed jobs, failed Vercel deploys, CI in red and Convex not responding. The same failure is one incident; it resolves on its own when the signal clears. What happens with each kind is set in Settings → Agents."), cx).text_xs());
    let list: Vec<Value> = if resolved_tab {
        match &app.incidents_resolved {
            Some(Load::Ready(Value::Array(l))) => l.clone(),
            Some(Load::Failed(e)) => return col.child(ui::muted(e.clone(), cx)).into_any_element(),
            _ => return col.child(ui::muted(t!("Loading…"), cx)).into_any_element(),
        }
    } else {
        app.incidents.clone()
    };
    if list.is_empty() {
        let text = if resolved_tab { t!("Nothing resolved yet.") } else { t!("No incidents. Everything Condx watches is fine.") };
        return col.child(ui::card(cx).child(ui::status_tag(if resolved_tab { Health::Unknown } else { Health::Ok }, text))).into_any_element();
    }
    for inc in &list {
        col = col.child(card(app, inc, window, cx));
    }
    col.into_any_element()
}
