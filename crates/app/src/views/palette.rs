// Paleta de comandos (cmd-K), con la CommandPalette de Ely: ir a una página, a un entorno o a
// una de sus pestañas, abrir una tabla de Datos, cambiar de proyecto y las acciones de siempre.
// Cada comando es un valor de texto (`page:…`, `env:…`, `table:…`) que `run` interpreta.

use ely_core::navigation::{Command, CommandPalette};
use ely_core::primitives::IconName;
use gpui_kit::*;
use serde_json::Value;

use crate::app::{DevtoolApp, EnvTab, Page};

const RECENT: usize = 5;

const TABS: [(EnvTab, &str); 7] = [
    (EnvTab::Data, "data"),
    (EnvTab::Schema, "schema"),
    (EnvTab::Logs, "logs"),
    (EnvTab::Live, "live"),
    (EnvTab::Deploys, "deploys"),
    (EnvTab::Jobs, "jobs"),
    (EnvTab::Keys, "keys"),
];

fn page_of(key: &str) -> Option<Page> {
    Some(match key {
        "overview" => Page::Overview,
        "local" => Page::Local,
        "workflows" => Page::Workflows,
        "repo" => Page::Repo,
        "terminal" => Page::Terminal,
        "jobs" => Page::Jobs,
        "approvals" => Page::Approvals,
        "guide" => Page::Guide(0),
        "settings" => Page::Settings,
        _ => return None,
    })
}

fn tab_of(key: &str) -> Option<EnvTab> {
    TABS.iter().find(|(_, k)| *k == key).map(|(t, _)| *t)
}

fn groups(app: &DevtoolApp, cx: &App) -> Vec<(SharedString, Vec<Command>)> {
    let page = |key: &str, label: &str, icon| Command::new(format!("page:{key}"), label.to_string()).icon(icon);
    let mut out: Vec<(SharedString, Vec<Command>)> = vec![(
        t!("Go to").into(),
        vec![
            page("overview", t!("Overview"), IconName::LayoutDashboard),
            page("local", t!("Local environment"), IconName::HardDrive),
            page("workflows", t!("Workflows"), IconName::Rocket),
            page("repo", t!("Git & CI"), IconName::GitBranch),
            page("terminal", t!("Terminal"), IconName::Terminal),
            page("jobs", t!("Jobs"), IconName::ListChecks),
            page("approvals", t!("Approvals"), IconName::CircleCheck),
            page("guide", t!("Guide"), IconName::BookOpen),
            page("settings", t!("Settings"), IconName::Settings),
        ],
    )];

    let envs: Vec<Value> = app.overview.as_ref().and_then(|o| o["environments"].as_array().cloned()).unwrap_or_default();
    let mut env_cmds = vec![];
    for e in &envs {
        let Some(name) = e["name"].as_str() else { continue };
        let icon = if e["kind"].as_str().is_some_and(|k| k.starts_with("cloud")) { IconName::Cloud } else { IconName::HardDrive };
        env_cmds.push(Command::new(format!("env:{name}"), name.to_string()).icon(icon));
        for (tab, key) in TABS {
            env_cmds.push(Command::new(format!("env:{name}:{key}"), format!("{name} › {}", tab.label())).icon(icon));
        }
    }
    if !env_cmds.is_empty() {
        out.push((t!("Environments").into(), env_cmds));
    }

    // Las tablas de los entornos cuya pestaña Datos ya se abrió (su lista ya está cargada).
    let mut tables = vec![];
    let mut views: Vec<_> = app.data_views.iter().collect();
    views.sort_by(|a, b| a.0.cmp(b.0));
    for (env, view) in views {
        for table in view.read(cx).table_names() {
            tables.push(Command::new(format!("table:{env}:{table}"), format!("{table} · {env}")).icon(IconName::Table));
        }
    }
    if !tables.is_empty() {
        out.push((t!("Tables").into(), tables));
    }

    let projects: Vec<Command> = app
        .projects
        .iter()
        .filter_map(|p| p["name"].as_str())
        .filter(|name| *name != app.project)
        .map(|name| Command::new(format!("project:{name}"), t!("Switch to {a}", a = name)).icon(IconName::FolderOpen))
        .collect();
    if !projects.is_empty() {
        out.push((t!("Projects").into(), projects));
    }

    out.push((
        t!("Actions").into(),
        vec![
            Command::new("act:start-all", t!("Start all local services")).icon(IconName::Play),
            Command::new("act:stop-all", t!("Stop all local services")).icon(IconName::Square),
            Command::new("act:add-project", t!("Add existing project…")).icon(IconName::Plus),
            Command::new("act:create-project", t!("Create a project in Convex…")).icon(IconName::Plus),
            Command::new("act:notifications", t!("Show notifications")).icon(IconName::Bell),
            Command::new("act:theme", t!("Switch theme")).icon(IconName::Moon),
        ],
    ));
    out
}

pub fn open(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) {
    app.palette_open = true;
    cx.notify();
}

fn run(app: &mut DevtoolApp, value: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) {
    app.palette_recent.retain(|v| v.as_ref() != value);
    app.palette_recent.insert(0, value.to_string().into());
    app.palette_recent.truncate(RECENT);
    let (kind, rest) = value.split_once(':').unwrap_or((value, ""));
    match kind {
        "page" => {
            if let Some(page) = page_of(rest) {
                app.open_page(page, window, cx);
            }
        }
        "env" => match rest.split_once(':') {
            Some((env, tab)) => {
                if let Some(tab) = tab_of(tab) {
                    app.open_env_tab(env.to_string(), tab, cx);
                }
            }
            None => app.open_page(Page::Env(rest.to_string()), window, cx),
        },
        "table" => {
            if let Some((env, table)) = rest.split_once(':') {
                app.open_env_tab(env.to_string(), EnvTab::Data, cx);
                if let Some(view) = app.data_views.get(env).cloned() {
                    let table = table.to_string();
                    view.update(cx, |v, cx| v.show_table(table, cx));
                }
            }
        }
        "project" => app.select_project(rest.to_string(), cx),
        "act" => match rest {
            "start-all" => app.service_action("all", "start", cx),
            "stop-all" => app.service_action("all", "stop", cx),
            "add-project" => super::local::open_add_project(app, window, cx),
            "create-project" => super::local::open_create_project(app, window, cx),
            "notifications" => app.toggle_notifications(cx),
            "theme" => app.cycle_theme(window, cx),
            _ => {}
        },
        _ => {}
    }
    cx.notify();
}

/// La paleta vive en su propia vista: su campo de búsqueda (estado con clave de Ely) notifica a
/// la vista que lo dibuja, y si fuera la raíz de la app, lo escrito no se vería hasta otro evento.
pub struct PaletteView {
    app: WeakEntity<DevtoolApp>,
}

impl PaletteView {
    pub fn new(app: WeakEntity<DevtoolApp>) -> Self {
        Self { app }
    }
}

impl Render for PaletteView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else { return div().into_any_element() };
        let (groups, recent) = {
            let a = app.read(cx);
            (groups(a, cx), a.palette_recent.clone())
        };
        let (closer, runner) = (self.app.clone(), self.app.clone());
        groups
            .into_iter()
            .fold(
                CommandPalette::new("condx-palette", move |_, cx| {
                    let _ = closer.update(cx, |app, cx| {
                        app.palette_open = false;
                        cx.notify();
                    });
                }),
                |palette, (title, commands)| palette.group(title, commands),
            )
            .recent(recent)
            .on_run(move |value, window, cx| {
                let value = value.to_string();
                let _ = runner.update(cx, |app, cx| {
                    app.palette_open = false;
                    run(app, &value, window, cx);
                });
            })
            .into_any_element()
    }
}

pub fn render(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) -> Option<AnyElement> {
    if !app.palette_open {
        app.palette_view = None;
        return None;
    }
    let me = cx.entity().downgrade();
    let view = app.palette_view.get_or_insert_with(|| cx.new(|_| PaletteView::new(me))).clone();
    Some(view.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cada_pagina_y_pestana_tiene_su_clave() {
        for key in ["overview", "local", "workflows", "repo", "terminal", "jobs", "approvals", "guide", "settings"] {
            assert!(page_of(key).is_some(), "{key}");
        }
        assert!(page_of("nope").is_none());
        for (tab, key) in TABS {
            assert_eq!(tab_of(key), Some(tab));
        }
    }
}
