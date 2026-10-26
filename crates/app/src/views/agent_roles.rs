// Configuración → Agentes → Agentes del catálogo (roles.rs del daemon): los agentes que los flujos
// ponen en sus nodos, cada uno con su responsabilidad (instrucciones), si solo lee o escribe, su
// modelo, herramientas, límites y la forma de su resultado (campos que el daemon valida y que las
// condiciones y los avisos del flujo leen). Los de fábrica se cambian, apagan o restauran.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

const FIELDS: [&str; 4] = ["role-name", "role-model", "role-instructions", "role-output"];

impl DevtoolApp {
    pub fn load_roles(&mut self, cx: &mut Context<Self>) {
        self.get("/api/agents/roles".into(), |app, res| match res {
            Load::Ready(v) => app.agent_roles = Some(v),
            Load::Failed(e) => app.roles_error = Some(e),
            Load::Loading => {}
        }, cx);
    }

    fn edit_role(&mut self, role: Value, cx: &mut Context<Self>) {
        for k in FIELDS {
            self.inputs.remove(k);
        }
        self.role_edit = Some(role);
        cx.notify();
    }

    fn save_role(&mut self, cx: &mut Context<Self>) {
        let Some(mut draft) = self.role_edit.clone() else { return };
        let value = |app: &Self, key: &str, cx: &Context<Self>| app.inputs.get(key).map(|s| s.read(cx).value().trim().to_string());
        if let Some(v) = value(self, "role-name", cx) {
            draft["name"] = json!(v);
        }
        draft["model"] = json!(value(self, "role-model", cx).filter(|v| !v.is_empty()));
        if let Some(v) = value(self, "role-instructions", cx) {
            draft["instructions"] = json!(v);
        }
        if let Some(v) = value(self, "role-output", cx) {
            draft["output"] = parse_output(&v);
        }
        self.post("/api/agents/roles".into(), draft, |app, res, cx| match res {
            Load::Ready(_) => {
                app.role_edit = None;
                app.roles_error = None;
                app.load_roles(cx);
            }
            Load::Failed(e) => app.roles_error = Some(e),
            Load::Loading => {}
        }, cx);
    }

    fn toggle_role(&mut self, mut role: Value, cx: &mut Context<Self>) {
        role["enabled"] = json!(role["enabled"] != true);
        self.post("/api/agents/roles".into(), role, |app, res, cx| match res {
            Load::Failed(e) => app.roles_error = Some(e),
            _ => app.load_roles(cx),
        }, cx);
    }

    fn delete_role(&mut self, id: String, cx: &mut Context<Self>) {
        self.delete(format!("/api/agents/roles/{id}"), |app, res, cx| match res {
            Load::Failed(e) => app.roles_error = Some(e),
            _ => app.load_roles(cx),
        }, cx);
    }

    fn reset_roles(&mut self, cx: &mut Context<Self>) {
        self.role_edit = None;
        self.post("/api/agents/roles/reset".into(), Value::Null, |app, res, cx| match res {
            Load::Failed(e) => app.roles_error = Some(e),
            _ => app.load_roles(cx),
        }, cx);
    }
}

/// «campo: tipo, campo: tipo» → `{campo: tipo}` (en orden).
pub fn parse_output(text: &str) -> Value {
    let mut map = serde_json::Map::new();
    for part in text.split([',', '\n']) {
        if let Some((k, t)) = part.split_once(':') {
            let (k, t) = (k.trim(), t.trim());
            if !k.is_empty() && !t.is_empty() {
                map.insert(k.to_string(), json!(t));
            }
        }
    }
    Value::Object(map)
}

pub fn output_text(output: &Value) -> String {
    output.as_object().map(|o| o.iter().map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or_default())).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

fn tokens_label(n: u64) -> String {
    match n {
        0 => t!("no limit").to_string(),
        n if n >= 1_000_000 => format!("{}M", n / 1_000_000),
        n => format!("{}k", n / 1000),
    }
}

pub fn card(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Div {
    let card = ui::card(cx).child(ui::section(t!("Agents for flows"), cx));
    let Some(data) = app.agent_roles.clone() else {
        if app.daemon.is_some() && app.roles_error.is_none() {
            app.load_roles(cx);
        }
        return card.child(ui::muted(app.roles_error.clone().unwrap_or_else(|| t!("Loading…").to_string()), cx));
    };
    let mut col = v_flex().gap_1().child(ui::muted(t!("Each agent has one responsibility. Put them in a flow (Workflows → Agent) and each one reads the event and the results of the nodes before it; its result has the fields below, which the next nodes, conditions and notices use."), cx).text_xs());
    // El logo de cada agente: el de su proveedor, o el del predeterminado.
    let providers = app.agents.as_ref().map(|a| a["providers"].clone()).unwrap_or(Value::Null);
    let default_provider = app.agents.as_ref().and_then(|a| a["settings"]["defaultProvider"].as_str().map(str::to_string));
    let logo_of = |role: &Value| -> String {
        let id = role["provider"].as_str().filter(|p| !p.is_empty()).map(str::to_string).or_else(|| default_provider.clone()).unwrap_or_default();
        providers.as_array().and_then(|ps| ps.iter().find(|p| p["id"] == id.as_str())).and_then(|p| p["logo"].as_str()).unwrap_or("").to_string()
    };
    for role in data["roles"].as_array().cloned().unwrap_or_default() {
        let id = role["id"].as_str().unwrap_or_default().to_string();
        let logo = logo_of(&role);
        let on = role["enabled"] == true;
        let (r_toggle, r_edit) = (role.clone(), role.clone());
        let del = (role["builtin"] != true).then(|| id.clone());
        let mode = if role["mode"] == "write" { t!("writes in its worktree") } else { t!("reads") };
        let out = output_text(&role["output"]);
        let detail = [mode.to_string(), if out.is_empty() { t!("free text").to_string() } else { out }, tokens_label(role["maxTokens"].as_u64().unwrap_or(0))].join(" · ");
        col = col.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::toggle(SharedString::from(format!("role-on-{id}")), on, cx).on_click(cx.listener(move |this, _, _, cx| this.toggle_role(r_toggle.clone(), cx))))
                .children(crate::logos::badge(&logo, 22., cx))
                .child(v_flex().flex_1().min_w_0().child(div().text_size(px(12.5)).child(ui::txt(&role["name"]).to_string())).child(ui::muted(detail, cx).text_xs().truncate()))
                .child(Button::new(SharedString::from(format!("role-edit-{id}"))).xsmall().ghost().label(t!("Edit")).on_click(cx.listener(move |this, _, _, cx| this.edit_role(r_edit.clone(), cx))))
                .when_some(del, |d, id| d.child(Button::new(SharedString::from(format!("role-del-{id}"))).xsmall().ghost().label(t!("Remove")).on_click(cx.listener(move |this, _, _, cx| this.delete_role(id.clone(), cx))))),
        );
    }
    col = col.child(
        h_flex()
            .gap_1()
            .pt_1()
            .child(Button::new("role-new").xsmall().outline().icon(IconName::Plus).label(t!("New agent")).on_click(cx.listener(|this, _, _, cx| {
                this.edit_role(json!({ "name": "", "enabled": true, "mode": "read", "instructions": "", "tools": [], "maxTokens": 80000, "maxMinutes": 10, "output": {} }), cx);
            })))
            .child(Button::new("role-reset").xsmall().ghost().label(t!("Restore built-in agents")).on_click(cx.listener(|this, _, _, cx| this.reset_roles(cx)))),
    );
    let providers = app.agents.as_ref().map(|a| a["providers"].clone()).unwrap_or(Value::Null);
    card.child(col)
        .children(editor(app, &data, &providers, window, cx))
        .when_some(app.roles_error.clone(), |c, e| c.child(ui::status_tag(Health::Bad, e)))
}

fn editor(app: &mut DevtoolApp, data: &Value, providers: &Value, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Option<Div> {
    let draft = app.role_edit.clone()?;
    let t = crate::theme::Tokens::current(cx);
    let text = |app: &mut DevtoolApp, key: &str, value: String, placeholder: String, window: &mut Window, cx: &mut Context<DevtoolApp>| {
        app.inputs.entry(key.to_string()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))).clone()
    };
    let name = text(app, "role-name", ui::txt(&draft["name"]).to_string(), t!("Name").to_string(), window, cx);
    let model = text(app, "role-model", draft["model"].as_str().unwrap_or_default().to_string(), t!("Default model").to_string(), window, cx);
    let instructions = text(app, "role-instructions", ui::txt(&draft["instructions"]).to_string(), t!("What it does, in a few sentences").to_string(), window, cx);
    let output = text(app, "role-output", output_text(&draft["output"]), t!("summary: string, risk: low|medium|high, files: list").to_string(), window, cx);
    let set = |label: String, id: String, selected: bool, field: &'static str, value: Value, cx: &mut Context<DevtoolApp>| {
        let b = Button::new(SharedString::from(id)).xsmall().label(label).on_click(cx.listener(move |this, _, _, cx| {
            if let Some(d) = this.role_edit.as_mut() {
                d[field] = value.clone();
            }
            cx.notify();
        }));
        ui::brand_if(selected, b, cx)
    };
    let list_has = |v: &str| draft["tools"].as_array().is_some_and(|l| l.iter().any(|x| x == v));
    let toggled = |v: &str| {
        let mut l: Vec<Value> = draft["tools"].as_array().cloned().unwrap_or_default();
        if l.iter().any(|x| x == v) {
            l.retain(|x| x != v);
        } else {
            l.push(json!(v));
        }
        json!(l)
    };
    let mut modes = h_flex().gap_1();
    for (m, label) in [("read", t!("Reads")), ("write", t!("Writes in its worktree"))] {
        modes = modes.child(set(label.to_string(), format!("role-mode-{m}"), draft["mode"] == m, "mode", json!(m), cx));
    }
    let mut provs = h_flex().gap_1().flex_wrap().child(set(t!("Default").to_string(), "role-prov-default".into(), draft["provider"].as_str().is_none_or(str::is_empty), "provider", Value::Null, cx));
    for p in providers.as_array().cloned().unwrap_or_default() {
        let id = p["id"].as_str().unwrap_or_default().to_string();
        // Con el logo del proveedor, como se verá en los hilos.
        let logo = p["logo"].as_str().unwrap_or("").to_string();
        provs = provs.child(h_flex().gap_1().items_center().children(crate::logos::badge(&logo, 18., cx)).child(set(ui::txt(&p["label"]).to_string(), format!("role-prov-{id}"), draft["provider"] == id.as_str(), "provider", json!(id), cx)));
    }
    // Los modelos del proveedor del agente (el suyo o el predeterminado): elegir uno lo pone en el campo.
    let default_provider = app.agents.as_ref().and_then(|a| a["settings"]["defaultProvider"].as_str().map(str::to_string)).unwrap_or_default();
    let prov_id = draft["provider"].as_str().filter(|p| !p.is_empty()).map(str::to_string).unwrap_or(default_provider);
    let prov = providers.as_array().and_then(|ps| ps.iter().find(|p| p["id"] == prov_id.as_str()).cloned()).unwrap_or(Value::Null);
    let mut model_list: Vec<String> = prov["models"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_string)).collect();
    if model_list.is_empty() {
        model_list.extend(prov["model"].as_str().map(str::to_string));
    }
    let typed = model.read(cx).value().trim().to_string();
    let mut model_chips = h_flex().gap_1().flex_wrap();
    for m in model_list {
        let pick = m.clone();
        let b = Button::new(SharedString::from(format!("role-model-{m}"))).xsmall().label(m.clone()).on_click(cx.listener(move |this, _, window, cx| {
            if let Some(input) = this.inputs.get("role-model").cloned() {
                input.update(cx, |s, cx| s.set_value(pick.clone(), window, cx));
            }
            cx.notify();
        }));
        model_chips = model_chips.child(ui::brand_if(typed == m, b, cx));
    }
    let mut tools = h_flex().gap_1().flex_wrap();
    for tool in data["tools"].as_array().cloned().unwrap_or_default() {
        let tool = tool.as_str().unwrap_or_default();
        tools = tools.child(set(tool.to_string(), format!("role-tool-{tool}"), list_has(tool), "tools", toggled(tool), cx));
    }
    let mut tokens = h_flex().gap_1();
    for n in [25_000u64, 80_000, 150_000, 500_000, 0] {
        tokens = tokens.child(set(tokens_label(n), format!("role-tok-{n}"), draft["maxTokens"].as_u64() == Some(n), "maxTokens", json!(n), cx));
    }
    let mut minutes = h_flex().gap_1();
    for n in [5u64, 10, 15, 30, 0] {
        let label = if n == 0 { t!("no limit").to_string() } else { format!("{n} min") };
        minutes = minutes.child(set(label, format!("role-min-{n}"), draft["maxMinutes"].as_u64() == Some(n), "maxMinutes", json!(n), cx));
    }
    Some(
        v_flex()
            .gap_1p5()
            .p_2()
            .mt_1()
            .border_1()
            .border_color(t.card_border)
            .child(ui::section(if draft["id"].as_str().is_some_and(|i| !i.is_empty()) { t!("Edit agent") } else { t!("New agent") }, cx))
            .child(ui::kv(t!("Name"), Input::new(&name).small(), cx))
            .child(ui::kv(t!("Responsibility"), Input::new(&instructions).small(), cx))
            .child(ui::kv(t!("Mode"), modes, cx))
            .child(ui::kv(t!("Result"), Input::new(&output).small(), cx))
            .child(ui::muted(t!("Fields and their type: string, boolean, number, list, or the options separated by | (low|medium|high). Empty: free text."), cx).text_xs())
            .child(ui::kv(t!("Provider"), provs, cx))
            .child(ui::kv(t!("Model"), v_flex().gap_1().child(model_chips).child(Input::new(&model).small()), cx))
            .child(ui::kv(t!("Tools"), tools, cx))
            .child(ui::muted(t!("None selected: all the tools of its mode."), cx).text_xs())
            .child(ui::kv(t!("Tokens per run"), tokens, cx))
            .child(ui::kv(t!("Minutes per run"), minutes, cx))
            .child(
                h_flex()
                    .gap_1()
                    .child(ui::brand(Button::new("role-save").small().icon(IconName::Check).label(t!("Save agent")), cx).on_click(cx.listener(|this, _, _, cx| this.save_role(cx))))
                    .child(Button::new("role-cancel").small().ghost().label(t!("Cancel")).on_click(cx.listener(|this, _, _, cx| {
                        this.role_edit = None;
                        cx.notify();
                    }))),
            ),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn el_resultado_se_escribe_en_una_linea() {
        let v = super::parse_output("summary: string, risk: low|medium|high,\nfiles: list, bad");
        assert_eq!(super::output_text(&v), "summary: string, risk: low|medium|high, files: list");
    }
}
