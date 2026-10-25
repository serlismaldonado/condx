// Estado de la app y su ventana principal: barra lateral con las secciones y
// la vista elegida. Todo sale del daemon; la app no corre comandos propios.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt;
use gpui_kit::*;
use serde_json::{json, Value};

use std::sync::Arc;

use crate::client::Daemon;
use crate::prefs::Prefs;
use crate::theme::Choice;
use crate::ui::{self, Health};
use crate::views;

#[derive(Clone, PartialEq, Debug)]
pub enum Page {
    Overview,
    Local,
    Terminal,
    Env(String),
    Live,
    Repo,
    /// El frontend del proyecto en Vercel (views::frontend).
    Frontend,
    /// Lo que falló, de todos los proyectos (views::incidents).
    Incidents,
    /// Conversar con el agente del proyecto (views::agents).
    Agents,
    Jobs,
    Approvals,
    /// Flujos de despliegue del proyecto (views::workflows).
    Workflows,
    Guide(usize),
    /// Idioma, tema y datos del daemon (views::settings).
    Settings,
}

/// Pestañas de un entorno. Cada tipo de entorno muestra las suyas (ver `views::env::tabs_for`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvTab {
    Summary,
    Services,
    Vars,
    Logs,
    Deploys,
    Schema,
    Jobs,
    Workflows,
    Data,
    Live,
    Keys,
}

impl EnvTab {
    pub fn label(self) -> &'static str {
        match self {
            EnvTab::Summary => t!("Summary"),
            EnvTab::Services => t!("Services"),
            EnvTab::Vars => t!("Variables"),
            EnvTab::Logs => t!("Logs"),
            EnvTab::Deploys => t!("Deploys"),
            EnvTab::Schema => t!("Schema"),
            EnvTab::Jobs => t!("Jobs"),
            EnvTab::Workflows => t!("Workflows"),
            EnvTab::Data => t!("Data"),
            EnvTab::Live => t!("Live"),
            EnvTab::Keys => t!("Keys"),
        }
    }
}

/// Pestañas de Configuración, como las de un entorno.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    General,
    Data,
    Agents,
    Connect,
    Cloud,
    Notifications,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 6] = [SettingsTab::General, SettingsTab::Data, SettingsTab::Agents, SettingsTab::Connect, SettingsTab::Cloud, SettingsTab::Notifications];

    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::General => t!("General"),
            SettingsTab::Data => t!("Data and changes"),
            SettingsTab::Agents => t!("Agents"),
            SettingsTab::Connect => t!("Connect an agent"),
            SettingsTab::Cloud => t!("Condx Cloud"),
            SettingsTab::Notifications => t!("Notifications"),
        }
    }
}

/// Cómo está el entorno local (sus servicios).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LocalState {
    Up,
    Partial,
    Down,
    Starting,
    Stopping,
}

/// Resultado de una carga a pedido.
#[derive(Clone)]
pub enum Load {
    Loading,
    Ready(Value),
    Failed(String),
}

impl From<anyhow::Result<Value>> for Load {
    fn from(r: anyhow::Result<Value>) -> Self {
        match r {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e.to_string()),
        }
    }
}

/// Una línea del diagrama del esquema: tabla de origen, campo y tabla de destino.
pub type RouteKey = (String, usize, String);
/// Los puntos de esa línea, ya calculados.
pub type Route = std::rc::Rc<Vec<(f32, f32)>>;

pub struct DevtoolApp {
    pub daemon: Option<Daemon>,
    pub page: Page,
    pub overview: Option<Value>,
    pub probes: HashMap<String, Value>,
    /// Último error al hablar con el daemon (se limpia con la siguiente respuesta).
    pub error: Option<String>,
    pub updated_at: Option<i64>,
    pub env_vars: HashMap<String, Load>,
    pub env_logs: HashMap<String, Load>,
    pub live_tab: Option<String>,
    pub snapshot: Option<Load>,
    pub action: Option<Load>,
    pub recipes: Vec<Value>,
    pub jobs: Vec<Value>,
    pub approvals: Vec<Value>,
    /// Trabajo abierto en «Trabajos», con su log acumulado.
    pub job: Option<String>,
    pub job_detail: Option<Value>,
    pub job_log: String,
    pub job_log_offset: u64,
    /// Formulario de trabajo nuevo.
    pub form_recipe: Option<String>,
    pub form_env: Option<String>,
    pub inputs: HashMap<String, Entity<gpui_kit::component::input::InputState>>,
    /// Campos de la receta `run-function` (selector de funciones y editor JSON).
    pub run_fn: crate::views::jobs::RunFn,
    pub form_result: Option<Load>,
    pub promotion: Option<Load>,
    /// Pestaña elegida en el detalle de entorno.
    pub env_tab: EnvTab,
    pub settings_tab: SettingsTab,
    pub env_deployments: HashMap<String, Load>,
    pub backups: Vec<Value>,
    /// Estado anterior de cada trabajo, para avisar cuando termina.
    pub services: Vec<Value>,
    /// Proyectos de Convex (heskala-erp primero) y el elegido en «Entorno local».
    pub projects: Vec<Value>,
    pub project: String,
    /// Repos candidatos para agregar (GET /api/projects/discover).
    pub discover: Option<Load>,
    /// Chequeo del primer arranque (GET /api/doctor) y la descarga del backend local.
    pub doctor: Option<Load>,
    /// La paleta de comandos (cmd-K) y lo último que se corrió en ella.
    pub palette_open: bool,
    pub palette_recent: Vec<SharedString>,
    pub palette_view: Option<Entity<views::palette::PaletteView>>,
    /// El último diff de esquemas pedido (GET /api/schema-diff).
    pub schema_diff: Option<Load>,
    /// Condx Cloud: vínculo y estado (GET /api/cloud), y la vinculación en curso.
    pub cloud: Option<Load>,
    pub cloud_linking: Option<Load>,
    /// Vincular un teléfono: el código abierto (con su QR), su estado y los teléfonos de confianza.
    pub cloud_pair: Option<Value>,
    pub cloud_pair_status: Option<Load>,
    pub cloud_devices: Option<Load>,
    pub backend_download: Option<Load>,
    pub service_logs: HashMap<String, String>,
    pub log_tab: usize,
    pub log_follow: bool,
    pub terminals: Vec<Entity<crate::terminal::TerminalView>>,
    pub term_active: usize,
    pub term_error: Option<String>,
    known_jobs: HashMap<String, String>,
    known_approvals: HashSet<String>,
    first_poll: bool,
    /// Preferencias y centro de notificaciones (SQLite de la app).
    pub prefs: Arc<Prefs>,
    /// Barra lateral plegada al riel de íconos.
    pub collapsed: bool,
    /// Grupos de la barra lateral cerrados.
    pub closed_groups: HashSet<String>,
    pub show_notifications: bool,
    pub project_menu: bool,
    pub theme: Choice,
    pub focus: FocusHandle,
    /// Foco de la barra lateral (flechas arriba y abajo para recorrerla).
    pub nav_focus: FocusHandle,
    /// Último error de acción ya avisado (para no repetirlo).
    notified_action: Option<String>,
    opened_at: i64,
    /// Campos de texto de Ely (búsquedas de los visores), por clave.
    pub ely_inputs: HashMap<String, Entity<ely_core::forms::TextInput>>,
    /// Pestaña Datos: una vista por entorno, que se conserva al cambiar de pestaña o de entorno
    /// (views::data).
    pub data_views: HashMap<String, Entity<crate::views::data::DataView>>,
    /// La pestaña Claves de cada entorno.
    pub key_views: HashMap<String, Entity<crate::views::keys::KeysView>>,
    /// Reglas de datos del daemon (`/api/policy`): leer y cambiar por tipo de entorno.
    pub policy: Option<Value>,
    pub policy_error: Option<String>,
    /// `GET /api/mcp`: comando del puente, URL y último uso (tarjeta «Conectar un agente»).
    pub mcp_status: Option<Value>,
    /// Nivel mínimo de cada visor de logs, por clave.
    pub log_levels: HashMap<String, ely_terminal::LogLevel>,
    /// Pasos de trabajo plegados o abiertos a mano (`job:índice`).
    pub step_folds: HashMap<String, bool>,
    /// La pestaña del trabajo abierto: 0 pasos, 1 argumentos, 2 log completo.
    pub job_tab: usize,
    /// Esquema de cada entorno, la tabla elegida y si el diagrama muestra todas.
    pub schema: HashMap<String, Load>,
    pub schema_table: HashMap<String, String>,
    pub schema_mode: crate::views::schema::Mode,
    /// Vista del diagrama (x, y, zoom) por entorno y modo; se guarda en las preferencias cada tanto.
    pub schema_views: HashMap<String, (f32, f32, f32)>,
    pub schema_saved_at: i64,
    /// Lo que el puntero tiene agarrado en el diagrama (el fondo o una caja).
    pub schema_grab: Option<crate::views::er::Grab>,
    /// Flujos de despliegue: lista, el elegido, su borrador (con cambios sin guardar), corridas.
    pub wf_data: Option<Load>,
    pub wf_selected: Option<String>,
    pub wf_draft: Option<Value>,
    pub wf_dirty: bool,
    pub wf_runs: Vec<Value>,
    pub wf_run_sel: Option<String>,
    pub wf_node_sel: Option<String>,
    pub wf_templates: Vec<Value>,
    pub wf_view: (f32, f32, f32),
    pub wf_grab: Option<crate::views::workflows::WfGrab>,
    pub wf_bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// JSON exportado, a copiar al portapapeles en el próximo dibujo.
    pub wf_clipboard: Option<String>,
    /// Plantilla elegida al crear un proyecto en Convex.
    pub create_template: String,
    /// Ventana angosta (menos de 1000 px): las vistas esconden columnas secundarias y la barra
    /// lateral se pliega sola (sin cambiar la preferencia).
    pub narrow: bool,
    pub tiny: bool,
    /// Acciones de servicios pedidas y aún no confirmadas por el daemon: «proyecto/servicio» →
    /// (acción, cuándo). Mientras tanto el servicio se ve «levantando» o «deteniendo».
    pub pending: HashMap<String, (String, i64)>,
    /// Scroll de las listas de Git (cambios, ramas), para pasar la rueda a la página en el borde.
    pub repo_scroll: (ScrollHandle, ScrollHandle),
    /// Rutas de las líneas del diagrama, por firma de las posiciones de las cajas.
    pub schema_routes: (u64, HashMap<RouteKey, Route>),
    /// El esquema ya leído, compartido entre cuadros (entorno, revisión).
    pub schema_data: Option<(String, u64, std::rc::Rc<serde_json::Value>)>,
    /// Carpeta elegida en el árbol (modo Carpeta), por entorno.
    pub schema_folder: HashMap<String, String>,
    pub schema_only_rel: bool,
    pub schema_fullscreen: bool,
    pub schema_height: f32,
    pub schema_drag: Option<(f32, f32)>,
    /// Cálculos del esquema (árbol, cajas, posiciones), por la clave de lo que los cambia.
    pub schema_cache: Option<(String, std::rc::Rc<crate::views::schema::Derived>)>,
    pub schema_rev: u64,
    /// Dónde está el diagrama en la ventana (para acercar donde se pellizca).
    pub schema_bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// Paso de la promoción abierto a mano (si no, el actual).
    /// Aprobaciones con las precondiciones abiertas o cerradas a mano.
    pub approval_folds: HashMap<String, bool>,
    /// Git de cada proyecto (cambios, ramas, commits).
    pub git: HashMap<String, Load>,
    /// `/api/frontend` por proyecto.
    pub frontend: HashMap<String, Load>,
    /// Incidentes abiertos e ignorados (`/api/incidents`, en cada sondeo) y los resueltos (a pedido).
    pub incidents: Vec<Value>,
    pub incidents_resolved: Option<Load>,
    pub incidents_tab_resolved: bool,
    /// La conversación de agente abierta en Incidentes (`/api/agent-runs/{id}`) y cuándo se pidió.
    pub agent_run_open: Option<String>,
    pub agent_run: Option<Load>,
    pub agent_run_at: i64,
    /// Página Agentes: la lista, la conversación abierta (None: un chat nuevo) y lo que se escribe.
    pub chats: Vec<Value>,
    pub chats_at: i64,
    pub chat_open: Option<String>,
    pub chat: Option<Load>,
    pub chat_at: i64,
    pub chat_sending: bool,
    pub chat_error: Option<String>,
    pub chat_input: Option<Entity<gpui_kit::component::input::TextareaState>>,
    pub chat_clear: bool,
    /// Para bajar a lo último cuando llegan mensajes nuevos.
    pub chat_scroll: ScrollHandle,
    pub chat_len: usize,
    known_incidents: std::collections::HashSet<String>,
    /// Configuración → Agentes (`/api/agents`) y el tipo de proveedor elegido para agregar.
    pub agents: Option<Value>,
    /// El catálogo de agentes para flujos (views::agent_roles) y el que se edita.
    pub agent_roles: Option<Value>,
    pub role_edit: Option<Value>,
    pub roles_error: Option<String>,
    pub agents_error: Option<String>,
    pub agent_kind: String,
    /// Lo que trajo «Importar de pi» (o por qué no).
    pub agents_import: Option<Load>,
    /// La plantilla de agente que se está editando (borrador; `id` vacío = nueva).
    pub template_edit: Option<Value>,
    /// Configuración → Notificaciones (`/api/notify`) y el resultado de guardar o probar.
    pub notify_cfg: Option<Value>,
    pub notify_result: Option<Result<String, String>>,
    pub live_json_open: bool,
    /// Pestaña y modo del esquema de cada entorno (los de `env_tab`/`schema_mode` son del abierto).
    pub env_tabs: HashMap<String, EnvTab>,
    pub schema_modes: HashMap<String, crate::views::schema::Mode>,
    /// Secciones plegables que controla la app (`id:clave` → abierta).
    pub folds: HashMap<String, bool>,
    /// La vista guardada ya se recuperó (hasta entonces no se guarda nada) y lo último guardado.
    pub(crate) restored: bool,
    pub(crate) saved_view: String,
}

const POLL: Duration = Duration::from_secs(3);
const LIVE_POLL: Duration = Duration::from_secs(2);

/// Si la vista ya no existe. En la web (un solo hilo) una actualización puede caer mientras la
/// ventana se dibuja y fallar sin que la vista se haya ido: los sondeos solo paran si se fue.
fn gone<T: 'static>(this: &WeakEntity<T>) -> bool {
    this.upgrade().is_none()
}

impl DevtoolApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (daemon, error) = match Daemon::from_home() {
            Ok(d) => (Some(d), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let prefs = Arc::new(Prefs::open().unwrap_or_else(|_| Prefs::memory()));
        let theme = Choice::parse(&prefs.get("theme").unwrap_or_default());
        crate::theme::apply(theme, window, cx);
        // Con el tema del sistema, seguir a macOS cuando cambia.
        cx.observe_window_appearance(window, |this, window, cx| {
            if this.theme == Choice::System {
                crate::theme::apply(Choice::System, window, cx);
            }
        })
        .detach();
        let closed_groups: HashSet<String> = prefs
            .get("sidebar.closed")
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        let collapsed = prefs.get_bool("sidebar.collapsed", false);
        let project = prefs.get("project").unwrap_or_else(|| "heskala-erp".into());
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut app = Self {
            daemon,
            page: Page::Overview,
            overview: None,
            probes: HashMap::new(),
            error,
            updated_at: None,
            env_vars: HashMap::new(),
            env_logs: HashMap::new(),
            live_tab: None,
            snapshot: None,
            action: None,
            recipes: vec![],
            jobs: vec![],
            approvals: vec![],
            job: None,
            job_detail: None,
            job_log: String::new(),
            job_log_offset: 0,
            form_recipe: None,
            form_env: None,
            run_fn: Default::default(),
            inputs: HashMap::new(),
            form_result: None,
            promotion: None,
            env_tab: EnvTab::Summary,
            settings_tab: SettingsTab::General,
            env_deployments: HashMap::new(),
            backups: vec![],
            services: vec![],
            projects: vec![],
            project,
            discover: None,
            doctor: None,
            palette_open: false,
            palette_recent: vec![],
            palette_view: None,
            schema_diff: None,
            cloud: None,
            cloud_linking: None,
            cloud_pair: None,
            cloud_pair_status: None,
            cloud_devices: None,
            backend_download: None,
            service_logs: HashMap::new(),
            log_tab: 0,
            log_follow: true,
            terminals: vec![],
            term_active: 0,
            term_error: None,
            known_jobs: HashMap::new(),
            known_approvals: HashSet::new(),
            first_poll: true,
            prefs,
            collapsed,
            closed_groups,
            show_notifications: false,
            project_menu: false,
            theme,
            focus,
            nav_focus: cx.focus_handle(),
            notified_action: None,
            opened_at: ui::now_ms(),
            ely_inputs: HashMap::new(),
            data_views: HashMap::new(),
            key_views: HashMap::new(),
            policy: None,
            policy_error: None,
            mcp_status: None,
            log_levels: HashMap::new(),
            step_folds: HashMap::new(),
            job_tab: 0,
            schema: HashMap::new(),
            schema_table: HashMap::new(),
            schema_mode: crate::views::schema::Mode::Focus,
            schema_views: HashMap::new(),
            schema_saved_at: 0,
            schema_grab: None,
            wf_data: None,
            wf_selected: None,
            wf_draft: None,
            wf_dirty: false,
            wf_runs: vec![],
            wf_run_sel: None,
            wf_node_sel: None,
            wf_templates: vec![],
            wf_view: (-40., -60., 1.),
            wf_grab: None,
            wf_bounds: std::rc::Rc::new(std::cell::Cell::new(Bounds::default())),
            wf_clipboard: None,
            create_template: "react-vite".into(),
            narrow: false,
            tiny: false,
            pending: HashMap::new(),
            repo_scroll: (ScrollHandle::new(), ScrollHandle::new()),
            schema_routes: (0, HashMap::new()),
            schema_data: None,
            schema_folder: HashMap::new(),
            schema_only_rel: false,
            schema_fullscreen: false,
            schema_height: 0.,
            schema_drag: None,
            schema_cache: None,
            schema_rev: 0,
            schema_bounds: std::rc::Rc::new(std::cell::Cell::new(Bounds::default())),
            approval_folds: HashMap::new(),
            git: HashMap::new(),
            frontend: HashMap::new(),
            incidents: vec![],
            incidents_resolved: None,
            incidents_tab_resolved: false,
            agent_run_open: None,
            agent_run: None,
            agent_run_at: 0,
            agent_roles: None,
            role_edit: None,
            roles_error: None,
            chats: vec![],
            chats_at: 0,
            chat_open: None,
            chat: None,
            chat_at: 0,
            chat_sending: false,
            chat_error: None,
            chat_input: None,
            chat_clear: false,
            chat_scroll: ScrollHandle::new(),
            chat_len: 0,
            known_incidents: Default::default(),
            agents: None,
            agents_error: None,
            agent_kind: "anthropic".into(),
            agents_import: None,
            template_edit: None,
            notify_cfg: None,
            notify_result: None,
            live_json_open: false,
            env_tabs: HashMap::new(),
            schema_modes: HashMap::new(),
            folds: HashMap::new(),
            restored: false,
            saved_view: String::new(),
        };
        app.start_polling(window, cx);
        app.load_policy(cx);
        app
    }

    pub fn load_policy(&mut self, cx: &mut Context<Self>) {
        self.get("/api/policy".into(), |app, res| {
            if let Load::Ready(v) = res {
                app.policy = Some(v);
            }
        }, cx);
    }

    /// Si la pestaña Datos puede leer los registros de un entorno de este tipo.
    pub fn can_read(&self, kind: &str) -> bool {
        self.policy.as_ref().and_then(|p| p["read"][kind].as_bool()).unwrap_or(kind != "cloud-prod")
    }

    /// Cambia una regla y la guarda en el daemon; las vistas de Datos se rehacen con la nueva.
    pub fn set_policy(&mut self, kind: &str, read: Option<bool>, write: Option<&str>, cx: &mut Context<Self>) {
        let Some(mut policy) = self.policy.clone() else { return };
        if let Some(r) = read {
            policy["read"][kind] = json!(r);
        }
        if let Some(w) = write {
            policy["write"][kind] = json!(w);
        }
        let body = json!({ "read": policy["read"], "write": policy["write"] });
        self.policy = Some(policy);
        self.data_views.clear();
        self.run_fn.approvals.clear();
        cx.notify();
        self.post("/api/policy".into(), body, |app, res, cx| match res {
            Load::Ready(v) => {
                app.policy = Some(v);
                app.policy_error = None;
            }
            Load::Failed(e) => {
                app.policy_error = Some(e);
                app.load_policy(cx);
            }
            Load::Loading => {}
        }, cx);
    }

    fn start_polling(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn_in(window, async move |this, cx| loop {
            let d = daemon.clone();
            let Ok(project) = this.update(cx, |app, _| app.project.clone()) else {
                if gone(&this) {
                    break;
                }
                cx.background_executor().timer(POLL).await;
                continue;
            };
            let services_path = format!("/api/services?project={project}");
            let overview_path = format!("/api/overview?project={project}");
            let jobs_path = format!("/api/jobs?limit=60&project={project}");
            let (overview, probes, jobs, approvals, recipes, backups, services, projects, incidents) = cx
                .background_executor()
                .spawn(async move {
                    (
                        d.get(&overview_path),
                        d.get("/api/probes"),
                        d.get(&jobs_path),
                        d.get("/api/approvals"),
                        d.get("/api/recipes"),
                        d.get("/api/backups"),
                        d.get(&services_path),
                        d.get("/api/projects"),
                        d.get("/api/incidents?limit=100"),
                    )
                })
                .await;
            let alive = this
                .update_in(cx, |app, window, cx| {
                    match overview {
                        // Otro proyecto elegido mientras se pedía: se descarta.
                        Ok(_) if app.project != project => {}
                        Ok(v) => {
                            app.notify_attention(&v, window, cx);
                            app.overview = Some(v);
                            app.error = None;
                            app.updated_at = Some(ui::now_ms());
                        }
                        Err(e) => app.error = Some(e.to_string()),
                    }
                    if let (Ok(Value::Array(list)), true) = (jobs, app.project == project) {
                        app.notify_jobs(&list, window, cx);
                        app.jobs = list;
                    }
                    if let Ok(Value::Array(list)) = approvals {
                        app.notify_approvals(&list, window, cx);
                        app.approvals = list;
                    }
                    if let Ok(Value::Array(list)) = incidents {
                        app.notify_incidents(&list, window, cx);
                        app.incidents = list;
                    }
                    if let Ok(Value::Array(list)) = backups {
                        app.backups = list;
                    }
                    // Solo si el proyecto no cambió mientras se pedía.
                    if let (Ok(Value::Array(list)), true) = (services, app.project == project) {
                        app.services = app.with_pending(list);
                    }
                    if let Ok(Value::Array(list)) = projects {
                        // El guardado ya no existe: el primero de la lista.
                        if !list.iter().any(|p| p["name"] == app.project.as_str()) {
                            app.project = list.first().and_then(|p| p["name"].as_str()).unwrap_or("heskala-erp").to_string();
                        }
                        app.projects = list;
                    }
                    app.first_poll = false;
                    if let Ok(Value::Array(list)) = recipes {
                        app.recipes = list;
                    }
                    // Con el primer sondeo ya se saben los entornos: volver a donde se estaba.
                    if !app.restored {
                        app.restore_view(window, cx);
                    }
                    if let Ok(Value::Array(list)) = probes {
                        app.probes = list
                            .into_iter()
                            .filter_map(|p| p["key"].as_str().map(|k| (k.to_string(), p.clone())))
                            .collect();
                    }
                    cx.notify();
                })
                .is_ok();
            if !alive && gone(&this) {
                break;
            }
            cx.background_executor().timer(POLL).await;
        })
        .detach();
    }

    /// Corre una herramienta del daemon en el fondo y guarda el resultado con `store`.
    pub fn call(
        &mut self,
        name: &'static str,
        args: Value,
        store: impl FnOnce(&mut Self, Load) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { daemon.tool(name, args) }).await;
            this.update(cx, |app, cx| {
                store(app, res.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Aviso (toast) cuando un trabajo termina.
    fn notify_jobs(&mut self, list: &[Value], window: &mut Window, cx: &mut Context<Self>) {
        for job in list {
            let (Some(id), Some(status)) = (job["id"].as_str(), job["status"].as_str()) else { continue };
            let before = self.known_jobs.insert(id.to_string(), status.to_string());
            // Uno que terminó entre dos sondeos también avisa, si se pidió con la app abierta.
            let unseen_new = before.is_none() && job["createdAt"].as_i64().unwrap_or(0) >= self.opened_at;
            if self.first_poll || before.as_deref() == Some(status) || (before.is_none() && !unseen_new) {
                continue;
            }
            let what = format!("{} · {}", job["title"].as_str().unwrap_or(t!("job")), job["env"].as_str().unwrap_or(""));
            let (kind, title, body) = match status {
                "succeeded" => ("success", t!("Job finished"), what),
                "failed" => ("error", t!("Job failed"), format!("{what}: {}", job["error"].as_str().unwrap_or(""))),
                "cancelled" => ("warning", t!("Job cancelled"), what),
                _ => continue,
            };
            self.notify(&format!("job:{id}:{status}"), kind, title, &body, window, cx);
        }
    }

    /// Aviso cuando alguien (un agente, el CLI) pide algo que espera aprobación.
    fn notify_approvals(&mut self, list: &[Value], window: &mut Window, cx: &mut Context<Self>) {
        for job in list {
            let Some(id) = job["id"].as_str() else { continue };
            if self.known_approvals.insert(id.to_string()) && !self.first_poll && job["requestedBy"] != "app" {
                let msg = t!("{a} · {b} (requested by {c})", a = ui::txt(&job["title"]), b = ui::txt(&job["env"]), c = ui::txt(&job["requestedBy"]));
                self.notify(&format!("approval:{id}"), "warning", t!("Waiting for your approval"), &msg, window, cx);
            }
        }
    }

    /// Un incidente nuevo (o que vuelve) se avisa una vez; los que ya estaban al abrir la app, no.
    fn notify_incidents(&mut self, list: &[Value], window: &mut Window, cx: &mut Context<Self>) {
        for inc in list.iter().filter(|i| i["status"] == "open") {
            let Some(id) = inc["id"].as_str() else { continue };
            let key = format!("incident:{id}:{}", inc["count"]);
            if self.known_incidents.insert(key.clone()) && !self.first_poll {
                self.notify(&key, "error", t!("Incident"), &ui::txt(&inc["title"]), window, cx);
            }
        }
    }

    /// Guarda una notificación en el centro y, si es nueva, la muestra como aviso.
    pub fn notify(&mut self, id: &str, kind: &str, title: &str, body: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.prefs.notify(id, kind, title, body, ui::now_ms()) {
            return;
        }
        let note = match kind {
            "success" => Notification::success(body.to_string()),
            "error" => Notification::error(body.to_string()),
            "warning" => Notification::warning(body.to_string()),
            _ => Notification::info(body.to_string()),
        };
        window.push_notification(note.title(title.to_string()), cx);
    }

    /// Lo que pide atención (la vista general) pasa al centro de notificaciones.
    fn notify_attention(&mut self, overview: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<String> = overview["attention"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)).collect();
        for a in items {
            // Las aprobaciones ya avisan por su cuenta.
            // Las aprobaciones y los trabajos fallidos ya avisan por su cuenta.
            if a.contains("waiting for your approval") || a.starts_with("Job failed") {
                continue;
            }
            let low = a.to_lowercase();
            let kind = if low.contains("failed") || low.contains("not responding") || low.contains("error") { "error" } else { "warning" };
            let id = format!("attention:{}:{a}", self.project);
            if self.first_poll {
                // Al abrir la app: al centro, sin aviso flotante.
                self.prefs.notify(&id, kind, t!("Needs attention"), &a, ui::now_ms());
            } else {
                self.notify(&id, kind, t!("Needs attention"), &a, window, cx);
            }
        }
    }

    /// Abre una página con lo que necesita para mostrarse (logs, terminal, promoción…).
    pub fn open_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.project_menu = false;
        match &page {
            Page::Guide(_) => {
                let ch = if let Page::Guide(c) = self.page { c } else { 0 };
                self.go(Page::Guide(ch), cx);
            }
            // Lo de un entorno vive en sus pestañas: estas páginas abren la del entorno que toca.
            Page::Local if self.env_of_kind(&["devbox", "throwaway"]).is_some() => {
                let env = self.env_of_kind(&["devbox", "throwaway"]).unwrap_or_default();
                self.open_env_tab(env, EnvTab::Services, cx);
            }
            Page::Live if self.env_of_kind(&["devbox", "throwaway"]).is_some() => {
                let env = self.env_of_kind(&["devbox", "throwaway"]).unwrap_or_default();
                self.open_env_tab(env, EnvTab::Live, cx);
            }
            Page::Local => {
                self.go(Page::Local, cx);
                self.follow_service_logs(cx);
            }
            Page::Terminal => {
                self.go(Page::Terminal, cx);
                if self.terminals.is_empty() {
                    self.new_terminal(window, cx);
                } else if let Some(t) = self.terminals.get(self.term_active) {
                    let h = t.read(cx).handle();
                    window.focus(&h, cx);
                }
            }
            Page::Workflows => {
                self.load_roles(cx);
                self.go(Page::Workflows, cx);
                self.load_workflows(cx);
                self.poll_workflows(cx);
            }
            Page::Repo => {
                self.go(Page::Repo, cx);
                self.load_git(cx);
            }
            Page::Frontend => {
                self.go(Page::Frontend, cx);
                self.load_frontend(false, cx);
            }
            Page::Agents => {
                self.go(Page::Agents, cx);
                self.load_chats(cx);
                self.load_agents(cx);
            }
            Page::Incidents => {
                self.go(Page::Incidents, cx);
                if self.incidents_tab_resolved {
                    self.load_resolved_incidents(cx);
                }
            }
            Page::Live => {
                self.snapshot = None;
                self.go(Page::Live, cx);
                // El bucle de telemetría termina al salir de la vista: rearrancarlo.
                self.start_polling_live(cx);
            }
            // Cada entorno vuelve a la pestaña en que se dejó.
            Page::Env(name) => {
                let tab = self.env_tabs.get(name).copied().unwrap_or(EnvTab::Summary);
                self.open_env_tab(name.clone(), tab, cx);
            }
            Page::Settings => {
                self.go(page, cx);
                self.load_policy(cx);
                self.load_agents(cx);
                self.load_notify(cx);
                self.get("/api/mcp".into(), |app, res| {
                    if let Load::Ready(v) = res {
                        app.mcp_status = Some(v);
                    }
                }, cx);
            }
            _ => self.go(page, cx),
        }
    }

    /// El primer entorno de alguno de esos tipos.
    pub fn env_of_kind(&self, kinds: &[&str]) -> Option<String> {
        self.overview
            .as_ref()?
            .get("environments")?
            .as_array()?
            .iter()
            .find(|e| kinds.iter().any(|k| e["kind"] == *k))
            .and_then(|e| e["name"].as_str().map(str::to_string))
    }

    /// Levanta, detiene o reinicia un servicio (o «all»). El cambio se ve al instante: los
    /// servicios quedan «levantando»/«deteniendo» hasta que el daemon confirma el estado final, y
    /// mientras tanto se consulta cada medio segundo (el sondeo normal es cada 3 s y una detención
    /// nativa dura menos: sin esto, el estado intermedio no se veía nunca).
    pub fn service_action(&mut self, name: &str, verb: &str, cx: &mut Context<Self>) {
        let now = ui::now_ms();
        let targets: Vec<String> = self
            .services
            .iter()
            .filter_map(|s| s["name"].as_str().map(str::to_string))
            .filter(|n| name == "all" || n == name)
            .collect();
        for n in &targets {
            self.pending.insert(format!("{}/{n}", self.project), (verb.to_string(), now));
        }
        let list = std::mem::take(&mut self.services);
        self.services = self.with_pending(list);
        cx.notify();
        self.post(format!("/api/services/{name}/{verb}?project={}", self.project), json!({}), |app, res, cx| {
            if let Load::Failed(e) = res {
                app.action = Some(Load::Failed(e));
                let project = app.project.clone();
                app.pending.retain(|k, _| !k.starts_with(&format!("{project}/")));
                let list = std::mem::take(&mut app.services);
                app.services = app.with_pending(list);
                cx.notify();
            }
        }, cx);
        self.follow_services(cx);
    }

    /// Pone «busy» en los servicios con una acción pendiente que el daemon aún no reflejó, y
    /// olvida las que ya terminaron (o llevan más de 2 minutos).
    pub fn with_pending(&mut self, mut list: Vec<Value>) -> Vec<Value> {
        let now = ui::now_ms();
        let project = self.project.clone();
        for s in list.iter_mut() {
            let key = format!("{project}/{}", s["name"].as_str().unwrap_or_default());
            let Some((verb, at)) = self.pending.get(&key).cloned() else { continue };
            let running = s["status"] == "running";
            let daemon_busy = s["busy"].is_string();
            // Lo pedido recién (menos de 1 s): el daemon puede no haberlo empezado todavía.
            let settled = !daemon_busy && now - at > 1000 && if verb == "stop" { !running } else { running };
            if settled || now - at > 120_000 {
                self.pending.remove(&key);
            } else if !daemon_busy {
                s["busy"] = json!(verb);
            }
        }
        list
    }

    /// Sondeo rápido de los servicios mientras haya acciones pendientes.
    fn follow_services(&self, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let Ok(project) = this.update(cx, |app, _| app.project.clone()) else {
                if gone(&this) {
                    break;
                }
                continue;
            };
            let d = daemon.clone();
            let path = format!("/api/services?project={project}");
            let res = cx.background_executor().spawn(async move { d.get(&path) }).await;
            let more = this
                .update(cx, |app, cx| {
                    if let (Ok(Value::Array(list)), true) = (res, app.project == project) {
                        app.services = app.with_pending(list);
                        cx.notify();
                    }
                    app.pending.keys().any(|k| k.starts_with(&format!("{project}/")))
                })
                .unwrap_or_else(|_| !gone(&this));
            if !more {
                break;
            }
        })
        .detach();
    }

    /// Estado del entorno local en una palabra, para los puntos y los botones.
    pub fn local_state(&self) -> LocalState {
        if let Some(b) = self.services.iter().find_map(|s| s["busy"].as_str()) {
            return if b == "stop" { LocalState::Stopping } else { LocalState::Starting };
        }
        let up = self.services.iter().filter(|s| s["status"] == "running").count();
        match up {
            0 => LocalState::Down,
            n if n == self.services.len() => LocalState::Up,
            _ => LocalState::Partial,
        }
    }

    /// Si se están mirando los servicios del devbox (en su resumen).
    pub fn viewing_services(&self) -> bool {
        match &self.page {
            Page::Env(e) => self.env_tab == EnvTab::Summary && self.env_of_kind(&["devbox"]).as_deref() == Some(e.as_str()),
            _ => false,
        }
    }

    /// Si se está mirando esa pestaña de un entorno.
    pub fn viewing(&self, tab: EnvTab) -> bool {
        matches!(self.page, Page::Env(_)) && self.env_tab == tab
    }

    /// Abre un entorno en una pestaña, con lo que esa pestaña necesita (logs que seguir, datos
    /// que refrescar mientras se mira).
    pub fn open_env_tab(&mut self, env: String, tab: EnvTab, cx: &mut Context<Self>) {
        self.project_menu = false;
        // Los servicios del devbox viven en su resumen.
        let tab = if tab == EnvTab::Services { EnvTab::Summary } else { tab };
        let page = Page::Env(env.clone());
        let moved = self.page != page;
        if moved {
            self.go(page, cx);
        }
        self.env_tabs.insert(env.clone(), tab);
        if !moved && self.env_tab == tab {
            return;
        }
        self.env_tab = tab;
        self.schema_fullscreen = false;
        match tab {
            EnvTab::Summary if self.viewing_services() => self.follow_service_logs(cx),
            EnvTab::Workflows => {
                self.load_workflows(cx);
                self.poll_workflows(cx);
            }
            EnvTab::Live => {
                // La pestaña de la app elegida se conserva; si no es de este entorno, la vista
                // elige la preferida de las suyas.
                self.snapshot = None;
                self.start_polling_live(cx);
            }
            EnvTab::Deploys => {
                self.env_deployments.insert(env.clone(), Load::Loading);
                self.get(format!("/api/deployments?env={env}"), move |app, r| {
                    app.env_deployments.insert(env, r);
                }, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Tras pedir un trabajo: en un entorno, su pestaña Trabajos; si no, la página de trabajos.
    pub fn show_jobs(&mut self, cx: &mut Context<Self>) {
        if let Page::Env(env) = self.page.clone() {
            self.open_env_tab(env, EnvTab::Jobs, cx);
        } else {
            self.go(Page::Jobs, cx);
        }
    }

    /// Tras pedir algo que espera aprobación: en la pestaña Promoción se aprueba ahí mismo; si no,
    /// la bandeja de aprobaciones.
    pub fn show_approvals(&mut self, cx: &mut Context<Self>) {
        if !self.viewing(EnvTab::Workflows) {
            self.go(Page::Approvals, cx);
        }
    }

    pub fn toggle_group(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.closed_groups.remove(id) {
            self.closed_groups.insert(id.to_string());
        }
        let list: Vec<&String> = self.closed_groups.iter().collect();
        self.prefs.set("sidebar.closed", &serde_json::to_string(&list).unwrap_or_default());
        cx.notify();
    }

    pub fn set_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.collapsed = collapsed;
        self.prefs.set_bool("sidebar.collapsed", collapsed);
        cx.notify();
    }

    /// Al cerrar el panel, lo visto queda leído.
    pub fn toggle_notifications(&mut self, cx: &mut Context<Self>) {
        if self.show_notifications {
            self.prefs.mark_all_read();
        }
        self.show_notifications = !self.show_notifications;
        cx.notify();
    }

    pub fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_theme(self.theme.next(), window, cx);
    }

    pub fn set_theme(&mut self, choice: crate::theme::Choice, window: &mut Window, cx: &mut Context<Self>) {
        self.theme = choice;
        self.prefs.set("theme", self.theme.as_str());
        crate::theme::apply(self.theme, window, cx);
        cx.notify();
    }

    /// Cambia el idioma de toda la app. Los campos de texto se vuelven a crear (sus
    /// textos de ayuda quedan con el idioma con que nacieron), así que pierden lo escrito.
    pub fn set_language(&mut self, lang: crate::i18n::Lang, window: &mut Window, cx: &mut Context<Self>) {
        crate::i18n::set_lang(lang);
        crate::i18n::apply_ely(cx);
        self.prefs.set("lang", lang.code());
        self.inputs.clear();
        crate::set_app_menus(cx);
        window.refresh();
        cx.notify();
    }


    /// Sigue los logs del servicio elegido mientras se mira «Entorno local».
    pub fn follow_service_logs(&mut self, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| loop {
            let target = this
                .update(cx, |app, _| (app.page == Page::Local || app.viewing_services()).then(|| (app.log_service(), app.project.clone())))
                .ok()
                .flatten();
            let Some((name, project)) = target else { break };
            let d = daemon.clone();
            let path = format!("/api/services/{name}/logs?tail=400&project={project}");
            let name = format!("{project}/{name}");
            let res = cx.background_executor().spawn(async move { d.get(&path) }).await;
            if this
                .update(cx, |app, cx| {
                    if let Ok(v) = res {
                        app.service_logs.insert(name, v["text"].as_str().unwrap_or_default().to_string());
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(2)).await;
        })
        .detach();
    }

    /// Servicio de la pestaña de logs elegida.
    pub fn log_service(&self) -> String {
        self.services.get(self.log_tab).or(self.services.first()).and_then(|s| s["name"].as_str()).unwrap_or("backend").to_string()
    }

    pub fn needs_welcome(&self) -> bool {
        !self.first_poll && self.error.is_none() && self.projects.is_empty()
    }

    /// Proyecto elegido (su JSON de /api/projects).
    pub fn current_project(&self) -> Option<&Value> {
        self.projects.iter().find(|p| p["name"] == self.project.as_str())
    }

    /// Cambia de proyecto en «Entorno local» y vuelve a pedir sus servicios.
    pub fn select_project(&mut self, name: String, cx: &mut Context<Self>) {
        if self.project == name {
            return;
        }
        self.project = name.clone();
        self.prefs.set("project", &name);
        self.services.clear();
        self.log_tab = 0;
        self.overview = None;
        self.jobs.clear();
        self.form_recipe = None;
        self.form_env = None;
        // Páginas que solo existen para heskala-erp.
        if name != "heskala-erp" && matches!(self.page, Page::Live | Page::Env(_)) {
            self.page = Page::Overview;
        }
        let n2 = name.clone();
        self.get(format!("/api/overview?project={name}"), move |app, res| {
            if let (Load::Ready(v), true) = (res, app.project == n2) {
                app.overview = Some(v);
            }
        }, cx);
        let n3 = name.clone();
        self.get(format!("/api/jobs?limit=60&project={name}"), move |app, res| {
            if let (Load::Ready(Value::Array(list)), true) = (res, app.project == n3) {
                app.jobs = list;
            }
        }, cx);
        self.get(format!("/api/services?project={name}"), move |app, res| {
            if let (Load::Ready(Value::Array(list)), true) = (res, app.project == name) {
                app.services = list;
            }
        }, cx);
        cx.notify();
    }

    /// Abre una terminal nueva en el repo del proyecto elegido.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project_repo = self.current_project().and_then(|p| p["repo"].as_str()).map(std::path::PathBuf::from);
        let cwd = project_repo
            .or_else(|| self.daemon.as_ref().and_then(|d| d.repo.clone()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| "/".into());
        match crate::terminal::Pty::open(&cwd) {
            Ok(pty) => {
                let n = self.terminals.len() + 1;
                let title = if self.project == "heskala-erp" { t!("Terminal {n}", n = n) } else { format!("{} · {n}", self.project) };
                let view = cx.new(|cx| crate::terminal::TerminalView::new(title, pty, window, cx));
                self.terminals.push(view);
                self.term_active = self.terminals.len() - 1;
                self.term_error = None;
            }
            Err(e) => self.term_error = Some(t!("Couldn't open the terminal: {e}", e = e)),
        }
        cx.notify();
    }

    /// Abre un trabajo y sigue su log mientras esté abierto.
    pub fn open_job(&mut self, id: String, cx: &mut Context<Self>) {
        self.job = Some(id.clone());
        self.job_detail = None;
        self.job_log.clear();
        self.job_log_offset = 0;
        cx.notify();
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| loop {
            let offset = this.update(cx, |app, _| (app.job.as_deref() == Some(id.as_str())).then_some(app.job_log_offset)).ok().flatten();
            let Some(offset) = offset else { break };
            let d = daemon.clone();
            let path = format!("/api/jobs/{id}");
            let (detail, log) =
                cx.background_executor().spawn(async move { (d.get(&path), d.get(&format!("{path}/log?offset={offset}"))) }).await;
            let finished = matches!(
                detail.as_ref().ok().and_then(|j| j["status"].as_str()),
                Some("succeeded" | "failed" | "cancelled" | "rejected" | "expired")
            );
            let keep = this
                .update(cx, |app, cx| {
                    if app.job.as_deref() != Some(id.as_str()) {
                        return false;
                    }
                    if let Ok(d) = detail {
                        app.job_detail = Some(d);
                    }
                    if let Ok(l) = log {
                        app.job_log.push_str(l["text"].as_str().unwrap_or_default());
                        app.job_log_offset = l["offset"].as_u64().unwrap_or(offset);
                        // La vista muestra las últimas ~400 líneas.
                        let lines: Vec<&str> = app.job_log.lines().collect();
                        if lines.len() > 400 {
                            app.job_log = lines[lines.len() - 400..].join("\n") + "\n";
                        }
                    }
                    cx.notify();
                    true
                })
                .unwrap_or_else(|_| !gone(&this));
            if !keep || finished {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(1)).await;
        })
        .detach();
    }

    /// POST al daemon en el fondo; `store` recibe el resultado.
    pub fn post(&mut self, path: String, body: Value, store: impl FnOnce(&mut Self, Load, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { daemon.post(&path, body) }).await;
            this.update(cx, |app, cx| {
                store(app, res.into(), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// DELETE al daemon en el fondo; `store` recibe el resultado.
    pub fn delete(&mut self, path: String, store: impl FnOnce(&mut Self, Load, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { daemon.delete(&path) }).await;
            this.update(cx, |app, cx| {
                store(app, res.into(), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// GET al daemon en el fondo; `store` recibe el resultado.
    pub fn get(&mut self, path: String, store: impl FnOnce(&mut Self, Load) + 'static, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { daemon.get(&path) }).await;
            this.update(cx, |app, cx| {
                store(app, res.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Aprobación reforzada: Touch ID primero, después el POST.
    pub fn approve_with_touch_id(&mut self, id: String, confirm: String, title: String, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        self.action = Some(Load::Loading);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reason = t!("approve “{title}” on {confirm}", title = title, confirm = confirm);
            let path = format!("/api/approvals/{id}");
            let res: anyhow::Result<Value> = cx
                .background_executor()
                .spawn(async move {
                    crate::touchid::authenticate(&reason).map_err(|e| anyhow::anyhow!(e))?;
                    daemon.post(&path, json!({ "approve": true, "confirm": confirm, "touchId": true }))
                })
                .await;
            this.update(cx, |app, cx| {
                match res {
                    Ok(_) => {
                        app.approvals.retain(|j| j["id"] != id.as_str());
                        app.action = None;
                        app.show_jobs(cx);
                        app.open_job(id, cx);
                    }
                    Err(e) => app.action = Some(Load::Failed(e.to_string())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if page != self.page {
            self.schema_fullscreen = false;
        }
        if let Page::Env(env) = &page {
            if page != self.page {
                self.env_tab = self.env_tabs.get(env).copied().unwrap_or(EnvTab::Summary);
                self.schema_mode = self.schema_modes.get(env).copied().unwrap_or(crate::views::schema::Mode::Focus);
            }
        }
        self.page = page;
        self.action = None;
        cx.notify();
    }

    pub fn env_health(&self, env: &Value) -> Health {
        // El devbox levantando o deteniéndose: en naranja, en todas partes.
        if env["kind"] == "devbox" && matches!(self.local_state(), LocalState::Starting | LocalState::Stopping) {
            return Health::Warn;
        }
        if env["convex"]["reachable"] == true {
            Health::Ok
        } else if env["convex"]["checkedAt"].is_null() {
            Health::Unknown
        } else {
            Health::Bad
        }
    }

    pub fn start_polling_live(&self, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| loop {
            let target = this.update(cx, |app, _| (app.page == Page::Live || app.viewing(EnvTab::Live)).then(|| app.live_tab.clone())).ok().flatten();
            let Some(tab) = target else { break };
            let d = daemon.clone();
            let args = match &tab {
                Some(id) => json!({ "tabId": id }),
                None => json!({}),
            };
            let snap = cx.background_executor().spawn(async move { d.tool("get_snapshot", args) }).await;
            if this
                .update(cx, |app, cx| {
                    if app.live_tab == tab {
                        app.snapshot = Some(snap.into());
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
            cx.background_executor().timer(LIVE_POLL).await;
        })
        .detach();
    }
}

impl Render for DevtoolApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        self.narrow = width < 1000.;
        self.tiny = width < 820.;
        // Los errores de una acción ya no son banners en la página: van al centro de notificaciones.
        if let Some(Load::Failed(e)) = self.action.clone() {
            if self.notified_action.as_deref() != Some(e.as_str()) {
                self.notified_action = Some(e.clone());
                let id = format!("action:{}:{e}", ui::now_ms());
                self.notify(&id, "error", t!("Couldn't complete"), &e, window, cx);
            }
        }
        if let Some(e) = self.term_error.take() {
            self.notify(&format!("terminal:{}", ui::now_ms()), "error", t!("Terminal"), &e, window, cx);
        }
        self.save_view();
        let content: AnyElement = match self.page.clone() {
            // Sin proyectos todavía (y con el daemon respondiendo): la guía del primer arranque.
            Page::Overview if self.needs_welcome() => views::welcome::render(self, window, cx).into_any_element(),
            Page::Overview => views::overview::render(self, window, cx).into_any_element(),
            Page::Local => views::local::render(self, window, cx).into_any_element(),
            Page::Terminal => views::terminal::render(self, window, cx).into_any_element(),
            Page::Env(name) => views::env::render(self, &name, window, cx).into_any_element(),
            Page::Live => views::live::render(self, window, cx).into_any_element(),
            Page::Repo => views::repo::render(self, window, cx).into_any_element(),
            Page::Frontend => views::frontend::render(self, window, cx).into_any_element(),
            Page::Incidents => views::incidents::render(self, window, cx).into_any_element(),
            Page::Agents => views::agents::render(self, window, cx).into_any_element(),
            Page::Workflows => views::workflows::render(self, window, cx).into_any_element(),
            Page::Jobs => views::jobs::render(self, window, cx).into_any_element(),
            Page::Approvals => views::approvals::render(self, window, cx).into_any_element(),
            Page::Guide(ch) => views::guide::render(self, ch, window, cx).into_any_element(),
            Page::Settings => views::settings::render(self, window, cx).into_any_element(),
        };
        let focus = self.focus.clone();
        let palette = views::palette::render(self, cx);
        div()
            .size_full()
            .on_action(cx.listener(|this, _: &crate::nav::OpenPalette, _, cx| views::palette::open(this, cx)))
            .child(ely_core::primitives::FocusScope::new(&focus).root().size_full().child(crate::shell::render(self, content, cx)))
            .children(palette)
    }
}


