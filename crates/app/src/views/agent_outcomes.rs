// Configuración → Agentes → Cómo les va (outcomes.rs del daemon): cuánto rinde cada agente y cada
// flujo en los últimos 30 días (corridas, cuántas salen bien, tokens y costo, arreglos que se
// hicieron, se descartaron o llegaron a desplegarse), para decidir qué conservar o cambiar.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable};
use gpui_kit::*;
use serde_json::Value;

use crate::app::{DevtoolApp, Load};
use crate::ui;

impl DevtoolApp {
    pub fn load_outcomes(&mut self, cx: &mut Context<Self>) {
        self.get("/api/agents/outcomes".into(), |app, res| app.outcomes = Some(res), cx);
    }
}

fn tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1000 => format!("{}k", n / 1000),
        n => n.to_string(),
    }
}

fn cell(text: String, w: f32, cx: &App) -> Div {
    div().w(px(w)).flex_none().text_size(px(12.)).text_color(crate::theme::Tokens::current(cx).fg).child(text)
}

fn head(labels: &[(&'static str, f32)], cx: &App) -> Div {
    let mut row = h_flex().gap_2().pb_1();
    for (l, w) in labels {
        row = row.child(div().w(px(*w)).flex_none().child(ui::muted(*l, cx).text_xs()));
    }
    row
}

pub fn card(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) -> Div {
    let card = ui::card(cx).child(
        h_flex()
            .items_center()
            .child(div().flex_1().child(ui::section(t!("How they're doing (last 30 days)"), cx)))
            .child(Button::new("outcomes-reload").xsmall().ghost().icon(IconName::RefreshCw).on_click(cx.listener(|this, _, _, cx| this.load_outcomes(cx)))),
    );
    let data = match app.outcomes.clone() {
        Some(Load::Ready(v)) => v,
        Some(Load::Failed(e)) => return card.child(ui::muted(e, cx)),
        _ => {
            if app.daemon.is_some() && app.outcomes.is_none() {
                app.load_outcomes(cx);
            }
            return card.child(ui::muted(t!("Loading…"), cx));
        }
    };
    let agents = data["agents"].as_array().cloned().unwrap_or_default();
    let flows = data["flows"].as_array().cloned().unwrap_or_default();
    if agents.is_empty() && flows.is_empty() {
        return card.child(ui::muted(t!("No agent has run yet. When they do, you'll see here how many runs end well, what they cost and which fixes made it."), cx).text_xs());
    }
    let money = |v: &Value| v.as_f64().filter(|c| *c > 0.).map(|c| format!("${c:.2}")).unwrap_or_else(|| "—".into());
    let mut a = v_flex().gap_1().child(ui::muted(t!("Agents"), cx).text_xs()).child(head(&[(t!("Agent"), 170.), (t!("Runs"), 50.), (t!("End well"), 70.), (t!("Tokens/run"), 80.), (t!("Cost"), 60.), (t!("Minutes"), 60.), (t!("Changes / discarded"), 120.)], cx));
    for r in agents {
        a = a.child(
            h_flex()
                .gap_2()
                .child(cell(ui::txt(&r["name"]).to_string(), 170., cx).truncate())
                .child(cell(r["runs"].to_string(), 50., cx))
                .child(cell(format!("{}%", r["okRate"].as_f64().unwrap_or(0.)), 70., cx))
                .child(cell(tokens(r["avgTokens"].as_u64().unwrap_or(0)), 80., cx))
                .child(cell(money(&r["cost"]), 60., cx))
                .child(cell(format!("{}", r["avgMinutes"].as_f64().unwrap_or(0.)), 60., cx))
                .child(cell(if r["changes"].as_u64().unwrap_or(0) + r["discarded"].as_u64().unwrap_or(0) > 0 { format!("{} / {}", r["changes"], r["discarded"]) } else { "—".into() }, 120., cx)),
        );
    }
    let mut f = v_flex().gap_1().pt_2().child(ui::muted(t!("Flows with agents"), cx).text_xs()).child(head(&[(t!("Flow"), 170.), (t!("Runs"), 50.), (t!("End well"), 70.), (t!("From events"), 80.), (t!("Tokens"), 60.), (t!("Cost"), 60.), (t!("Fixes / deployed"), 120.)], cx));
    for r in flows {
        f = f.child(
            h_flex()
                .gap_2()
                .child(cell(ui::txt(&r["name"]).to_string(), 170., cx).truncate())
                .child(cell(r["runs"].to_string(), 50., cx))
                .child(cell(format!("{}%", r["okRate"].as_f64().unwrap_or(0.)), 70., cx))
                .child(cell(r["fromEvents"].to_string(), 80., cx))
                .child(cell(tokens(r["tokens"].as_u64().unwrap_or(0)), 60., cx))
                .child(cell(money(&r["cost"]), 60., cx))
                .child(cell(format!("{} / {}", r["fixes"], r["deployed"]), 120., cx)),
        );
    }
    card.child(div().id("outcomes-scroll").overflow_x_scroll().child(v_flex().child(a).child(f)))
}
