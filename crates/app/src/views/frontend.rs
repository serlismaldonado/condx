// Frontend del proyecto en Vercel (`/api/frontend`, frontend.rs del daemon): producción con su
// commit y si apunta al Convex que corresponde, lo que se está construyendo, el último despliegue
// que falló con el final de su log, y las previews (la última de cada rama), que se abren con un
// clic. Lo que se publica en Vercel se lee con la sesión de la CLI en la Mac.

use ely_core::data_display::Tone;
use ely_core::tables::{Cell, Column, DataTable, Row};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;

use crate::app::{DevtoolApp, Load};
use crate::ui::{self, Health};

impl DevtoolApp {
    /// Con `fresh`, sin el informe que el daemon guarda unos segundos (el botón Recargar).
    pub fn load_frontend(&mut self, fresh: bool, cx: &mut Context<Self>) {
        let project = self.project.clone();
        if !matches!(self.frontend.get(&project), Some(Load::Ready(_))) {
            self.frontend.insert(project.clone(), Load::Loading);
        }
        self.get(format!("/api/frontend?project={project}&fresh={fresh}"), move |app, res| {
            app.frontend.insert(project, res);
        }, cx);
    }
}

fn short(v: &Value) -> String {
    v.as_str().map(|s| s[..s.len().min(8)].to_string()).unwrap_or_else(|| "—".into())
}

fn when(v: &Value) -> String {
    v.as_i64().map(ui::ago).unwrap_or_default()
}

/// A qué Convex apunta lo publicado, como punto + texto.
fn convex_line(c: &Value) -> Option<(Health, String)> {
    let expected = c["expectedEnv"].as_str();
    let found = c["matches"].as_array().cloned().unwrap_or_default();
    let names: Vec<String> = found.iter().map(|m| m["env"].as_str().map(str::to_string).unwrap_or_else(|| ui::txt(&m["url"]).to_string())).collect();
    Some(match c["status"].as_str()? {
        "ok" => (Health::Ok, t!("Points to {env}'s Convex", env = expected.unwrap_or("?")).to_string()),
        "mismatch" => (Health::Bad, t!("Points to {found}'s Convex, not {env}'s", found = names.join(", "), env = expected.unwrap_or("?")).to_string()),
        "mixed" => (Health::Warn, t!("Has more than one Convex: {found}", found = names.join(", ")).to_string()),
        "unknown-env" => (Health::Unknown, t!("Points to {found} (no environment of this kind to compare with)", found = names.join(", ")).to_string()),
        "not-found" => (Health::Unknown, t!("Couldn't find a Convex URL in the published site").to_string()),
        "protected" => (Health::Unknown, t!("Can't read it: the site asks for a login (Vercel protection)").to_string()),
        _ => (Health::Unknown, t!("Couldn't read the site: {e}", e = ui::txt(&c["detail"])).to_string()),
    })
}

/// El commit del frontend de producción frente al de Convex de producción.
fn skew_line(s: &Value) -> Option<(Health, String)> {
    let (f, c) = (ui::txt(&s["frontend"]), ui::txt(&s["convex"]));
    Some(match s["status"].as_str()? {
        "same" => (Health::Ok, t!("Same commit as Convex ({f})", f = f).to_string()),
        "frontend-ahead" => (
            Health::Warn,
            t!("The frontend is {n} commits ahead of Convex ({f} vs {c}): it may call functions that aren't deployed", n = s["frontendAhead"].as_i64().unwrap_or(0), f = f, c = c).to_string(),
        ),
        "convex-ahead" => (Health::Ok, t!("Convex is {n} commits ahead of the frontend ({c} vs {f})", n = s["convexAhead"].as_i64().unwrap_or(0), c = c, f = f).to_string()),
        "diverged" => (Health::Warn, t!("The frontend ({f}) and Convex ({c}) come from different branches", f = f, c = c).to_string()),
        _ if s["convex"].is_null() => (Health::Unknown, t!("No Convex deploy recorded for production yet").to_string()),
        _ => (Health::Unknown, t!("Can't compare {f} with {c} (not in the local repo)", f = f, c = c).to_string()),
    })
}

fn open_button(id: impl Into<SharedString>, url: Option<String>) -> impl IntoElement {
    Button::new(id.into()).xsmall().ghost().icon(IconName::ExternalLink).label(t!("Open")).when_some(url, |b, url| b.on_click(move |_, _, cx| cx.open_url(&url)))
}

pub fn render(app: &mut DevtoolApp, _: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let data = match app.frontend.get(&app.project) {
        None => {
            app.load_frontend(false, cx);
            return super::empty(t!("Asking Vercel…").to_string(), cx).into_any_element();
        }
        Some(Load::Loading) => return super::empty(t!("Asking Vercel…").to_string(), cx).into_any_element(),
        Some(Load::Failed(e)) => return super::empty(e.clone(), cx).into_any_element(),
        Some(Load::Ready(v)) => v.clone(),
    };
    let t = crate::theme::Tokens::current(cx);
    let toolbar = h_flex()
        .gap_2()
        .items_center()
        .child(ely_core::primitives::Icon::new(ely_core::primitives::IconName::Globe).size(ely_core::theme::IconSize::Sm).color(t.subtle))
        .child(div().text_color(t.fg).child(data["vercelProject"].as_str().map(|p| format!("Vercel · {p}")).unwrap_or_else(|| "Vercel".into())))
        .child(div().flex_1())
        .when_some(data["checkedAt"].as_i64(), |d, at| d.child(ui::muted(t!("checked {a}", a = ui::ago(at)), cx)))
        .child(Button::new("frontend-reload").xsmall().ghost().icon(IconName::RefreshCw).label(t!("Reload")).on_click(cx.listener(|this, _, _, cx| this.load_frontend(true, cx))));
    let mut col = v_flex().gap_3().p_4().child(toolbar);
    if let Some(err) = data["error"].as_str() {
        return col.child(ui::card(cx).child(ui::status_tag(Health::Bad, err.to_string()))).into_any_element();
    }

    // Producción.
    let prod = &data["production"];
    let mut card = ui::card(cx).child(
        h_flex()
            .gap_2()
            .items_center()
            .child(ui::section(t!("Production"), cx))
            .child(div().flex_1())
            .when(prod.is_object(), |d| d.child(open_button("frontend-open-prod", prod["url"].as_str().map(str::to_string)))),
    );
    if prod.is_object() {
        card = card
            .child(ui::kv(t!("Commit"), h_flex().gap_2().child(ui::mono(short(&prod["commit"]))).child(ui::muted(ui::txt(&prod["message"]), cx)), cx))
            .child(ui::kv(t!("Deployed"), ui::muted(format!("{} · {}", when(&prod["createdAt"]), ui::txt(&prod["branch"])), cx), cx))
            .when_some(prod["behindMain"].as_i64(), |c, n| {
                c.child(ui::kv(t!("Behind main"), if n == 0 { ui::status_tag(Health::Ok, t!("up to date")) } else { ui::status_tag(Health::Warn, t!("{n} commits", n = n)) }, cx))
            })
            .when_some(convex_line(&prod["convex"]), |c, (h, text)| c.child(ui::kv("Convex", ui::status_tag(h, text), cx)))
            .when_some(skew_line(&prod["skew"]), |c, (h, text)| c.child(ui::kv(t!("Commit vs Convex"), ui::status_tag(h, text), cx)));
    } else {
        card = card.child(ui::muted(t!("Nothing in production yet."), cx));
    }
    col = col.child(card);

    // En curso.
    let building = data["building"].as_array().cloned().unwrap_or_default();
    if !building.is_empty() {
        let mut card = ui::card(cx).child(ui::section(t!("Building · {n}", n = building.len()), cx));
        for (i, b) in building.iter().enumerate() {
            let target = if b["target"] == "production" { t!("production") } else { t!("preview") };
            card = card.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(ui::status_tag(Health::Warn, format!("{} · {}", ui::txt(&b["state"]).to_lowercase(), target)))
                    .child(ui::mono(short(&b["commit"])))
                    .child(div().flex_1().min_w_0().truncate().child(ui::muted(format!("{} · {}", ui::txt(&b["branch"]), when(&b["createdAt"])), cx)))
                    .child(open_button(SharedString::from(format!("frontend-open-building-{i}")), b["url"].as_str().map(str::to_string))),
            );
        }
        col = col.child(card);
    }

    // El último que falló, si nada lo arregló después: con el final de su log.
    let failed = &data["lastFailed"];
    if failed.is_object() && failed["resolved"] != true {
        let target = if failed["target"] == "production" { t!("production") } else { t!("preview") };
        let mut card = ui::card(cx).child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::status_tag(Health::Bad, t!("Latest {target} deploy failed · {b} · {w}", target = target, b = ui::txt(&failed["branch"]), w = when(&failed["createdAt"]))))
                .child(div().flex_1())
                .child(open_button("frontend-open-failed", failed["url"].as_str().map(str::to_string))),
        );
        card = card.child(ui::kv(t!("Commit"), h_flex().gap_2().child(ui::mono(short(&failed["commit"]))).child(ui::muted(ui::txt(&failed["message"]), cx)), cx));
        let log = &failed["log"];
        if let Some(tail) = log["tail"].as_str() {
            let lines: Vec<String> = tail.lines().map(str::to_string).collect();
            card = card.child(ui::section(t!("End of the build log"), cx)).child(
                ui::contained().child(
                    div()
                        .id("frontend-failed-log")
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .p_2()
                        .bg(t.ground)
                        .border_1()
                        .border_color(t.card_border)
                        .children(lines.into_iter().map(|l| {
                            let bad = l.to_lowercase().contains("error");
                            ui::mono(l).text_size(px(11.5)).when(bad, |d| d.text_color(rgb(0xe5756e)))
                        })),
                ),
            );
        } else if let Some(e) = log["error"].as_str() {
            card = card.child(ui::muted(t!("Couldn't read its log: {e}", e = e), cx));
        }
        col = col.child(card);
    }

    // Previews: la última de cada rama; un clic la abre.
    let previews = data["previews"].as_array().cloned().unwrap_or_default();
    let mut card = ui::card(cx).child(ui::section(t!("Previews · latest per branch"), cx));
    if previews.is_empty() {
        card = card.child(ui::muted(t!("No previews yet."), cx));
    } else {
        if let Some((h, text)) = previews.iter().find_map(|p| convex_line(&p["convex"])) {
            card = card.child(ui::kv(t!("Latest preview"), ui::status_tag(h, text), cx));
        }
        let urls: Vec<(String, String)> = previews.iter().enumerate().map(|(i, p)| (format!("pv-{i}"), p["url"].as_str().unwrap_or_default().to_string())).collect();
        let rows: Vec<Row> = previews
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (label, tone) = match p["state"].as_str() {
                    Some("READY") => (t!("ready"), Tone::Success),
                    Some("ERROR") => (t!("failed"), Tone::Danger),
                    Some("CANCELED") => (t!("cancelled"), Tone::Warning),
                    _ => (t!("building"), Tone::Info),
                };
                Row::new(
                    format!("pv-{i}"),
                    [
                        Cell::Tag(label.to_string().into(), tone),
                        Cell::Text(ui::txt(&p["branch"])),
                        Cell::Text(short(&p["commit"]).into()),
                        Cell::Text(ui::txt(&p["message"])),
                        Cell::Text(when(&p["createdAt"]).into()),
                    ],
                )
            })
            .collect();
        card = card.child(ui::muted(t!("Click a preview to open it."), cx)).child(
            DataTable::new("frontend-previews", [
                Column::new("state", t!("Status")).width(rems(6.)),
                Column::new("branch", t!("Branch")).width(rems(10.)),
                Column::new("commit", t!("Commit")).width(rems(6.)),
                Column::new("msg", t!("Message")),
                Column::new("when", t!("When")).width(rems(7.)),
            ])
            .rows(rows)
            .selected(None::<SharedString>)
            .on_select(move |keys, _, cx| {
                if let Some(url) = keys.last().and_then(|k| urls.iter().find(|(id, _)| id == k.as_ref())).map(|(_, u)| u.clone()) {
                    if !url.is_empty() {
                        cx.open_url(&url);
                    }
                }
            }),
        );
    }
    col.child(card).into_any_element()
}
