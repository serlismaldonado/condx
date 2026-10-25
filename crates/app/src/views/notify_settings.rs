// Configuración → Notificaciones (notify.rs del daemon): avisos al teléfono con ntfy, sin cuentas de
// Apple ni de Google. La persona instala ntfy, se suscribe a su tema y elige qué avisa Condx. El
// token del servidor (si lo pide) va al Llavero; aquí solo se ve si hay uno.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

impl DevtoolApp {
    pub fn load_notify(&mut self, cx: &mut Context<Self>) {
        self.get("/api/notify".into(), |app, res| {
            if let Load::Ready(v) = res {
                app.notify_cfg = Some(v);
            }
        }, cx);
    }

    /// Guarda con `change` aplicado al borrador; lee los textos de los campos.
    fn save_notify(&mut self, change: impl FnOnce(&mut Value), cx: &mut Context<Self>) {
        let Some(mut s) = self.notify_cfg.as_ref().map(|c| c["settings"].clone()) else { return };
        let value = |app: &Self, key: &str, cx: &Context<Self>| app.inputs.get(key).map(|i| i.read(cx).value().trim().to_string());
        if let Some(v) = value(self, "ntfy-server", cx).filter(|v| !v.is_empty()) {
            s["server"] = json!(v);
        }
        if let Some(v) = value(self, "ntfy-topic", cx) {
            s["topic"] = json!(v);
        }
        if let Some(v) = value(self, "ntfy-token", cx).filter(|v| !v.is_empty()) {
            s["token"] = json!(v);
        }
        change(&mut s);
        self.post("/api/notify".into(), s, |app, res, cx| {
            match res {
                Load::Ready(_) => {
                    app.notify_result = None;
                    app.inputs.remove("ntfy-token");
                    app.load_notify(cx);
                }
                Load::Failed(e) => app.notify_result = Some(Err(e)),
                Load::Loading => {}
            }
        }, cx);
    }

    fn test_notify(&mut self, cx: &mut Context<Self>) {
        self.notify_result = None;
        self.post("/api/notify/test".into(), Value::Null, |app, res, _| {
            app.notify_result = Some(match res {
                Load::Ready(_) => Ok(t!("Sent. It should be on your phone now.").to_string()),
                Load::Failed(e) => Err(e),
                Load::Loading => return,
            });
        }, cx);
    }
}

pub fn card(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    let card = ui::card(cx).child(ui::section(t!("Notifications on your phone"), cx));
    let Some(cfg) = app.notify_cfg.clone() else {
        if app.daemon.is_some() {
            app.load_notify(cx);
        }
        return card.child(ui::muted(t!("Loading…"), cx));
    };
    let s = cfg["settings"].clone();
    let on = s["enabled"] == true;
    let input = |app: &mut DevtoolApp, key: &str, value: String, placeholder: String, masked: bool, window: &mut Window, cx: &mut Context<DevtoolApp>| {
        app.inputs.entry(key.to_string()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(placeholder).default_value(value))).clone()
    };
    let topic_now = s["topic"].as_str().filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| cfg["suggestedTopic"].as_str().unwrap_or("").to_string());
    let server = input(app, "ntfy-server", s["server"].as_str().unwrap_or("https://ntfy.sh").to_string(), "https://ntfy.sh".into(), false, window, cx);
    let topic = input(app, "ntfy-topic", topic_now, t!("Topic").to_string(), false, window, cx);
    let token = input(app, "ntfy-token", String::new(), if cfg["hasToken"] == true { t!("Token saved (type to replace it)").to_string() } else { t!("Optional: the server's access token").to_string() }, true, window, cx);
    let mut kinds = h_flex().gap_1().flex_wrap();
    for (field, label) in [("incidents", t!("Incidents")), ("agents", t!("Agents done")), ("approvals", t!("Approvals"))] {
        let selected = s[field] == true;
        let b = Button::new(SharedString::from(format!("ntfy-{field}"))).xsmall().label(label).on_click(cx.listener(move |this, _, _, cx| this.save_notify(move |v| v[field] = json!(!selected), cx)));
        kinds = kinds.child(ui::brand_if(selected, b, cx));
    }
    card.child(ui::muted(t!("Install ntfy (free, iOS and Android), subscribe to this topic, and Condx tells you when something breaks, when an agent finishes and when something waits for your approval. Messages are short and carry no logs; on ntfy.sh anyone who knows the topic can read it, so keep it random (or use your own server)."), cx).text_xs())
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::toggle("ntfy-on", on, cx).on_click(cx.listener(move |this, _, _, cx| this.save_notify(move |v| v["enabled"] = json!(!on), cx))))
                .child(ui::muted(if on { t!("On") } else { t!("Off") }, cx)),
        )
        .child(ui::kv(t!("Server"), Input::new(&server).small(), cx))
        .child(ui::kv(
            t!("Topic"),
            h_flex().gap_1().child(div().flex_1().child(Input::new(&topic).small())).child(Button::new("ntfy-random").xsmall().ghost().label(t!("Random")).on_click(cx.listener(|this, _, _, cx| {
                this.inputs.remove("ntfy-topic");
                let random = this.notify_cfg.as_ref().and_then(|c| c["suggestedTopic"].as_str()).unwrap_or("").to_string();
                this.save_notify(move |v| v["topic"] = json!(random), cx);
            }))),
            cx,
        ))
        .child(ui::kv(t!("Token"), Input::new(&token).small(), cx))
        .child(ui::kv(t!("Tell me about"), kinds, cx))
        .child(
            h_flex()
                .gap_1()
                .child(ui::brand(Button::new("ntfy-save").small().icon(IconName::Check).label(t!("Save")), cx).on_click(cx.listener(|this, _, _, cx| this.save_notify(|_| {}, cx))))
                .child(Button::new("ntfy-test").small().outline().label(t!("Send a test")).on_click(cx.listener(|this, _, _, cx| this.test_notify(cx)))),
        )
        .when_some(app.notify_result.clone(), |c, r| {
            c.child(match r {
                Ok(m) => ui::status_tag(Health::Ok, m),
                Err(e) => ui::status_tag(Health::Bad, e),
            })
        })
}
