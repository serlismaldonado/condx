// Vista general (§4.2), como el tablero del ERP: cifras arriba, entornos con
// su commit, frontend de producción, CI, deployer y repositorio. Lo que pide
// atención ya no es un banner: va al centro de notificaciones.

use ely_core::primitives::IconName;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme};
use gpui_kit::*;
use serde_json::Value;

use crate::app::{DevtoolApp, Page};
use crate::ui::{self, Health};

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let Some(o) = app.overview.clone() else {
        return super::empty(app.error.clone().unwrap_or_else(|| t!("Waiting for the daemon…").into()), cx).into_any_element();
    };

    let envs_list: Vec<Value> = o["environments"].as_array().cloned().unwrap_or_default();
    let online = envs_list.iter().filter(|e| e["convex"]["reachable"] == true).count();
    let attention = o["attention"].as_array().map(Vec::len).unwrap_or(0);
    let running = app.jobs.iter().filter(|j| matches!(j["status"].as_str(), Some("running" | "queued"))).count();
    let ahead = o["git"]["ahead"].as_i64().unwrap_or(0);
    let stats = h_flex()
        .gap_3()
        .flex_wrap()
        .items_stretch()
        .child(ui::stat_card(format!("{online}/{}", envs_list.len()), t!("Online"), Some(t!("environments")), IconName::Server, ui::Hue::Brand, cx))
        .child(ui::stat_card(running.to_string(), t!("Running"), Some(t!("jobs")), IconName::ListChecks, ui::Hue::Green, cx))
        .child(ui::stat_card(app.approvals.len().to_string(), t!("To approve"), Some(t!("jobs")), IconName::CircleCheck, ui::Hue::Amber, cx))
        .child(ui::stat_card(
            attention.to_string(),
            t!("Attention"),
            Some(if attention == 0 { t!("all good") } else { t!("see notifications") }),
            IconName::Bell,
            if attention == 0 { ui::Hue::Green } else { ui::Hue::Red },
            cx,
        ))
        .child(ui::stat_card(ahead.to_string(), t!("Unpushed"), Some("commits"), IconName::GitBranch, ui::Hue::Violet, cx));

    let t = crate::theme::Tokens::current(cx);
    let mut envs = v_flex().border_1().border_color(t.card_border);
    for (i, env) in o["environments"].as_array().into_iter().flatten().enumerate() {
        envs = envs.child(env_row(app, env, i > 0, cx));
    }

    let front = &o["frontendProduction"];
    let front_card = v_flex()
        .child(if front["error"].is_string() {
            ui::muted(ui::txt(&front["error"]), cx)
        } else {
            v_flex()
                .gap_1()
                .child(ui::kv(t!("Commit"), ui::mono(ui::txt(&front["commit"])), cx))
                .child(ui::kv(t!("Branch"), ui::txt(&front["branch"]), cx))
                .child(ui::kv(t!("Behind main"), behind_text(&front["behindMain"]), cx))
                .child(ui::kv(
                    t!("Deployed"),
                    front["deployedAt"].as_i64().map(ui::ago).unwrap_or_else(|| "—".into()),
                    cx,
                ))
        });

    let mut ci_card = v_flex().gap_1();
    for run in o["ci"].as_array().into_iter().flatten() {
        let health = match (run["status"].as_str(), run["conclusion"].as_str()) {
            (Some("completed"), Some("success")) => Health::Ok,
            (Some("completed"), Some("failure")) => Health::Bad,
            (Some("completed"), _) => Health::Warn,
            (Some(_), _) => Health::Unknown,
            _ => Health::Unknown,
        };
        ci_card = ci_card.child(
            h_flex()
                .gap_2()
                .items_center()
                .text_sm()
                .child(ui::dot(health, cx))
                .child(div().w(px(190.)).child(ui::txt(&run["repo"])))
                .child(div().flex_1().truncate().text_color(cx.theme().muted_foreground).child(if run["status"].is_null() {
                    t!("no recent valid runs").into()
                } else {
                    ui::txt(&run["title"])
                })),
        );
    }

    let deployer = &o["deployer"];
    let phase = deployer["phase"].as_str().unwrap_or("unknown");
    let deployer_health = match phase {
        "idle" => Health::Ok,
        "syncing" | "starting" => Health::Warn,
        "error" => Health::Bad,
        _ => Health::Unknown,
    };
    let deployer_card = v_flex()
        .gap_1()
        .child(ui::kv(t!("Phase"), ui::status_tag(deployer_health, ui::phase_label(phase)), cx))
        .child(ui::kv(t!("Image"), ui::txt(&deployer["image"]), cx))
        .child(ui::kv(t!("Image created"), ui::txt(&deployer["imageCreatedAt"]), cx));

    let git = &o["git"];
    let git_card = v_flex()
        .gap_1()
        .child(ui::kv(t!("Branch"), format!("{} · {}", ui::txt(&git["branch"]), ui::txt(&git["head"])), cx))
        .child(ui::kv(t!("Unpushed"), format!("{} commits", git["ahead"].as_i64().unwrap_or(0)), cx))
        .child(ui::kv(
            t!("Changes"),
            t!("{a} modified · {b} untracked", a = git["changed"].as_i64().unwrap_or(0), b = git["untracked"].as_i64().unwrap_or(0)),
            cx,
        ))
        .child(div().text_sm().truncate().text_color(cx.theme().muted_foreground).child(ui::txt(&git["subject"])));

    // Otro proyecto: sin Vercel ni CI propios (por ahora).
    let other = o["project"].is_object();
    // Plegados, con su estado en el título: se abren cuando hace falta mirar el detalle.
    let ci_runs: Vec<&Value> = o["ci"].as_array().into_iter().flatten().collect();
    let ci_green = ci_runs.iter().filter(|r| r["conclusion"] == "success").count();
    let front_title = if front["error"].is_string() {
        t!("Production frontend · no data").to_string()
    } else {
        t!("Production frontend · {a} · {b}", a = ui::txt(&front["commit"]), b = behind_text(&front["behindMain"]))
    };
    let git_title = t!("Repository · {a} · {b} unpushed · {c} changes", a = ui::txt(&git["branch"]), b = git["ahead"].as_i64().unwrap_or(0), c = git["changed"].as_i64().unwrap_or(0) + git["untracked"].as_i64().unwrap_or(0));
    let mut details = ui::Folds::new(if other { "overview-details-project" } else { "overview-details" });
    if !other {
        details = details.item("front", front_title, front_card).item("ci", t!("CI · {ci_green} of {a} green", ci_green = ci_green, a = ci_runs.len()), ci_card);
    }
    details = details.item("deployer", format!("Deployer · {}", ui::phase_label(phase)), deployer_card).item("git", git_title, git_card);
    let details = details.render(app, cx);
    // Agregar producción (o la key de cloud dev) pegando su deploy key: para cualquier proyecto.
    let project = app.project.clone();
    let (daemon, me) = (app.daemon.clone(), cx.entity().downgrade());
    let add_env = app.add_env_views.entry(project.clone()).or_insert_with(|| cx.new(|cx| super::add_env::AddEnvView::new(project, daemon, me, window, cx))).clone();
    v_flex().gap_3().p_4().child(stats).child(ui::section(t!("Environments"), cx)).child(envs).child(add_env).child(details).into_any_element()
}

fn behind_text(v: &Value) -> SharedString {
    match v.as_i64() {
        Some(0) => t!("up to date").into(),
        Some(n) => t!("{n} commits", n = n).into(),
        None => "—".into(),
    }
}

/// Un entorno en una fila: salud, nombre y tipo, latencia de Convex, commit y atraso. Clic: su detalle.
fn env_row(app: &DevtoolApp, env: &Value, border: bool, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let name = env["name"].as_str().unwrap_or_default().to_string();
    let reachable = env["convex"]["reachable"] == true;
    let deployed = &env["deployed"];
    let commit: SharedString = match deployed["source"].as_str() {
        Some("unknown") => t!("unknown").into(),
        _ => {
            let dirty = if deployed["uncommittedChanges"] == true { t!(" + changes") } else { "" };
            format!("{}{dirty}", ui::txt(&deployed["commit"])).into()
        }
    };
    let kind = env["kind"].as_str().unwrap_or_default();
    let icon = match kind {
        "cloud-prod" => IconName::Rocket,
        "cloud-dev" => IconName::Cloud,
        _ => IconName::HardDrive,
    };
    let page = Page::Env(name.clone());
    let t = crate::theme::Tokens::current(cx);
    h_flex()
        .id(SharedString::from(format!("env-row-{name}")))
        .gap_3()
        .px_3()
        .h(px(36.))
        .items_center()
        .cursor_pointer()
        .when(border, |d| d.border_t_1().border_color(t.card_border))
        .hover(move |s| s.bg(t.hover))
        .on_click(cx.listener(move |this, _, window, cx| this.open_page(page.clone(), window, cx)))
        .child(ui::dot(app.env_health(env), cx))
        .child(ely_core::primitives::Icon::new(icon).size(ely_core::theme::IconSize::Sm).color(t.subtle))
        .child(div().w(px(220.)).min_w(px(80.)).flex_shrink(1.).truncate().text_color(t.fg).child(name.clone()))
        .child(div().w(px(110.)).child(ui::kind_tag(kind)))
        // Angosta: sin latencia ni atraso (siguen en el resumen del entorno).
        .when(!app.narrow, |d| d.child(div().w(px(110.)).flex_none().child(ui::muted(if reachable { format!("{} ms", env["convex"]["latencyMs"]) } else { t!("not responding").into() }, cx))))
        .child(div().w(px(200.)).min_w(px(90.)).flex_shrink(1.).truncate().child(ui::mono(commit)))
        .child(div().flex_1().min_w_0().truncate().when(!app.narrow, |d| d.child(ui::muted(t!("behind {a}: {b}", a = ui::txt(&deployed["branch"]), b = behind_text(&deployed["behindBranch"])), cx))))
        // El devbox se levanta o se detiene desde aquí: es lo primero que se ve al abrir la app.
        .when(kind == "devbox" && !app.services.is_empty(), |d| d.child(super::local::service_actions(app, cx)))
}
