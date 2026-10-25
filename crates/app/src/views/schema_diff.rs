// Comparar esquemas antes de desplegar: lo del backend local del proyecto (el checkout) contra lo
// desplegado en un cloud (GET /api/schema-diff, con su deploy key). Muestra tablas y campos que
// entran, salen o cambian, índices, y los riesgos que harían fallar el despliegue.

use ely_core::primitives::{Icon, IconName as IconName2};
use ely_core::theme::IconSize;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable, WindowExt};
use gpui_kit::*;
use serde_json::Value;

use crate::app::{DevtoolApp, Load};
use crate::ui;

/// El otro lado de la comparación: desde un cloud, el local del proyecto; desde el local, el
/// primer cloud dev. Siempre `(desde, hacia)` con el local como origen.
pub fn pair(app: &DevtoolApp, env: &str) -> Option<(String, String)> {
    let envs: Vec<Value> = app.overview.as_ref().and_then(|o| o["environments"].as_array().cloned()).unwrap_or_default();
    let kind = |name: &str| envs.iter().find(|e| e["name"] == name).and_then(|e| e["kind"].as_str().map(str::to_string)).unwrap_or_default();
    let first = |k: &str| envs.iter().find(|e| e["kind"] == k).and_then(|e| e["name"].as_str().map(str::to_string));
    match kind(env).as_str() {
        "devbox" => first("cloud-dev").or_else(|| first("cloud-prod")).map(|to| (env.to_string(), to)),
        "cloud-dev" | "cloud-prod" => first("devbox").map(|from| (from, env.to_string())),
        _ => None,
    }
}

pub fn open(app: &mut DevtoolApp, from: String, to: String, window: &mut Window, cx: &mut Context<DevtoolApp>) {
    app.schema_diff = Some(Load::Loading);
    app.get(format!("/api/schema-diff?from={from}&to={to}"), |app, res| app.schema_diff = Some(res), cx);
    let me = cx.entity();
    window.open_sheet(cx, move |sheet, _, cx| {
        let body = match me.read(cx).schema_diff.clone() {
            Some(Load::Ready(v)) => result(&v, cx),
            Some(Load::Failed(e)) => ui::muted(e, cx).into_any_element(),
            _ => h_flex().gap_2().child(Spinner::new().small()).child(ui::muted(t!("Reading both schemas…"), cx)).into_any_element(),
        };
        let (f, t) = (from.clone(), to.clone());
        let again = me.clone();
        sheet
            .title(t!("Schema: {a} → {b}", a = from, b = to))
            .size(px(560.))
            .child(
                v_flex()
                    .gap_3()
                    .child(ui::muted(t!("What deploying the local schema to {a} would change. Read with its deploy key; nothing is deployed.", a = to), cx))
                    .child(body)
                    .child(Button::new("schema-diff-again").xsmall().ghost().icon(IconName::RefreshCw).label(t!("Compare again")).on_click(move |_, window, cx| {
                        window.close_sheet(cx);
                        let (f, t) = (f.clone(), t.clone());
                        again.update(cx, |app, cx| open(app, f, t, window, cx));
                    })),
            )
    });
}

fn names(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string).or_else(|| x["name"].as_str().map(str::to_string))).collect()
}

fn line(sign: &str, text: String, color: Hsla, cx: &App) -> Div {
    let t = crate::theme::Tokens::current(cx);
    h_flex().gap_2().child(div().w(px(12.)).text_color(color).child(sign.to_string())).child(div().text_color(t.fg).child(ui::mono(text)))
}

fn result(v: &Value, cx: &App) -> AnyElement {
    let t = crate::theme::Tokens::current(cx);
    let (green, red, amber): (Hsla, Hsla, Hsla) = (rgb(0x10b981).into(), rgb(0xef4444).into(), rgb(0xf59e0b).into());
    if v["same"] == true && names(&v["envVars"]["missing"]).is_empty() {
        return h_flex().gap_2().child(Icon::new(IconName2::CircleCheck).size(IconSize::Sm).color(green)).child(div().text_color(t.fg).child(t!("Same schema: nothing to change."))).into_any_element();
    }
    let risks = names(&v["risks"]);
    let mut col = v_flex().gap_3();
    col = col.child(if risks.is_empty() {
        h_flex().gap_2().child(Icon::new(IconName2::CircleCheck).size(IconSize::Sm).color(green)).child(div().text_color(t.fg).child(t!("Safe to deploy: no risky changes.")))
    } else {
        h_flex().gap_2().child(Icon::new(IconName2::TriangleAlert).size(IconSize::Sm).color(amber)).child(div().text_color(t.fg).child(t!("{n} risky changes: review them before deploying.", n = risks.len())))
    });
    if !risks.is_empty() {
        let mut list = v_flex().gap_1().p_2().border_1().border_color(amber.opacity(0.4)).bg(amber.opacity(0.06));
        for r in risks {
            list = list.child(h_flex().gap_2().items_start().child(div().text_color(amber).child("!")).child(div().text_size(px(12.)).text_color(t.fg).child(r)));
        }
        col = col.child(list);
    }
    // Variables de entorno (solo nombres): lo que el código lee en local y allá no existe.
    let vars = &v["envVars"];
    let missing = names(&vars["missing"]);
    if let Some(e) = vars["error"].as_str() {
        col = col.child(ui::muted(t!("Couldn't compare environment variables: {a}", a = e), cx));
    } else if !missing.is_empty() {
        let mut list = v_flex().gap_0p5().child(ui::section(t!("Environment variables missing in {a}", a = ui::txt(&v["to"])), cx));
        for n in missing {
            list = list.child(line("!", n, amber, cx));
        }
        col = col.child(list);
    }
    let added = names(&v["tablesAdded"]);
    let removed = names(&v["tablesRemoved"]);
    if !added.is_empty() || !removed.is_empty() {
        let mut tables = v_flex().gap_0p5().child(ui::section(t!("Tables"), cx));
        for n in added {
            tables = tables.child(line("+", n, green, cx));
        }
        for n in removed {
            tables = tables.child(line("−", n, red, cx));
        }
        col = col.child(tables);
    }
    for c in v["changed"].as_array().into_iter().flatten() {
        let table = c["table"].as_str().unwrap_or_default();
        let mut card = v_flex().gap_0p5().child(ui::section(table.to_string(), cx));
        for f in c["fieldsAdded"].as_array().into_iter().flatten() {
            let opt = if f["optional"] == true { "?" } else { "" };
            card = card.child(line("+", format!("{}{opt}: {}", ui::txt(&f["name"]), ui::txt(&f["type"])), green, cx));
        }
        for f in c["fieldsRemoved"].as_array().into_iter().flatten() {
            card = card.child(line("−", format!("{}: {}", ui::txt(&f["name"]), ui::txt(&f["type"])), red, cx));
        }
        for f in c["fieldsChanged"].as_array().into_iter().flatten() {
            let side = |s: &Value| format!("{}{}", ui::txt(&s["type"]), if s["optional"] == true { "?" } else { "" });
            card = card.child(line("~", format!("{}: {} → {}", ui::txt(&f["name"]), side(&f["from"]), side(&f["to"])), amber, cx));
        }
        for i in names(&c["indexesAdded"]) {
            card = card.child(line("+", t!("index {a}", a = i), green, cx));
        }
        for i in names(&c["indexesRemoved"]) {
            card = card.child(line("−", t!("index {a}", a = i), red, cx));
        }
        col = col.child(card);
    }
    col.into_any_element()
}
