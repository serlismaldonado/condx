// Estado de la vista que se recuerda entre sesiones: página, pestaña de cada entorno, esquema,
// plegados, filtros y selecciones. Se guarda como JSON en la clave `view` de las preferencias,
// solo cuando cambia; las demás claves (tema, barra lateral, esquema.*) siguen como estaban.

use std::collections::{BTreeMap, HashMap};

use ely_terminal::LogLevel;
use serde::{Deserialize, Serialize};

use gpui_kit::{Context, Window};

use crate::app::{DevtoolApp, EnvTab, Page};
use crate::prefs::Prefs;
use crate::views::schema::Mode;

pub const KEY: &str = "view";

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct ViewState {
    pub page: String,
    /// Pestaña elegida en cada entorno.
    pub env_tabs: BTreeMap<String, String>,
    /// Modo, carpeta y tabla del esquema, por entorno.
    pub schema_modes: BTreeMap<String, String>,
    pub schema_folder: BTreeMap<String, String>,
    pub schema_table: BTreeMap<String, String>,
    pub schema_only_rel: bool,
    /// Nivel mínimo de cada visor de logs.
    pub log_levels: BTreeMap<String, String>,
    pub log_tab: usize,
    /// Plegados: pasos del trabajo abierto, precondiciones de aprobaciones y secciones (`id:clave`).
    pub step_folds: BTreeMap<String, bool>,
    pub approval_folds: BTreeMap<String, bool>,
    pub folds: BTreeMap<String, bool>,
    pub job_log_open: bool,
    pub live_json_open: bool,
    /// Trabajo abierto y formulario de trabajo nuevo.
    pub job: Option<String>,
    pub form_recipe: Option<String>,
    pub form_env: Option<String>,
    /// Pestaña de la app elegida en «En vivo».
    pub live_tab: Option<String>,
}

impl ViewState {
    pub fn load(prefs: &Prefs) -> Self {
        prefs.get(KEY).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

pub fn page_key(page: &Page) -> String {
    match page {
        Page::Overview => "overview".into(),
        Page::Local => "local".into(),
        Page::Terminal => "terminal".into(),
        Page::Env(name) => format!("env:{name}"),
        Page::Live => "live".into(),
        Page::Repo => "repo".into(),
        Page::Frontend => "frontend".into(),
        Page::Incidents => "incidents".into(),
        Page::Jobs => "jobs".into(),
        Page::Approvals => "approvals".into(),
        Page::Workflows => "workflows".into(),
        Page::Guide(ch) => format!("guide:{ch}"),
        Page::Settings => "settings".into(),
    }
}

pub fn parse_page(key: &str) -> Option<Page> {
    Some(match key {
        "overview" => Page::Overview,
        "local" => Page::Local,
        "terminal" => Page::Terminal,
        // La promoción fija pasó a ser un flujo.
        "promotion" => Page::Workflows,
        "live" => Page::Live,
        "repo" => Page::Repo,
        "frontend" => Page::Frontend,
        "incidents" => Page::Incidents,
        "jobs" => Page::Jobs,
        "approvals" => Page::Approvals,
        "workflows" => Page::Workflows,
        "settings" => Page::Settings,
        _ => {
            if let Some(name) = key.strip_prefix("env:") {
                Page::Env(name.to_string())
            } else {
                let ch = key.strip_prefix("guide:")?;
                Page::Guide(ch.parse().ok()?)
            }
        }
    })
}

const TABS: [(EnvTab, &str); 11] = [
    (EnvTab::Summary, "summary"),
    (EnvTab::Services, "services"),
    (EnvTab::Vars, "vars"),
    (EnvTab::Logs, "logs"),
    (EnvTab::Deploys, "deploys"),
    (EnvTab::Schema, "schema"),
    (EnvTab::Jobs, "jobs"),
    (EnvTab::Workflows, "workflows"),
    (EnvTab::Data, "data"),
    (EnvTab::Live, "live"),
    (EnvTab::Keys, "keys"),
];

pub fn tab_key(tab: EnvTab) -> &'static str {
    TABS.iter().find(|(t, _)| *t == tab).map(|(_, k)| *k).unwrap_or("summary")
}

pub fn parse_tab(key: &str) -> Option<EnvTab> {
    if key == "promotion" {
        return Some(EnvTab::Workflows);
    }
    TABS.iter().find(|(_, k)| *k == key).map(|(t, _)| *t)
}

pub fn mode_key(mode: Mode) -> &'static str {
    match mode {
        Mode::Focus => "focus",
        Mode::Folder => "folder",
        Mode::All => "all",
    }
}

pub fn parse_mode(key: &str) -> Option<Mode> {
    match key {
        "focus" => Some(Mode::Focus),
        "folder" => Some(Mode::Folder),
        "all" => Some(Mode::All),
        _ => None,
    }
}

const LEVELS: [(LogLevel, &str); 5] =
    [(LogLevel::Trace, "trace"), (LogLevel::Debug, "debug"), (LogLevel::Info, "info"), (LogLevel::Warn, "warn"), (LogLevel::Error, "error")];

pub fn level_key(level: LogLevel) -> &'static str {
    LEVELS.iter().find(|(l, _)| *l == level).map(|(_, k)| *k).unwrap_or("trace")
}

pub fn parse_level(key: &str) -> Option<LogLevel> {
    LEVELS.iter().find(|(_, k)| *k == key).map(|(l, _)| *l)
}

impl DevtoolApp {
    /// Lo que se ve ahora, para guardarlo.
    pub fn view_state(&self) -> ViewState {
        // Solo los plegados que todavía existen: los pasos del trabajo abierto y las aprobaciones en espera.
        let job_prefix = self.job.as_ref().map(|j| format!("{j}:"));
        ViewState {
            page: page_key(&self.page),
            env_tabs: self.env_tabs.iter().map(|(k, t)| (k.clone(), tab_key(*t).to_string())).collect(),
            schema_modes: self.schema_modes.iter().map(|(k, m)| (k.clone(), mode_key(*m).to_string())).collect(),
            schema_folder: sorted(&self.schema_folder),
            schema_table: sorted(&self.schema_table),
            schema_only_rel: self.schema_only_rel,
            log_levels: self.log_levels.iter().map(|(k, l)| (k.clone(), level_key(*l).to_string())).collect(),
            log_tab: self.log_tab,
            step_folds: self
                .step_folds
                .iter()
                .filter(|(k, _)| job_prefix.as_ref().is_some_and(|p| k.starts_with(p.as_str())))
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            approval_folds: self
                .approval_folds
                .iter()
                .filter(|(k, _)| self.approvals.iter().any(|a| a["id"] == k.as_str()))
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            folds: sorted(&self.folds),
            job_log_open: self.job_tab == 2,
            live_json_open: self.live_json_open,
            job: self.job.clone(),
            form_recipe: self.form_recipe.clone(),
            form_env: self.form_env.clone(),
            live_tab: self.live_tab.clone(),
        }
    }

    /// Guarda la vista si cambió desde la última vez (se llama en cada cuadro; escribe poco).
    pub fn save_view(&mut self) {
        if !self.restored {
            return;
        }
        if let Page::Env(env) = &self.page {
            self.schema_modes.insert(env.clone(), self.schema_mode);
        }
        let json = self.view_state().to_json();
        if json != self.saved_view {
            self.prefs.set(KEY, &json);
            self.saved_view = json;
        }
    }

    /// Vuelve a la vista guardada. Pasa por `open_page`/`open_env_tab` para que arranque lo que
    /// cada pestaña necesita (logs de servicios, promoción, En vivo, el log del trabajo).
    pub fn restore_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restored = true;
        let v = ViewState::load(&self.prefs);
        self.saved_view = v.to_json();
        self.env_tabs = v.env_tabs.iter().filter_map(|(k, t)| Some((k.clone(), parse_tab(t)?))).collect();
        self.schema_modes = v.schema_modes.iter().filter_map(|(k, m)| Some((k.clone(), parse_mode(m)?))).collect();
        self.schema_folder = unsorted(&v.schema_folder);
        self.schema_table = unsorted(&v.schema_table);
        self.schema_only_rel = v.schema_only_rel;
        self.log_levels = v.log_levels.iter().filter_map(|(k, l)| Some((k.clone(), parse_level(l)?))).collect();
        self.log_tab = v.log_tab;
        self.step_folds = unsorted(&v.step_folds);
        self.approval_folds = unsorted(&v.approval_folds);
        self.folds = unsorted(&v.folds);
        self.job_tab = if v.job_log_open { 2 } else { 0 };
        self.live_json_open = v.live_json_open;
        self.form_recipe = v.form_recipe.clone();
        self.form_env = v.form_env.clone();
        self.live_tab = v.live_tab.clone();
        if let Some(job) = v.job.clone() {
            self.open_job(job, cx);
        }
        // Si ya se navegó mientras llegaba el primer sondeo, no se mueve.
        if self.page != Page::Overview {
            return;
        }
        let known = |name: &str| {
            self.overview
                .as_ref()
                .and_then(|o| o["environments"].as_array())
                .is_some_and(|list| list.iter().any(|e| e["name"] == name))
        };
        match parse_page(&v.page) {
            None | Some(Page::Overview) => {}
            Some(Page::Env(name)) if !known(&name) => {}
            Some(Page::Guide(ch)) => self.go(Page::Guide(ch), cx),
            Some(page) => self.open_page(page, window, cx),
        }
    }
}

/// Pasa un mapa a uno ordenado (para que el JSON no cambie de orden entre cuadros).
pub fn sorted<V: Clone>(map: &HashMap<String, V>) -> BTreeMap<String, V> {
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

pub fn unsorted<V: Clone>(map: &BTreeMap<String, V>) -> HashMap<String, V> {
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarda_y_lee_el_estado_de_la_vista() {
        let prefs = Prefs::memory();
        assert_eq!(ViewState::load(&prefs), ViewState::default());
        let mut v = ViewState { page: page_key(&Page::Env("cloud-dev".into())), ..Default::default() };
        v.env_tabs.insert("cloud-dev".into(), tab_key(EnvTab::Schema).into());
        v.env_tabs.insert("devbox".into(), tab_key(EnvTab::Logs).into());
        v.schema_modes.insert("cloud-dev".into(), mode_key(Mode::Folder).into());
        v.schema_folder.insert("cloud-dev".into(), "ventas".into());
        v.schema_table.insert("cloud-dev".into(), "invoices".into());
        v.log_levels.insert("job".into(), level_key(LogLevel::Warn).into());
        v.folds.insert("git-details:1".into(), true);
        v.job = Some("j1".into());
        prefs.set(KEY, &v.to_json());

        let back = ViewState::load(&prefs);
        assert_eq!(back, v);
        assert_eq!(parse_page(&back.page), Some(Page::Env("cloud-dev".into())));
        assert_eq!(back.env_tabs.get("cloud-dev").and_then(|t| parse_tab(t)), Some(EnvTab::Schema));
        assert_eq!(back.env_tabs.get("devbox").and_then(|t| parse_tab(t)), Some(EnvTab::Logs));
        assert!(back.schema_modes.get("cloud-dev").and_then(|m| parse_mode(m)) == Some(Mode::Folder));
        assert_eq!(back.log_levels.get("job").and_then(|l| parse_level(l)), Some(LogLevel::Warn));
    }

    #[test]
    fn lee_cada_pagina_y_pestana() {
        for p in [Page::Overview, Page::Local, Page::Terminal, Page::Env("a:b".into()), Page::Live, Page::Repo, Page::Frontend, Page::Incidents, Page::Jobs, Page::Approvals, Page::Guide(3), Page::Settings] {
            assert_eq!(parse_page(&page_key(&p)), Some(p));
        }
        for (t, _) in TABS {
            assert_eq!(parse_tab(tab_key(t)), Some(t));
        }
        for (l, _) in LEVELS {
            assert_eq!(parse_level(level_key(l)), Some(l));
        }
        assert_eq!(parse_page("nada"), None);
    }

    #[test]
    fn un_valor_viejo_o_roto_no_rompe() {
        let prefs = Prefs::memory();
        prefs.set(KEY, "{no es json");
        assert_eq!(ViewState::load(&prefs), ViewState::default());
        prefs.set(KEY, r#"{"page":"repo","algo":1}"#);
        assert_eq!(ViewState::load(&prefs).page, "repo");
    }
}
