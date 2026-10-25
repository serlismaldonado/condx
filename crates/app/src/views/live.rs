// «En vivo»: lo mismo que el panel DevTools de la web, para la pestaña
// elegida (consumo de Convex por función y errores), sin abrir el navegador.
// La pestaña se elige en un desplegable; funciones y errores van en tablas
// ordenables y el snapshot completo, plegado, como árbol JSON.

use ely_core::forms::Select;
use ely_core::layout::Collapsible;
use ely_core::primitives::IconName as Ico;
use ely_core::tables::{Cell, Column, DataTable, Row};
use ely_devtools::JsonViewer;
use gpui_kit::component::button::Button;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable};
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui;

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    render_for(app, None, window, cx)
}

/// Misma URL de Convex, sin importar `localhost` o `127.0.0.1` ni la barra final.
fn same_url(a: &str, b: &str) -> bool {
    let norm = |u: &str| u.trim_end_matches('/').replace("://localhost", "://127.0.0.1");
    !a.is_empty() && norm(a) == norm(b)
}

/// En la pestaña de un entorno, solo las pestañas de la app conectadas a su URL de Convex.
pub fn render_for(app: &mut DevtoolApp, url: Option<&str>, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let tabs: Vec<Value> = app
        .overview
        .as_ref()
        .and_then(|o| o["tabs"]["list"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|t| url.is_none_or(|u| same_url(t["convexUrl"].as_str().unwrap_or_default(), u)))
        .collect();
    if tabs.is_empty() {
        let msg = if url.is_some() {
            t!("No app tab connected to this environment. Open the app pointing at its Convex URL (in development, or with localStorage heskala:devd=1).")
        } else {
            t!("No app tab connected. Open the app in development (or with localStorage heskala:devd=1).")
        };
        return super::empty(msg.to_string(), cx).into_any_element();
    }
    let preferred = tabs.iter().find(|t| t["preferred"] == true).or(tabs.first()).and_then(|t| t["tabId"].as_str()).unwrap_or_default().to_string();
    // La pestaña elegida tiene que ser de este entorno; si no, la preferida de las suyas.
    if url.is_some() && !app.live_tab.as_ref().is_some_and(|id| tabs.iter().any(|t| t["tabId"] == id.as_str())) {
        app.live_tab = Some(preferred.clone());
        app.snapshot = None;
    }
    let current = app.live_tab.clone().unwrap_or(preferred);
    let choices: Vec<_> = tabs
        .iter()
        .map(|t| {
            let id = t["tabId"].as_str().unwrap_or_default().to_string();
            let visible = if t["visibility"] == "visible" { t!("visible") } else { t!("hidden") };
            crate::ely::choice_note(id.clone(), format!("{} · {}", ui::txt(&t["route"]), id), format!("{visible} · {}/min · {}", t["perMinute"].as_i64().unwrap_or(0), ui::txt(&t["origin"])))
        })
        .collect();
    let this = cx.entity().downgrade();
    let picker = Select::new("live-tab", choices).selected(current.clone()).on_change(move |v, _, cx| {
        let v = v.to_string();
        let _ = this.update(cx, |app, cx| {
            app.live_tab = Some(v);
            app.snapshot = None;
            cx.notify();
        });
    });
    let reset_args = json!({ "tabId": current });
    let toolbar = h_flex()
        .gap_2()
        .items_center()
        .child(ui::section(t!("Tab"), cx))
        .child(div().w(px(460.)).min_w(px(160.)).flex_shrink(1.).child(picker))
        .child(div().flex_1())
        .child(Button::new("reset").xsmall().outline().icon(IconName::RefreshCw).label(t!("Reset counters")).on_click(cx.listener(move |this, _, _, cx| {
            this.call("reset_counters", reset_args.clone(), |app, r| app.action = Some(r), cx);
        })));

    let body = match &app.snapshot {
        None | Some(Load::Loading) => ui::muted(t!("Reading the tab…"), cx).into_any_element(),
        Some(Load::Failed(e)) => ui::muted(e.clone(), cx).text_color(cx.theme().danger).into_any_element(),
        Some(Load::Ready(snap)) => snapshot_view(app, &snap.clone(), window, cx).into_any_element(),
    };
    v_flex().gap_3().p_4().child(toolbar).child(body).into_any_element()
}

fn snapshot_view(app: &mut DevtoolApp, snap: &Value, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let totals = &snap["convex"]["totals"];
    let session = &snap["session"];
    let n = |v: &Value| v.as_f64().map(|f| if f.fract() == 0.0 { format!("{f:.0}") } else { format!("{f:.1}") }).unwrap_or_else(|| "—".into());
    let errors_total = totals["errors"].as_i64().unwrap_or(0);
    let stats = h_flex()
        .gap_3()
        .flex_wrap()
        .items_stretch()
        .child(ui::stat_card(n(&totals["perMinute"]), t!("Per minute"), Some(t!("calls")), Ico::Activity, ui::Hue::Brand, cx))
        .child(ui::stat_card(n(&totals["projectedPerHour"]), t!("Per hour"), Some(t!("projected")), Ico::Clock, ui::Hue::Brand, cx))
        .child(ui::stat_card(n(&totals["activeSubscriptions"]), t!("Subscriptions"), Some(t!("active")), Ico::Wifi, ui::Hue::Green, cx))
        .child(ui::stat_card(n(&totals["calls"]), t!("Calls"), None, Ico::Zap, ui::Hue::Violet, cx))
        .child(ui::stat_card(n(&totals["reruns"]), t!("Reruns"), None, Ico::RotateCw, ui::Hue::Amber, cx))
        .child(ui::stat_card(errors_total.to_string(), t!("Errors"), None, Ico::CircleX, if errors_total > 0 { ui::Hue::Red } else { ui::Hue::Green }, cx));

    let info = ui::muted(
        t!("{a} · Convex {b} · version {c} · role {d} · updated {e}", a = ui::txt(&snap["page"]["route"]), b = ui::txt(&session["convexUrl"]), c = ui::txt(&session["version"]), d = ui::txt(&session["roleType"]), e = snap["receivedAt"].as_i64().map(ui::ago).unwrap_or_else(|| "—".into())),
        cx,
    );

    let num = |v: &Value| Cell::Number(v.as_f64().unwrap_or(0.0));
    let rows: Vec<Row> = snap["convex"]["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, f)| {
            Row::new(
                format!("f{i}"),
                [Cell::Text(ui::txt(&f["name"])), Cell::Text(ui::txt(&f["kind"])), num(&f["active"]), num(&f["runs"]), num(&f["reruns"]), num(&f["perMinute"]), num(&f["errors"])],
            )
        })
        .collect();
    let functions = DataTable::new("live-functions", [
        Column::new("name", t!("Function")),
        Column::new("kind", t!("Type")).width(rems(6.)),
        Column::new("active", t!("Active")).width(rems(5.)).end(),
        Column::new("runs", t!("Calls")).width(rems(6.)).end(),
        Column::new("reruns", "Re-ejec.").width(rems(5.)).end(),
        Column::new("pm", "/min").width(rems(5.)).end().decimals(1),
        Column::new("errors", t!("Errors")).width(rems(5.)).end(),
    ])
    .rows(rows)
    .paged(20);

    let error_rows: Vec<Row> = snap["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, e)| {
            let first = e["message"].as_str().unwrap_or_default().lines().next().unwrap_or_default().to_string();
            Row::new(format!("e{i}"), [Cell::Text(ui::txt(&e["source"])), Cell::Number(e["count"].as_f64().unwrap_or(1.0)), Cell::Text(first.into())])
        })
        .collect();
    let errors: AnyElement = if error_rows.is_empty() {
        ui::muted(t!("No errors."), cx).into_any_element()
    } else {
        DataTable::new("live-errors", [Column::new("source", t!("Source")).width(rems(7.)), Column::new("count", t!("Count")).width(rems(5.)).end(), Column::new("message", t!("Message"))])
            .rows(error_rows)
            .paged(15)
            .into_any_element()
    };

    let search = app.ely_input("live-json-search", t!("Search the snapshot…"), window, cx);
    let open = app.live_json_open;
    let t = crate::theme::Tokens::current(cx);
    v_flex()
        .gap_3()
        .child(stats)
        .child(info)
        .child(ui::card(cx).child(ui::section(t!("Convex functions"), cx)).child(functions))
        .child(ui::card(cx).child(ui::section(t!("Errors"), cx)).child(errors))
        .child(
            div()
                .id("live-json-toggle")
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .child(ely_core::primitives::Icon::new(if open { Ico::ChevronDown } else { Ico::ChevronRight }).size(ely_core::theme::IconSize::Xs).color(t.subtle))
                .child(ui::section(t!("Full snapshot (JSON)"), cx))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.live_json_open = !this.live_json_open;
                    cx.notify();
                })),
        )
        .child(Collapsible::new("live-json", open).child(ui::contained().h(px(420.)).child(JsonViewer::new("live-snapshot", serde_json::to_string(snap).unwrap_or_default(), &search))))
}
