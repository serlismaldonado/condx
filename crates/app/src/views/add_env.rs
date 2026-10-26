// Agregar un entorno cloud a un proyecto (o guardar la key de uno que ya existe) pegando su deploy
// key de Convex: la key dice si es producción (`prod:`) o cloud dev (`dev:`) y de qué deployment,
// así que no hay más que elegir. La key va al Llavero por el daemon y no vuelve a mostrarse. Sirve
// para todos los proyectos (la pestaña Claves de un entorno solo existe en heskala-erp).

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, Disableable, Sizable};
use gpui_kit::*;
use serde_json::json;

use crate::app::DevtoolApp;
use crate::client::Daemon;
use crate::ui;

pub struct AddEnvView {
    project: String,
    daemon: Option<Daemon>,
    app: WeakEntity<DevtoolApp>,
    input: Entity<InputState>,
    saving: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl AddEnvView {
    pub fn new(project: String, daemon: Option<Daemon>, app: WeakEntity<DevtoolApp>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("prod:<deployment>|…  ·  dev:<deployment>|…"));
        let subscriptions = vec![cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify())];
        Self { project, daemon, app, input, saving: false, error: None, _subscriptions: subscriptions }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        let key = self.input.read(cx).value().trim().to_string();
        self.saving = true;
        self.error = None;
        cx.notify();
        let path = format!("/api/projects/{}/environments", self.project);
        cx.spawn_in(window, async move |this, cx| {
            let res = cx.background_executor().spawn(async move { daemon.post(&path, json!({ "deployKey": key })) }).await;
            this.update_in(cx, |view, window, cx| {
                view.saving = false;
                match res {
                    Ok(v) => {
                        view.input.update(cx, |input, cx| input.set_value("", window, cx));
                        let name = v["name"].as_str().unwrap_or_default().to_string();
                        view.app.update(cx, |app, cx| app.reload_overview(cx)).ok();
                        use gpui_kit::component::notification::Notification;
                        use gpui_kit::component::WindowExt;
                        window.push_notification(Notification::success(t!("{a} is ready: deploy key saved in the Keychain", a = name)), cx);
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

impl Render for AddEnvView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let typed = !self.input.read(cx).value().trim().is_empty();
        let save = Button::new("add-env-save")
            .small()
            .label(t!("Add"))
            .loading(self.saving)
            .disabled(!typed || self.saving)
            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)));
        let mut card = ui::card(cx)
            .child(ui::section(t!("Add a cloud environment"), cx))
            .child(ui::muted(t!("Paste its Convex deploy key (dashboard → Settings → Deploy keys): a prod: key adds production, a dev: key adds cloud dev or saves the key of the one it already has."), cx))
            .child(h_flex().gap_2().items_center().child(div().flex_1().min_w_0().child(Input::new(&self.input).small().mask_toggle())).child(ui::brand(save, cx)))
            .child(ui::muted(t!("It's stored in the macOS Keychain and never shown again. Agents through the MCP can't read, save or delete it."), cx).text_xs());
        if let Some(e) = &self.error {
            card = card.child(ui::muted(e.clone(), cx).text_xs());
        }
        card
    }
}
