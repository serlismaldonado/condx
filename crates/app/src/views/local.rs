// Entorno local: levantar y detener el backend, el deployer y el frontend del
// proyecto (los maneja el daemon) en una lista al estilo del ContainerList de
// Ely, con su CPU y memoria, el monitor de recursos, los logs con nivel y
// búsqueda, y plegados el proyecto (agregar y quitar) y el modo Docker/nativo.

use ely_core::buttons::IconButton;
use ely_core::data_display::{Badge, Tone};
use ely_core::forms::{SearchInput, Select};
use ely_core::primitives::{Icon as Icon2, IconName as IconName2};
use ely_devtools::{Reading, ResourceMonitor};
use ely_terminal::LogViewer;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, IconName, Sizable, WindowExt};
use gpui_kit::component::StyledExt;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};


fn action(app: &mut DevtoolApp, name: &str, verb: &str, cx: &mut Context<DevtoolApp>) {
    app.service_action(name, verb, cx);
}

/// Panel lateral para agregar un proyecto: la carpeta a mano o uno de los encontrados en la
/// carpeta de proyectos. Al elegir, se cierra y el proyecto nuevo queda elegido; si falla, el
/// error va a las notificaciones.
pub fn open_add_project(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) {
    app.project_menu = false;
    if !matches!(app.discover, Some(Load::Ready(_))) {
        app.discover = Some(Load::Loading);
        app.get("/api/projects/discover".into(), |app, res| app.discover = Some(res), cx);
    }
    let state = app
        .inputs
        .entry("project-path".into())
        .or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder("~/Documents/Software/mi-proyecto")))
        .clone();
    let me = cx.entity();
    window.open_sheet(cx, move |sheet, _, cx| {
        let t = crate::theme::Tokens::current(cx);
        let (m_add, s_add) = (me.clone(), state.clone());
        let mut body = v_flex()
            .gap_4()
            .child(ui::muted(
                t!("A folder with convex/ and package.json. Condx gives it its own ports and starts its backend, deployer and frontend without touching its .env.local."),
                cx,
            ))
            .child(
                v_flex()
                    .gap_1()
                    .child(ui::section(t!("Project folder"), cx))
                    .child(
                        h_flex().gap_2().child(div().flex_1().child(Input::new(&state).small())).child(ui::brand(
                            Button::new("project-register").small().label(t!("Add")).on_click(move |_, window, cx| {
                                let path = s_add.read(cx).value().trim().to_string();
                                if path.is_empty() {
                                    return;
                                }
                                window.close_sheet(cx);
                                m_add.update(cx, |app, cx| register(app, path, cx));
                            }),
                        cx)),
                    ),
            );
        // Los encontrados en la carpeta de proyectos, en filas con su proyecto de Convex y su frontend.
        let discover = me.read(cx).discover.clone();
        let mut found = v_flex().gap_1();
        match discover {
            Some(Load::Ready(v)) => {
                found = found.child(ui::section(t!("Found in {a}", a = ui::txt(&v["root"])), cx));
                let candidates: Vec<Value> = v["candidates"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| c["project"].is_null()).collect();
                if candidates.is_empty() {
                    found = found.child(ui::muted(t!("Nothing left to add."), cx));
                }
                let mut list = v_flex().border_1().border_color(t.card_border);
                for (i, c) in candidates.into_iter().enumerate() {
                    let path = c["path"].as_str().unwrap_or_default().to_string();
                    let label = path.rsplit('/').next().unwrap_or(&path).to_string();
                    let extra = [c["convexProject"].as_str().map(|p| t!("Convex: {p}", p = p)), c["frontend"].as_str().map(str::to_string)]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let m = me.clone();
                    list = list.child(
                        h_flex()
                            .gap_2()
                            .px_2()
                            .py_1p5()
                            .items_center()
                            .when(i > 0, |d| d.border_t_1().border_color(t.card_border))
                            .child(v_flex().flex_1().min_w_0().child(div().text_color(t.fg).truncate().child(label)).child(div().truncate().child(ui::muted(if extra.is_empty() { path.clone() } else { extra }, cx))))
                            .child(Button::new(SharedString::from(format!("cand-{i}"))).xsmall().outline().icon(IconName::Plus).label(t!("Add")).on_click(move |_, window, cx| {
                                window.close_sheet(cx);
                                let path = path.clone();
                                m.update(cx, |app, cx| register(app, path, cx));
                            })),
                    );
                }
                found = found.child(list);
            }
            Some(Load::Failed(e)) => found = found.child(ui::muted(e, cx)),
            _ => found = found.child(h_flex().gap_2().child(Spinner::new().small()).child(ui::muted(t!("Looking for projects…"), cx))),
        }
        body = body.child(found);
        sheet.title(t!("Add existing project")).size(px(460.)).child(body)
    });
}

/// Panel lateral para crear un proyecto nuevo en Convex (receta project-create): carpeta con
/// plantilla, dependencias, proyecto en la cuenta de Convex y registro en el devtool. Pide
/// aprobación (crea recursos en la cuenta), así que al pedirlo se va a la bandeja.
pub fn open_create_project(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) {
    app.project_menu = false;
    let field = |app: &mut DevtoolApp, key: &str, default: &str, placeholder: &str, window: &mut Window, cx: &mut Context<DevtoolApp>| {
        let (d, p) = (default.to_string(), placeholder.to_string());
        app.inputs.entry(key.into()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(p).default_value(d))).clone()
    };
    let name = field(app, "create-name", "", "mi-proyecto", window, cx);
    let parent = field(app, "create-parent", "~/Documents/Software", "~/Documents/Software", window, cx);
    let team = field(app, "create-team", "", t!("the one from your session"), window, cx);
    let me = cx.entity();
    window.open_sheet(cx, move |sheet, _, cx| {
        let row = |label: &'static str, input: &Entity<InputState>, note: &'static str, cx: &App| {
            v_flex().gap_1().child(ui::section(label, cx)).child(Input::new(input).small()).child(ui::muted(note, cx))
        };
        let (m, n, p, tm) = (me.clone(), name.clone(), parent.clone(), team.clone());
        // Las plantillas de create-convex más usadas para aplicaciones web, por frontend.
        let templates = [
            ("react-vite", "React + Vite", t!("SPA, the simplest")),
            ("react-vite-shadcn", "React + Vite + shadcn/ui", t!("with UI components")),
            ("react-vite-convexauth-shadcn", "React + Vite + Convex Auth", t!("with login and shadcn/ui")),
            ("react-vite-clerk-shadcn", "React + Vite + Clerk", t!("with Clerk login and shadcn/ui")),
            ("react-vite-authkit", "React + Vite + WorkOS AuthKit", t!("with WorkOS login")),
            ("nextjs", "Next.js", "App Router"),
            ("nextjs-shadcn", "Next.js + shadcn/ui", t!("App Router with UI components")),
            ("nextjs-convexauth", "Next.js + Convex Auth", t!("with login")),
            ("nextjs-clerk-shadcn", "Next.js + Clerk", t!("with Clerk login and shadcn/ui")),
            ("nextjs-authkit", "Next.js + WorkOS AuthKit", t!("with WorkOS login")),
            ("tanstack-start", "TanStack Start", t!("Full-stack React with typed routes")),
            ("tanstack-start-clerk", "TanStack Start + Clerk", t!("with Clerk login")),
            // Better Auth: los ejemplos oficiales del componente (convex/better-auth); el paso de
            // plantilla cambia sus dependencias locales del monorepo por versiones de npm.
            ("convex/better-auth/examples/react", "React + Vite + Better Auth", t!("with Better Auth login")),
            ("convex/better-auth/examples/next", "Next.js + Better Auth", t!("with Better Auth login")),
            ("convex/better-auth/examples/tanstack", "TanStack Start + Better Auth", t!("with Better Auth login")),
            ("bare", t!("Convex only"), t!("no frontend")),
        ];
        let chosen = me.read(cx).create_template.clone();
        let m_tpl = me.downgrade();
        let picker = Select::new("create-template", templates.iter().map(|(v, l, n)| crate::ely::choice_note(*v, *l, *n)).collect::<Vec<_>>())
            .selected(chosen.clone())
            .on_change(move |v, _, cx| {
                let v = v.to_string();
                let _ = m_tpl.update(cx, |app, cx| {
                    app.create_template = v;
                    cx.notify();
                });
            });
        let body = v_flex()
            .gap_4()
            .child(ui::muted(
                t!("Creates a project from scratch in your Convex account: generates the folder from a template, installs dependencies, creates the project and its development deployment, and adds it to Condx. Uses this Mac's Convex CLI session (npx convex login). For a folder that already exists, use “Add existing project”."),
                cx,
            ))
            .child(row(t!("Name"), &name, t!("Folder and Convex project: lowercase letters, digits and hyphens."), cx))
            .child(row(t!("Parent folder"), &parent, t!("The project goes in a new folder inside this one."), cx))
            .child(v_flex().gap_1().child(ui::section(t!("Frontend template"), cx)).child(picker).child(ui::muted(t!("From create-convex (Convex's official templates)."), cx)))
            .child(row(t!("Convex team"), &team, t!("Its slug. Empty: your session's team, if you only have one."), cx))
            .child(ui::muted(t!("Needs approval: it creates resources in your Convex account. Then follow it in the Jobs tab."), cx))
            .child(h_flex().child(div().flex_1()).child(ui::brand(Button::new("project-create").small().icon(IconName::Plus).label(t!("Create project")).on_click(move |_, window, cx| {
                let value = |s: &Entity<InputState>, cx: &App| s.read(cx).value().trim().to_string();
                let (name, parent) = (value(&n, cx), value(&p, cx));
                let template = m.read(cx).create_template.clone();
                let team = Some(value(&tm, cx)).filter(|t| !t.is_empty()).unwrap_or_else(|| "auto".into());
                if name.is_empty() {
                    return;
                }
                window.close_sheet(cx);
                m.update(cx, |app, cx| {
                    let Some(env) = app.env_of_kind(&["devbox"]) else {
                        app.action = Some(Load::Failed(t!("there's no devbox environment to run the creation in").into()));
                        return;
                    };
                    let body = json!({ "recipe": "project-create", "env": env, "args": { "name": name, "parent": parent, "template": template, "team": team } });
                    app.post("/api/jobs".into(), body, |app, res, cx| match res {
                        Load::Ready(job) if job["status"] == "waiting-approval" => app.go(crate::app::Page::Approvals, cx),
                        Load::Ready(job) => {
                            if let Some(id) = job["id"].as_str() {
                                app.go(crate::app::Page::Jobs, cx);
                                app.open_job(id.to_string(), cx);
                            }
                        }
                        Load::Failed(e) => app.action = Some(Load::Failed(e)),
                        _ => {}
                    }, cx);
                });
            }), cx)));
        sheet.title(t!("Create Convex project")).size(px(460.)).child(body)
    });
}

/// Desvincula un proyecto del devtool, con un «¿seguro?»: deja de aparecer, se detienen sus
/// servicios y se quitan sus entornos. No borra su carpeta, sus datos locales ni su proyecto en
/// Convex: se puede volver a agregar.
pub fn confirm_unlink(app: &mut DevtoolApp, name: String, window: &mut Window, cx: &mut Context<DevtoolApp>) {
    app.project_menu = false;
    let entity = cx.entity().downgrade();
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (entity, name) = (entity.clone(), name.clone());
        alert
            .title(t!("Remove “{name}” from Condx?", name = name))
            .description(t!("It stops showing here, its services stop and its environments are removed. Its folder, local data and Convex project are not deleted: you can add it again."))
            .ok_text(t!("Remove"))
            .cancel_text(t!("Cancel"))
            .show_cancel(true)
            .on_ok(move |_, _, cx| {
                let name = name.clone();
                let _ = entity.update(cx, |app, cx| unlink(app, name, cx));
                true
            })
    });
}

fn unlink(app: &mut DevtoolApp, name: String, cx: &mut Context<DevtoolApp>) {
    app.delete(format!("/api/projects/{name}"), move |app, res, cx| match res {
        Load::Ready(_) => {
            app.projects.retain(|p| p["name"] != name.as_str());
            // Vuelve a aparecer entre los encontrados al agregar.
            app.discover = None;
            if app.project == name {
                app.select_project("heskala-erp".into(), cx);
            }
            cx.notify();
        }
        Load::Failed(e) => app.action = Some(Load::Failed(e)),
        _ => {}
    }, cx);
}

fn register(app: &mut DevtoolApp, path: String, cx: &mut Context<DevtoolApp>) {
    app.post("/api/projects".into(), json!({ "path": path }), |app, res, cx| match res {
        Load::Ready(p) => {
            app.discover = None;
            app.inputs.remove("project-path");
            app.action = None;
            if let Some(name) = p["name"].as_str() {
                app.projects.push(p.clone());
                app.select_project(name.to_string(), cx);
            }
        }
        Load::Failed(e) => app.action = Some(Load::Failed(e)),
        _ => {}
    }, cx);
}

/// La ficha del proyecto elegido, con el botón para agregar otro (abre un panel lateral).
fn projects_card(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    // El proyecto se elige en la barra lateral; aquí, su ficha y agregar otros.
    let mut chips = h_flex().gap_2().items_center().child(ui::section(t!("Project · {a}", a = app.project), cx));
    chips = chips.child(div().flex_1()).child(
        Button::new("project-add")
            .small()
            .outline()
            .icon(IconName::Plus)
            .label(t!("Add existing"))
            .on_click(cx.listener(|this, _, window, cx| open_add_project(this, window, cx))),
    ).child(
        Button::new("project-new")
            .small()
            .ghost()
            .icon(IconName::Plus)
            .label(t!("Create in Convex"))
            .on_click(cx.listener(|this, _, window, cx| open_create_project(this, window, cx))),
    );
    let mut card = ui::card(cx).child(chips);
    if let Some(p) = app.current_project().cloned() {
        let convex = match (p["convexTeam"].as_str(), p["convexProject"].as_str()) {
            (Some(t), Some(c)) => format!("{t} / {c}"),
            (_, Some(c)) => c.to_string(),
            _ => t!("not detected").into(),
        };
        let frontend = match p["frontendUrl"].as_str() {
            Some(url) => format!("{} · {}", p["frontendKind"].as_str().unwrap_or("frontend"), url),
            None => t!("not detected (neither Vite nor Next)").into(),
        };
        let mut info = v_flex()
            .gap_1()
            .child(ui::kv(t!("Folder"), ui::txt(&p["repo"]), cx))
            .child(ui::kv(t!("Convex project"), convex, cx))
            .child(ui::kv(t!("Local backend"), t!("{a} (site {b})", a = ui::txt(&p["backendUrl"]), b = ui::txt(&p["siteUrl"])), cx))
            .child(ui::kv(t!("Frontend"), frontend, cx))
            .child(ui::kv(t!("Data"), ui::txt(&p["dataDir"]), cx));
        if p["default"] != true {
            let name = app.project.clone();
            info = info.child(ui::kv(
                t!("Cloud dev"),
                p["cloudDev"].as_str().map(str::to_string).unwrap_or_else(|| t!("no CONVEX_DEPLOYMENT=dev:… in its .env.local").into()),
                cx,
            ));
            let devbox = app
                .overview
                .as_ref()
                .and_then(|o| o["environments"].as_array()?.iter().find(|e| e["kind"] == "devbox").and_then(|e| e["name"].as_str()).map(str::to_string))
                .unwrap_or_else(|| "local".into());
            let has_cloud = p["cloudDev"].is_string();
            let seed = |id: &'static str, label: &'static str, recipe: &'static str, cx: &mut Context<DevtoolApp>| {
                let (name, devbox) = (name.clone(), devbox.clone());
                Button::new(id).xsmall().outline().icon(IconName::ArrowDown).label(label).disabled(!has_cloud).on_click(cx.listener(move |this, _, _, cx| {
                    let body = json!({ "recipe": recipe, "env": devbox.clone(), "args": { "project": name.clone() } });
                    this.post("/api/jobs".into(), body, |app, res, cx| match res {
                        Load::Ready(job) if job["status"] == "waiting-approval" => app.show_approvals(cx),
                        Load::Ready(job) => {
                            if let Some(id) = job["id"].as_str() {
                                app.show_jobs(cx);
                                app.open_job(id.to_string(), cx);
                            }
                        }
                        Load::Failed(e) => app.action = Some(Load::Failed(e)),
                        _ => {}
                    }, cx);
                }))
            };
            info = info.child(
                h_flex()
                    .gap_1()
                    .child(seed("seed-dev", t!("Pull data and variables from cloud dev"), "project-seed", cx))
                    .child(seed("seed-prod", t!("Copy production (with approval)"), "project-seed-prod", cx)),
            );
            info = info.child(
                h_flex().child(
                    Button::new("project-remove")
                        .xsmall()
                        .ghost()
                        .icon(IconName::Delete)
                        .label(t!("Remove from Condx (keeps the data)"))
                        .on_click(cx.listener(move |this, _, window, cx| confirm_unlink(this, name.clone(), window, cx))),
                ),
            );
        }
        card = card.child(info);
    }
    card
}

/// Levantar y detener todo, según cómo esté el entorno: con nada arriba, solo «Levantar todo»;
/// con todo arriba, solo «Detener todo» (en rojo); a medias, los dos. Van en la fila de las
/// pestañas del entorno y en su fila de la vista general.
pub fn service_actions(app: &DevtoolApp, cx: &mut Context<DevtoolApp>) -> Div {
    use crate::app::LocalState;
    let state = app.local_state();
    let mut row = h_flex().gap_2().items_center();
    // Mientras levanta o detiene: un solo botón apagado, en naranja, con lo que está haciendo.
    if matches!(state, LocalState::Starting | LocalState::Stopping) {
        return row.child(
            Button::new("services-busy")
                .xsmall()
                .warning()
                .loading(true)
                .label(if state == LocalState::Stopping { "Deteniendo…" } else { "Levantando…" })
                // Sin `disabled`: apagado se perdía el naranja. No hace nada al tocarlo.
                .on_click(|_, _, cx| cx.stop_propagation()),
        );
    }
    if state != LocalState::Up {
        row = row.child(ui::brand(
            Button::new("start-all")
                .xsmall()
                .icon(IconName::Play)
                .label(if state == LocalState::Down { t!("Start all") } else { t!("Start the rest") })
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    action(this, "all", "start", cx)
                })),
            cx,
        ));
    }
    if state != LocalState::Down {
        row = row.child(
            Button::new("stop-all")
                .xsmall()
                .danger()
                .icon(IconName::Square)
                .label(t!("Stop all"))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    action(this, "all", "stop", cx)
                })),
        );
    }
    row
}

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    render_with(app, true, None, window, cx)
}

/// Los servicios; con `actions`, con sus botones arriba (si no, los pone quien la monta). `extra`
/// es un plegado más (la ficha de Convex del entorno), entre los logs y el proyecto.
pub fn render_with(app: &mut DevtoolApp, actions: bool, extra: Option<(SharedString, AnyElement)>, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let header = actions.then(|| h_flex().child(div().flex_1()).child(service_actions(app, cx)));
    let services = services_list(app, cx);
    let monitor = resource_monitor(app);
    let logs = logs_card(app, window, cx);

    // Modo del entorno local: Docker o nativo (sin Docker).
    let native = app.services.first().map(|s| s["mode"] == "native").unwrap_or(false);
    let devbox = app
        .overview
        .as_ref()
        .and_then(|o| o["environments"].as_array()?.iter().find(|e| e["kind"] == "devbox").and_then(|e| e["name"].as_str()).map(str::to_string))
        .unwrap_or_else(|| "local".into());
    let mode_card = {
        let (d1, d2) = (devbox.clone(), devbox.clone());
        let migrate = Button::new("migrate-native")
            .small()
            .icon(IconName::HardDrive)
            .label(t!("Migrate to native mode"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.post("/api/jobs".into(), json!({ "recipe": "migrate-native", "env": d1.clone() }), |app, res, cx| {
                    match res {
                        Load::Ready(_) => app.show_approvals(cx),
                        Load::Failed(e) => app.action = Some(Load::Failed(e)),
                        _ => {}
                    }
                }, cx);
            }));
        let switch = Button::new("switch-mode")
            .small()
            .outline()
            .label(if native { t!("Back to Docker") } else { t!("Use native mode") })
            .on_click(cx.listener(move |this, _, _, cx| {
                let mode = if native { "docker" } else { "native" };
                this.post("/api/jobs".into(), json!({ "recipe": "local-mode", "env": d2.clone(), "args": { "mode": mode } }), |app, res, cx| {
                    match res {
                        Load::Ready(job) => {
                            if let Some(id) = job["id"].as_str() {
                                app.show_jobs(cx);
                                app.open_job(id.to_string(), cx);
                            }
                        }
                        Load::Failed(e) => app.action = Some(Load::Failed(e)),
                        _ => {}
                    }
                }, cx);
            }));
        let text = if native {
            t!("Native mode: the backend is the convex-local-backend binary and the deployer is convex dev, both as daemon processes. No Docker or OrbStack needed.")
        } else {
            t!("Docker mode: backend and deployer run in containers (OrbStack). “Migrate to native mode” copies the data and variables to the native backend and stops needing Docker; the volume stays intact.")
        };
        ui::card(cx)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().font_semibold().child(t!("Local environment mode")))
                    .child(ui::status_tag(if native { Health::Ok } else { Health::Unknown }, if native { t!("native") } else { t!("Docker") }))
                    .child(div().flex_1())
                    .children((!native).then(|| ui::brand(migrate, cx)))
                    .child(switch),
            )
            .child(ui::muted(text, cx))
    };

    let is_default = app.project == "heskala-erp";
    let projects = projects_card(app, cx);
    // Debajo de los servicios, todo plegado: los logs (abiertos al entrar), la ficha de Convex del
    // entorno, el proyecto y el modo. Los errores van a las notificaciones.
    let mut details = ui::Folds::new("local-details").first_open().item("logs", t!("Logs"), logs);
    if let Some((title, el)) = extra {
        details = details.item("convex", title, el);
    }
    details = details.item("project", t!("Project · {a}", a = app.project), projects);
    if is_default {
        details = details.item("mode", t!("Local environment mode"), mode_card);
    }
    let details = details.render(app, cx);
    v_flex().gap_3().p_4().children(header).children(monitor).child(services).child(details)
}


fn service_icon(name: &str) -> IconName2 {
    match name {
        "backend" => IconName2::Server,
        "deployer" => IconName2::Upload,
        _ => IconName2::Globe,
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_048_576.0)
}

/// Estado del servicio en una insignia de Ely (en español).
fn service_badge(s: &Value) -> Badge {
    let (label, tone) = if let Some(b) = s["busy"].as_str() {
        // Los estados intermedios van en naranja, como en los botones y los puntos.
        (match b { "start" => t!("starting"), "stop" => t!("stopping"), _ => t!("restarting") }, Tone::Warning)
    } else {
        match s["status"].as_str() {
            Some("running") if s["healthy"] == false && s["name"] != "deployer" => (t!("booting"), Tone::Warning),
            Some("running") => (t!("running"), Tone::Success),
            Some("external") => (t!("port in use"), Tone::Warning),
            Some("absent") => (t!("no container"), Tone::Neutral),
            _ => (t!("stopped"), Tone::Neutral),
        }
    };
    Badge::new(label).tone(tone).dot()
}

/// Los servicios como filas (imagen, puerto, carga y acciones), como el ContainerList de Ely.
fn services_list(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let t = crate::theme::Tokens::current(cx);
    let mut list = v_flex().border_1().border_color(t.card_border);
    if app.services.is_empty() {
        return list.child(div().p_3().child(ui::muted(t!("Waiting for the daemon…"), cx)));
    }
    for (i, s) in app.services.clone().into_iter().enumerate() {
        let name = s["name"].as_str().unwrap_or_default().to_string();
        let running = matches!(s["status"].as_str(), Some("running"));
        let busy = s["busy"].is_string();
        let mut line = vec![];
        if let Some(p) = s["port"].as_u64() {
            line.push(format!(":{p}"));
        }
        if let (Some(c), Some(m)) = (s["cpu"].as_f64(), s["memory"].as_u64()) {
            line.push(format!("{c:.0}% CPU"));
            line.push(mb(m));
        }
        if let Some(phase) = s["phase"].as_str().filter(|p| *p != "unknown") {
            line.push(t!("phase {phase}", phase = ui::phase_label(phase)));
        }
        if line.is_empty() {
            line.push(ui::txt(&s["detail"]).to_string());
        }
        let (n1, n2, n3) = (name.clone(), name.clone(), name.clone());
        let selected_log = app.log_service() == name;
        let mut actions = h_flex().gap_0p5().items_center();
        actions = actions.child(
            IconButton::new(SharedString::from(format!("toggle-{name}")), if running { IconName2::Square } else { IconName2::Play })
                .tooltip(if running { t!("Stop") } else { t!("Start") })
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| action(this, &n1, if running { "stop" } else { "start" }, cx))),
        );
        actions = actions.child(
            IconButton::new(SharedString::from(format!("restart-{name}")), IconName2::RotateCw)
                .tooltip(t!("Restart"))
                .disabled(!running || busy)
                .on_click(cx.listener(move |this, _, _, cx| action(this, &n2, "restart", cx))),
        );
        actions = actions.child(
            IconButton::new(SharedString::from(format!("logs-{name}")), IconName2::Terminal)
                .tooltip(t!("View its logs"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(ix) = this.services.iter().position(|s| s["name"] == n3.as_str()) {
                        this.log_tab = ix;
                        this.follow_service_logs(cx);
                        cx.notify();
                    }
                })),
        );
        if let (Some(url), true) = (s["url"].as_str().map(str::to_string), matches!(name.as_str(), "vite" | "frontend")) {
            actions = actions.child(IconButton::new("open-frontend", IconName2::ExternalLink).tooltip(t!("Open in browser")).disabled(!running).on_click(move |_, _, cx| cx.open_url(&url)));
        }
        let error = s["error"].as_str().map(str::to_string);
        list = list.child(
            v_flex()
                .when(i > 0, |d| d.border_t_1().border_color(t.card_border))
                .when(selected_log, |d| d.bg(t.hover))
                .child(
                    h_flex()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .items_center()
                        .child(Icon2::new(service_icon(&name)).size(ely_core::theme::IconSize::Sm).color(t.subtle))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_color(t.fg).child(ui::txt(&s["title"])))
                                .child(div().text_size(px(12.)).text_color(t.subtle).truncate().child(line.join(" · "))),
                        )
                        .when_some(s["convexUrl"].as_str().map(str::to_string), |d, u| d.child(ui::muted(format!("Convex {u}"), cx)))
                        .child(service_badge(&s))
                        .child(actions),
                )
                .when_some(error, |d, e| d.child(div().px_3().pb_2().text_size(px(12.)).text_color(cx.theme().danger).child(e))),
        );
    }
    list
}

/// CPU y memoria de los servicios que corren, sumados, con su minigráfica.
fn resource_monitor(app: &DevtoolApp) -> Option<ResourceMonitor> {
    let running: Vec<&Value> = app.services.iter().filter(|s| s["cpuHistory"].is_array()).collect();
    if running.is_empty() {
        return None;
    }
    let len = running.iter().map(|s| s["cpuHistory"].as_array().map(Vec::len).unwrap_or(0)).max().unwrap_or(0);
    let sum = |key: &str, scale: f64| -> Vec<f32> {
        (0..len)
            .map(|i| {
                running
                    .iter()
                    .map(|s| {
                        let h = s[key].as_array().cloned().unwrap_or_default();
                        // Alineadas por el final: la más nueva de cada uno en la última posición.
                        let off = len - h.len();
                        if i < off { 0.0 } else { h[i - off].as_f64().unwrap_or(0.0) / scale }
                    })
                    .sum::<f64>() as f32
            })
            .collect()
    };
    let mut readings = vec![
        Reading::new(t!("Services CPU"), "%", sum("cpuHistory", 1.0)),
        Reading::new(t!("Services memory"), "MB", sum("memoryHistory", 1_048_576.0)),
    ];
    for s in &running {
        let name = s["name"].as_str().unwrap_or_default();
        let h: Vec<f32> = s["memoryHistory"].as_array().into_iter().flatten().map(|v| (v.as_f64().unwrap_or(0.0) / 1_048_576.0) as f32).collect();
        readings.push(Reading::new(t!("Memory · {name}", name = name), "MB", h));
    }
    Some(ResourceMonitor::new(SharedString::from(format!("monitor-{}", app.project)), readings))
}

/// Logs del servicio elegido: desplegable de servicio, búsqueda, nivel mínimo y seguir el final.
fn logs_card(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let current = format!("{}/{}", app.project, app.log_service());
    let text = app.service_logs.get(&current).cloned().unwrap_or_default();
    let choices: Vec<_> = app
        .services
        .iter()
        .map(|s| {
            let n = s["name"].as_str().unwrap_or_default().to_string();
            crate::ely::choice(n.clone(), ui::txt(&s["title"]).to_string())
        })
        .collect();
    let this = cx.entity().downgrade();
    let select = Select::new("log-service", choices).selected(app.log_service()).on_change(move |v, _, cx| {
        let v = v.to_string();
        let _ = this.update(cx, |app, cx| {
            if let Some(ix) = app.services.iter().position(|s| s["name"] == v.as_str()) {
                app.log_tab = ix;
                app.follow_service_logs(cx);
                cx.notify();
            }
        });
    });
    let search = app.ely_input("service-log-search", t!("Search the log…"), window, cx);
    let query = search.read(cx).text().to_string();
    let key = format!("service:{current}");
    let least = app.log_level(&key);
    let lines = crate::ely::log_lines(&text);
    let (this1, this2) = (cx.entity().downgrade(), cx.entity().downgrade());
    let viewer = LogViewer::new("service-log-viewer", lines)
        .least(least)
        .query(query)
        .follow(app.log_follow)
        .on_least(move |l, _, cx| {
            let key = key.clone();
            let _ = this1.update(cx, |app, cx| {
                app.log_levels.insert(key, l);
                cx.notify();
            });
        })
        .on_follow(move |f, _, cx| {
            let _ = this2.update(cx, |app, cx| {
                app.log_follow = f;
                cx.notify();
            });
        });
    ui::card(cx)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().w(px(220.)).min_w(px(120.)).flex_shrink(1.).child(select))
                .child(div().flex_1())
                .child(div().w(px(260.)).min_w(px(120.)).flex_shrink(1.).child(SearchInput::new("service-log-search", &search))),
        )
        .child(ui::chained_log("service-log-viewer", if text.is_empty() { ui::muted(t!("(no output yet; refreshes every 2 s)"), cx).into_any_element() } else { viewer.into_any_element() }).h(px(420.)))
}
