// Aprobaciones: lo que pidieron los agentes (o el CLI) y espera al usuario.
// Solo la app tiene el token que puede aprobar.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable, WindowExt};
use gpui_kit::component::Disableable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ely_core::layout::Collapsible;
use serde_json::json;

use crate::app::{DevtoolApp, Load};
use crate::ui;

const TTL_MS: i64 = 30 * 60_000;

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    render_for(app, None, window, cx)
}

/// Las aprobaciones de un entorno (en su pestaña Promoción) o, sin entorno, todas (la bandeja a
/// la que lleva el aviso «por aprobar»).
pub fn render_for(app: &mut DevtoolApp, only: Option<&str>, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let mut col = v_flex().gap_3().p_4();
    let list: Vec<serde_json::Value> = app.approvals.iter().filter(|j| only.is_none_or(|e| j["env"] == e)).cloned().collect();
    if list.is_empty() {
        return col.child(ui::card(cx).child(ui::muted(if only.is_some() { t!("Nothing in this environment is waiting for your approval.") } else { t!("Nothing is waiting for your approval.") }, cx)));
    }
    for job in list {
        let id = job["id"].as_str().unwrap_or_default().to_string();
        // Con un proyecto (recetas project-*), se confirma su nombre en vez del entorno.
        let env = job["args"]["project"].as_str().or(job["env"].as_str()).unwrap_or_default().to_string();
        let level = job["approval"].as_str().unwrap_or("simple").to_string();
        let left = (TTL_MS - (ui::now_ms() - job["createdAt"].as_i64().unwrap_or(0))).max(0) / 60_000;

        let confirm = (level == "typed" || level == "reinforced").then(|| {
            app.inputs
                .entry(format!("approve:{id}"))
                .or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(t!("type “{env}” to approve", env = env))))
                .clone()
        });
        let typed_ok = confirm.as_ref().map(|s| s.read(cx).value().trim() == env).unwrap_or(true);

        let mut pre = v_flex().gap_1();
        for p in job["preconditions"].as_array().cloned().unwrap_or_default() {
            pre = pre.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(ui::dot(if p["ok"] == true { ui::Health::Ok } else { ui::Health::Bad }, cx))
                    .child(div().w(px(300.)).min_w(px(120.)).flex_shrink(1.).child(ui::txt(&p["label"])))
                    .child(div().flex_1().text_color(cx.theme().muted_foreground).child(ui::txt(&p["detail"]))),
            );
        }
        let pre_list = job["preconditions"].as_array().cloned().unwrap_or_default();
        let has_pre = !pre_list.is_empty();
        let pre_ok = pre_list.iter().filter(|p| p["ok"] == true).count();
        // Abiertas solas si alguna falla; si no, plegadas.
        let pre_open = app.approval_folds.get(&id).copied().unwrap_or(pre_ok < pre_list.len());
        let fold_id = id.clone();
        let t = crate::theme::Tokens::current(cx);
        let mut args = v_flex().gap_1();
        for (k, v) in job["args"].as_object().cloned().unwrap_or_default().into_iter().filter(|(k, _)| k != "commit") {
            args = args.child(ui::kv(k, ui::mono(v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())), cx));
        }

        let (approve_id, reject_id, confirm_state) = (id.clone(), id.clone(), confirm.clone());
        let reinforced = level == "reinforced";
        let title = job["title"].as_str().unwrap_or_default().to_string();
        let summary = [
            Some(t!("requested by {a} {b}", a = ui::txt(&job["requestedBy"]), b = job["createdAt"].as_i64().map(ui::ago).unwrap_or_default())),
            Some(t!("recipe {a}", a = ui::txt(&job["recipe"]))),
            job["commit"].as_str().map(|c| t!("commit {a}", a = &c[..c.len().min(8)])),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let card = ui::card(cx)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(ely_core::primitives::Icon::new(ely_core::primitives::IconName::CircleCheck).size(ely_core::theme::IconSize::Sm).color(rgb(0xe8b45e)))
                    .child(div().text_color(t.fg).child(format!("{} · {}", ui::txt(&job["title"]), env)))
                    .child(ui::kind_tag(job["envKind"].as_str().unwrap_or_default()))
                    .child(div().flex_1())
                    .child(ui::muted(t!("expires in {left} min", left = left), cx)),
            )
            .child(ui::muted(summary, cx))
            .child(args)
            .when(has_pre, |c| {
                c.child(
                    h_flex()
                        .id(SharedString::from(format!("pre-toggle-{id}")))
                        .gap_1()
                        .items_center()
                        .cursor_pointer()
                        .child(ely_core::primitives::Icon::new(if pre_open { ely_core::primitives::IconName::ChevronDown } else { ely_core::primitives::IconName::ChevronRight }).size(ely_core::theme::IconSize::Xs).color(t.subtle))
                        .child(ui::section(t!("Preconditions · {pre_ok} of {a} OK (checked again on start)", pre_ok = pre_ok, a = pre_list.len()), cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.approval_folds.insert(fold_id.clone(), !pre_open);
                            cx.notify();
                        })),
                )
                .child(Collapsible::new(SharedString::from(format!("pre-fold-{id}")), pre_open).child(pre))
            })
            .when_some(confirm, |c, state| c.child(ui::kv(t!("Confirmation"), Input::new(&state).small().w(px(260.)), cx)))
            .child(
                h_flex()
                    .gap_2()
                    .child(ui::brand(
                        Button::new(SharedString::from(format!("approve-{id}")))
                            .small()
                            .icon(IconName::Check)
                            .label(if reinforced { t!("Approve with Touch ID") } else { t!("Approve") })
                            .disabled(!typed_ok || matches!(app.action, Some(Load::Loading)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let confirm = confirm_state.as_ref().map(|s| s.read(cx).value().trim().to_string());
                                let id = approve_id.clone();
                                if reinforced {
                                    this.approve_with_touch_id(id, confirm.unwrap_or_default(), title.clone(), cx);
                                    return;
                                }
                                // En producción o con nombre escrito, un último «¿seguro?».
                                if confirm.is_some() {
                                    let entity = cx.entity().downgrade();
                                    let (title, env) = (title.clone(), confirm.clone().unwrap_or_default());
                                    window.open_alert_dialog(cx, move |alert, _, _| {
                                        let (entity, id, confirm) = (entity.clone(), id.clone(), confirm.clone());
                                        alert
                                            .title(t!("Approve “{title}” on {env}?", title = title, env = env))
                                            .description(t!("The job starts as soon as you approve it. Follow the log in the environment's Jobs tab."))
                                            .ok_text(t!("Approve"))
                                            .cancel_text(t!("Cancel"))
                                            .show_cancel(true)
                                            .on_ok(move |_, _, cx| {
                                                let (id, confirm) = (id.clone(), confirm.clone());
                                                let _ = entity.update(cx, |this, cx| approve(this, id, confirm, cx));
                                                true
                                            })
                                    });
                                    return;
                                }
                                approve(this, id, confirm, cx);
                            })),
                        cx,
                    ))
                    .child(
                        Button::new(SharedString::from(format!("reject-{id}")))
                            .danger()
                            .outline()
                            .small()
                            .icon(IconName::Close)
                            .label(t!("Reject"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let id = reject_id.clone();
                                this.post(format!("/api/approvals/{id}"), json!({ "approve": false }), move |app, res, _| {
                                    if matches!(res, Load::Ready(_)) {
                                        app.approvals.retain(|j| j["id"] != id.as_str());
                                    }
                                    app.action = Some(res);
                                }, cx);
                            })),
                    ),
            );
        col = col.child(card);
    }
    col
}

fn approve(app: &mut DevtoolApp, id: String, confirm: Option<String>, cx: &mut Context<DevtoolApp>) {
    app.post(format!("/api/approvals/{id}"), json!({ "approve": true, "confirm": confirm }), move |app, res, cx| {
        if matches!(res, Load::Ready(_)) {
            app.approvals.retain(|j| j["id"] != id.as_str());
            // Desde la pestaña Flujos se sigue el flujo ahí mismo; si no, el trabajo aprobado.
            if app.viewing(crate::app::EnvTab::Workflows) {
                app.load_workflows(cx);
            } else {
                app.show_jobs(cx);
                app.open_job(id, cx);
            }
        } else {
            app.action = Some(res);
        }
    }, cx);
}
