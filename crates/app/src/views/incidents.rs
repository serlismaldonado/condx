// Incidentes (incidents.rs del daemon): lo que falló — un trabajo, un despliegue de Vercel, el CI,
// Convex que no responde — con su evidencia, cuántas veces pasó y desde cuándo. La misma falla es
// el mismo incidente; se resuelve solo cuando la señal desaparece. Ignorar o resolver se hace aquí
// (un agente por el MCP solo los lee).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

impl DevtoolApp {
    pub fn load_resolved_incidents(&mut self, cx: &mut Context<Self>) {
        self.incidents_resolved = Some(Load::Loading);
        self.get("/api/incidents?status=resolved&limit=50".into(), |app, res| app.incidents_resolved = Some(res), cx);
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

fn card(inc: &Value, cx: &mut Context<DevtoolApp>) -> Div {
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
    actions = match status {
        "open" => actions.child(action(t!("Ignore"), "ignore", id.clone(), cx)).child(action(t!("Mark resolved"), "resolve", id.clone(), cx)),
        "ignored" => actions.child(action(t!("Reopen"), "reopen", id.clone(), cx)).child(action(t!("Mark resolved"), "resolve", id.clone(), cx)),
        _ => actions.child(action(t!("Reopen"), "reopen", id.clone(), cx)),
    };
    c.child(actions)
}

pub fn render(app: &mut DevtoolApp, _: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
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
        col = col.child(card(inc, cx));
    }
    col.into_any_element()
}
