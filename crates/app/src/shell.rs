// El marco de la ventana: la estructura de Heskala ERP (barra lateral que se
// pliega a un riel, selector de proyecto, grupos y pie con notificaciones,
// tema y daemon) con un estilo plano y compacto como Zed o Cursor: filas de
// 26 px, etiquetas de sección pequeñas, bordes de 1 px rectos y sin sombras.
// El acento solo marca lo puntual. Las páginas no repiten su título: la
// barra lateral marca dónde se está.

use std::sync::{Arc, OnceLock};

use ely_core::data_display::CountBadge;
use ely_core::layout::Sidebar;
use ely_core::primitives::{Icon, IconName, Tooltip};
use ely_core::theme::IconSize;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex, Sizable, TitleBar};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;

use crate::app::{DevtoolApp, Page};
use crate::theme::{Choice, Tokens};
use crate::ui::{self, Health};

struct NavItem {
    page: Page,
    label: String,
    icon: IconName,
    /// Número a la derecha (trabajos en curso, aprobaciones…).
    count: Option<usize>,
    /// Punto de salud (entornos).
    health: Option<Health>,
}

struct NavGroup {
    id: &'static str,
    label: &'static str,
    icon: IconName,
    items: Vec<NavItem>,
}

fn item(page: Page, label: impl Into<String>, icon: IconName) -> NavItem {
    NavItem { page, label: label.into(), icon, count: None, health: None }
}

fn logo() -> Arc<Image> {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    LOGO.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../assets/condx-mark.png").to_vec()))).clone()
}

/// El sidebar parte de los entornos: cada uno abre su vista con todo lo suyo en pestañas
/// (servicios, trabajos, promoción, en vivo…). Lo del proyecto entero (Git y CI, la terminal) va
/// aparte, y lo que espera al usuario (trabajos en curso, aprobaciones) está en los avisos de arriba.
fn groups(app: &DevtoolApp) -> Vec<NavGroup> {
    let services_up = app.services.iter().filter(|s| s["status"] == "running").count();
    let mut list = vec![NavGroup { id: "inicio", label: t!("Home"), icon: IconName::LayoutDashboard, items: vec![item(Page::Overview, t!("Overview"), IconName::LayoutDashboard)] }];
    let envs: Vec<NavItem> = app
        .overview
        .as_ref()
        .and_then(|o| o["environments"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|e| {
            let name = e["name"].as_str().unwrap_or_default().to_string();
            let kind = e["kind"].as_str().unwrap_or_default();
            let icon = if matches!(kind, "devbox" | "throwaway") { IconName::HardDrive } else { IconName::Cloud };
            let mut it = item(Page::Env(name.clone()), name.clone(), icon);
            it.health = Some(app.env_health(e));
            // En el número: los servicios arriba (devbox) o lo que espera aprobación en él.
            let waiting = app.approvals.iter().filter(|j| j["env"] == name.as_str()).count();
            it.count = if kind == "devbox" { (services_up > 0).then_some(services_up) } else { (waiting > 0).then_some(waiting) };
            it
        })
        .collect();
    if envs.is_empty() {
        // Sin entornos registrados (otro proyecto), lo local queda a mano.
        let mut local = item(Page::Local, t!("Local environment"), IconName::HardDrive);
        local.count = (services_up > 0).then_some(services_up);
        list.push(NavGroup { id: "entornos", label: t!("Environments"), icon: IconName::Server, items: vec![local] });
    } else {
        list.push(NavGroup { id: "entornos", label: t!("Environments"), icon: IconName::Server, items: envs });
    }
    let mut terminal = item(Page::Terminal, t!("Terminal"), IconName::Terminal);
    terminal.count = (!app.terminals.is_empty()).then_some(app.terminals.len());
    let mut incidents = item(Page::Incidents, t!("Incidents"), IconName::TriangleAlert);
    let open = app.incidents.iter().filter(|i| i["status"] == "open").count();
    incidents.count = (open > 0).then_some(open);
    list.push(NavGroup {
        id: "proyecto",
        label: t!("Project"),
        icon: IconName::GitBranch,
        items: vec![incidents, item(Page::Workflows, t!("Workflows"), IconName::Rocket), item(Page::Frontend, t!("Frontend"), IconName::Globe), item(Page::Repo, t!("Git & CI"), IconName::GitBranch), terminal],
    });
    // Guía y Configuración van como íconos en la barra de abajo (render_footer).
    list
}

/// Las páginas de la barra lateral en el orden en que se ven (sin las de grupos plegados, salvo
/// en el riel, que las muestra todas): lo que recorren las flechas.
fn nav_pages(app: &DevtoolApp) -> Vec<Page> {
    let rail = app.collapsed || app.tiny;
    groups(app)
        .into_iter()
        .filter(|g| rail || g.items.len() == 1 || !app.closed_groups.contains(g.id))
        .flat_map(|g| g.items.into_iter().map(|it| it.page))
        .collect()
}

impl DevtoolApp {
    /// Flecha arriba o abajo con el foco en la barra lateral: abre la página anterior o la
    /// siguiente.
    fn nav_step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let pages = nav_pages(self);
        let current = pages.iter().position(|p| is_active(self, p));
        if let Some(ix) = crate::nav::step(current, delta, pages.len()) {
            if Some(ix) != current {
                self.open_page(pages[ix].clone(), window, cx);
            }
            // Abrir una página puede llevarse el foco (la terminal); en las demás se queda acá.
            if !matches!(pages[ix], Page::Terminal) {
                window.focus(&self.nav_focus, cx);
            }
        }
    }
}

fn is_active(app: &DevtoolApp, page: &Page) -> bool {
    match (page, &app.page) {
        (Page::Guide(_), Page::Guide(_)) => true,
        (a, b) => a == b,
    }
}

fn count_pill(n: usize, t: Tokens) -> Div {
    div().text_color(t.subtle).text_size(px(11.)).child(n.to_string())
}

fn nav_row(app: &DevtoolApp, it: &NavItem, child: bool, cx: &mut Context<DevtoolApp>) -> AnyElement {
    let t = Tokens::current(cx);
    let active = is_active(app, &it.page);
    let page = it.page.clone();
    let id = SharedString::from(format!("nav-{}", it.label));
    let _ = child;
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .h(px(26.))
        .px_2()
        .cursor_pointer()
        .when(active, |d| d.bg(t.active).text_color(t.fg))
        .when(!active, |d| d.text_color(t.muted).hover(move |s| s.bg(t.hover).text_color(t.fg)))
        .child(Icon::new(it.icon).size(IconSize::Sm).color(if active { t.accent_ink } else { t.subtle }))
        .child(div().flex_1().truncate().child(it.label.clone()))
        .when_some(it.count, |d, n| d.child(count_pill(n, t)))
        .when_some(it.health, |d, h| d.child(ui::dot(h, cx)))
        .on_click(cx.listener(move |this, _, window, cx| {
            // El clic deja el foco en la barra lateral: después se sigue con las flechas.
            window.focus(&this.nav_focus, cx);
            this.open_page(page.clone(), window, cx)
        }))
        .into_any_element()
}

/// Ícono del riel (barra plegada), con su nombre en un tooltip.
fn rail_button(id: impl Into<SharedString>, icon: IconName, label: impl Into<SharedString>, active: bool, cx: &App) -> Stateful<Div> {
    let t = Tokens::current(cx);
    let id: SharedString = id.into();
    div()
        .id(id)
        .relative()
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(active, |d| d.bg(t.active))
        .hover(move |s| s.bg(t.hover))
        .child(Icon::new(icon).size(IconSize::Sm).color(if active { t.accent_ink } else { t.muted }))
        .tooltip(Tooltip::text(label.into()))
}

fn render_nav(app: &DevtoolApp, cx: &mut Context<DevtoolApp>) -> AnyElement {
    let t = Tokens::current(cx);
    let list = groups(app);
    if app.collapsed || app.tiny {
        let mut rail = v_flex().items_center().gap_1().py_3();
        for g in &list {
            for it in &g.items {
                let page = it.page.clone();
                let mut b = rail_button(format!("rail-{}", it.label), it.icon, it.label.clone(), is_active(app, &it.page), cx).on_click(cx.listener(move |this, _, window, cx| {
                    window.focus(&this.nav_focus, cx);
                    this.open_page(page.clone(), window, cx)
                }));
                if let Some(h) = it.health {
                    // Un punto chico en la esquina, que no tape el ícono.
                    b = b.child(div().absolute().bottom(px(6.)).right(px(6.)).child(ui::dot(h, cx).size(px(4.))));
                } else if let Some(n) = it.count {
                    let _ = n;
                    b = b.child(div().absolute().top(px(5.)).right(px(5.)).size(px(5.)).rounded_full().bg(t.muted));
                }
                rail = rail.child(b);
            }
        }
        return rail.into_any_element();
    }
    let mut nav = v_flex().gap(px(1.)).px_1().py_2();
    for g in list {
        // Un grupo de un solo ítem se ve como ítem suelto (igual que en el ERP).
        if g.items.len() == 1 {
            nav = nav.child(nav_row(app, &g.items[0], false, cx));
            continue;
        }
        let open = !app.closed_groups.contains(g.id);
        let gid = g.id;
        let _ = g.icon;
        let header = div()
            .id(SharedString::from(format!("group-{gid}")))
            .flex()
            .items_center()
            .w_full()
            .h(px(24.))
            .mt_2()
            .px_2()
            .gap_1()
            .text_size(px(10.5))
            .cursor_pointer()
            .text_color(t.subtle)
            .hover(move |s| s.text_color(t.fg))
            .child(div().flex_1().child(g.label.to_uppercase()))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(IconSize::Xs).color(t.subtle))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(gid, cx)));
        let mut group = v_flex().child(header);
        if open {
            let mut children = v_flex().gap(px(1.));
            for it in &g.items {
                children = children.child(nav_row(app, it, true, cx));
            }
            group = group.child(children);
        }
        nav = nav.child(group);
    }
    nav.into_any_element()
}

/// Para que un clic en la barra de título no empiece a mover la ventana.
fn no_drag<E: InteractiveElement>(e: E) -> E {
    e.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}
/// La barra de título integrada (como en Zed): plegar la barra lateral, el
/// logo, el selector de proyecto y la rama; a la derecha, lo que corre.
fn render_titlebar(app: &DevtoolApp, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let t = Tokens::current(cx);
    let open = app.project_menu;
    let current = app.current_project().cloned().unwrap_or(Value::Null);
    let tip = current["convexProject"].as_str().map(|p| t!("Convex project: {p}", p = p)).unwrap_or_else(|| t!("Choose project").into());
    let switcher = no_drag(
        div()
            .id("project-switcher")
            .flex()
            .items_center()
            .gap_1()
            .h(px(24.))
            .px_1p5()
            .cursor_pointer()
            .text_color(t.fg)
            .when(open, |d| d.bg(t.active))
            .hover(move |s| s.bg(t.hover))
            .child(if app.needs_welcome() { SharedString::from(t!("No project yet")) } else { SharedString::from(app.project.clone()) })
            .child(Icon::new(IconName::ChevronDown).size(IconSize::Xs).color(t.subtle))
            .tooltip(Tooltip::text(tip))
            .on_click(cx.listener(|this, _, _, cx| {
                this.project_menu = !this.project_menu;
                cx.notify();
            })),
    );
    let git = app.overview.as_ref().map(|o| o["git"].clone()).unwrap_or(Value::Null);
    let branch = match (git["branch"].as_str(), git["head"].as_str()) {
        (Some(b), Some(h)) => format!("{b} · {h}"),
        (Some(b), None) => b.to_string(),
        _ => String::new(),
    };
    // En un entorno, su nombre y su tipo después del commit.
    let env_crumb = match &app.page {
        Page::Env(name) => {
            let kind = app
                .overview
                .as_ref()
                .and_then(|o| o["environments"].as_array()?.iter().find(|e| e["name"] == name.as_str()).and_then(|e| e["kind"].as_str().map(str::to_string)))
                .unwrap_or_default();
            Some((name.clone(), kind))
        }
        _ => None,
    };
    TitleBar::new()
        .bg(t.ground)
        .border_color(t.card_border)
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(div().w(px(4.)))
                .child(img(logo()).size(px(14.)).flex_none())
                .child(switcher)
                .when(!branch.is_empty(), |d| {
                    d.child(div().text_color(t.subtle).child("/")).child(
                        h_flex()
                            .gap_1()
                            .px_1()
                            .items_center()
                            .child(Icon::new(IconName::GitBranch).size(IconSize::Xs).color(t.subtle))
                            .child(div().text_size(px(12.)).font_family(crate::theme::MONO_FONT).text_color(t.muted).child(branch)),
                    )
                })
                .when_some(env_crumb, |d, (name, kind)| {
                    d.child(div().text_color(t.subtle).child("/"))
                        .child(h_flex().gap_1p5().px_1().items_center().child(div().text_size(px(12.)).text_color(t.fg).child(name)).child(crate::ui::kind_tag(&kind)))
                }),
        )
        .child(no_drag(div().id("titlebar-right").flex().items_center().gap_2().pr_2()).child(header_chips(app, cx)).child(daemon_status(app, cx)))
}

/// Lista de proyectos bajo la barra de título.
fn render_project_menu(app: &DevtoolApp, cx: &mut Context<DevtoolApp>) -> AnyElement {
    let t = Tokens::current(cx);
    // `occlude`: los clics del menú no pasan a lo que hay debajo (la barra lateral).
    let mut menu = v_flex().id("project-menu").occlude().absolute().top(px(34.)).left(px(104.)).w(px(260.)).py_1().border_1().border_color(t.card_border).bg(t.panel);
    let row = |id: SharedString| div().id(id).flex().items_center().gap_2().h(px(26.)).px_2().cursor_pointer().text_color(t.muted).hover(move |s| s.bg(t.hover).text_color(t.fg));
    for p in &app.projects {
        let name = p["name"].as_str().unwrap_or_default().to_string();
        let selected = name == app.project;
        let key = name.clone();
        let convex = p["convexProject"].as_str().unwrap_or_default().to_string();
        menu = menu.child(
            row(SharedString::from(format!("pick-{name}")))
                .when(selected, |d| d.text_color(t.fg))
                .child(Icon::new(if selected { IconName::FolderOpen } else { IconName::Folder }).size(IconSize::Xs).color(t.subtle))
                .child(div().flex_1().truncate().child(name))
                .child(div().text_size(px(11.)).text_color(t.subtle).child(convex))
                .when(selected, |d| d.child(Icon::new(IconName::Check).size(IconSize::Xs).color(t.accent_ink)))
                // Quitarlo del devtool (pide confirmación). heskala-erp no se puede quitar.
                .when(p["default"] != true, |d| {
                    let target = key.clone();
                    d.child(
                        div()
                            .id(SharedString::from(format!("unlink-{key}")))
                            .p(px(2.))
                            .rounded(px(3.))
                            .hover(move |s| s.bg(t.active))
                            .tooltip(Tooltip::text(t!("Remove from Condx")))
                            .child(Icon::new(IconName::X).size(IconSize::Xs).color(t.subtle))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                crate::views::local::confirm_unlink(this, target.clone(), window, cx);
                            })),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.project_menu = false;
                    this.select_project(key.clone(), cx);
                    if this.page == Page::Local || this.viewing_services() {
                        this.follow_service_logs(cx);
                    }
                })),
        );
    }
    menu.child(div().h(px(1.)).my_1().bg(t.card_border))
        // Dos cosas distintas: registrar una carpeta que ya existe, o crear un proyecto nuevo en
        // la cuenta de Convex.
        .child(
            row("pick-add".into())
                .child(Icon::new(IconName::FolderOpen).size(IconSize::Xs).color(t.subtle))
                .child(t!("Add existing project…"))
                .on_click(cx.listener(|this, _, window, cx| crate::views::local::open_add_project(this, window, cx))),
        )
        .child(
            row("pick-new".into())
                .child(Icon::new(IconName::Plus).size(IconSize::Xs).color(t.subtle))
                .child(t!("Create Convex project…"))
                .on_click(cx.listener(|this, _, window, cx| crate::views::local::open_create_project(this, window, cx))),
        )
        .into_any_element()
}

fn theme_icon(choice: Choice) -> (IconName, &'static str) {
    match choice {
        Choice::System => (IconName::Monitor, t!("Theme: system")),
        Choice::Dark => (IconName::Moon, t!("Theme: dark")),
        Choice::Light => (IconName::Sun, t!("Theme: light")),
    }
}

fn render_footer(app: &DevtoolApp, cx: &mut Context<DevtoolApp>) -> AnyElement {
    let t = Tokens::current(cx);
    let unread = app.prefs.unread();
    let bell = rail_button("bell", IconName::Bell, if unread > 0 { t!("Notifications · {unread} unread", unread = unread) } else { t!("Notifications").into() }, app.show_notifications, cx)
        .on_click(cx.listener(|this, _, _, cx| this.toggle_notifications(cx)));
    let bell: AnyElement = if unread > 0 { CountBadge::new("bell-count", unread).max(99).over(bell).into_any_element() } else { bell.into_any_element() };
    let (icon, label) = theme_icon(app.theme);
    let theme = rail_button("theme", icon, label, false, cx).on_click(cx.listener(|this, _, window, cx| this.cycle_theme(window, cx)));
    let guide = rail_button("guide", IconName::BookOpen, t!("Guide"), matches!(app.page, Page::Guide(_)), cx)
        .on_click(cx.listener(|this, _, window, cx| this.open_page(Page::Guide(0), window, cx)));
    let settings = rail_button("settings", IconName::Settings, t!("Settings"), app.page == Page::Settings, cx)
        .on_click(cx.listener(|this, _, window, cx| this.open_page(Page::Settings, window, cx)));
    let collapsed = app.collapsed || app.tiny;
    let toggle = rail_button("toggle-sidebar", IconName::PanelLeft, if collapsed { t!("Show sidebar") } else { t!("Collapse sidebar") }, false, cx)
        .on_click(cx.listener(move |this, _, _, cx| this.set_collapsed(!collapsed, cx)));
    if collapsed {
        return v_flex().items_center().gap_1().py_2().border_t_1().border_color(t.card_border).child(bell).child(theme).child(guide).child(settings).child(toggle).into_any_element();
    }
    // Plegar va a la derecha, separado de los demás.
    h_flex().h(px(36.)).px_1().gap_1().items_center().border_t_1().border_color(t.card_border).child(bell).child(theme).child(guide).child(settings).child(div().flex_1()).child(toggle).into_any_element()
}

/// El estado del daemon, en la esquina superior derecha de la ventana.
fn daemon_status(app: &DevtoolApp, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let t = Tokens::current(cx);
    let (health, status): (Health, SharedString) = match (&app.error, app.updated_at) {
        (Some(e), _) => (Health::Bad, e.clone().into()),
        (None, Some(at)) => (Health::Ok, t!("Daemon · {a}", a = ui::ago(at)).into()),
        (None, None) => (Health::Unknown, t!("Connecting to the daemon…").into()),
    };
    // Con la ventana angosta, solo el punto (el texto queda en el tooltip).
    let narrow = app.narrow;
    div()
        .id("daemon-status")
        .flex()
        .items_center()
        .gap_1p5()
        .px_1()
        .child(ui::dot(health, cx))
        .when(!narrow, |d| d.child(div().text_size(px(11.)).text_color(t.subtle).whitespace_nowrap().child(status.clone())))
        .tooltip(Tooltip::text(status))
}

/// Lo que corre o espera, a la derecha del encabezado.
fn header_chips(app: &DevtoolApp, cx: &mut Context<DevtoolApp>) -> Div {
    let t = Tokens::current(cx);
    let running: Vec<&Value> = app.jobs.iter().filter(|j| j["status"] == "running").collect();
    let queued = app.jobs.iter().filter(|j| j["status"] == "queued").count();
    let mut row = h_flex().gap_2().items_center();
    let chip = |id: &'static str| div().id(id).flex().items_center().gap_1p5().h(px(22.)).px_2().border_1().border_color(t.card_border).text_size(px(12.)).text_color(t.muted).cursor_pointer().hover(move |s| s.bg(t.hover).text_color(t.fg));
    if !running.is_empty() || queued > 0 {
        let text = match running.first() {
            Some(j) if running.len() == 1 => format!("{} · {}", ui::txt(&j["title"]), ui::txt(&j["env"])),
            Some(_) => t!("{a} jobs running", a = running.len()),
            None => t!("{queued} queued", queued = queued),
        };
        row = row.child(
            chip("chip-jobs")
                .child(Spinner::new().xsmall())
                .child(div().max_w(px(260.)).truncate().child(text))
                .on_click(cx.listener(|this, _, window, cx| this.open_page(Page::Jobs, window, cx))),
        );
    }
    if !app.approvals.is_empty() {
        row = row.child(
            chip("chip-approvals")
                .child(Icon::new(IconName::CircleCheck).size(IconSize::Xs).color(rgb(0xe8b45e)))
                .child(t!("{a} to approve", a = app.approvals.len()))
                .on_click(cx.listener(|this, _, window, cx| this.open_page(Page::Approvals, window, cx))),
        );
    }
    row
}

fn kind_style(kind: &str) -> (IconName, Hsla) {
    match kind {
        "error" => (IconName::CircleX, rgb(0xe5756e).into()),
        "warning" => (IconName::TriangleAlert, rgb(0xe8b45e).into()),
        "success" => (IconName::CircleCheck, rgb(0x6bbc89).into()),
        _ => (IconName::Info, rgb(0x77abec).into()),
    }
}

/// Panel de notificaciones (lo que antes eran banners en las páginas).
fn render_notifications(app: &DevtoolApp, cx: &mut Context<DevtoolApp>) -> AnyElement {
    let t = Tokens::current(cx);
    let list = app.prefs.notifications();
    let mut body = v_flex().id("notifications-list").flex_1().min_h_0().overflow_y_scroll().on_scroll_wheel(|_, _, cx| cx.stop_propagation());
    if list.is_empty() {
        body = body.child(
            v_flex()
                .items_center()
                .gap_2()
                .py_10()
                .child(Icon::new(IconName::Bell).size(IconSize::Xl).color(t.subtle))
                .child(div().text_sm().text_color(t.muted).child(t!("No notifications")))
                .child(div().text_xs().text_color(t.subtle).child(t!("Daemon notices, jobs and errors show up here."))),
        );
    }
    for n in list {
        let id = n["id"].as_str().unwrap_or_default().to_string();
        let (icon, color) = kind_style(n["kind"].as_str().unwrap_or("info"));
        let unread = n["read"] == false;
        let dismiss = id.clone();
        body = body.child(
            h_flex()
                .items_start()
                .gap_2p5()
                .px_3()
                .py_2p5()
                .border_b_1()
                .border_color(t.card_border)
                .when(unread, |d| d.bg(t.hover))
                .child(div().pt(px(2.)).child(Icon::new(icon).size(IconSize::Sm).color(color)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_color(t.fg).child(ui::txt(&n["title"])))
                        .when(n["body"].as_str().map(|b| !b.is_empty()).unwrap_or(false), |d| d.child(div().text_size(px(12.)).text_color(t.muted).child(ui::txt(&n["body"]))))
                        .child(div().text_size(px(10.)).text_color(t.subtle).child(n["at"].as_i64().map(ui::ago).unwrap_or_default())),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("dismiss-{id}")))
                        .p_1()
                        .cursor_pointer()
                        .hover(move |s| s.bg(t.active))
                        .child(Icon::new(IconName::X).size(IconSize::Xs).color(t.subtle))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.prefs.dismiss(&dismiss);
                            cx.notify();
                        })),
                ),
        );
    }
    let action = |id: &'static str, label: &'static str| {
        div().id(id).px_2().h(px(22.)).flex().items_center().text_size(px(12.)).text_color(t.muted).cursor_pointer().hover(move |s| s.bg(t.hover).text_color(t.fg)).child(label)
    };
    v_flex()
        .absolute()
        .top_0()
        .right_0()
        .h_full()
        .w(px(360.))
        .bg(t.panel)
        .border_l_1()
        .border_color(t.card_border)
        .child(
            h_flex()
                .h(px(40.))
                .pl_3()
                .pr_1()
                .gap_1()
                .items_center()
                .border_b_1()
                .border_color(t.card_border)
                .child(div().flex_1().text_color(t.fg).child(t!("Notifications")))
                .child(action("notif-read", t!("Mark as read")).on_click(cx.listener(|this, _, _, cx| {
                    this.prefs.mark_all_read();
                    cx.notify();
                })))
                .child(action("notif-clear", t!("Clear")).on_click(cx.listener(|this, _, _, cx| {
                    this.prefs.clear();
                    cx.notify();
                })))
                .child(rail_button("notif-close", IconName::X, t!("Close"), false, cx).size(px(24.)).on_click(cx.listener(|this, _, _, cx| this.toggle_notifications(cx)))),
        )
        .child(body)
        .into_any_element()
}

/// La ventana completa: barra lateral, panel del contenido y notificaciones.
pub fn render(app: &mut DevtoolApp, content: AnyElement, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let t = Tokens::current(cx);
    let sidebar = Sidebar::new("sidebar", app.collapsed || app.tiny)
        .child(
            div()
                .id("nav-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                // Con el foco en la barra lateral, las flechas recorren sus páginas.
                .track_focus(&app.nav_focus)
                .key_context(crate::nav::SIDEBAR)
                .on_action(cx.listener(|this, _: &crate::nav::SelectPrev, window, cx| this.nav_step(-1, window, cx)))
                .on_action(cx.listener(|this, _: &crate::nav::SelectNext, window, cx| this.nav_step(1, window, cx)))
                .child(render_nav(app, cx)),
        )
        .child(render_footer(app, cx));
    // El esquema a pantalla completa ocupa la ventana: sin barra lateral ni scroll de página.
    let schema_full = app.schema_fullscreen && matches!(app.page, Page::Env(_)) && app.env_tab == crate::app::EnvTab::Schema;
    // La pestaña Datos se ajusta al alto de la ventana y desplaza su lista y su tabla por dentro:
    // sin scroll de página, para que la rueda no mueva las dos secciones a la vez.
    let data_tab = app.viewing(crate::app::EnvTab::Data);
    let scroll = app.page != Page::Terminal && !schema_full && !data_tab;
    let body = if scroll {
        div().id("content").flex_1().min_h_0().overflow_y_scroll().child(content)
    } else {
        div().id("content").flex_1().min_h_0().overflow_hidden().flex().flex_col().child(content)
    };
    // Sin encabezado de página: la barra lateral ya dice dónde se está.
    let mut panel = v_flex().relative().size_full().overflow_hidden().bg(t.panel).child(body);
    if app.show_notifications {
        panel = panel.child(render_notifications(app, cx));
    }
    let mut root = v_flex()
        .relative()
        .size_full()
        .bg(t.ground)
        .text_color(t.fg)
        .font_family(crate::theme::UI_FONT)
        .text_size(px(crate::theme::UI_SIZE))
        .child(render_titlebar(app, cx))
        .child(h_flex().flex_1().min_h_0().children((!schema_full).then_some(sidebar)).child(div().flex_1().min_w_0().h_full().child(panel)));
    if app.project_menu {
        root = root.child(render_project_menu(app, cx));
    }
    root
}
