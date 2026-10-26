// Configuración → Condx SDK (docs/design/agent-flows.md, §2.8): las llaves con las que una app de
// Convex (cualquiera, no solo esta) manda a Condx Cloud los errores de su frontend y su backend y
// los reportes de sus usuarios, y cómo instalar el SDK. La secreta se ve una sola vez.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::json;

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

impl DevtoolApp {
    pub fn load_sdk(&mut self, cx: &mut Context<Self>) {
        self.get("/api/sdk".into(), |app, res| app.sdk = Some(res), cx);
    }

    fn create_sdk_key(&mut self, cx: &mut Context<Self>) {
        let project = self.inputs.get("sdk-project").map(|s| s.read(cx).value().trim().to_string()).filter(|p| !p.is_empty()).unwrap_or_else(|| self.project.clone());
        self.sdk_error = None;
        self.post("/api/sdk/keys".into(), json!({ "project": project }), |app, res, cx| match res {
            Load::Ready(v) => {
                app.sdk_new = Some(v);
                app.load_sdk(cx);
            }
            Load::Failed(e) => app.sdk_error = Some(e),
            Load::Loading => {}
        }, cx);
    }

    fn revoke_sdk_key(&mut self, key: String, cx: &mut Context<Self>) {
        self.delete(format!("/api/sdk/keys/{key}"), |app, res, cx| match res {
            Load::Failed(e) => app.sdk_error = Some(e),
            _ => app.load_sdk(cx),
        }, cx);
    }
}

fn snippet(id: &str, label: &'static str, text: String, cx: &mut Context<DevtoolApp>) -> Div {
    v_flex()
        .gap_1()
        .child(h_flex().items_center().child(div().flex_1().child(ui::muted(label, cx).text_xs())).child(Clipboard::new(SharedString::from(format!("sdk-copy-{id}"))).value(text.clone()).tooltip(t!("Copy"))))
        .child(ui::contained().px_2().py_1().overflow_hidden().child(ui::mono(text).text_xs().whitespace_normal()))
}

pub fn card(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    let card = ui::card(cx).child(ui::section(t!("Condx SDK"), cx)).child(
        ui::muted(t!("Add it to any Convex app: the frontend reports its errors (uncaught, render, failed Convex calls) and its users can report a problem; the backend component reports its actions' errors. Everything goes to your Condx Cloud and starts the flows you set up (Workflows)."), cx).text_xs(),
    );
    let data = match app.sdk.clone() {
        Some(Load::Ready(v)) => v,
        Some(Load::Failed(e)) => return card.child(ui::status_tag(Health::Warn, e)),
        _ => {
            if app.daemon.is_some() {
                app.load_sdk(cx);
            }
            return card.child(ui::muted(t!("Loading…"), cx));
        }
    };
    let endpoint = data["endpoint"].as_str().unwrap_or_default().to_string();
    let mut col = v_flex().gap_1p5().child(ui::kv(t!("Endpoint"), ui::mono(endpoint.clone()), cx));
    let keys = data["keys"].as_array().cloned().unwrap_or_default();
    if keys.is_empty() {
        col = col.child(ui::muted(t!("No keys yet."), cx).text_xs());
    }
    for k in keys {
        let key = k["publicKey"].as_str().unwrap_or_default().to_string();
        let revoked = k["revoked"] == true;
        let short = format!("{}…{}", &key[..key.len().min(14)], &key[key.len().saturating_sub(4)..]);
        let r_key = key.clone();
        col = col.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().w(px(110.)).text_size(px(12.5)).child(ui::txt(&k["project"]).to_string()))
                .child(div().flex_1().min_w_0().child(ui::mono(short).text_xs().truncate()))
                .child(ui::muted(k["createdAt"].as_i64().map(ui::ago).unwrap_or_default(), cx).text_xs())
                .child(if revoked {
                    ui::muted(t!("revoked"), cx).text_xs().into_any_element()
                } else {
                    Button::new(SharedString::from(format!("sdk-revoke-{key}"))).xsmall().ghost().label(t!("Revoke")).on_click(cx.listener(move |this, _, _, cx| this.revoke_sdk_key(r_key.clone(), cx))).into_any_element()
                }),
        );
    }
    let project = app.project.clone();
    let input = app.inputs.entry("sdk-project".into()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(project.clone()).default_value(project.clone()))).clone();
    col = col.child(
        h_flex()
            .gap_2()
            .items_center()
            .pt_1()
            .child(div().w(px(200.)).child(Input::new(&input).small()))
            .child(ui::brand(Button::new("sdk-create").small().icon(IconName::Plus).label(t!("New key")), cx).on_click(cx.listener(|this, _, _, cx| this.create_sdk_key(cx)))),
    );
    // La llave recién creada: la secreta solo se ve ahora, con lo que hay que pegar en la app.
    if let Some(new) = app.sdk_new.clone() {
        let public = ui::txt(&new["publicKey"]).to_string();
        let secret = ui::txt(&new["secret"]).to_string();
        col = col.child(
            v_flex()
                .gap_2()
                .mt_1()
                .p_2()
                .border_1()
                .border_color(crate::theme::Tokens::current(cx).accent_ink)
                .child(ui::status_tag(Health::Warn, t!("Copy the secret now: it isn't shown again.").to_string()))
                .child(snippet("install", t!("1. Install"), "npm install @condx/react @condx/convex".to_string(), cx))
                .child(snippet(
                    "react",
                    t!("2. Frontend: around your app, next to ConvexProvider"),
                    format!("import {{ CondxProvider, ReportProblem }} from \"@condx/react\";\n\n<CondxProvider ingestKey=\"{public}\" endpoint=\"{endpoint}\" convex={{convex}} app=\"{}\">\n  <App />\n  <ReportProblem />\n</CondxProvider>", ui::txt(&new["project"])),
                    cx,
                ))
                .child(snippet(
                    "convex",
                    t!("3. Backend: the component (convex/convex.config.ts) and a Condx instance"),
                    "import condx from \"@condx/convex/convex.config\";\napp.use(condx);\n\n// convex/condx.ts\nimport { Condx } from \"@condx/convex\";\nimport { components } from \"./_generated/api\";\nexport const condx = new Condx(components.condx);\n// action({ handler: condx.wrap(\"billing:sync\", async (ctx) => { … }) })".to_string(),
                    cx,
                ))
                .child(snippet("env", t!("4. The deployment's environment (the secret stays on the server)"), format!("npx convex env set CONDX_ENDPOINT {endpoint}\nnpx convex env set CONDX_SECRET {secret}"), cx))
                .child(h_flex().child(Button::new("sdk-done").xsmall().ghost().label(t!("Done, I copied it")).on_click(cx.listener(|this, _, _, cx| {
                    this.sdk_new = None;
                    cx.notify();
                })))),
        );
    }
    card.child(col).when_some(app.sdk_error.clone(), |c, e| c.child(ui::status_tag(Health::Bad, e)))
}
