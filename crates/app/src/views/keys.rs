// Pestaña Claves de un entorno: su deploy key de Convex, guardada en el Llavero de macOS por el
// daemon. Con ella la pestaña Datos lee tamaños y páginas en cloud, y producción puede
// desplegar. La key se escribe una vez y nunca vuelve a mostrarse; solo la app puede guardarla
// o borrarla (un agente por el MCP no). Los entornos locales usan la admin key de su .env.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex, Disableable, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::client::Daemon;
use crate::ui;

pub struct KeysView {
    env: String,
    kind: String,
    daemon: Option<Daemon>,
    app: WeakEntity<DevtoolApp>,
    /// Lo que dice el daemon de las claves del entorno (nunca su valor).
    info: Load,
    input: Entity<InputState>,
    saving: bool,
    /// El último error al guardar o borrar.
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl KeysView {
    pub fn new(env: String, kind: String, daemon: Option<Daemon>, app: WeakEntity<DevtoolApp>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prefix = if kind == "cloud-prod" { "prod:" } else { "dev:" };
        let input = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(format!("{prefix}<deployment>|…")));
        // El campo es una entidad propia: escribir en él no notifica a esta vista.
        let subscriptions = vec![cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify())];
        let mut view = Self { env, kind, daemon, app, info: Load::Loading, input, saving: false, error: None, _subscriptions: subscriptions };
        view.load(cx);
        view
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        let path = format!("/api/environments/{}/keys", self.env);
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { daemon.get(&path) }).await;
            this.update(cx, |view, cx| {
                view.info = res.into();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Guarda (`Some`) o borra (`None`) la deploy key; después vuelve a leer y rehace la pestaña
    /// Datos del entorno, que lee distinto con o sin key.
    fn write(&mut self, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        self.saving = true;
        self.error = None;
        cx.notify();
        let path = format!("/api/environments/{}/keys", self.env);
        let saved = key.is_some();
        cx.spawn_in(window, async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    match key {
                        Some(k) => daemon.post(&path, json!({ "deployKey": k })),
                        None => daemon.delete(&path),
                    }
                })
                .await;
            this.update_in(cx, |view, window, cx| {
                view.saving = false;
                match res {
                    Ok(_) => {
                        view.input.update(cx, |input, cx| input.set_value("", window, cx));
                        let env = view.env.clone();
                        view.app.update(cx, |app, _| {
                            app.data_views.remove(&env);
                            app.run_fn.approvals.clear();
                        })
                        .ok();
                        use gpui_kit::component::notification::Notification;
                        use gpui_kit::component::WindowExt;
                        window.push_notification(Notification::success(if saved { t!("Deploy key saved in the Keychain") } else { t!("Deploy key deleted") }), cx);
                        view.load(cx);
                    }
                    Err(e) => view.error = Some(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for KeysView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = v_flex().gap_3().px_4().pt_3().max_w(rems(45.));
        if matches!(self.kind.as_str(), "devbox" | "throwaway") {
            return body.child(
                ui::card(cx)
                    .child(ui::section(t!("Keys"), cx))
                    .child(ui::muted(t!("Local environments use the admin key from their .env file: there's nothing to save here."), cx)),
            );
        }
        let prod = self.kind == "cloud-prod";
        let what = if prod {
            t!("Lets Condx deploy to production and, if the rules in Settings allow it, read its records in Data.")
        } else {
            t!("Lets the Data tab show how many records each table has and page through them.")
        };
        let mut card = ui::card(cx)
            .child(ui::section(t!("Deploy key"), cx))
            .child(ui::muted(what, cx))
            .child(ui::muted(t!("It's stored in the macOS Keychain and never shown again. Agents through the MCP can't read, save or delete it."), cx).text_xs());

        let info: &Value = match &self.info {
            Load::Loading => return body.child(card.child(ui::muted(t!("Loading…"), cx))),
            Load::Failed(e) => return body.child(card.child(ui::muted(e.clone(), cx))),
            Load::Ready(v) => v,
        };
        let key = &info["deployKey"];
        let stored = key["stored"] == true;
        let shared = key["shared"] == true;
        let status = if stored {
            let deployment = key["deployment"].as_str().unwrap_or_default();
            if shared {
                t!("Saved · {a} (general key from condxd secret)", a = deployment)
            } else {
                t!("Saved · {a}", a = deployment)
            }
        } else {
            t!("Not saved").to_string()
        };
        card = card.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::dot(if stored && key["valid"] != false { ui::Health::Ok } else if stored { ui::Health::Bad } else { ui::Health::Unknown }, cx))
                .child(div().text_sm().child(status)),
        );
        if let Some(problem) = key["problem"].as_str() {
            card = card.child(ui::muted(problem.to_string(), cx).text_xs());
        }

        let typed = !self.input.read(cx).value().trim().is_empty();
        let save = Button::new("keys-save")
            .small()
            .label(if stored { t!("Replace") } else { t!("Save") })
            .loading(self.saving)
            .disabled(!typed || self.saving)
            .on_click(cx.listener(|this, _, window, cx| {
                let key = this.input.read(cx).value().trim().to_string();
                this.write(Some(key), window, cx);
            }));
        card = card.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().min_w_0().child(Input::new(&self.input).small().mask_toggle()))
                .child(ui::brand(save, cx)),
        );
        if let Some(e) = &self.error {
            card = card.child(ui::muted(e.clone(), cx).text_xs());
        }
        let deployment = info["deployment"].as_str().map(str::to_string);
        card = card.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::muted(t!("Create it in the Convex dashboard: Settings → Deploy keys."), cx).text_xs())
                .child(div().flex_1())
                .when_some(deployment, |h, d| {
                    let url = format!("https://dashboard.convex.dev/d/{d}/settings");
                    h.child(Button::new("keys-dashboard").xsmall().ghost().icon(IconName::ExternalLink).label(t!("Open settings")).on_click(move |_, _, cx| cx.open_url(&url)))
                })
                .when(stored && !shared, |h| {
                    h.child(Button::new("keys-delete").xsmall().ghost().icon(IconName::Delete).label(t!("Delete")).disabled(self.saving).on_click(cx.listener(|this, _, window, cx| this.write(None, window, cx))))
                }),
        );
        body.child(card)
    }
}
