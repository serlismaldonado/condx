// Tarjeta «Condx Cloud» de Configuración (C3): vincular este daemon a un deployment de Convex
// propio, ver qué se sincroniza y desvincular. La admin o deploy key se usa una vez para
// registrar el daemon y no se guarda; el campo la oculta y se vacía al terminar.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, IconName, Sizable};
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui;

pub fn load(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) {
    app.get("/api/cloud".into(), |app, res| app.cloud = Some(res), cx);
}

fn input(app: &mut DevtoolApp, key: &str, placeholder: &str, masked: bool, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Entity<InputState> {
    app.inputs.entry(key.into()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(placeholder.to_string()))).clone()
}

pub fn card(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    if app.cloud.is_none() {
        load(app, cx);
    }
    let head = h_flex()
        .gap_2()
        .items_center()
        .child(ui::section(t!("Condx Cloud"), cx))
        .child(div().flex_1())
        .child(Button::new("cloud-refresh").xsmall().ghost().icon(IconName::RefreshCw).on_click(cx.listener(|this, _, _, cx| load(this, cx))));
    let card = ui::card(cx).child(head).child(ui::muted(
        t!("Optional. Publishes this daemon's environments, jobs and approvals to a Convex deployment of your own, to see them from anywhere. Keys, tokens, job arguments and variable values never leave this Mac."),
        cx,
    ));
    let v = match app.cloud.clone() {
        Some(Load::Ready(v)) => v,
        Some(Load::Failed(e)) => return card.child(ui::muted(e, cx).text_color(cx.theme().danger)),
        _ => return card.child(ui::muted(t!("Loading…"), cx)),
    };
    match v["link"].as_object() {
        Some(link) => card.child(linked(&Value::Object(link.clone()), &v["status"], cx)),
        None => card.child(link_form(app, window, cx)),
    }
}

fn linked(link: &Value, status: &Value, cx: &mut Context<DevtoolApp>) -> Div {
    let data = &status["data"];
    let health = match (status["ok"].as_bool(), data["pending"].as_u64()) {
        (Some(true), Some(0)) => (ui::Health::Ok, t!("In sync").to_string()),
        (Some(true), Some(n)) => (ui::Health::Warn, t!("{n} changes to send", n = n).to_string()),
        (Some(false), _) => (ui::Health::Bad, status["error"].as_str().unwrap_or("error").chars().take(120).collect()),
        _ => (ui::Health::Unknown, t!("Starting…").to_string()),
    };
    let mut rows = v_flex()
        .gap_1()
        .child(ui::kv(t!("Status"), ui::status_tag(health.0, health.1), cx))
        .child(ui::kv(t!("Deployment"), ui::mono(ui::txt(&link["url"])), cx))
        .child(ui::kv(t!("This daemon"), format!("{} · {}", ui::txt(&link["name"]), ui::txt(&link["daemonId"])), cx));
    for s in data["scopes"].as_array().into_iter().flatten() {
        let scope = s["scope"].as_str().unwrap_or_default();
        let kind = scope.split(':').next().unwrap_or(scope);
        rows = rows.child(ui::kv(kind.to_string(), t!("{n} rows", n = s["rows"].as_u64().unwrap_or(0)), cx));
    }
    v_flex().gap_3().child(rows).child(
        h_flex().child(
            Button::new("cloud-unlink")
                .small()
                .outline()
                .label(t!("Unlink"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.post("/api/cloud/unlink".into(), json!({}), |app, res, cx| {
                        if let Load::Failed(e) = res {
                            app.action = Some(Load::Failed(e));
                        }
                        load(app, cx);
                    }, cx);
                })),
        ),
    )
}

fn link_form(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    let url = input(app, "cloud-url", "https://happy-otter-123.convex.cloud", false, window, cx);
    let key = input(app, "cloud-key", "prod:happy-otter-123|… (admin or deploy key)", true, window, cx);
    let name = input(app, "cloud-name", "My Mac", false, window, cx);
    let busy = matches!(app.cloud_linking, Some(Load::Loading));
    let (u, k, n) = (url.clone(), key.clone(), name.clone());
    v_flex()
        .gap_2()
        .child(ui::muted(t!("Deploy the Condx Cloud backend (cloud/ in the Condx repo) to a Convex project of yours, then paste its URL and a deploy key. The key is used once to register this daemon and isn't stored."), cx))
        .child(Input::new(&url).small())
        .child(Input::new(&key).small())
        .child(Input::new(&name).small())
        .child(h_flex().child(ui::brand(
            Button::new("cloud-link").small().label(if busy { t!("Linking…") } else { t!("Link") }).loading(busy).disabled(busy).on_click(cx.listener(move |this, _, _, cx| {
                let body = json!({ "url": u.read(cx).value().trim(), "key": k.read(cx).value().trim(), "name": n.read(cx).value().trim() });
                this.cloud_linking = Some(Load::Loading);
                this.post("/api/cloud/link".into(), body, |app, res, cx| {
                    // La key no queda en ningún campo, salga bien o mal.
                    app.inputs.remove("cloud-key");
                    if let Load::Failed(e) = &res {
                        app.action = Some(Load::Failed(e.clone()));
                    }
                    app.cloud_linking = Some(res);
                    load(app, cx);
                }, cx);
            })),
            cx,
        )))
}
