// Configuración → Agentes (agents.rs del daemon): los proveedores de modelos con su API key, que
// se manda una vez y el daemon guarda cifrada (aquí solo vuelve una pista, `…abcd`); qué hacer con
// cada tipo de incidente (nada, avisar, diagnosticar o preparar un fix); el proveedor y el modelo por
// defecto y el presupuesto. Solo esta app lo cambia.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

const LEVELS: [&str; 4] = ["off", "notify", "diagnose", "fix"];

impl DevtoolApp {
    pub fn load_agents(&mut self, cx: &mut Context<Self>) {
        self.get("/api/agents".into(), |app, res| match res {
            Load::Ready(v) => app.agents = Some(v),
            Load::Failed(e) => app.agents_error = Some(e),
            Load::Loading => {}
        }, cx);
    }

    /// Cambia la configuración guardada con `change` y la manda entera.
    fn save_agent_settings(&mut self, change: impl FnOnce(&mut Value), cx: &mut Context<Self>) {
        let Some(mut settings) = self.agents.as_ref().map(|a| a["settings"].clone()) else { return };
        change(&mut settings);
        self.post("/api/agents/settings".into(), settings, |app, res, cx| match res {
            Load::Ready(_) => {
                app.agents_error = None;
                app.load_agents(cx);
            }
            Load::Failed(e) => app.agents_error = Some(e),
            Load::Loading => {}
        }, cx);
    }

    fn save_provider(&mut self, cx: &mut Context<Self>) {
        let value = |app: &Self, key: &str, cx: &Context<Self>| app.inputs.get(key).map(|s| s.read(cx).value().trim().to_string()).filter(|v| !v.is_empty());
        let body = json!({
            "kind": self.agent_kind,
            "label": value(self, "agent-label", cx),
            "baseUrl": value(self, "agent-url", cx),
            "model": value(self, "agent-model", cx),
            "apiKey": value(self, "agent-key", cx),
        });
        self.post("/api/agents/providers".into(), body, |app, res, cx| match res {
            Load::Ready(_) => {
                app.agents_error = None;
                // Campos nuevos (vacíos) la próxima vez: la clave no se queda en la pantalla.
                for k in ["agent-label", "agent-url", "agent-model", "agent-key"] {
                    app.inputs.remove(k);
                }
                app.load_agents(cx);
            }
            Load::Failed(e) => app.agents_error = Some(e),
            Load::Loading => {}
        }, cx);
    }

    /// El daemon lee la configuración de pi (~/.pi/agent) y guarda sus claves cifradas.
    fn import_from_pi(&mut self, cx: &mut Context<Self>) {
        self.agents_import = Some(Load::Loading);
        self.post("/api/agents/import-pi".into(), Value::Null, |app, res, cx| {
            app.agents_import = Some(res);
            app.load_agents(cx);
        }, cx);
    }

    fn edit_template(&mut self, template: Value, cx: &mut Context<Self>) {
        for k in ["tpl-name", "tpl-match", "tpl-model", "tpl-instructions"] {
            self.inputs.remove(k);
        }
        self.template_edit = Some(template);
        cx.notify();
    }

    fn save_template(&mut self, cx: &mut Context<Self>) {
        let Some(mut draft) = self.template_edit.clone() else { return };
        let value = |app: &Self, key: &str, cx: &Context<Self>| app.inputs.get(key).map(|s| s.read(cx).value().trim().to_string());
        if let Some(v) = value(self, "tpl-name", cx) {
            draft["name"] = json!(v);
        }
        draft["match"] = json!(value(self, "tpl-match", cx).filter(|v| !v.is_empty()));
        draft["model"] = json!(value(self, "tpl-model", cx).filter(|v| !v.is_empty()));
        if let Some(v) = value(self, "tpl-instructions", cx) {
            draft["instructions"] = json!(v);
        }
        self.post("/api/agents/templates".into(), draft, |app, res, cx| match res {
            Load::Ready(_) => {
                app.template_edit = None;
                app.agents_error = None;
                app.load_agents(cx);
            }
            Load::Failed(e) => app.agents_error = Some(e),
            Load::Loading => {}
        }, cx);
    }

    /// Enciende o apaga una plantilla (la guarda entera con `enabled` cambiado).
    fn toggle_template(&mut self, mut template: Value, cx: &mut Context<Self>) {
        template["enabled"] = json!(template["enabled"] != true);
        self.post("/api/agents/templates".into(), template, |app, res, cx| match res {
            Load::Failed(e) => app.agents_error = Some(e),
            _ => app.load_agents(cx),
        }, cx);
    }

    fn delete_template(&mut self, id: String, cx: &mut Context<Self>) {
        self.delete(format!("/api/agents/templates/{id}"), |app, res, cx| match res {
            Load::Failed(e) => app.agents_error = Some(e),
            _ => app.load_agents(cx),
        }, cx);
    }

    fn reset_templates(&mut self, cx: &mut Context<Self>) {
        self.template_edit = None;
        self.post("/api/agents/templates/reset".into(), Value::Null, |app, res, cx| match res {
            Load::Failed(e) => app.agents_error = Some(e),
            _ => app.load_agents(cx),
        }, cx);
    }

    fn delete_provider(&mut self, id: String, cx: &mut Context<Self>) {
        self.delete(format!("/api/agents/providers/{id}"), |app, res, cx| match res {
            Load::Failed(e) => app.agents_error = Some(e),
            _ => app.load_agents(cx),
        }, cx);
    }
}

fn source_label(source: &str) -> &'static str {
    match source {
        "job" => t!("Failed jobs"),
        "vercel" => t!("Vercel deploys"),
        "ci" => t!("CI"),
        "convex" => t!("Convex not responding"),
        _ => t!("other"),
    }
}

fn level_label(level: &str) -> &'static str {
    match level {
        "off" => t!("Off"),
        "notify" => t!("Notify"),
        "diagnose" => t!("Diagnose"),
        _ => t!("Fix and ask"),
    }
}

fn input(app: &mut DevtoolApp, key: &str, placeholder: String, masked: bool, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Entity<InputState> {
    app.inputs.entry(key.to_string()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(placeholder))).clone()
}

fn tokens_label(n: u64) -> String {
    match n {
        0 => t!("no limit").to_string(),
        n if n >= 1_000_000 => format!("{}M", n / 1_000_000),
        n => format!("{}k", n / 1000),
    }
}

/// Una línea por plantilla: encendida o no, nombre, cuándo aplica y sus límites.
fn template_rows(app: &mut DevtoolApp, data: &Value, cx: &mut Context<DevtoolApp>) -> Div {
    let mut col = v_flex().gap_1().child(ui::muted(t!("How the agent works on each kind of incident. The first enabled template that matches is used; the last one catches the rest."), cx).text_xs());
    for tpl in data["templates"].as_array().cloned().unwrap_or_default() {
        let id = tpl["id"].as_str().unwrap_or_default().to_string();
        let on = tpl["enabled"] == true;
        let sources: Vec<String> = tpl["sources"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect();
        let when = if sources.is_empty() { t!("any incident").to_string() } else { sources.join(", ") };
        let detail = [
            Some(when),
            tpl["match"].as_str().filter(|m| !m.is_empty()).map(|m| t!("with “{m}”", m = m).to_string()),
            Some(if tpl["maxMode"] == "diagnose" { t!("diagnose only").to_string() } else { t!("up to a fix").to_string() }),
            tpl["model"].as_str().filter(|m| !m.is_empty()).map(str::to_string),
            Some(format!("{} · {} min", tokens_label(tpl["maxTokens"].as_u64().unwrap_or(0)), tpl["maxMinutes"].as_u64().unwrap_or(0))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let (t_toggle, t_edit) = (tpl.clone(), tpl.clone());
        let del = (!tpl["builtin"].as_bool().unwrap_or(false)).then(|| id.clone());
        col = col.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::toggle(SharedString::from(format!("tpl-on-{id}")), on, cx).on_click(cx.listener(move |this, _, _, cx| this.toggle_template(t_toggle.clone(), cx))))
                .child(div().w(px(190.)).flex_none().truncate().text_size(px(12.5)).child(ui::txt(&tpl["name"])))
                .child(div().flex_1().min_w_0().truncate().child(ui::muted(detail, cx).text_xs()))
                .child(Button::new(SharedString::from(format!("tpl-edit-{id}"))).xsmall().ghost().label(t!("Edit")).on_click(cx.listener(move |this, _, _, cx| this.edit_template(t_edit.clone(), cx))))
                .when_some(del, |d, id| d.child(Button::new(SharedString::from(format!("tpl-del-{id}"))).xsmall().ghost().label(t!("Remove")).on_click(cx.listener(move |this, _, _, cx| this.delete_template(id.clone(), cx))))),
        );
    }
    let _ = app;
    col.child(
        h_flex()
            .gap_1()
            .child(Button::new("tpl-new").xsmall().outline().icon(IconName::Plus).label(t!("New template")).on_click(cx.listener(|this, _, _, cx| {
                this.edit_template(json!({ "id": "", "name": "", "enabled": true, "sources": [], "maxMode": "fix", "tools": [], "maxTokens": 150_000, "maxMinutes": 15, "instructions": "" }), cx)
            })))
            .child(Button::new("tpl-reset").xsmall().ghost().label(t!("Restore built-in templates")).on_click(cx.listener(|this, _, _, cx| this.reset_templates(cx)))),
    )
}

/// El editor de una plantilla: los botones cambian el borrador; los textos se leen al guardar.
fn template_editor(app: &mut DevtoolApp, data: &Value, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Option<Div> {
    let draft = app.template_edit.clone()?;
    let t = crate::theme::Tokens::current(cx);
    let text = |app: &mut DevtoolApp, key: &str, value: &Value, placeholder: String, window: &mut Window, cx: &mut Context<DevtoolApp>| {
        let v = value.as_str().unwrap_or_default().to_string();
        app.inputs.entry(key.to_string()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(v))).clone()
    };
    let name = text(app, "tpl-name", &draft["name"], t!("Name").to_string(), window, cx);
    let matcher = text(app, "tpl-match", &draft["match"], t!("Optional: text in the title or the error (e.g. stripe)").to_string(), window, cx);
    let model = text(app, "tpl-model", &draft["model"], t!("Default model").to_string(), window, cx);
    let instructions = text(app, "tpl-instructions", &draft["instructions"], t!("Extra instructions for the agent").to_string(), window, cx);
    // Un botón que cambia un campo del borrador.
    let set = |label: String, id: String, selected: bool, field: &'static str, value: Value, cx: &mut Context<DevtoolApp>| {
        let b = Button::new(SharedString::from(id)).xsmall().label(label).on_click(cx.listener(move |this, _, _, cx| {
            if let Some(d) = this.template_edit.as_mut() {
                d[field] = value.clone();
            }
            cx.notify();
        }));
        ui::brand_if(selected, b, cx)
    };
    let list_has = |field: &str, v: &str| draft[field].as_array().is_some_and(|l| l.iter().any(|x| x == v));
    let toggled = |field: &str, v: &str| {
        let mut l: Vec<Value> = draft[field].as_array().cloned().unwrap_or_default();
        if l.iter().any(|x| x == v) {
            l.retain(|x| x != v);
        } else {
            l.push(json!(v));
        }
        json!(l)
    };
    let mut sources = h_flex().gap_1().flex_wrap();
    for s in data["sources"].as_array().cloned().unwrap_or_default() {
        let s = s.as_str().unwrap_or_default();
        sources = sources.child(set(s.to_string(), format!("tpl-src-{s}"), list_has("sources", s), "sources", toggled("sources", s), cx));
    }
    let mut modes = h_flex().gap_1();
    for (m, label) in [("diagnose", t!("Diagnose only")), ("fix", t!("Up to a fix"))] {
        modes = modes.child(set(label.to_string(), format!("tpl-mode-{m}"), draft["maxMode"] == m, "maxMode", json!(m), cx));
    }
    let mut providers = h_flex().gap_1().flex_wrap().child(set(t!("Default").to_string(), "tpl-prov-default".into(), draft["provider"].as_str().is_none_or(str::is_empty), "provider", Value::Null, cx));
    for p in data["providers"].as_array().cloned().unwrap_or_default() {
        let id = p["id"].as_str().unwrap_or_default().to_string();
        providers = providers.child(set(ui::txt(&p["label"]).to_string(), format!("tpl-prov-{id}"), draft["provider"] == id.as_str(), "provider", json!(id), cx));
    }
    let mut tools = h_flex().gap_1().flex_wrap();
    for tool in data["tools"].as_array().cloned().unwrap_or_default() {
        let tool = tool.as_str().unwrap_or_default();
        tools = tools.child(set(tool.to_string(), format!("tpl-tool-{tool}"), list_has("tools", tool), "tools", toggled("tools", tool), cx));
    }
    let mut tokens = h_flex().gap_1();
    for n in [25_000u64, 50_000, 150_000, 500_000, 0] {
        tokens = tokens.child(set(tokens_label(n), format!("tpl-tok-{n}"), draft["maxTokens"].as_u64() == Some(n), "maxTokens", json!(n), cx));
    }
    let mut minutes = h_flex().gap_1();
    for n in [5u64, 15, 30, 0] {
        let label = if n == 0 { t!("no limit").to_string() } else { format!("{n} min") };
        minutes = minutes.child(set(label, format!("tpl-min-{n}"), draft["maxMinutes"].as_u64() == Some(n), "maxMinutes", json!(n), cx));
    }
    Some(
        v_flex()
            .gap_1p5()
            .p_2()
            .border_1()
            .border_color(t.card_border)
            .child(ui::section(if draft["id"].as_str().is_some_and(|i| !i.is_empty()) { t!("Edit template") } else { t!("New template") }, cx))
            .child(ui::kv(t!("Name"), Input::new(&name).small(), cx))
            .child(ui::kv(t!("Sources"), sources, cx))
            .child(ui::muted(t!("None selected: any incident."), cx).text_xs())
            .child(ui::kv(t!("Matches"), Input::new(&matcher).small(), cx))
            .child(ui::kv(t!("Highest mode"), modes, cx))
            .child(ui::kv(t!("Provider"), providers, cx))
            .child(ui::kv(t!("Model"), Input::new(&model).small(), cx))
            .child(ui::kv(t!("Instructions"), Input::new(&instructions).small(), cx))
            .child(ui::kv(t!("Tools"), tools, cx))
            .child(ui::muted(t!("None selected: all the tools of the mode (the ones that write only exist when preparing a fix)."), cx).text_xs())
            .child(ui::kv(t!("Tokens per run"), tokens, cx))
            .child(ui::kv(t!("Minutes per run"), minutes, cx))
            .child(
                h_flex()
                    .gap_1()
                    .child(ui::brand(Button::new("tpl-save").small().icon(IconName::Check).label(t!("Save template")), cx).on_click(cx.listener(|this, _, _, cx| this.save_template(cx))))
                    .child(Button::new("tpl-cancel").small().ghost().label(t!("Cancel")).on_click(cx.listener(|this, _, _, cx| {
                        this.template_edit = None;
                        cx.notify();
                    }))),
            ),
    )
}

pub fn card(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    let card = ui::card(cx).child(ui::section(t!("Agents · providers and autonomy"), cx));
    let Some(data) = app.agents.clone() else {
        if app.daemon.is_some() && app.agents_error.is_none() {
            app.load_agents(cx);
        }
        return card.child(ui::muted(app.agents_error.clone().unwrap_or_else(|| t!("Loading…").to_string()), cx));
    };
    let settings = data["settings"].clone();
    let providers = data["providers"].as_array().cloned().unwrap_or_default();
    let kinds = data["kinds"].as_array().cloned().unwrap_or_default();
    let default_provider = settings["defaultProvider"].as_str().map(str::to_string);

    // Qué hacer con cada tipo de incidente.
    let mut autonomy = v_flex().gap_1p5().child(ui::muted(t!("What happens when something breaks. Diagnose and Fix start an agent on their own; a fix stays on its own branch and nothing is deployed without your approval."), cx).text_xs());
    for source in data["sources"].as_array().cloned().unwrap_or_default() {
        let source = source.as_str().unwrap_or_default().to_string();
        let current = settings["autonomy"][&source].as_str().unwrap_or("notify").to_string();
        let mut buttons = h_flex().gap_1();
        for level in LEVELS {
            let s = source.clone();
            let b = Button::new(SharedString::from(format!("auto-{source}-{level}"))).xsmall().label(level_label(level)).on_click(cx.listener(move |this, _, _, cx| {
                let s = s.clone();
                this.save_agent_settings(move |v| v["autonomy"][&s] = json!(level), cx);
            }));
            buttons = buttons.child(ui::brand_if(current == level, b, cx));
        }
        autonomy = autonomy.child(h_flex().gap_3().items_center().child(ui::muted(source_label(&source), cx).text_xs().w(px(150.))).child(buttons));
    }

    // Proveedores guardados.
    let mut saved = v_flex().gap_1();
    if providers.is_empty() {
        saved = saved.child(ui::muted(t!("No providers yet. Add one below: its API key is stored encrypted on this Mac and never shown again."), cx).text_xs());
    }
    for p in &providers {
        let id = p["id"].as_str().unwrap_or_default().to_string();
        let is_default = default_provider.as_deref() == Some(id.as_str());
        let kind = kinds.iter().find(|k| k["id"] == p["kind"]).and_then(|k| k["label"].as_str()).unwrap_or("?").to_string();
        let detail = [Some(kind), p["model"].as_str().map(str::to_string), p["baseUrl"].as_str().map(str::to_string), p["keyHint"].as_str().map(|h| t!("key {h}", h = h).to_string())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let (id_def, id_del, model) = (id.clone(), id.clone(), p["model"].clone());
        saved = saved.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::status_tag(if is_default { Health::Ok } else { Health::Unknown }, ui::txt(&p["label"]).to_string()))
                .child(div().flex_1().min_w_0().truncate().child(ui::muted(detail, cx)))
                .when(!is_default, |d| {
                    d.child(Button::new(SharedString::from(format!("prov-default-{id}"))).xsmall().ghost().label(t!("Use by default")).on_click(cx.listener(move |this, _, _, cx| {
                        let (id, model) = (id_def.clone(), model.clone());
                        this.save_agent_settings(move |v| {
                            v["defaultProvider"] = json!(id);
                            v["defaultModel"] = model;
                        }, cx);
                    })))
                })
                .when(is_default, |d| d.child(ui::muted(t!("default"), cx)))
                .child(Button::new(SharedString::from(format!("prov-del-{id}"))).xsmall().ghost().label(t!("Remove")).on_click(cx.listener(move |this, _, _, cx| this.delete_provider(id_del.clone(), cx)))),
        );
    }

    // Traer los de pi (el agente de código), si los tiene: lo hace el daemon, sin pasar por aquí.
    let import = {
        let mut row = v_flex().gap_1().child(
            h_flex()
                .gap_2()
                .items_center()
                .child(Button::new("prov-import-pi").xsmall().outline().icon(IconName::ArrowDown).label(t!("Import from pi")).on_click(cx.listener(|this, _, _, cx| this.import_from_pi(cx))))
                .child(ui::muted(t!("Brings pi's providers and API keys (~/.pi/agent). OAuth logins aren't copied."), cx).text_xs()),
        );
        match &app.agents_import {
            Some(Load::Loading) => row = row.child(ui::muted(t!("Importing…"), cx)),
            Some(Load::Failed(e)) => row = row.child(ui::status_tag(Health::Bad, e.clone())),
            Some(Load::Ready(v)) => {
                let names: Vec<String> = v["imported"].as_array().into_iter().flatten().map(|p| format!("{} ({})", ui::txt(&p["id"]), ui::txt(&p["model"]))).collect();
                row = row.child(ui::status_tag(if names.is_empty() { Health::Unknown } else { Health::Ok }, if names.is_empty() { t!("Nothing new to import").to_string() } else { t!("Imported: {list}", list = names.join(", ")).to_string() }));
                if let Some(d) = v["default"]["provider"].as_str() {
                    row = row.child(ui::muted(t!("Now the default: {p} · {m}", p = d, m = ui::txt(&v["default"]["model"])), cx).text_xs());
                }
                for s in v["skipped"].as_array().into_iter().flatten() {
                    row = row.child(ui::muted(format!("{} · {}", ui::txt(&s["name"]), ui::txt(&s["reason"])), cx).text_xs());
                }
            }
            None => {}
        }
        row
    };

    // Agregar uno: tipo, nombre, URL base (si la pide), modelo y la clave.
    let kind = kinds.iter().find(|k| k["id"] == app.agent_kind.as_str()).cloned().unwrap_or(Value::Null);
    let mut kind_buttons = h_flex().gap_1().flex_wrap();
    for k in &kinds {
        let id = k["id"].as_str().unwrap_or_default().to_string();
        let selected = id == app.agent_kind;
        let i = id.clone();
        let b = Button::new(SharedString::from(format!("kind-{id}"))).xsmall().label(ui::txt(&k["label"])).on_click(cx.listener(move |this, _, _, cx| {
            this.agent_kind = i.clone();
            this.inputs.remove("agent-model");
            cx.notify();
        }));
        kind_buttons = kind_buttons.child(ui::brand_if(selected, b, cx));
    }
    let label = input(app, "agent-label", kind["label"].as_str().unwrap_or("").to_string(), false, window, cx);
    let model = input(app, "agent-model", kind["model"].as_str().filter(|m| !m.is_empty()).unwrap_or("model").to_string(), false, window, cx);
    let key = input(app, "agent-key", t!("API key (stored encrypted)").to_string(), true, window, cx);
    let needs_url = kind["needsBaseUrl"] == true;
    let url = needs_url.then(|| input(app, "agent-url", "http://127.0.0.1:11434/v1".into(), false, window, cx));
    let add = v_flex()
        .gap_1p5()
        .child(ui::section(t!("Add a provider"), cx))
        .child(kind_buttons)
        .child(ui::kv(t!("Name"), Input::new(&label).small(), cx))
        .when_some(url, |c, url| c.child(ui::kv(t!("Base URL"), Input::new(&url).small(), cx)))
        .child(ui::kv(t!("Model"), Input::new(&model).small(), cx))
        .child(ui::kv(if needs_url { t!("API key (optional)") } else { t!("API key") }, Input::new(&key).small(), cx))
        .child(h_flex().child(ui::brand(Button::new("prov-save").small().icon(IconName::Check).label(t!("Save provider")), cx).on_click(cx.listener(|this, _, _, cx| this.save_provider(cx)))));

    // Presupuesto y cuántos a la vez.
    let budget = settings["dailyTokenBudget"].as_u64().unwrap_or(0);
    let max = settings["maxConcurrent"].as_u64().unwrap_or(1);
    let mut budget_buttons = h_flex().gap_1();
    for b in [100_000u64, 500_000, 2_000_000, 10_000_000] {
        let label = if b >= 1_000_000 { format!("{}M", b / 1_000_000) } else { format!("{}k", b / 1000) };
        let button = Button::new(SharedString::from(format!("budget-{b}"))).xsmall().label(label).on_click(cx.listener(move |this, _, _, cx| this.save_agent_settings(move |v| v["dailyTokenBudget"] = json!(b), cx)));
        budget_buttons = budget_buttons.child(ui::brand_if(budget == b, button, cx));
    }
    let mut max_buttons = h_flex().gap_1();
    for n in 1u64..=4 {
        let button = Button::new(SharedString::from(format!("max-{n}"))).xsmall().label(n.to_string()).on_click(cx.listener(move |this, _, _, cx| this.save_agent_settings(move |v| v["maxConcurrent"] = json!(n), cx)));
        max_buttons = max_buttons.child(ui::brand_if(max == n, button, cx));
    }

    let templates = template_rows(app, &data, cx);
    let editor = template_editor(app, &data, window, cx);
    card.child(ui::section(t!("Autonomy"), cx))
        .child(autonomy)
        .child(ui::section(t!("Templates"), cx))
        .child(templates)
        .children(editor)
        .child(ui::section(t!("Providers"), cx))
        .child(saved)
        .child(import)
        .child(add)
        .child(ui::section(t!("Limits"), cx))
        .child(h_flex().gap_3().items_center().child(ui::muted(t!("Tokens per day"), cx).text_xs().w(px(150.))).child(budget_buttons))
        .child(h_flex().gap_3().items_center().child(ui::muted(t!("Agents at once"), cx).text_xs().w(px(150.))).child(max_buttons))
        .child(ui::muted(t!("Used today: {u} of {b} tokens", u = data["usageToday"].as_u64().unwrap_or(0), b = budget), cx).text_xs())
        .child({
            let host = &data["host"];
            let (h, text) = if host["available"] != true {
                (Health::Bad, t!("The agent host isn't installed (agent-host/ with its node_modules, and Node)").to_string())
            } else if let Some(e) = host["error"].as_str() {
                (Health::Bad, t!("The agent host didn't start: {e}", e = e).to_string())
            } else if host["running"] == true {
                (Health::Ok, t!("Agent host running").to_string())
            } else {
                (Health::Unknown, t!("The agent host starts with the first agent").to_string())
            };
            ui::status_tag(h, text)
        })
        .when_some(app.agents_error.clone(), |c, e| c.child(ui::status_tag(Health::Bad, e)))
}
