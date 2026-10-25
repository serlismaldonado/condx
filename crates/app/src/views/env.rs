// Un entorno es la entrada a todo lo suyo, en pestañas: resumen (Convex y
// despliegue), servicios (el backend, el deployer y el frontend del devbox),
// variables (solo nombres), logs de funciones, despliegues registrados,
// esquema, sus trabajos, la promoción a producción (con sus aprobaciones) y
// las pestañas de la app conectadas a él («En vivo»). Cada tipo de entorno
// muestra solo las que le aplican (`tabs_for`).

use ely_core::forms::SearchInput;
use ely_core::tables::{Cell, Column, DataTable, Row};
use ely_git::Commit;
use ely_git::CommitList;
use ely_terminal::LogViewer;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, EnvTab, Load};
use crate::ui;

/// Las pestañas de un entorno según su tipo, de lo que más se mira a lo que menos: el resumen (en
/// el devbox, con sus servicios arriba), la promoción (producción), lo que pasa ahora (En vivo,
/// logs, trabajos), los despliegues (cloud) y al final variables, esquema y claves. En vivo, solo en el
/// ERP (la telemetría es de su app web).
pub fn tabs_for(kind: &str, erp: bool) -> Vec<EnvTab> {
    let local = matches!(kind, "devbox" | "throwaway");
    // Flujos va en todos: es por donde se despliega a cualquier entorno.
    let mut tabs = vec![EnvTab::Summary, EnvTab::Workflows];
    if erp {
        tabs.push(EnvTab::Live);
    }
    tabs.extend([EnvTab::Logs, EnvTab::Jobs]);
    if !local {
        tabs.push(EnvTab::Deploys);
    }
    tabs.extend([EnvTab::Data, EnvTab::Vars, EnvTab::Schema, EnvTab::Keys]);
    tabs
}

pub fn render(app: &mut DevtoolApp, name: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let env = app
        .overview
        .as_ref()
        .and_then(|o| o["environments"].as_array()?.iter().find(|e| e["name"] == name).cloned());
    let Some(env) = env else {
        return super::empty(t!("The daemon doesn't know the environment “{name}”.", name = name), cx).into_any_element();
    };
    let kind = env["kind"].as_str().unwrap_or_default().to_string();
    let dashboard = env["convex"]["deployment"].as_str().map(|d| format!("https://dashboard.convex.dev/d/{d}"));
    // Esquema en pantalla completa: solo el esquema, sin pestañas.
    if app.schema_fullscreen && app.env_tab == EnvTab::Schema {
        return v_flex().size_full().p_3().child(super::schema::render(app, name, window, cx)).into_any_element();
    }

    let erp = app.project == "heskala-erp";
    let list = tabs_for(&kind, erp);
    // Si la pestaña elegida no existe en este entorno (venía de otro), se ve el resumen.
    let current = if list.contains(&app.env_tab) { app.env_tab } else { EnvTab::Summary };
    let n = name.to_string();
    let tab_list = list.clone();
    let mut tabs = TabBar::new("env-tabs").underline().small().selected_index(list.iter().position(|t| *t == current).unwrap_or(0));
    for t in &list {
        let mut tab = Tab::new().label(t.label());
        // Lo que espera al usuario se ve desde la pestaña.
        let waiting = app.approvals.iter().filter(|j| j["env"] == name).count();
        if *t == EnvTab::Workflows && waiting > 0 {
            tab = tab.label(t!("Workflows · {waiting}", waiting = waiting));
        }
        tabs = tabs.child(tab);
    }
    let tabs = tabs.on_click(cx.listener(move |this, ix: &usize, _, cx| {
        if let Some(t) = tab_list.get(*ix) {
            this.open_env_tab(n.clone(), *t, cx);
        }
    }));

    // Una sola fila arriba: el entorno, sus pestañas y el enlace al dashboard (el resto del
    // encabezado va en el resumen).
    // El nombre y el tipo del entorno están en la barra de título; aquí, las pestañas a la
    // izquierda y las acciones a la derecha.
    let mut top = h_flex().gap_3().items_center().child(div().flex_1().min_w_0().child(tabs));
    if kind == "devbox" && current == EnvTab::Summary {
        top = top.child(super::local::service_actions(app, cx));
    }
    if let Some(url) = dashboard {
        top = top.child(Button::new("open-dashboard").ghost().xsmall().icon(IconName::ExternalLink).label(t!("Dashboard")).tooltip(t!("Open the Convex dashboard")).on_click(move |_, _, cx| cx.open_url(&url)));
    }

    let url = ui::txt(&env["convex"]["url"]).to_string();
    let body: AnyElement = match current {
        // En el devbox, el resumen empieza por sus servicios: es lo primero que hay que ver.
        EnvTab::Summary | EnvTab::Services if kind == "devbox" => {
            let ficha = summary(&env, cx).into_any_element();
            div().mx(px(-16.)).mt(px(-12.)).child(super::local::render_with(app, false, Some((t!("Convex & deploy").into(), ficha)), window, cx)).into_any_element()
        }
        EnvTab::Services => summary(&env, cx).into_any_element(),
        EnvTab::Vars => vars(app, name, window, cx).into_any_element(),
        EnvTab::Logs => logs(app, name, window, cx).into_any_element(),
        EnvTab::Deploys => deployments(app, name, cx).into_any_element(),
        EnvTab::Schema => super::schema::render(app, name, window, cx).into_any_element(),
        EnvTab::Jobs => super::jobs::render_for(app, Some(name), window, cx).into_any_element(),
        EnvTab::Workflows => v_flex()
            .gap_3()
            .child(div().px_4().pt_3().child(super::workflows::render_for_env(app, name, window, cx)))
            .child(div().px_4().child(ui::section(t!("Approvals for this environment"), cx)))
            .child(super::approvals::render_for(app, Some(name), window, cx))
            .into_any_element(),
        EnvTab::Live => super::live::render_for(app, Some(&url), window, cx).into_any_element(),
        // Una vista propia por entorno, dibujada con caché: los redibujos de la app (el sondeo
        // del daemon) no la reconstruyen; se redibuja cuando ella misma cambia.
        // Hasta que llegan las reglas de datos no se sabe si puede leer: se espera.
        EnvTab::Data if app.policy.is_none() => div().px_4().pt_3().child(ui::muted(t!("Loading…"), cx)).into_any_element(),
        EnvTab::Data => {
            let daemon = app.daemon.clone();
            let readable = app.can_read(&kind);
            let (env_name, env_kind) = (name.to_string(), kind.clone());
            let view = app.data_views.entry(name.to_string()).or_insert_with(|| cx.new(|cx| super::data::DataView::new(env_name, env_kind, readable, daemon, window, cx))).clone();
            view.cached(StyleRefinement::default().size_full()).into_any_element()
        }
        EnvTab::Keys => {
            let (daemon, me) = (app.daemon.clone(), cx.entity().downgrade());
            let (env_name, env_kind) = (name.to_string(), kind.clone());
            app.key_views.entry(name.to_string()).or_insert_with(|| cx.new(|cx| super::keys::KeysView::new(env_name, env_kind, daemon, me, window, cx))).clone().into_any_element()
        }
        EnvTab::Summary => summary(&env, cx).into_any_element(),
    };
    // Las vistas que vienen de páginas propias ya traen su margen.
    let padded = matches!(current, EnvTab::Summary | EnvTab::Vars | EnvTab::Logs | EnvTab::Deploys | EnvTab::Schema);
    // Datos llena el alto de la página (sin scroll de página) y se desplaza por dentro.
    let fill = current == EnvTab::Data;
    let body = if padded { body } else { div().mx(px(-16.)).when(fill, |d| d.flex_1().min_h_0()).child(body).into_any_element() };
    v_flex().gap_3().px_4().pt_1().when(!fill, |d| d.pb_4()).when(fill, |d| d.size_full()).child(top).child(body).into_any_element()
}

fn summary(env: &Value, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let convex = &env["convex"];
    let deployed = &env["deployed"];
    let state = if convex["reachable"] == true {
        ui::status_tag(ui::Health::Ok, t!("responding · {a} ms", a = convex["latencyMs"]))
    } else {
        ui::status_tag(ui::Health::Bad, ui::txt(&convex["error"]))
    };
    let behind = match deployed["behindBranch"].as_i64() {
        Some(0) => t!("up to date").to_string(),
        Some(n) => t!("{n} commits", n = n),
        None => "—".into(),
    };
    v_flex()
        .gap_4()
        .child(
            DescriptionList::new()
                .columns(2)
                .bordered(true)
                .label_width(px(150.))
                .item(t!("Environment"), ui::txt(&env["label"]).to_string(), 2)
                .item("URL", ui::mono(ui::txt(&convex["url"])).into_any_element(), 2)
                .item(t!("Deployment"), ui::txt(&convex["deployment"]).to_string(), 1)
                .item(t!("Status"), h_flex().child(state).into_any_element(), 1)
                .item(t!("Backend version"), ui::txt(&convex["version"]).to_string(), 1)
                .item(t!("Checked"), convex["checkedAt"].as_i64().map(ui::ago).unwrap_or_else(|| "—".into()), 1),
        )
        .child(ui::section(t!("Deploy"), cx))
        .child(
            DescriptionList::new()
                .columns(2)
                .bordered(true)
                .label_width(px(150.))
                .item(t!("Commit"), ui::mono(ui::txt(&deployed["commit"])).into_any_element(), 1)
                .item(t!("Expected branch"), ui::txt(&deployed["branch"]).to_string(), 1)
                .item(t!("Behind {a}", a = ui::txt(&deployed["branch"])), behind, 1)
                .item(t!("Source"), source_text(&deployed["source"]).to_string(), 1),
        )
}

fn vars(app: &mut DevtoolApp, name: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let n = name.to_string();
    let search = app.ely_input(&format!("vars-search-{name}"), t!("Search variables…"), window, cx);
    let query = search.read(cx).text().to_string();
    let card = ui::card(cx).child(
        h_flex()
            .gap_2()
            .items_center()
            .child(ui::section(t!("Environment variables (names only)"), cx))
            .child(div().flex_1())
            .child(div().w(px(240.)).min_w(px(120.)).flex_shrink(1.).child(SearchInput::new("vars-search", &search)))
            .child(
                Button::new("load-vars")
                    .small()
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label(t!("Load"))
                    .loading(matches!(app.env_vars.get(name), Some(Load::Loading)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let key = n.clone();
                        this.env_vars.insert(key.clone(), Load::Loading);
                        this.call("get_env_var_names", json!({ "name": key.clone() }), move |app, r| {
                            app.env_vars.insert(key, r);
                        }, cx);
                    })),
            ),
    );
    match app.env_vars.get(name) {
        None => card.child(ui::muted(t!("Not loaded. Values are never shown."), cx)),
        Some(Load::Loading) => card.child(ui::muted("Cargando…", cx)),
        Some(Load::Failed(e)) => card.child(ui::muted(e.clone(), cx).text_color(cx.theme().danger)),
        Some(Load::Ready(v)) => {
            let rows: Vec<Row> = v["names"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str())
                .map(|n| Row::new(n.to_string(), [Cell::Text(n.to_string().into()), Cell::Text(t!("•••• (never shown)").into())]))
                .collect();
            let count = rows.len();
            card.child(ui::muted(t!("{count} variables", count = count), cx)).child(
                DataTable::new(SharedString::from(format!("vars-{name}")), [Column::new("name", t!("Name")), Column::new("value", t!("Value")).unsorted()])
                    .rows(rows)
                    .query(query)
                    .paged(25),
            )
        }
    }
}

fn logs(app: &mut DevtoolApp, name: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let n = name.to_string();
    let search = app.ely_input(&format!("logs-search-{name}"), t!("Search logs (function or text)…"), window, cx);
    let query = search.read(cx).text().to_string();
    let key = format!("env:{name}");
    let least = app.log_level(&key);
    let card = ui::card(cx).child(
        h_flex()
            .gap_2()
            .items_center()
            .child(ui::section(t!("Function logs"), cx))
            .child(div().flex_1())
            .child(div().w(px(280.)).min_w(px(120.)).flex_shrink(1.).child(SearchInput::new("logs-search", &search)))
            .child(
                Button::new("load-logs")
                    .small()
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label(t!("Load last 300"))
                    .loading(matches!(app.env_logs.get(name), Some(Load::Loading)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let key = n.clone();
                        this.env_logs.insert(key.clone(), Load::Loading);
                        let args = json!({ "name": key.clone(), "limit": 300, "errorsOnly": false });
                        this.call("get_function_logs", args, move |app, r| {
                            app.env_logs.insert(key, r);
                        }, cx);
                    })),
            ),
    );
    match app.env_logs.get(name) {
        None => card.child(ui::muted(t!("Not loaded (takes about 8 s). Minimum level and search filter without fetching again."), cx)),
        Some(Load::Loading) => card.child(ui::muted(t!("Reading logs…"), cx)),
        Some(Load::Failed(e)) => card.child(ui::muted(e.clone(), cx).text_color(cx.theme().danger)),
        Some(Load::Ready(v)) => {
            let raw = v["logs"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let lines = crate::ely::function_log_lines(raw);
            if lines.is_empty() {
                return card.child(ui::muted(t!("No logs in this range."), cx));
            }
            // Arriba, el rendimiento por función de estas mismas ejecuciones.
            let insights = super::insights::table(name, raw, cx);
            let this = cx.entity().downgrade();
            let card = card.child(
                ui::chained_log(
                    SharedString::from(format!("env-logs-{name}")),
                    LogViewer::new(SharedString::from(format!("env-logs-{name}")), lines)
                        .least(least)
                        .query(query)
                        .on_least(move |l, _, cx| {
                            let key = key.clone();
                            let _ = this.update(cx, |app, cx| {
                                app.log_levels.insert(key, l);
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                )
                .h(px(460.)),
            );
            v_flex().gap_3().children(insights).child(card)
        }
    }
}

fn deployments(app: &mut DevtoolApp, name: &str, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let card = ui::card(cx).child(ui::section(t!("Deploys made by the daemon"), cx));
    match app.env_deployments.get(name) {
        None | Some(Load::Loading) => card.child(ui::muted("Cargando…", cx)),
        Some(Load::Failed(e)) => card.child(ui::muted(e.clone(), cx).text_color(cx.theme().danger)),
        Some(Load::Ready(v)) => {
            let list = v.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                return card.child(ui::muted(t!("None yet: deploys made outside the daemon (the devbox deployer, a manual convex deploy) aren't recorded."), cx));
            }
            let commits: Vec<Commit> = list
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let sha = d["commit"].as_str().unwrap_or_default();
                    Commit {
                        id: sha.to_string().into(),
                        parents: vec![],
                        subject: d["subject"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| t!("deployed by {a}", a = ui::txt(&d["by"]))).into(),
                        author: d["author"].as_str().unwrap_or_default().to_string().into(),
                        when: d["at"].as_i64().map(ui::ago).unwrap_or_default().into(),
                        refs: if i == 0 { vec!["actual".into()] } else { vec![] },
                    }
                })
                .collect();
            let count = commits.len();
            card.child(div().h(px((count * 36 + 8) as f32)).child(CommitList::new(SharedString::from(format!("deploys-{name}")), std::rc::Rc::new(commits))))
        }
    }
}

fn source_text(v: &Value) -> SharedString {
    match v.as_str() {
        Some("checkout") => t!("checkout watched by the deployer").into(),
        Some("history") => t!("daemon deploy history").into(),
        _ => t!("unknown (no deploys recorded by the daemon)").into(),
    }
}
