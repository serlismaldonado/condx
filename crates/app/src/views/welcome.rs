// Primer arranque: sin proyectos todavía, el resumen pasa a ser una guía de tres pasos.
// 1) lo que hace falta en esta Mac (GET /api/doctor), con el comando que lo arregla y la
// descarga del backend local; 2) agregar o crear el proyecto de Convex; 3) levantarlo.

use ely_core::primitives::{Icon, IconName as IconName2};
use ely_core::theme::IconSize;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex, Disableable, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::app::{DevtoolApp, Load};
use crate::ui;

fn label(id: &str) -> String {
    match id {
        "node" => t!("Node.js 18 or newer"),
        "convex-login" => t!("Convex CLI session"),
        "backend" => t!("Local Convex backend"),
        "git" => t!("Git"),
        "gh" => t!("GitHub CLI (CI status)"),
        "docker" => t!("Docker"),
        other => other,
    }
    .to_string()
}

pub fn load(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) {
    app.doctor = Some(Load::Loading);
    app.get("/api/doctor".into(), |app, res| app.doctor = Some(res), cx);
}

fn download_backend(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) {
    app.backend_download = Some(Load::Loading);
    app.post("/api/doctor/backend".into(), json!({}), |app, res, cx| {
        if let Load::Failed(e) = &res {
            app.action = Some(Load::Failed(e.clone()));
        }
        app.backend_download = Some(res);
        load(app, cx);
    }, cx);
}

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    if app.doctor.is_none() {
        load(app, cx);
    }
    let t = crate::theme::Tokens::current(cx);
    let head = v_flex()
        .gap_1()
        .child(div().text_size(px(22.)).text_color(t.fg).child(t!("Welcome to Condx")))
        .child(ui::muted(t!("Three steps and your Convex project runs on this Mac, with its own ports and data."), cx));

    let ready = matches!(&app.doctor, Some(Load::Ready(v)) if v["ready"] == true);
    let checks = step(1, t!("Check this Mac"), ready, cx).child(checks(app, cx));
    let add = step(2, t!("Add your Convex project"), false, cx).child(add_project(app, window, cx));
    let start = step(3, t!("Start it"), false, cx).child(ui::muted(
        t!("Open Local environment and press Start all: backend, deployer (convex dev) and frontend, in order. They keep running if you close the app."),
        cx,
    ));
    v_flex().gap_4().max_w(px(760.)).child(head).child(checks).child(add).child(start)
}

fn step(n: u8, title: &str, done: bool, cx: &App) -> Div {
    let t = crate::theme::Tokens::current(cx);
    let badge = div()
        .flex_none()
        .size(px(20.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .border_1()
        .border_color(t.card_border)
        .text_color(t.muted)
        .when(done, |d| d.bg(rgb(0x10b981)).border_color(rgb(0x10b981)).text_color(rgb(0xffffff)))
        .child(if done { "✓".to_string() } else { n.to_string() });
    ui::card(cx).gap_3().child(h_flex().gap_2().items_center().child(badge).child(div().text_color(t.fg).child(title.to_string())))
}

fn checks(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) -> AnyElement {
    let t = crate::theme::Tokens::current(cx);
    let v = match &app.doctor {
        Some(Load::Ready(v)) => v.clone(),
        Some(Load::Failed(e)) => return ui::muted(e.clone(), cx).into_any_element(),
        _ => return h_flex().gap_2().child(Spinner::new().small()).child(ui::muted(t!("Checking…"), cx)).into_any_element(),
    };
    let downloading = matches!(app.backend_download, Some(Load::Loading));
    let mut list = v_flex().border_1().border_color(t.card_border);
    for (i, c) in v["checks"].as_array().cloned().unwrap_or_default().into_iter().enumerate() {
        let id = c["id"].as_str().unwrap_or_default().to_string();
        let ok = c["ok"] == true;
        let optional = c["optional"] == true;
        let (icon, color) = match (ok, optional) {
            (true, _) => (IconName2::CircleCheck, rgb(0x10b981)),
            (false, true) => (IconName2::CircleAlert, rgb(0xf59e0b)),
            (false, false) => (IconName2::CircleX, rgb(0xef4444)),
        };
        let mut row = h_flex()
            .gap_2()
            .px_2()
            .py_1p5()
            .items_center()
            .when(i > 0, |d| d.border_t_1().border_color(t.card_border))
            .child(Icon::new(icon).size(IconSize::Sm).color(color))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(h_flex().gap_1p5().items_center().child(div().text_color(t.fg).child(label(&id))).when(optional, |d| d.child(div().text_color(t.subtle).text_size(px(11.)).child(t!("optional")))))
                    .child(div().truncate().child(ui::muted(ui::txt(&c["detail"]), cx))),
            );
        if let Some(fix) = c["fix"].as_str() {
            row = row.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().px_1p5().py_0p5().bg(t.card_border).child(ui::mono(fix.to_string())))
                    .child(Clipboard::new(SharedString::from(format!("fix-{id}"))).value(fix.to_string()).tooltip(t!("Copy command"))),
            );
        }
        if id == "backend" && !ok {
            row = row.child(ui::brand(
                Button::new("backend-download")
                    .small()
                    .icon(IconName::ArrowDown)
                    .label(if downloading { t!("Downloading…") } else { t!("Download") })
                    .loading(downloading)
                    .disabled(downloading)
                    .on_click(cx.listener(|this, _, _, cx| download_backend(this, cx))),
                cx,
            ));
        }
        list = list.child(row);
    }
    v_flex()
        .gap_2()
        .child(list)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::muted(
                    if v["ready"] == true { t!("All set. Optional items only add extras.") } else { t!("Fix the red items, then check again.") },
                    cx,
                ))
                .child(div().flex_1())
                .child(Button::new("doctor-again").xsmall().ghost().icon(IconName::Redo).label(t!("Check again")).on_click(cx.listener(|this, _, _, cx| load(this, cx)))),
        )
        .into_any_element()
}

fn add_project(app: &mut DevtoolApp, _window: &mut Window, cx: &mut Context<DevtoolApp>) -> AnyElement {
    if app.discover.is_none() {
        app.discover = Some(Load::Loading);
        app.get("/api/projects/discover".into(), |app, res| app.discover = Some(res), cx);
    }
    let t = crate::theme::Tokens::current(cx);
    let buttons = h_flex()
        .gap_2()
        .child(ui::brand(
            Button::new("welcome-add")
                .small()
                .icon(IconName::Plus)
                .label(t!("Add existing project…"))
                .on_click(cx.listener(|this, _, window, cx| super::local::open_add_project(this, window, cx))),
            cx,
        ))
        .child(
            Button::new("welcome-create")
                .small()
                .outline()
                .icon(IconName::Plus)
                .label(t!("Create in Convex"))
                .on_click(cx.listener(|this, _, window, cx| super::local::open_create_project(this, window, cx))),
        );
    let mut body = v_flex().gap_3().child(ui::muted(t!("A folder with convex/ and package.json. Condx never touches its .env.local."), cx)).child(buttons);
    // Los repos de Convex que ya hay en la carpeta de proyectos, a un clic.
    if let Some(Load::Ready(v)) = &app.discover {
        let candidates: Vec<Value> = v["candidates"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| c["project"].is_null()).take(6).collect();
        if !candidates.is_empty() {
            let mut list = v_flex().border_1().border_color(t.card_border);
            for (i, c) in candidates.into_iter().enumerate() {
                let path = c["path"].as_str().unwrap_or_default().to_string();
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                list = list.child(
                    h_flex()
                        .gap_2()
                        .px_2()
                        .py_1p5()
                        .items_center()
                        .when(i > 0, |d| d.border_t_1().border_color(t.card_border))
                        .child(v_flex().flex_1().min_w_0().child(div().text_color(t.fg).truncate().child(name)).child(div().truncate().child(ui::muted(path.clone(), cx))))
                        .child(Button::new(SharedString::from(format!("welcome-cand-{i}"))).xsmall().outline().icon(IconName::Plus).label(t!("Add")).on_click(cx.listener(
                            move |this, _, _, cx| super::local::register(this, path.clone(), cx),
                        ))),
                );
            }
            body = body.child(v_flex().gap_1().child(ui::section(t!("Found in {a}", a = ui::txt(&v["root"])), cx)).child(list));
        }
    }
    body.into_any_element()
}
