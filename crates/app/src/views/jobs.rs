// Trabajos: pedir uno (receta y entorno en desplegables, argumentos), la
// lista reciente en una tabla ordenable y el detalle del elegido: cada paso
// como un bloque plegable con su salida, los argumentos como árbol JSON y el
// log completo con filtro por nivel y búsqueda.

use gpui_kit::component::button::{Button, ButtonVariants};
use ely_core::data_display::Tone;
use ely_core::forms::Select;
use ely_core::tables::{Cell, Column, DataTable, Row};
use ely_devtools::JsonViewer;
use ely_terminal::{CommandBlock, CommandState, LogViewer};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable};
use gpui_kit::component::Disableable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashMap;

use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

pub fn status_health(status: &str) -> Health {
    match status {
        "succeeded" => Health::Ok,
        "failed" => Health::Bad,
        "running" => Health::Warn,
        _ => Health::Unknown,
    }
}

pub fn status_label(status: &str) -> &'static str {
    match status {
        "waiting-approval" => t!("waiting for approval"),
        "queued" => t!("queued"),
        "running" => t!("running"),
        "succeeded" => t!("done"),
        "failed" => t!("failed"),
        "cancelled" => t!("cancelled"),
        "rejected" => t!("rejected"),
        "expired" => t!("expired"),
        _ => "—",
    }
}

fn approval_label(level: &str) -> &'static str {
    match level {
        "none" => t!("no approval"),
        "simple" => t!("needs approval"),
        "typed" => t!("needs approval by typing the environment"),
        "reinforced" => t!("needs approval with Touch ID"),
        _ => t!("doesn't run here"),
    }
}

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    render_for(app, None, window, cx)
}

/// Trabajos de un entorno (en su pestaña): las recetas que corren en él, sin elegir entorno, y
/// solo sus trabajos. Sin entorno, todos (la página a la que lleva el aviso de trabajos en curso).
pub fn render_for(app: &mut DevtoolApp, env: Option<&str>, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let show_detail = match (env, &app.job_detail) {
        (Some(e), Some(j)) => j["env"] == e,
        _ => true,
    };
    v_flex()
        .gap_4()
        .p_4()
        .child(new_job(app, env, window, cx))
        .child(list(app, env, cx))
        .when(show_detail, |c| c.child(detail(app, window, cx)))
}

fn input(app: &mut DevtoolApp, key: String, default: &str, placeholder: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) -> Entity<InputState> {
    let (default, placeholder) = (default.to_string(), placeholder.to_string());
    app.inputs
        .entry(key)
        .or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(default)))
        .clone()
}

fn new_job(app: &mut DevtoolApp, fixed: Option<&str>, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    // En otro proyecto, solo las recetas que corren en sus entornos.
    let other = app.project != "heskala-erp";
    let fixed_kind: Option<String> = fixed.and_then(|e| {
        app.overview.as_ref()?.get("environments")?.as_array()?.iter().find(|x| x["name"] == e).and_then(|x| x["kind"].as_str().map(str::to_string))
    });
    let choices: Vec<_> = app
        .recipes
        .iter()
        .filter(|r| !other || r["projects"] == true)
        // En la pestaña de un entorno, las recetas que corren en su tipo.
        .filter(|r| fixed_kind.as_ref().is_none_or(|k| r["kinds"].as_array().is_some_and(|ks| ks.iter().any(|x| x == k.as_str()))))
        .map(|r| {
            let name = r["name"].as_str().unwrap_or_default().to_string();
            let kinds: Vec<&str> = r["kinds"].as_array().into_iter().flatten().filter_map(|k| k.as_str()).collect();
            crate::ely::choice_note(name.clone(), r["title"].as_str().unwrap_or(&name).to_string(), kinds.join(" · "))
        })
        .collect();
    let this = cx.entity().downgrade();
    let mut recipe_select = Select::new("recipe-select", choices).placeholder(t!("Choose a recipe…")).on_change(move |value, _, cx| {
        let value = value.to_string();
        let _ = this.update(cx, |app, cx| {
            app.form_recipe = Some(value);
            app.form_env = None;
            app.form_result = None;
            cx.notify();
        });
    });
    if let Some(r) = &app.form_recipe {
        recipe_select = recipe_select.selected(r.clone());
    }
    let mut card = ui::card(cx).child(ui::section(t!("New job"), cx)).child(ui::kv(t!("Recipe"), div().w(px(420.)).min_w(px(160.)).flex_shrink(1.).child(recipe_select), cx));

    let recipe = app.form_recipe.as_ref().and_then(|n| app.recipes.iter().find(|r| r["name"] == n.as_str()).cloned());
    let Some(recipe) = recipe else {
        let hint = if app.project == "heskala-erp" {
            t!("Choose a recipe. To take a commit through several steps (backup, rehearsal, deploy, approval), use a workflow: the environment's Workflows tab.")
        } else {
            t!("Choose a recipe. This project shows the ones that run in its environments. To chain several steps, use a workflow (the environment's Workflows tab).")
        };
        return card.child(ui::muted(hint, cx));
    };
    let recipe_name = recipe["name"].as_str().unwrap_or_default().to_string();
    card = card.child(ui::muted(ui::txt(&recipe["description"]), cx));

    let kinds: Vec<String> = recipe["kinds"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
    let creates = kinds.iter().any(|k| k == "new");
    let env_value: Option<String> = if let (Some(e), false) = (fixed, creates) {
        card = card.child(ui::kv(t!("Environment"), ui::mono(e.to_string()), cx));
        Some(e.to_string())
    } else if creates {
        let state = input(app, format!("{recipe_name}:__env"), "", t!("name of the new environment, e.g. rehearsal"), window, cx);
        card = card.child(ui::kv(t!("New environment"), Input::new(&state).small().w(px(260.)), cx));
        let v = state.read(cx).value().to_string();
        (!v.trim().is_empty()).then(|| v.trim().to_string())
    } else {
        let envs: Vec<Value> = app.overview.as_ref().and_then(|o| o["environments"].as_array().cloned()).unwrap_or_default();
        let env_choices: Vec<_> = envs
            .iter()
            .filter(|e| kinds.iter().any(|k| e["kind"] == k.as_str()))
            .map(|e| {
                let name = e["name"].as_str().unwrap_or_default().to_string();
                crate::ely::choice_note(name.clone(), name, e["kind"].as_str().unwrap_or_default().to_string())
            })
            .collect();
        if env_choices.is_empty() {
            card = card.child(ui::kv(t!("Environment"), ui::muted(t!("No environments of this kind."), cx), cx));
        } else {
            let this = cx.entity().downgrade();
            let mut env_select = Select::new(SharedString::from(format!("env-select-{recipe_name}")), env_choices).placeholder(t!("Choose an environment…")).on_change(move |value, _, cx| {
                let value = value.to_string();
                let _ = this.update(cx, |app, cx| {
                    app.form_env = Some(value);
                    cx.notify();
                });
            });
            if let Some(e) = &app.form_env {
                env_select = env_select.selected(e.clone());
            }
            card = card.child(ui::kv(t!("Environment"), div().w(px(420.)).min_w(px(160.)).flex_shrink(1.).child(env_select), cx));
        }
        app.form_env.clone()
    };
    if let Some(env) = &env_value {
        let kind = if creates {
            "new".to_string()
        } else {
            app.overview
                .as_ref()
                .and_then(|o| o["environments"].as_array()?.iter().find(|e| e["name"] == env.as_str()).map(|e| e["kind"].as_str().unwrap_or_default().to_string()))
                .unwrap_or_default()
        };
        // Al crear un entorno, lo que dice la receta; en uno que existe, lo que calcula el daemon
        // (más abajo, cuando se sabe la función).
        if creates {
            let level = recipe["approval"][kind.as_str()].as_str().unwrap_or("simple").to_string();
            card = card.child(ui::kv(t!("Approval"), ui::status_tag(if level == "none" { Health::Ok } else { Health::Warn }, approval_label(&level)), cx));
        }
    }

    let mut args = json!({});
    // Lo que falta para poder pedir el trabajo (función sin elegir, JSON inválido).
    let mut blocked: Option<String> = None;
    for arg in recipe["args"].as_array().cloned().unwrap_or_default() {
        let name = arg["name"].as_str().unwrap_or_default().to_string();
        let key = format!("{recipe_name}:{name}");
        match (arg["type"].as_str(), &env_value) {
            (Some("function"), Some(env)) if !creates => {
                let (field, chosen) = function_field(app, &key, env, &recipe_name, window, cx);
                match chosen {
                    Some(f) => args[name.as_str()] = json!(f),
                    None => blocked = blocked.or(Some(t!("Choose a function.").into())),
                }
                card = card.child(ui::kv(name, field, cx));
                continue;
            }
            (Some("json"), _) => {
                let (field, parsed) = json_field(app, &key, arg["default"].as_str().unwrap_or("{}"), window, cx);
                match parsed {
                    Ok(Some(v)) => args[name.as_str()] = json!(v.to_string()),
                    Ok(None) => {}
                    Err(e) => blocked = blocked.or(Some(e)),
                }
                card = card.child(ui::kv(name, field, cx));
                continue;
            }
            _ => {}
        }
        let state = input(
            app,
            format!("{recipe_name}:{name}"),
            arg["default"].as_str().unwrap_or_default(),
            arg["description"].as_str().unwrap_or_default(),
            window,
            cx,
        );
        let value = state.read(cx).value().to_string();
        if !value.trim().is_empty() {
            args[name.as_str()] = json!(value);
        }
        card = card.child(ui::kv(name, Input::new(&state).small().w(px(520.)).min_w(px(160.)).flex_shrink(1.), cx));
    }

    // La aprobación real (receta, tipo de función y reglas de Configuración) y con qué credencial
    // corre la CLI, según el daemon.
    if let (Some(env), false) = (&env_value, creates) {
        let function = args["function"].as_str().unwrap_or_default().to_string();
        card = card.child(approval_rows(app, &recipe_name, env, &function, cx));
    }

    // Promoción: las precondiciones se pueden revisar antes de pedirla.
    if recipe_name == "promote-prod" {
        if let Some(env) = env_value.clone() {
            let commit = args["commit"].as_str().unwrap_or("dev").to_string();
            let check = Button::new("check-promotion")
                .small()
                .outline()
                .icon(IconName::CircleCheck)
                .label(t!("Check preconditions"))
                .loading(matches!(app.promotion, Some(Load::Loading)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.promotion = Some(Load::Loading);
                    this.get(format!("/api/promotion?env={env}&commit={commit}"), |app, res| app.promotion = Some(res), cx);
                }));
            card = card.child(check);
            match &app.promotion {
                Some(Load::Ready(p)) => {
                    for c in p["preconditions"].as_array().cloned().unwrap_or_default() {
                        card = card.child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .child(ui::dot(if c["ok"] == true { Health::Ok } else { Health::Bad }, cx))
                                .child(div().w(px(300.)).min_w(px(120.)).flex_shrink(1.).child(ui::txt(&c["label"])))
                                .child(div().flex_1().text_color(cx.theme().muted_foreground).child(ui::txt(&c["detail"]))),
                        );
                    }
                }
                Some(Load::Failed(e)) => card = card.child(ui::muted(e.clone(), cx).text_color(cx.theme().danger)),
                _ => {}
            }
        }
    }

    let ready = env_value.is_some() && blocked.is_none();
    let env_for_submit = env_value.clone().unwrap_or_default();
    let submit = Button::new("submit-job")
        .small()
        .icon(IconName::Play)
        .label(t!("Request job"))
        .disabled(!ready)
        .loading(matches!(app.form_result, Some(Load::Loading)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.form_result = Some(Load::Loading);
            let body = json!({ "recipe": recipe_name.clone(), "env": env_for_submit.clone(), "args": args.clone() });
            this.post("/api/jobs".into(), body, |app, res, cx| {
                if let Load::Ready(job) = &res {
                    if let Some(id) = job["id"].as_str() {
                        app.open_job(id.to_string(), cx);
                    }
                }
                app.form_result = Some(res);
            }, cx);
        }));
    card.child(h_flex().gap_3().items_center().child(ui::brand(submit, cx)).child(match &app.form_result {
        _ if env_value.is_some() && blocked.is_some() => ui::muted(blocked.clone().unwrap_or_default(), cx),
        Some(Load::Failed(e)) => ui::muted(e.clone(), cx).text_color(cx.theme().danger),
        Some(Load::Ready(job)) => ui::muted(t!("Requested: {a} ({b})", a = ui::txt(&job["id"]), b = status_label(job["status"].as_str().unwrap_or_default())), cx),
        _ => div(),
    }))
}

/// Estado de los campos de `run-function`: funciones de cada entorno, la elegida y los editores JSON.
#[derive(Default)]
pub struct RunFn {
    /// `GET /api/environments/{env}/functions`, por entorno (la lista va en `lists`).
    pub functions: HashMap<String, Load>,
    /// Las funciones leídas, por entorno (sin copiarlas en cada pintado).
    pub lists: HashMap<String, std::rc::Rc<Vec<Value>>>,
    /// Función elegida, por campo y entorno (`receta:arg@entorno`).
    pub chosen: HashMap<String, String>,
    /// Editores de los argumentos JSON, por campo (`receta:arg`).
    pub editors: HashMap<String, Entity<EditorState>>,
    /// El último payload de partida puesto en cada editor: mientras no se toque, elegir otra
    /// función lo cambia por el de la nueva.
    pub templates: HashMap<String, String>,
    /// `GET /api/approval`: la aprobación real y la credencial, por `receta|entorno|función`.
    pub approvals: HashMap<String, Load>,
}

/// Filas que se muestran a la vez en el selector (la lista no es virtual).
const MAX_FUNCTION_ROWS: usize = 80;

fn load_functions(app: &mut DevtoolApp, env: &str, fresh: bool, cx: &mut Context<DevtoolApp>) {
    app.run_fn.functions.insert(env.to_string(), Load::Loading);
    let e = env.to_string();
    let q = if fresh { "?fresh=true" } else { "" };
    app.get(format!("/api/environments/{env}/functions{q}"), move |app, res| {
        if let Load::Ready(v) = &res {
            let list = v["functions"].as_array().cloned().unwrap_or_default();
            app.run_fn.lists.insert(e.clone(), std::rc::Rc::new(list));
        }
        app.run_fn.functions.insert(e, res);
    }, cx);
}

/// Etiqueta corta de los argumentos: `companyId: id<companies>, dryRun?: boolean`.
fn args_signature(f: &Value) -> String {
    match f["args"].as_array() {
        None => t!("arguments without a validator").into(),
        Some(a) if a.is_empty() => t!("no arguments").into(),
        Some(a) => a
            .iter()
            .map(|x| format!("{}{}: {}", ui::txt(&x["name"]), if x["optional"] == true { "?" } else { "" }, ui::txt(&x["type"])))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// La aprobación que pedirá el trabajo, por qué, y con qué credencial corre; con un enlace a
/// Configuración, donde se cambian las reglas.
fn approval_rows(app: &mut DevtoolApp, recipe: &str, env: &str, function: &str, cx: &mut Context<DevtoolApp>) -> Div {
    let key = format!("{recipe}|{env}|{function}");
    if !app.run_fn.approvals.contains_key(&key) {
        app.run_fn.approvals.insert(key.clone(), Load::Loading);
        let k = key.clone();
        let q = if function.is_empty() { String::new() } else { format!("&function={function}") };
        app.get(format!("/api/approval?recipe={recipe}&env={env}{q}"), move |app, res| {
            app.run_fn.approvals.insert(k, res);
        }, cx);
    }
    let rows = v_flex().gap_2();
    let info = match app.run_fn.approvals.get(&key) {
        Some(Load::Ready(v)) => v.clone(),
        Some(Load::Failed(e)) => return rows.child(ui::kv(t!("Approval"), ui::muted(e.clone(), cx), cx)),
        _ => return rows.child(ui::kv(t!("Approval"), ui::muted(t!("Loading…"), cx), cx)),
    };
    let level = info["approval"].as_str().unwrap_or("simple");
    let why = match info["why"].as_str() {
        Some("query") => t!("a query only reads, and Settings lets this environment's data be read"),
        Some("settings") => t!("set by the rules in Settings › Data and changes"),
        _ => t!("what the recipe asks; the rules in Settings › Data and changes can change it"),
    };
    let via = match info["credential"].as_str() {
        Some("admin-key") => t!("the admin key from the environment's .env"),
        Some("deploy-key") => t!("the deploy key saved in the environment's Keys tab (Convex applies the permissions you gave it)"),
        _ => t!("your npx convex session (save a deploy key in the Keys tab to use it instead)"),
    };
    let settings = Button::new("approval-settings").xsmall().ghost().icon(IconName::Settings).label(t!("Change in Settings")).on_click(cx.listener(|this, _, window, cx| {
        this.open_page(crate::app::Page::Settings, window, cx);
    }));
    rows.child(ui::kv(
        t!("Approval"),
        h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(ui::status_tag(if level == "none" { Health::Ok } else { Health::Warn }, approval_label(level)))
            .child(ui::muted(why, cx).text_xs())
            .child(settings),
        cx,
    ))
    .child(ui::kv(t!("Runs with"), ui::muted(via, cx), cx))
}

/// Payload de partida con los argumentos obligatorios de la función.
fn payload_template(f: &Value) -> String {
    let mut obj = serde_json::Map::new();
    for a in f["args"].as_array().into_iter().flatten().filter(|a| a["optional"] != true) {
        let t = a["type"].as_str().unwrap_or_default();
        let v = match t {
            "string" => json!(""),
            "number" | "int64" => json!(0),
            "boolean" => json!(false),
            _ if t.starts_with("id<") || t.starts_with('"') => json!(""),
            _ if t.ends_with("[]") => json!([]),
            _ if t.starts_with("object") || t.starts_with("record") => json!({}),
            _ => Value::Null,
        };
        obj.insert(ui::txt(&a["name"]).to_string(), v);
    }
    if obj.is_empty() {
        "{}".into()
    } else {
        serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_else(|_| "{}".into())
    }
}

/// Funciones que coinciden con lo escrito (en la ruta o en la nota), hasta `MAX_FUNCTION_ROWS`.
pub fn filter_functions<'a>(list: &'a [Value], query: &str) -> (Vec<&'a Value>, usize) {
    let q = query.trim().to_lowercase();
    let all: Vec<&Value> = list
        .iter()
        .filter(|f| q.is_empty() || f["identifier"].as_str().is_some_and(|i| i.to_lowercase().contains(&q)) || function_note(f).to_lowercase().contains(&q))
        .collect();
    let total = all.len();
    (all.into_iter().take(MAX_FUNCTION_ROWS).collect(), total)
}

fn function_note(f: &Value) -> String {
    let kind = f["kind"].as_str().unwrap_or_default();
    if f["visibility"] == "internal" {
        t!("{kind} · internal", kind = kind)
    } else {
        kind.to_string()
    }
}

/// Selector de funciones del entorno con búsqueda; devuelve el campo y la función elegida.
fn function_field(app: &mut DevtoolApp, key: &str, env: &str, recipe: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) -> (AnyElement, Option<String>) {
    if !app.run_fn.functions.contains_key(env) {
        load_functions(app, env, false, cx);
    }
    let slot = format!("{key}@{env}");
    let chosen = app.run_fn.chosen.get(&slot).cloned();
    let state = app.run_fn.functions.get(env).map(|l| match l {
        Load::Failed(e) => Load::Failed(e.clone()),
        Load::Ready(_) => Load::Ready(Value::Null),
        Load::Loading => Load::Loading,
    });
    let list = match state {
        Some(Load::Ready(_)) => app.run_fn.lists.get(env).cloned().unwrap_or_default(),
        Some(Load::Failed(e)) => {
            // Sin lista (backend apagado, CLI con error): se puede escribir la ruta a mano.
            let state = input(app, format!("{key}:manual"), "", t!("path, e.g. migrations:backfillDefaults"), window, cx);
            let value = state.read(cx).value().trim().to_string();
            let env_r = env.to_string();
            let field = v_flex()
                .gap_1()
                .child(Input::new(&state).small().w(px(520.)).min_w(px(160.)).flex_shrink(1.))
                .child(
                    h_flex()
                        .gap_2()
                        .child(ui::muted(t!("Couldn't read the functions: {e}", e = e), cx).text_color(cx.theme().danger))
                        .child(Button::new(SharedString::from(format!("fn-retry-{slot}"))).xsmall().ghost().icon(IconName::RefreshCw).label(t!("Retry")).on_click(cx.listener(
                            move |this, _, _, cx| load_functions(this, &env_r, true, cx),
                        ))),
                )
                .into_any_element();
            return (field, (!value.is_empty()).then_some(value));
        }
        _ => return (ui::muted(t!("Reading the environment's functions…"), cx).into_any_element(), None),
    };

    let search = app.ely_input(&format!("{slot}:search"), t!("Type to filter, e.g. perm"), window, cx);
    let text = search.read(cx).text().to_string();
    // Con la elegida escrita en el campo se muestran todas (como hace el Combobox).
    let query = if chosen.as_deref() == Some(text.as_str()) { "" } else { text.as_str() };
    let (rows, total) = filter_functions(&list, query);
    let mut choices: Vec<_> = rows.iter().map(|f| crate::ely::choice_note(ui::txt(&f["identifier"]), ui::txt(&f["identifier"]), function_note(f))).collect();
    let picked = chosen.as_ref().and_then(|c| list.iter().find(|f| f["identifier"] == c.as_str()));
    if let Some(f) = picked {
        if !rows.iter().any(|r| r["identifier"] == f["identifier"]) {
            choices.push(crate::ely::choice_note(ui::txt(&f["identifier"]), ui::txt(&f["identifier"]), function_note(f)));
        }
    }

    let this = cx.entity().downgrade();
    let (slot_c, editor_key) = (slot.clone(), format!("{recipe}:payload"));
    let by_id: HashMap<String, Value> = rows.iter().chain(picked.iter()).map(|f| (ui::txt(&f["identifier"]).to_string(), (*f).clone())).collect();
    let mut combo = ely_core::forms::Combobox::new(SharedString::from(format!("fn-combo-{slot}")), &search, choices).on_change(move |value, window, cx| {
        let value = value.to_string();
        let template = by_id.get(&value).map(payload_template);
        let _ = this.update(cx, |app, cx| {
            app.run_fn.chosen.insert(slot_c.clone(), value);
            // El payload de partida con los argumentos de la función, si el actual está vacío o
            // es el de partida de la anterior (sin tocar).
            if let (Some(t), Some(editor)) = (template, app.run_fn.editors.get(&editor_key).cloned()) {
                let current = editor.read(cx).value().to_string();
                let untouched = current.trim().is_empty() || current.trim() == "{}" || app.run_fn.templates.get(&editor_key).is_some_and(|prev| prev.trim() == current.trim());
                if untouched {
                    editor.update(cx, |s, cx| s.set_value(t.clone(), window, cx));
                    app.run_fn.templates.insert(editor_key.clone(), t);
                }
            }
            cx.notify();
        });
    });
    if let Some(c) = &chosen {
        combo = combo.selected(c.clone());
    }

    let (internal, kinds) = (list.iter().filter(|f| f["visibility"] == "internal").count(), list.len());
    let env_r = env.to_string();
    let mut info = h_flex().gap_2().items_center();
    info = info.child(ui::muted(
        if total > rows.len() {
            t!("{a} of {total} match; keep typing to narrow down.", a = rows.len(), total = total)
        } else if query.is_empty() {
            t!("{kinds} functions ({internal} internal).", kinds = kinds, internal = internal)
        } else {
            t!("{total} match.", total = total)
        },
        cx,
    ));
    info = info.child(Button::new(SharedString::from(format!("fn-refresh-{slot}"))).xsmall().ghost().icon(IconName::RefreshCw).label(t!("Refresh")).on_click(cx.listener(
        move |this, _, _, cx| load_functions(this, &env_r, true, cx),
    )));
    let mut field = v_flex().gap_1().child(div().w(px(520.)).min_w(px(160.)).flex_shrink(1.).child(combo)).child(info);
    if let Some(f) = picked {
        field = field.child(ui::muted(format!("{} · {}", function_note(f), args_signature(f)), cx));
        let optional: Vec<&str> = f["args"].as_array().into_iter().flatten().filter(|a| a["optional"] == true).filter_map(|a| a["name"].as_str()).collect();
        if !optional.is_empty() {
            field = field.child(ui::muted(t!("Optional (add them to the payload if you need them): {a}", a = optional.join(", ")), cx).text_xs());
        }
    }
    (field.into_any_element(), chosen.filter(|c| list.iter().any(|f| f["identifier"] == c.as_str())))
}

/// Error de un payload: JSON inválido (con línea y columna) o algo que no es un objeto.
pub fn check_payload(text: &str) -> Result<Option<Value>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(text) {
        Ok(v @ Value::Object(_)) => Ok(Some(v)),
        Ok(_) => Err(t!("Arguments must be a JSON object: { … }").into()),
        Err(e) => Err(t!("Invalid JSON at line {a}, column {b}: {c}", a = e.line(), b = e.column(), c = json_error_text(&e))),
    }
}

fn json_error_text(e: &serde_json::Error) -> String {
    // «expected `,` or `}` at line 2 column 5» → sin la posición (va aparte).
    let s = e.to_string();
    s.split(" at line ").next().unwrap_or(&s).to_string()
}

/// Editor de código JSON; devuelve el campo y el objeto leído (o el error que no deja pedir).
fn json_field(app: &mut DevtoolApp, key: &str, default: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) -> (AnyElement, Result<Option<Value>, String>) {
    let default = default.to_string();
    let editor = app
        .run_fn
        .editors
        .entry(key.to_string())
        .or_insert_with(|| cx.new(|cx| EditorState::new(window, cx).language("json").default_value(default)))
        .clone();
    let text = editor.read(cx).value().to_string();
    let parsed = check_payload(&text);
    let pretty = parsed.as_ref().ok().and_then(|v| v.as_ref()).and_then(|v| serde_json::to_string_pretty(v).ok()).filter(|p| *p != text);
    let ed = editor.clone();
    let mut foot = h_flex().gap_2().items_center();
    foot = match &parsed {
        Err(e) => foot.child(ui::dot(Health::Bad, cx)).child(ui::muted(e.clone(), cx).text_color(cx.theme().danger)),
        Ok(_) => foot.child(ui::dot(Health::Ok, cx)).child(ui::muted(t!("Valid JSON"), cx)),
    };
    foot = foot.when_some(pretty, |d, p| {
        d.child(Button::new(SharedString::from(format!("json-format-{key}"))).xsmall().ghost().icon(IconName::CircleCheck).label(t!("Format")).on_click(move |_, window, cx| {
            ed.update(cx, |s, cx| s.set_value(p.clone(), window, cx));
        }))
    });
    let field = v_flex()
        .gap_1()
        .w(px(520.))
        .min_w(px(160.))
        .flex_shrink(1.)
        .child(Editor::new(&editor).h(px(160.)))
        .child(foot)
        .into_any_element();
    (field, parsed)
}

/// Lo que distingue a un trabajo de otros de la misma receta: la función o el commit.
fn job_subject(job: &Value) -> String {
    if let Some(f) = job["args"]["function"].as_str() {
        return f.rsplit(':').next().unwrap_or(f).to_string();
    }
    if let Some(p) = job["args"]["project"].as_str() {
        return p.to_string();
    }
    job["commit"].as_str().map(|c| c[..c.len().min(8)].to_string()).unwrap_or_default()
}

fn status_tone(status: &str) -> Tone {
    match status {
        "succeeded" => Tone::Success,
        "failed" | "rejected" => Tone::Danger,
        "running" | "queued" => Tone::Info,
        "waiting-approval" => Tone::Warning,
        _ => Tone::Neutral,
    }
}

/// Los trabajos recientes en una tabla: ordenable por columna y con su fila elegida.
fn list(app: &mut DevtoolApp, env: Option<&str>, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let card = ui::card(cx).child(ui::section(t!("Recent"), cx));
    let mine = |job: &&Value| env.is_none_or(|e| job["env"] == e);
    if !app.jobs.iter().any(|j| mine(&j)) {
        return card.child(ui::muted(if env.is_some() { t!("No jobs in this environment yet.") } else { t!("No jobs yet.") }, cx));
    }
    let rows: Vec<Row> = app
        .jobs
        .iter()
        .filter(mine)
        .take(60)
        .map(|job| {
            let status = job["status"].as_str().unwrap_or_default();
            Row::new(
                job["id"].as_str().unwrap_or_default().to_string(),
                [
                    Cell::Tag(status_label(status).into(), status_tone(status)),
                    Cell::Text(ui::txt(&job["title"])),
                    Cell::Text(job_subject(job).into()),
                    Cell::Text(ui::txt(&job["env"])),
                    Cell::Text(ui::txt(&job["requestedBy"])),
                    Cell::Text(job["createdAt"].as_i64().map(ui::ago).unwrap_or_default().into()),
                    Cell::Text(job["error"].as_str().unwrap_or_default().to_string().into()),
                ],
            )
        })
        .collect();
    let columns = [
        Column::new("status", t!("Status")).width(rems(9.)),
        Column::new("recipe", t!("Recipe")).width(rems(14.)),
        Column::new("subject", t!("Details")).width(rems(10.)),
        Column::new("env", t!("Environment")).width(rems(10.)),
        Column::new("by", t!("Requested by")).width(rems(6.)),
        Column::new("when", t!("When")).width(rems(7.)).unsorted(),
        Column::new("error", t!("Error")).unsorted(),
    ];
    let this = cx.entity().downgrade();
    // Con selección (aunque vacía) la tabla acepta clics en las filas.
    let table = DataTable::new("jobs-table", columns).rows(rows).paged(12).selected(app.job.clone()).on_select(move |keys, _, cx| {
        if let Some(id) = keys.last().map(|k| k.to_string()) {
            let _ = this.update(cx, |app, cx| app.open_job(id, cx));
        }
    });
    card.child(table)
}

fn detail(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let Some(job) = app.job_detail.clone() else {
        return div().into_any_element();
    };
    let id = job["id"].as_str().unwrap_or_default().to_string();
    let status = job["status"].as_str().unwrap_or_default().to_string();
    let total = job["steps"].as_array().map(Vec::len).unwrap_or(0).max(1);
    let done = job["steps"].as_array().into_iter().flatten().filter(|s| matches!(s["status"].as_str(), Some("ok" | "skipped"))).count();
    let progress = done as f32 * 100.0 / total as f32;

    // Cada paso como bloque: su salida (el tramo del log entre «▶ paso» y el siguiente), su código y su duración.
    let outputs = crate::ely::step_outputs(&app.job_log);
    let mut steps = v_flex().gap_1();
    for (i, step) in job["steps"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        let name = step["name"].as_str().unwrap_or_default().to_string();
        let st = step["status"].as_str().unwrap_or("pending");
        let output = outputs.iter().find(|(n, _)| *n == name).map(|(_, o)| o.clone()).unwrap_or_default();
        if matches!(st, "pending" | "skipped") && output.is_empty() {
            steps = steps.child(
                h_flex()
                    .gap_2()
                    .h(px(26.))
                    .px_2()
                    .items_center()
                    .border_1()
                    .border_color(crate::theme::Tokens::current(cx).card_border)
                    .child(ui::dot(Health::Unknown, cx))
                    .child(ui::muted(name, cx))
                    .child(div().flex_1())
                    .child(ui::muted(if st == "skipped" { t!("skipped") } else { t!("pending") }, cx)),
            );
            continue;
        }
        let took = match (step["startedAt"].as_i64(), step["endedAt"].as_i64()) {
            (Some(a), Some(b)) => std::time::Duration::from_millis((b - a).max(0) as u64),
            _ => std::time::Duration::ZERO,
        };
        let state = match st {
            "running" => CommandState::Running,
            "failed" => CommandState::Done { code: 1, took },
            _ => CommandState::Done { code: 0, took },
        };
        let message = step["message"].as_str().unwrap_or_default();
        let text = if output.trim().is_empty() { message.to_string() } else { output };
        let key = format!("{id}:{i}");
        let folded = app.step_folds.get(&key).copied().unwrap_or(!matches!(st, "running" | "failed"));
        let this = cx.entity().downgrade();
        steps = steps.child(
            CommandBlock::new(SharedString::from(format!("step-{key}")), ui::txt(&job["env"]), name, text, state).folded(folded).on_fold(move |f, _, cx| {
                let key = key.clone();
                let _ = this.update(cx, |app, cx| {
                    app.step_folds.insert(key, f);
                    cx.notify();
                });
            }),
        );
    }

    let can_cancel = matches!(status.as_str(), "running" | "queued" | "waiting-approval");
    let can_retry = matches!(status.as_str(), "failed" | "cancelled");
    let (cancel_id, retry_id) = (id.clone(), id.clone());
    let actions = h_flex()
        .gap_2()
        .when(can_cancel, |d| {
            d.child(Button::new("cancel-job").small().outline().icon(IconName::CircleX).label(t!("Cancel")).on_click(cx.listener(move |this, _, _, cx| {
                this.post(format!("/api/jobs/{cancel_id}/cancel"), json!({}), |app, res, _| app.action = Some(res), cx);
            })))
        })
        .when(can_retry, |d| {
            d.child(Button::new("retry-job").small().outline().icon(IconName::RefreshCw).label(t!("Retry from the failed step")).on_click(cx.listener(
                move |this, _, _, cx| {
                    let id = retry_id.clone();
                    this.post(format!("/api/jobs/{id}/retry"), json!({}), move |app, res, cx| {
                        app.action = Some(res);
                        app.open_job(id, cx);
                    }, cx);
                },
            )))
        });

    let search = app.ely_input("job-args-search", t!("Search arguments…"), window, cx);
    let args_text = serde_json::to_string(&job["args"]).unwrap_or_default();
    let lines = crate::ely::log_lines(&app.job_log);
    let least = app.log_level("job");
    let this = cx.entity().downgrade();
    let log = LogViewer::new("job-log-viewer", lines).least(least).follow(status == "running").on_least(move |l, _, cx| {
        let _ = this.update(cx, |app, cx| {
            app.log_levels.insert("job".into(), l);
            cx.notify();
        });
    });
    let has_args = job["args"].as_object().is_some_and(|o| !o.is_empty());
    let tab = if app.job_tab == 1 && !has_args { 0 } else { app.job_tab };

    // Quién y cuándo, en una línea (sin tabla aparte).
    let mut meta = vec![
        format!("{} {}", t!("Requested by"), ui::txt(&job["requestedBy"])),
        job["createdAt"].as_i64().map(ui::ago).unwrap_or_default().to_string(),
    ];
    if job["approvedBy"].is_string() {
        meta.push(format!("{} {}", t!("Approved by"), ui::txt(&job["approvedBy"])));
    }
    if let Some(c) = job["commit"].as_str() {
        meta.push(format!("{} {}", t!("Commit"), &c[..c.len().min(8)]));
    }
    meta.push(id.clone());

    let mut tabs = TabBar::new("job-tabs").underline().small().selected_index(tab);
    tabs = tabs.child(Tab::new().label(t!("Steps · {done}/{total}", done = done, total = total)));
    tabs = tabs.child(Tab::new().label(t!("Arguments")).disabled(!has_args));
    tabs = tabs.child(Tab::new().label(t!("Full log")));
    let tabs = tabs.on_click(cx.listener(|this, ix: &usize, _, cx| {
        this.job_tab = *ix;
        cx.notify();
    }));
    let body = match tab {
        1 => ui::contained().max_h(px(380.)).child(JsonViewer::new("job-args", args_text, &search)).into_any_element(),
        2 => ui::chained_log("job-log-viewer", log.into_any_element()).h(px(380.)).into_any_element(),
        _ => steps.into_any_element(),
    };

    // Un solo bloque: encabezado con estado y acciones, quién y cuándo, el error si hubo, y en
    // pestañas los pasos, los argumentos y el log completo.
    ui::card(cx)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{} · {}", ui::txt(&job["title"]), ui::txt(&job["env"]))))
                .child(ui::status_tag(status_health(&status), status_label(&status)))
                .child(div().flex_1())
                .child(actions),
        )
        .child(ui::muted(meta.join(" · "), cx).text_xs())
        .when(status == "running", |c| c.child(Progress::new("job-progress").value(progress)))
        .when_some(job["error"].as_str().map(str::to_string), |c, e| c.child(div().text_sm().text_color(cx.theme().danger).child(e)))
        .child(tabs)
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn valida_el_payload() {
        assert_eq!(super::check_payload("{}"), Ok(Some(json!({}))));
        assert_eq!(super::check_payload("  "), Ok(None));
        assert!(super::check_payload("[1]").unwrap_err().contains("object"));
        let e = super::check_payload("{\n  \"a\": 1,\n}").unwrap_err();
        assert!(e.contains("line 3"), "{e}");
    }

    #[test]
    fn filtra_y_acota_las_funciones() {
        let list: Vec<_> = (0..200).map(|i| json!({ "identifier": format!("m{i}:f"), "kind": "query", "visibility": "public" })).collect();
        let mut list = list;
        list.push(json!({ "identifier": "system/permissions/mutations:backfill", "kind": "mutation", "visibility": "internal" }));
        let (rows, total) = super::filter_functions(&list, "PERM");
        assert_eq!((rows.len(), total), (1, 1));
        let (rows, total) = super::filter_functions(&list, "");
        assert_eq!((rows.len(), total), (super::MAX_FUNCTION_ROWS, 201));
        assert_eq!(super::filter_functions(&list, "interna").1, 1);
    }

    #[test]
    fn plantilla_con_los_obligatorios() {
        let f = json!({ "args": [{ "name": "companyId", "type": "id<companies>", "optional": false }, { "name": "dry", "type": "boolean", "optional": true }] });
        assert_eq!(serde_json::from_str::<serde_json::Value>(&super::payload_template(&f)).unwrap(), json!({ "companyId": "" }));
        assert_eq!(super::payload_template(&json!({ "args": [] })), "{}");
    }
}
