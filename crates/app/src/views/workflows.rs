// «Flujos»: los flujos de despliegue configurables del proyecto (como n8n). A
// la izquierda, la lista; al centro, el editor: un lienzo con los nodos (se
// arrastran, se conectan tirando del punto de salida a otro nodo) y una paleta
// para agregar; un clic en un nodo abre su configuración en un panel lateral.
// Debajo, las corridas: elegir una pinta el estado de cada nodo en el lienzo
// y deja cancelarla o reintentarla desde el nodo que falló. Lo que necesita
// aprobación se aprueba en la bandeja de aprobaciones (solo la app aprueba).

use ely_core::forms::Select;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, Disableable, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use super::er::Viewport;
use crate::app::{DevtoolApp, EnvTab, Load, Page};
use crate::theme::Tokens;
use crate::ui;

const NODE_W: f32 = 220.;
const NODE_H: f32 = 72.;
const PORT: f32 = 7.;

/// Lo que el puntero tiene agarrado en el lienzo de flujos.
#[derive(Clone)]
pub enum WfGrab {
    Pan { start: (f32, f32), view: (f32, f32, f32) },
    Move { id: String, start: (f32, f32), at: (f32, f32), moved: bool },
    Wire { from: String, now: (f32, f32) },
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "recipe" => t!("Recipe"),
        "wait" => t!("Wait"),
        "approval" => t!("Approval"),
        "check" => t!("Check"),
        "notify" => t!("Notice"),
        "agent" => t!("Agent"),
        "ask" => t!("Ask me"),
        _ => t!("Node"),
    }
}

fn status_label(s: &str) -> &'static str {
    match s {
        "pending" => t!("pending"),
        "running" => t!("running"),
        "waiting-approval" => t!("waiting for approval"),
        "waiting-answer" => t!("waiting for your answer"),
        "succeeded" => t!("done"),
        "failed" => t!("failed"),
        "skipped" => t!("skipped"),
        "cancelled" => t!("cancelled"),
        _ => "",
    }
}

fn status_color(s: &str, cx: &App) -> Hsla {
    use gpui_kit::component::ActiveTheme;
    let t = cx.theme();
    match s {
        "succeeded" => t.success,
        "failed" => t.danger,
        "running" | "waiting-approval" | "waiting-answer" => t.warning,
        "cancelled" | "skipped" => t.muted_foreground,
        _ => Tokens::current(cx).subtle,
    }
}

impl DevtoolApp {
    /// Lee los flujos del proyecto (y crea la promoción de siempre en heskala-erp si no hay).
    pub fn load_workflows(&mut self, cx: &mut Context<Self>) {
        let project = self.project.clone();
        self.get(format!("/api/workflows?project={project}"), move |app, res| {
            if let Load::Ready(v) = &res {
                let list = v["workflows"].as_array().cloned().unwrap_or_default();
                let exists = |id: &str| list.iter().any(|w| w["workflow"]["id"] == id);
                if !app.wf_selected.as_deref().is_some_and(exists) {
                    app.wf_selected = list.first().and_then(|w| w["workflow"]["id"].as_str()).map(str::to_string);
                    app.wf_draft = None;
                }
                if app.wf_draft.is_none() || !app.wf_dirty {
                    app.wf_draft = app.wf_selected.as_ref().and_then(|id| list.iter().find(|w| w["workflow"]["id"] == id.as_str())).map(|w| w["workflow"].clone());
                    app.wf_dirty = false;
                }
            }
            app.wf_data = Some(res);
        }, cx);
        self.load_wf_runs(cx);
    }

    fn load_wf_runs(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.wf_selected.clone() else { return };
        self.get(format!("/api/workflow-runs?workflow={id}&limit=15"), |app, res| {
            if let Load::Ready(Value::Array(list)) = res {
                app.wf_runs = list;
            }
        }, cx);
    }

    /// Sondea flujos y corridas cada 2 s mientras se mira la página o la pestaña Flujos de un entorno.
    pub fn poll_workflows(&mut self, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            let target = this.update(cx, |app, _| (app.page == Page::Workflows || app.viewing(EnvTab::Workflows)).then(|| (app.project.clone(), app.wf_selected.clone()))).ok().flatten();
            let Some((project, sel)) = target else { break };
            let d = daemon.clone();
            let (list, runs) = cx
                .background_executor()
                .spawn(async move {
                    let list = d.get(&format!("/api/workflows?project={project}"));
                    let runs = sel.map(|id| d.get(&format!("/api/workflow-runs?workflow={id}&limit=15")));
                    (list, runs)
                })
                .await;
            let alive = this
                .update(cx, |app, cx| {
                    if let Ok(v) = list {
                        if !app.wf_dirty {
                            let sel = app.wf_selected.clone();
                            app.wf_draft = sel.and_then(|id| v["workflows"].as_array()?.iter().find(|w| w["workflow"]["id"] == id.as_str()).map(|w| w["workflow"].clone()));
                        }
                        app.wf_data = Some(Load::Ready(v));
                    }
                    if let Some(Ok(Value::Array(list))) = runs {
                        app.wf_runs = list;
                    }
                    cx.notify();
                })
                .is_ok();
            if !alive {
                break;
            }
        })
        .detach();
    }

    fn wf_mutate(&mut self, f: impl FnOnce(&mut Value)) {
        if let Some(d) = self.wf_draft.as_mut() {
            f(d);
            self.wf_dirty = true;
        }
    }

    fn wf_save(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.wf_draft.clone() else { return };
        self.post("/api/workflows".into(), d, |app, res, cx| match res {
            Load::Ready(w) => {
                app.wf_selected = w["id"].as_str().map(str::to_string);
                app.wf_draft = Some(w);
                app.wf_dirty = false;
                app.load_workflows(cx);
            }
            Load::Failed(e) => app.action = Some(Load::Failed(e)),
            _ => {}
        }, cx);
    }

    fn wf_add_node(&mut self, kind: &str) {
        let n = self.wf_draft.as_ref().and_then(|d| d["nodes"].as_array().map(Vec::len)).unwrap_or(0);
        let (vx, vy, z) = self.wf_view;
        let id = format!("{kind}-{}", ui::now_ms() % 100_000);
        let params = match kind {
            "recipe" => json!({ "recipe": "", "env": "", "args": {} }),
            "wait" => json!({ "minutes": 5 }),
            "approval" => json!({ "env": "", "level": "simple", "message": "" }),
            "check" => json!({ "check": "env-reachable", "env": "" }),
            "agent" => json!({ "agent": "", "instructions": "" }),
            "ask" => json!({ "question": "" }),
            _ => json!({ "message": "" }),
        };
        // Aparece cerca del centro de lo que se ve, corrido un poco por cada nodo.
        let node = json!({ "id": id, "kind": kind, "title": kind_label(kind), "enabled": true, "onFailure": "stop", "params": params, "x": vx + 120. / z + (n % 5) as f32 * 30., "y": vy + 80. / z + (n % 5) as f32 * 30. });
        self.wf_mutate(|d| {
            if let Some(list) = d["nodes"].as_array_mut() {
                list.push(node);
            }
        });
        self.wf_node_sel = Some(id);
    }
}

fn nodes_of(d: &Value) -> Vec<Value> {
    d["nodes"].as_array().cloned().unwrap_or_default()
}

fn hit(nodes: &[Value], p: (f32, f32)) -> Option<String> {
    nodes.iter().rev().find(|n| {
        let (x, y) = (n["x"].as_f64().unwrap_or(0.) as f32, n["y"].as_f64().unwrap_or(0.) as f32);
        p.0 >= x && p.0 <= x + NODE_W && p.1 >= y && p.1 <= y + NODE_H
    }).and_then(|n| n["id"].as_str().map(str::to_string))
}

/// ¿Está `p` sobre el punto de salida (a la derecha) de algún nodo?
fn hit_port(nodes: &[Value], p: (f32, f32), z: f32) -> Option<String> {
    let r = (PORT + 4.) / z.max(0.3);
    nodes.iter().find(|n| {
        let (x, y) = (n["x"].as_f64().unwrap_or(0.) as f32 + NODE_W, n["y"].as_f64().unwrap_or(0.) as f32 + NODE_H / 2.);
        (p.0 - x).abs() <= r && (p.1 - y).abs() <= r
    }).and_then(|n| n["id"].as_str().map(str::to_string))
}

#[allow(clippy::too_many_arguments)]
fn text(s: &str, size: f32, color: Hsla, weight: FontWeight, at: (f32, f32), max_w: f32, window: &mut Window, cx: &mut App) {
    let fit = (max_w / (size * 0.55)).floor() as usize;
    let shown: SharedString = if s.chars().count() > fit { format!("{}…", s.chars().take(fit.saturating_sub(1)).collect::<String>()).into() } else { s.to_string().into() };
    if shown.is_empty() {
        return;
    }
    let mut f = font(crate::theme::UI_FONT);
    f.weight = weight;
    let run = TextRun { len: shown.len(), font: f, color, background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(shown, px(size), &[run], None);
    let _ = line.paint(point(px(at.0), px(at.1)), px(size * 1.3), TextAlign::Left, None, window, cx);
}

/// El lienzo del flujo: nodos, conexiones y, si hay una corrida elegida, el estado de cada nodo.
fn canvas_el(app: &DevtoolApp, d: Value, run: Option<Value>, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let t = Tokens::current(cx);
    let (vx, vy, z) = app.wf_view;
    let view = Viewport::new(vx, vy, z);
    let grab = app.wf_grab.clone();
    let selected = app.wf_node_sel.clone();
    let bounds = app.wf_bounds.clone();
    let me = cx.entity();
    let status_of = move |id: &str| -> Option<String> { run.as_ref()?["nodes"].as_array()?.iter().find(|n| n["id"] == id).and_then(|n| n["status"].as_str().map(str::to_string)) };
    let nodes = nodes_of(&d);
    let edges: Vec<(String, String)> = d["edges"].as_array().into_iter().flatten().filter_map(|e| Some((e[0].as_str()?.to_string(), e[1].as_str()?.to_string()))).collect();
    let conds: std::collections::HashMap<String, String> = d["conditions"].as_object().map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.trim().to_string()))).filter(|(_, v)| !v.is_empty()).collect()).unwrap_or_default();
    // Estados y colores calculados aquí (el pintado no tiene acceso al tema de Kit).
    let painted: Vec<(Value, Option<(String, Hsla)>)> = nodes
        .iter()
        .map(|n| {
            let s = status_of(n["id"].as_str().unwrap_or_default());
            let c = s.as_ref().map(|s| (status_label(s).to_string(), status_color(s, cx)));
            (n.clone(), c)
        })
        .collect();
    let (b_paint, b_down, b_wheel) = (bounds.clone(), bounds.clone(), bounds.clone());
    let nodes_down = nodes.clone();
    let surface = canvas(
        move |b, _, _| b_paint.set(b),
        move |b, _, window, cx| {
            let o = b.origin;
            window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                window.paint_quad(fill(b, t.panel));
                // Puntos de fondo.
                let mut step = 24. * z;
                while step < 14. {
                    step *= 2.;
                }
                let (ox, oy) = view.to_view((0., 0.));
                let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
                let mut y = oy.rem_euclid(step);
                while y < h {
                    let mut x = ox.rem_euclid(step);
                    while x < w {
                        window.paint_quad(fill(Bounds::new(o + point(px(x), px(y)), size(px(1.5), px(1.5))), t.card_border));
                        x += step;
                    }
                    y += step;
                }
                let pos = |id: &str| -> Option<(f32, f32)> {
                    let n = painted.iter().find(|(n, _)| n["id"] == id)?;
                    let (mut x, mut y) = (n.0["x"].as_f64().unwrap_or(0.) as f32, n.0["y"].as_f64().unwrap_or(0.) as f32);
                    if let Some(WfGrab::Move { id: gid, start, at, .. }) = &grab {
                        if gid == id {
                            let _ = start;
                            x = at.0;
                            y = at.1;
                        }
                    }
                    Some((x, y))
                };
                let wire = |a: (f32, f32), bb: (f32, f32), color: Hsla, width: f32, window: &mut Window| {
                    let (va, vb) = (view.to_view(a), view.to_view(bb));
                    let pull = ((vb.0 - va.0).abs() / 2.).max(30.);
                    let mut p = PathBuilder::stroke(px(width));
                    p.move_to(o + point(px(va.0), px(va.1)));
                    p.cubic_bezier_to(o + point(px(vb.0), px(vb.1)), o + point(px(va.0 + pull), px(va.1)), o + point(px(vb.0 - pull), px(vb.1)));
                    if let Ok(path) = p.build() {
                        window.paint_path(path, color);
                    }
                };
                for (a, bb) in &edges {
                    if let (Some(pa), Some(pb)) = (pos(a), pos(bb)) {
                        let cond = conds.get(&format!("{a}>{bb}"));
                        wire((pa.0 + NODE_W, pa.1 + NODE_H / 2.), (pb.0, pb.1 + NODE_H / 2.), if cond.is_some() { t.accent_ink } else { t.subtle }, 1.6, window);
                        // La condición, en una etiqueta a mitad de camino.
                        if let Some(cond) = cond {
                            let mid = view.to_view(((pa.0 + NODE_W + pb.0) / 2., (pa.1 + pb.1) / 2. + NODE_H / 2.));
                            let fs = (11. * z).max(8.);
                            let w = (cond.chars().count() as f32 * fs * 0.56 + 12.).min(260. * z);
                            // Encima de la línea, para no tapar los puntos de los nodos.
                            let top = mid.1 - fs * 2. - 8. * z;
                            let lb = Bounds::new(o + point(px(mid.0 - w / 2.), px(top)), size(px(w), px(fs * 2.)));
                            window.paint_quad(quad(lb, px(4.), t.panel, px(1.), t.accent_ink, BorderStyle::default()));
                            text(cond, fs, t.accent_ink, FontWeight::MEDIUM, (f32::from(o.x) + mid.0 - w / 2. + 6., f32::from(o.y) + top + fs * 0.4), w - 10., window, cx);
                        }
                    }
                }
                if let Some(WfGrab::Wire { from, now }) = &grab {
                    if let Some(pa) = pos(from) {
                        wire((pa.0 + NODE_W, pa.1 + NODE_H / 2.), *now, t.accent_ink, 2., window);
                    }
                }
                for (n, st) in &painted {
                    let id = n["id"].as_str().unwrap_or_default();
                    let Some((x, y)) = pos(id) else { continue };
                    let (sx, sy) = view.to_view((x, y));
                    let r = Bounds::new(o + point(px(sx), px(sy)), size(px(NODE_W * z), px(NODE_H * z)));
                    let chosen = selected.as_deref() == Some(id);
                    let enabled = n["enabled"] != false;
                    let border = match st {
                        Some((_, c)) => *c,
                        None if chosen => t.accent_ink,
                        None => t.card_border,
                    };
                    window.paint_quad(quad(r, px(6. * z), if enabled { t.card } else { t.hover }, px(if chosen || st.is_some() { 2. } else { 1. }), border, BorderStyle::default()));
                    // Franja de la izquierda con el color del tipo de nodo.
                    let kind = n["kind"].as_str().unwrap_or_default();
                    let stripe = match kind {
                        "approval" => t.accent,
                        "wait" => t.subtle,
                        "check" => t.muted,
                        "agent" => rgb(0x60a5fa).into(),
                        "ask" => t.accent,
                        _ => t.accent_ink.opacity(0.6),
                    };
                    window.paint_quad(fill(Bounds::new(r.origin + point(px(1.), px(1.)), size(px(4. * z), px(NODE_H * z - 2.))), stripe));
                    let (ax, ay) = (f32::from(o.x) + sx, f32::from(o.y) + sy);
                    let fs = (12. * z).max(8.);
                    text(n["title"].as_str().unwrap_or_default(), fs + 1., if enabled { t.fg } else { t.muted }, FontWeight::SEMIBOLD, (ax + 12. * z, ay + 10. * z), NODE_W * z - 20. * z, window, cx);
                    let sub = if enabled { kind_label(kind).to_string() } else { t!("{a} · off", a = kind_label(kind)) };
                    text(&sub, fs, t.muted, FontWeight::NORMAL, (ax + 12. * z, ay + 30. * z), NODE_W * z - 20. * z, window, cx);
                    if let Some((label, c)) = st {
                        text(label, fs, *c, FontWeight::MEDIUM, (ax + 12. * z, ay + 48. * z), NODE_W * z - 20. * z, window, cx);
                    }
                    // Puntos de entrada y salida.
                    let d = PORT * z.max(0.5);
                    for (px_, filled) in [(sx, false), (sx + NODE_W * z, true)] {
                        let c = Bounds::new(o + point(px(px_ - d / 2.), px(sy + NODE_H * z / 2. - d / 2.)), size(px(d), px(d)));
                        window.paint_quad(quad(c, px(d), if filled { t.accent_ink } else { t.card }, px(1.), t.accent_ink, BorderStyle::default()));
                    }
                }
            });
            if grab.is_none() {
                return;
            }
            let (mv, up) = (me.clone(), me.clone());
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                mv.update(cx, |app, cx| {
                    let b = app.wf_bounds.get();
                    let p = (f32::from(ev.position.x - b.origin.x), f32::from(ev.position.y - b.origin.y));
                    let (vx, vy, z) = app.wf_view;
                    let view = Viewport::new(vx, vy, z);
                    match app.wf_grab.as_mut() {
                        Some(WfGrab::Pan { start, view: v0 }) => {
                            let v = Viewport::new(v0.0, v0.1, v0.2).panned((p.0 - start.0, p.1 - start.1));
                            app.wf_view = (v.x, v.y, v.zoom);
                        }
                        Some(WfGrab::Move { start, at, moved, id }) => {
                            let (dx, dy) = (p.0 - start.0, p.1 - start.1);
                            *moved |= dx.abs() + dy.abs() > 3.;
                            let base = app.wf_draft.as_ref().and_then(|d| d["nodes"].as_array()?.iter().find(|n| n["id"] == id.as_str()).map(|n| (n["x"].as_f64().unwrap_or(0.) as f32, n["y"].as_f64().unwrap_or(0.) as f32)));
                            if let Some((bx, by)) = base {
                                *at = (bx + dx / z, by + dy / z);
                            }
                        }
                        Some(WfGrab::Wire { now, .. }) => *now = view.to_canvas(p),
                        None => return,
                    }
                    cx.notify();
                });
            });
            window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                up.update(cx, |app, cx| {
                    let b = app.wf_bounds.get();
                    let p = (f32::from(ev.position.x - b.origin.x), f32::from(ev.position.y - b.origin.y));
                    let (vx, vy, z) = app.wf_view;
                    let c = Viewport::new(vx, vy, z).to_canvas(p);
                    match app.wf_grab.take() {
                        Some(WfGrab::Move { id, at, moved, .. }) => {
                            if moved {
                                app.wf_mutate(|d| {
                                    if let Some(n) = d["nodes"].as_array_mut().and_then(|l| l.iter_mut().find(|n| n["id"] == id.as_str())) {
                                        n["x"] = json!(at.0.round());
                                        n["y"] = json!(at.1.round());
                                    }
                                });
                            } else {
                                app.wf_node_sel = Some(id.clone());
                                open_node_sheet(app, id, window, cx);
                            }
                        }
                        Some(WfGrab::Wire { from, .. }) => {
                            let nodes = app.wf_draft.as_ref().map(nodes_of).unwrap_or_default();
                            if let Some(to) = hit(&nodes, c).filter(|to| *to != from) {
                                app.wf_mutate(|d| {
                                    let exists = d["edges"].as_array().is_some_and(|l| l.iter().any(|e| e[0] == from.as_str() && e[1] == to.as_str()));
                                    if !exists {
                                        if !d["edges"].is_array() {
                                            d["edges"] = json!([]);
                                        }
                                        d["edges"].as_array_mut().unwrap().push(json!([from, to]));
                                    }
                                });
                            }
                        }
                        _ => {}
                    }
                    cx.notify();
                });
            });
        },
    )
    .size_full();
    let editable = app.wf_run_sel.is_none();
    div()
        .id("wf-canvas")
        .relative()
        .size_full()
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, cx.listener(move |app, ev: &MouseDownEvent, _, cx| {
            let b = b_down.get();
            let p = (f32::from(ev.position.x - b.origin.x), f32::from(ev.position.y - b.origin.y));
            let (vx, vy, z) = app.wf_view;
            let c = Viewport::new(vx, vy, z).to_canvas(p);
            app.wf_grab = Some(if let Some(from) = hit_port(&nodes_down, c, z).filter(|_| editable) {
                WfGrab::Wire { from, now: c }
            } else if let Some(id) = hit(&nodes_down, c) {
                let at = nodes_down.iter().find(|n| n["id"] == id.as_str()).map(|n| (n["x"].as_f64().unwrap_or(0.) as f32, n["y"].as_f64().unwrap_or(0.) as f32)).unwrap_or_default();
                if editable {
                    WfGrab::Move { id, start: p, at, moved: false }
                } else {
                    app.wf_node_sel = Some(id);
                    WfGrab::Pan { start: p, view: app.wf_view }
                }
            } else {
                WfGrab::Pan { start: p, view: app.wf_view }
            });
            cx.notify();
        }))
        .on_scroll_wheel(cx.listener(move |app, ev: &ScrollWheelEvent, window, cx| {
            cx.stop_propagation();
            let (vx, vy, z) = app.wf_view;
            let v = Viewport::new(vx, vy, z);
            let (dx, dy) = match ev.delta {
                ScrollDelta::Pixels(d) => (f32::from(d.x), f32::from(d.y)),
                ScrollDelta::Lines(l) => (l.x * f32::from(window.line_height()), l.y * f32::from(window.line_height())),
            };
            let b = b_wheel.get();
            let at = (f32::from(ev.position.x - b.origin.x), f32::from(ev.position.y - b.origin.y));
            let next = if ev.modifiers.platform || ev.modifiers.control { v.zoomed((dy / 240.).exp(), at) } else { v.panned((dx, dy)) };
            app.wf_view = (next.x, next.y, next.zoom.clamp(0.3, 2.));
            cx.notify();
        }))
        .on_pinch(cx.listener(|app, ev: &PinchEvent, _, cx| {
            let (vx, vy, z) = app.wf_view;
            let b = app.wf_bounds.get();
            let next = Viewport::new(vx, vy, z).zoomed(1. + ev.delta, (f32::from(ev.position.x - b.origin.x), f32::from(ev.position.y - b.origin.y)));
            app.wf_view = (next.x, next.y, next.zoom.clamp(0.3, 2.));
            cx.notify();
        }))
        .child(surface)
}

/// Panel lateral con la configuración de un nodo.
pub fn open_node_sheet(app: &mut DevtoolApp, id: String, window: &mut Window, cx: &mut Context<DevtoolApp>) {
    let Some(node) = app.wf_draft.as_ref().and_then(|d| d["nodes"].as_array()?.iter().find(|n| n["id"] == id.as_str()).cloned()) else { return };
    let field = |app: &mut DevtoolApp, key: String, value: String, window: &mut Window, cx: &mut Context<DevtoolApp>| {
        let st = app.inputs.entry(key).or_insert_with(|| cx.new(|cx| InputState::new(window, cx))).clone();
        st.update(cx, |s, cx| s.set_value(value, window, cx));
        st
    };
    let p = node["params"].clone();
    let sv = |k: &str| p[k].as_str().map(str::to_string).unwrap_or_else(|| if p[k].is_null() { String::new() } else { p[k].to_string() });
    let title = field(app, format!("wf-{id}-title"), node["title"].as_str().unwrap_or_default().to_string(), window, cx);
    // En un nodo agente, el mismo campo son sus instrucciones para este flujo.
    let message = field(app, format!("wf-{id}-message"), sv(match node["kind"].as_str() { Some("agent") => "instructions", Some("ask") => "question", _ => "message" }), window, cx);
    let minutes = field(app, format!("wf-{id}-minutes"), sv("minutes"), window, cx);
    let hours = field(app, format!("wf-{id}-hours"), sv("hours"), window, cx);
    // Una entrada por arista que llega aquí, con su condición.
    let cond_keys: Vec<String> = app
        .wf_draft
        .as_ref()
        .and_then(|d| d["edges"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|e| e[1] == id.as_str())
        .filter_map(|e| Some(format!("{}>{id}", e[0].as_str()?)))
        .collect();
    for key in &cond_keys {
        let current = app.wf_draft.as_ref().and_then(|d| d["conditions"][key.as_str()].as_str().map(str::to_string)).unwrap_or_default();
        field(app, format!("wf-cond-{key}"), current, window, cx);
    }
    // Los desplegables escriben directo en el borrador.
    let me = cx.entity();
    window.open_sheet(cx, move |sheet, window, cx| {
        // Nodo de receta: el entorno se elige entre los de los tipos donde corre la receta (y
        // solo, si hay uno), y cada argumento de la receta tiene su campo.
        prepare_recipe_node(&me, &id, window, cx);
        let app_now = me.read(cx);
        let Some(node) = app_now.wf_draft.as_ref().and_then(|d| d["nodes"].as_array()?.iter().find(|n| n["id"] == id.as_str()).cloned()) else {
            return sheet.title(t!("Node")).child(ui::muted(t!("The node no longer exists."), cx));
        };
        let kind = node["kind"].as_str().unwrap_or_default().to_string();
        let p = node["params"].clone();
        let envs: Vec<(String, String)> = match &app_now.wf_data {
            Some(Load::Ready(v)) => v["environments"].as_array().into_iter().flatten().filter_map(|e| Some((e[0].as_str()?.to_string(), e[1].as_str()?.to_string()))).collect(),
            _ => vec![],
        };
        let recipes: Vec<(String, String)> = app_now.recipes.iter().filter_map(|r| Some((r["name"].as_str()?.to_string(), r["title"].as_str().unwrap_or_default().to_string()))).filter(|(n, _)| !n.starts_with("workflow-")).collect();
        let edges: Vec<(String, String)> = app_now.wf_draft.as_ref().and_then(|d| d["edges"].as_array().cloned()).unwrap_or_default().iter().filter_map(|e| Some((e[0].as_str()?.to_string(), e[1].as_str()?.to_string()))).filter(|(a, b)| *a == id || *b == id).collect();
        let set_param = |me: &Entity<DevtoolApp>, id: &str, key: &'static str| {
            let (me, id) = (me.downgrade(), id.to_string());
            move |v: &SharedString, _: &mut Window, cx: &mut App| {
                let v = v.to_string();
                let _ = me.update(cx, |app, cx| {
                    app.wf_mutate(|d| {
                        if let Some(n) = d["nodes"].as_array_mut().and_then(|l| l.iter_mut().find(|n| n["id"] == id.as_str())) {
                            if key == "onFailure" {
                                n["onFailure"] = json!(v);
                            } else if key == "enabled" {
                                n["enabled"] = json!(v == "yes");
                            } else {
                                n["params"][key] = json!(v);
                            }
                        }
                    });
                    cx.notify();
                });
            }
        };
        let select = |sid: String, choices: Vec<(String, String)>, current: String, key: &'static str| {
            Select::new(SharedString::from(sid), choices.into_iter().map(|(v, l)| crate::ely::choice(v, l)).collect::<Vec<_>>()).selected(current).on_change(set_param(&me, &id, key))
        };
        let row = |label: &'static str, el: AnyElement, cx: &App| v_flex().gap_1().child(ui::section(label, cx)).child(el);
        let mut body = v_flex().gap_3().child(row(t!("Title"), Input::new(&title).small().into_any_element(), cx));
        let node_on = node["enabled"] != false;
        let toggle = set_param(&me, &id, "enabled");
        body = body.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().child(ui::muted(if node_on { t!("On: this node runs") } else { t!("Off: the run skips this node") }, cx).text_xs()))
                .child(ui::toggle(SharedString::from(format!("wf-en-{id}")), node_on, cx).on_click(move |_, window, cx| toggle(&SharedString::from(if node_on { "no" } else { "yes" }), window, cx))),
        );
        body = body.child(row(t!("On failure"), select(format!("wf-of-{id}"), vec![("stop".into(), t!("Stop the workflow").into()), ("continue".into(), t!("Continue with the next").into())], node["onFailure"].as_str().unwrap_or("stop").into(), "onFailure").into_any_element(), cx));
        let env_choices: Vec<(String, String)> = envs.iter().map(|(n, k)| (n.clone(), format!("{n} · {k}"))).collect();
        let s = |k: &str| p[k].as_str().unwrap_or_default().to_string();
        match kind.as_str() {
            "recipe" => {
                body = body.child(row(t!("Recipe"), select(format!("wf-rc-{id}"), recipes.iter().map(|(n, t)| (n.clone(), t.clone())).collect(), s("recipe"), "recipe").into_any_element(), cx));
                let recipe = app_now.recipes.iter().find(|r| r["name"] == s("recipe").as_str()).cloned();
                if let Some(recipe) = recipe {
                    body = body.child(ui::muted(ui::txt(&recipe["description"]), cx).text_xs());
                    let kinds: Vec<String> = recipe["kinds"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
                    let fitting: Vec<(String, String)> = envs.iter().filter(|(_, k)| kinds.contains(k)).map(|(n, k)| (n.clone(), format!("{n} · {k}"))).collect();
                    let kinds_text = kinds.iter().map(|k| kind_name(k)).collect::<Vec<_>>().join(", ");
                    let env_el = match fitting.len() {
                        0 => ui::muted(t!("This project has no environment where this recipe runs ({a}).", a = kinds_text), cx).into_any_element(),
                        1 => v_flex()
                            .gap_1()
                            .child(ui::mono(fitting[0].1.clone()))
                            .child(ui::muted(t!("It's the only environment where this recipe runs ({a}).", a = kinds_text), cx).text_xs())
                            .into_any_element(),
                        _ => v_flex()
                            .gap_1()
                            .child(select(format!("wf-ev-{id}"), fitting.clone(), s("env"), "env"))
                            .child(ui::muted(t!("Only the environments where this recipe runs ({a}).", a = kinds_text), cx).text_xs())
                            .into_any_element(),
                    };
                    body = body.child(row(t!("Environment"), env_el, cx));
                    // Un campo por argumento de la receta (con su descripción).
                    let rname = s("recipe");
                    for arg in recipe["args"].as_array().cloned().unwrap_or_default() {
                        let name = arg["name"].as_str().unwrap_or_default().to_string();
                        let Some(state) = app_now.inputs.get(&arg_key(&id, &rname, &name)).cloned() else { continue };
                        body = body.child(
                            v_flex()
                                .gap_1()
                                .child(ui::section(name.clone(), cx))
                                .child(Input::new(&state).small())
                                .child(ui::muted(ui::txt(&arg["description"]), cx).text_xs()),
                        );
                    }
                    body = body.child(variables(app_now, &id, cx));
                }
            }
            "wait" => body = body.child(row(t!("Minutes"), Input::new(&minutes).small().into_any_element(), cx)),
            "approval" => {
                body = body
                    .child(row(t!("Environment"), select(format!("wf-ev-{id}"), env_choices.clone(), s("env"), "env").into_any_element(), cx))
                    .child(row(t!("Level"), select(format!("wf-lv-{id}"), vec![("simple".into(), t!("Simple").into()), ("strict".into(), t!("Reinforced (type the name; Touch ID in production)").into())], s("level"), "level").into_any_element(), cx))
                    .child(row(t!("Message"), Input::new(&message).small().into_any_element(), cx));
            }
            "check" => {
                body = body
                    .child(row(t!("Check"), select(format!("wf-ck-{id}"), vec![("env-reachable".into(), t!("The environment responds").into()), ("recent-backup".into(), t!("There's a recent backup").into()), ("ci-green".into(), t!("CI is green").into()), ("schema-safe".into(), t!("The local schema is safe to deploy here").into())], s("check"), "check").into_any_element(), cx))
                    .child(row(t!("Environment"), select(format!("wf-ev-{id}"), env_choices.clone(), s("env"), "env").into_any_element(), cx));
                if s("check") == "recent-backup" {
                    body = body.child(row(t!("Within the last (hours)"), Input::new(&hours).small().into_any_element(), cx));
                }
            }
            "agent" => {
                let roles: Vec<Value> = app_now.agent_roles.as_ref().and_then(|r| r["roles"].as_array().cloned()).unwrap_or_default();
                let choices: Vec<(String, String)> = roles.iter().filter(|r| r["enabled"] == true).filter_map(|r| Some((r["id"].as_str()?.to_string(), r["name"].as_str()?.to_string()))).collect();
                body = body.child(row(t!("Agent"), select(format!("wf-ag-{id}"), choices, s("agent"), "agent").into_any_element(), cx));
                if let Some(role) = roles.iter().find(|r| r["id"] == s("agent").as_str()) {
                    let out = super::agent_roles::output_text(&role["output"]);
                    body = body
                        .child(ui::muted(ui::txt(&role["instructions"]), cx).text_xs())
                        .child(ui::muted(if out.is_empty() { t!("Its result: free text (in {node.<id>.report}).").to_string() } else { t!("Its result: {f}. Use them as {{node.{id}.<field>}} in later nodes.", f = out, id = id).to_string() }, cx).text_xs());
                }
                body = body
                    .child(row(t!("Instructions for this flow"), Input::new(&message).small().into_any_element(), cx))
                    .child(ui::muted(t!("It reads the event that started the flow and the results of the nodes connected to it."), cx).text_xs());
            }
            "ask" => {
                body = body
                    .child(row(t!("Question"), Input::new(&message).small().into_any_element(), cx))
                    .child(ui::muted(t!("The run stops here and asks you in its thread (Agents) and on the phone. Your answer is {node.<id>.answer}, for the conditions and agents after it."), cx).text_xs());
            }
            "notify" => {
                body = body
                    .child(row(t!("Message"), Input::new(&message).small().into_any_element(), cx))
                    .child(row(t!("Priority"), select(format!("wf-pr-{id}"), vec![("normal".into(), t!("Normal").into()), ("high".into(), t!("High (urgent on the phone)").into())], if s("priority") == "high" { "high".into() } else { "normal".into() }, "priority").into_any_element(), cx))
                    .child(ui::muted(t!("It goes to the phone (Settings → Notifications) and stays in the run. Use {node.<id>.<field>} for the agents' results."), cx).text_xs());
            }
            _ => body = body.child(row(t!("Message"), Input::new(&message).small().into_any_element(), cx)),
        }
        // Conexiones del nodo, con su × para quitarlas.
        if !edges.is_empty() {
            let mut list = v_flex().gap_1().child(ui::section(t!("Connections"), cx));
            // Las que llegan a este nodo pueden tener condición (cuándo se pasa por ellas).
            for (a, b) in edges.iter().filter(|(_, b)| *b == id) {
                if let Some(st) = app_now.inputs.get(&format!("wf-cond-{a}>{b}")).cloned() {
                    list = list.child(v_flex().gap_0p5().child(ui::muted(t!("From {a}, only if (empty: always):", a = a), cx).text_xs()).child(Input::new(&st).small()));
                }
            }
            list = list.child(ui::muted(t!("For example: risk.risk >= high and risk.immediate_fix == true · review.verdict == approve · deploy.status == failed"), cx).text_xs());
            for (i, (a, b)) in edges.into_iter().enumerate() {
                let m = me.downgrade();
                let (a2, b2) = (a.clone(), b.clone());
                list = list.child(h_flex().gap_2().items_center().child(div().flex_1().child(ui::mono(format!("{a} → {b}")))).child(Button::new(SharedString::from(format!("wf-edge-x-{i}"))).xsmall().ghost().icon(IconName::Delete).on_click(move |_, _, cx| {
                    let (a, b) = (a2.clone(), b2.clone());
                    let _ = m.update(cx, |app, cx| {
                        app.wf_mutate(|d| {
                            if let Some(l) = d["edges"].as_array_mut() {
                                l.retain(|e| !(e[0] == a.as_str() && e[1] == b.as_str()));
                            }
                        });
                        cx.notify();
                    });
                })));
            }
            body = body.child(list);
        }
        let (m_ok, m_del) = (me.clone(), me.clone());
        let cond_keys = cond_keys.clone();
        let (id_ok, id_del) = (id.clone(), id.clone());
        let (t_, m_, mi_, h_) = (title.clone(), message.clone(), minutes.clone(), hours.clone());
        body = body.child(
            h_flex()
                .gap_2()
                .child(Button::new("wf-node-del").small().danger().icon(IconName::Delete).label(t!("Remove node")).on_click(move |_, window, cx| {
                    let id = id_del.clone();
                    m_del.update(cx, |app, cx| {
                        app.wf_mutate(|d| {
                            if let Some(l) = d["nodes"].as_array_mut() {
                                l.retain(|n| n["id"] != id.as_str());
                            }
                            if let Some(l) = d["edges"].as_array_mut() {
                                l.retain(|e| e[0] != id.as_str() && e[1] != id.as_str());
                            }
                        });
                        app.wf_node_sel = None;
                        cx.notify();
                    });
                    window.close_sheet(cx);
                }))
                .child(div().flex_1())
                .child(ui::brand(Button::new("wf-node-ok").small().icon(IconName::Check).label(t!("Apply")).on_click(move |_, window, cx| {
                    let val = |s: &Entity<InputState>, cx: &App| s.read(cx).value().trim().to_string();
                    let (title, message, minutes, hours) = (val(&t_, cx), val(&m_, cx), val(&mi_, cx), val(&h_, cx));
                    let id = id_ok.clone();
                    let cond_keys = cond_keys.clone();
                    m_ok.update(cx, |app, cx| {
                        let parsed_args = recipe_args_of(app, &id, cx);
                        let conds: Vec<(String, String)> = cond_keys.iter().filter_map(|k| app.inputs.get(&format!("wf-cond-{k}")).map(|s| (k.clone(), s.read(cx).value().trim().to_string()))).collect();
                        app.wf_mutate(|d| {
                            if !d["conditions"].is_object() {
                                d["conditions"] = json!({});
                            }
                            for (k, v) in &conds {
                                if v.is_empty() {
                                    d["conditions"].as_object_mut().map(|o| o.remove(k));
                                } else {
                                    d["conditions"][k.as_str()] = json!(v);
                                }
                            }
                            if let Some(n) = d["nodes"].as_array_mut().and_then(|l| l.iter_mut().find(|n| n["id"] == id.as_str())) {
                                if !title.is_empty() {
                                    n["title"] = json!(title);
                                }
                                match n["kind"].as_str().unwrap_or_default() {
                                    "recipe" => n["params"]["args"] = parsed_args,
                                    "wait" => n["params"]["minutes"] = json!(minutes.replace(',', ".").parse::<f64>().unwrap_or(1.)),
                                    "check" => {
                                        if let Ok(h) = hours.parse::<f64>() {
                                            n["params"]["hours"] = json!(h);
                                        }
                                    }
                                    "agent" => n["params"]["instructions"] = json!(message),
                                    "ask" => n["params"]["question"] = json!(message),
                                    _ => n["params"]["message"] = json!(message),
                                }
                            }
                        });
                        cx.notify();
                    });
                    window.close_sheet(cx);
                }), cx)),
        );
        sheet.title(format!("{} · {}", kind_label(&kind), node["title"].as_str().unwrap_or_default())).size(px(420.)).child(body)
    });
}

fn arg_key(node: &str, recipe: &str, arg: &str) -> String {
    format!("wf-{node}-arg-{recipe}-{arg}")
}

/// Nombre legible de un tipo de entorno.
fn kind_name(kind: &str) -> &'static str {
    match kind {
        "devbox" => "devbox",
        "throwaway" => t!("throwaway"),
        "cloud-dev" => t!("cloud dev"),
        "cloud-prod" => t!("production"),
        "new" => t!("a new environment"),
        _ => "?",
    }
}

/// Antes de dibujar el panel de un nodo de receta: si hay un solo entorno donde corre la receta,
/// lo elige; y crea el campo de cada argumento (con lo guardado, o `{run.commit}` en los commits).
fn prepare_recipe_node(me: &Entity<DevtoolApp>, id: &str, window: &mut Window, cx: &mut App) {
    me.update(cx, |app, cx| {
        let Some(node) = app.wf_draft.as_ref().and_then(|d| d["nodes"].as_array()?.iter().find(|n| n["id"] == id).cloned()) else { return };
        if node["kind"] != "recipe" {
            return;
        }
        let rname = node["params"]["recipe"].as_str().unwrap_or_default().to_string();
        let Some(recipe) = app.recipes.iter().find(|r| r["name"] == rname.as_str()).cloned() else { return };
        let kinds: Vec<String> = recipe["kinds"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
        let envs: Vec<(String, String)> = match &app.wf_data {
            Some(Load::Ready(v)) => v["environments"].as_array().into_iter().flatten().filter_map(|e| Some((e[0].as_str()?.to_string(), e[1].as_str()?.to_string()))).collect(),
            _ => vec![],
        };
        let fitting: Vec<&String> = envs.iter().filter(|(_, k)| kinds.contains(k)).map(|(n, _)| n).collect();
        let current = node["params"]["env"].as_str().unwrap_or_default().to_string();
        // Uno solo: ese. Si el elegido no sirve para esta receta, se quita.
        let next = match fitting.as_slice() {
            [only] => Some((*only).clone()),
            list if !current.is_empty() && !list.iter().any(|n| **n == current) => Some(String::new()),
            _ => None,
        };
        if let Some(env) = next.filter(|e| *e != current) {
            let id = id.to_string();
            app.wf_mutate(|d| {
                if let Some(n) = d["nodes"].as_array_mut().and_then(|l| l.iter_mut().find(|n| n["id"] == id.as_str())) {
                    n["params"]["env"] = json!(env);
                }
            });
        }
        for arg in recipe["args"].as_array().cloned().unwrap_or_default() {
            let name = arg["name"].as_str().unwrap_or_default().to_string();
            let key = arg_key(id, &rname, &name);
            if app.inputs.contains_key(&key) {
                continue;
            }
            let saved = match &node["params"]["args"][name.as_str()] {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            let value = if !saved.is_empty() {
                saved
            } else if arg["type"] == "commit" {
                "{run.commit}".to_string()
            } else {
                arg["default"].as_str().unwrap_or_default().to_string()
            };
            let placeholder = arg["default"].as_str().unwrap_or_default().to_string();
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
            app.inputs.insert(key, state);
        }
    });
}

/// Los argumentos del nodo de receta según sus campos (los vacíos no se guardan).
fn recipe_args_of(app: &DevtoolApp, id: &str, cx: &App) -> Value {
    let node = app.wf_draft.as_ref().and_then(|d| d["nodes"].as_array()?.iter().find(|n| n["id"] == id).cloned()).unwrap_or_default();
    let rname = node["params"]["recipe"].as_str().unwrap_or_default();
    let mut args = json!({});
    if let Some(recipe) = app.recipes.iter().find(|r| r["name"] == rname) {
        for arg in recipe["args"].as_array().into_iter().flatten() {
            let name = arg["name"].as_str().unwrap_or_default();
            if let Some(state) = app.inputs.get(&arg_key(id, rname, name)) {
                let v = state.read(cx).value().trim().to_string();
                if !v.is_empty() {
                    args[name] = json!(v);
                }
            }
        }
    }
    args
}

/// Las variables que puede usar un argumento: el commit de la corrida y lo que dejan los nodos
/// de receta que corren antes (los que llegan a este por las conexiones). Un clic copia una.
fn variables(app: &DevtoolApp, id: &str, cx: &App) -> Div {
    let draft = app.wf_draft.clone().unwrap_or_default();
    let edges: Vec<(String, String)> = draft["edges"].as_array().into_iter().flatten().filter_map(|e| Some((e[0].as_str()?.to_string(), e[1].as_str()?.to_string()))).collect();
    // Los nodos anteriores: hacia atrás por las conexiones, el más cercano primero.
    let mut before: Vec<String> = vec![];
    let mut queue = std::collections::VecDeque::from([id.to_string()]);
    while let Some(n) = queue.pop_front() {
        for (a, b) in &edges {
            if *b == n && !before.contains(a) && a != id {
                before.push(a.clone());
                queue.push_back(a.clone());
            }
        }
    }
    let mut items: Vec<(String, String)> = vec![("{run.commit}".into(), t!("The commit or branch you choose when you start the run (Run → commit).").into())];
    for nid in &before {
        let Some(node) = draft["nodes"].as_array().and_then(|l| l.iter().find(|n| n["id"] == nid.as_str())) else { continue };
        let Some(recipe) = app.recipes.iter().find(|r| r["name"] == node["params"]["recipe"]) else { continue };
        for out in recipe["outputs"].as_array().into_iter().flatten() {
            let name = out["name"].as_str().unwrap_or_default();
            items.push((format!("{{node.{nid}.{name}}}"), format!("{} · {}", ui::txt(&out["description"]), t!("left by «{a}»", a = ui::txt(&node["title"])))));
        }
    }
    let mut list = v_flex()
        .gap_1()
        .child(ui::section(t!("Variables"), cx))
        .child(ui::muted(t!("Write them in a field and the daemon replaces them when the node runs. Click one to copy it."), cx).text_xs());
    for (i, (var, desc)) in items.into_iter().enumerate() {
        let copy = var.clone();
        list = list.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(Button::new(SharedString::from(format!("wf-var-{i}"))).xsmall().outline().label(var).on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    use gpui_kit::component::notification::Notification;
                    use gpui_kit::component::WindowExt;
                    window.push_notification(Notification::success(t!("Copied {a}", a = copy)), cx);
                }))
                .child(div().flex_1().min_w_0().child(ui::muted(desc, cx).text_xs())),
        );
    }
    if before.is_empty() {
        list = list.child(ui::muted(t!("Connect earlier nodes to this one to use what they leave (e.g. the number of a backup)."), cx).text_xs());
    }
    list
}

/// Panel para crear un flujo desde una plantilla o importar uno en JSON.
fn open_new_sheet(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) {
    let project = app.project.clone();
    app.get(format!("/api/workflows/templates?project={project}"), |app, res| {
        if let Load::Ready(Value::Array(list)) = res {
            app.wf_templates = list;
        }
    }, cx);
    let import = app.inputs.entry("wf-import".into()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder("{\"condxWorkflow\": 1, …}"))).clone();
    let me = cx.entity();
    window.open_sheet(cx, move |sheet, _, cx| {
        let t = Tokens::current(cx);
        let templates = me.read(cx).wf_templates.clone();
        let mut list = v_flex().border_1().border_color(t.card_border);
        for (i, tpl) in templates.into_iter().enumerate() {
            let m = me.clone();
            let name = tpl["name"].as_str().unwrap_or_default().to_string();
            let desc = tpl["description"].as_str().unwrap_or_default().to_string();
            let n = tpl["nodes"].as_array().map(Vec::len).unwrap_or(0);
            list = list.child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .items_center()
                    .when(i > 0, |d| d.border_t_1().border_color(t.card_border))
                    .child(v_flex().flex_1().min_w_0().child(div().text_color(t.fg).child(name)).child(ui::muted(if desc.is_empty() { t!("{n} nodes", n = n) } else { t!("{desc} · {n} nodes", desc = desc, n = n) }, cx)))
                    .child(Button::new(SharedString::from(format!("wf-tpl-{i}"))).xsmall().outline().icon(IconName::Plus).label(t!("Use")).on_click(move |_, window, cx| {
                        let tpl = tpl.clone();
                        m.update(cx, |app, cx| {
                            app.post("/api/workflows".into(), tpl, |app, res, cx| match res {
                                Load::Ready(w) => {
                                    app.wf_selected = w["id"].as_str().map(str::to_string);
                                    app.wf_draft = Some(w);
                                    app.wf_dirty = false;
                                    app.wf_run_sel = None;
                                    app.load_workflows(cx);
                                }
                                Load::Failed(e) => app.action = Some(Load::Failed(e)),
                                _ => {}
                            }, cx);
                        });
                        window.close_sheet(cx);
                    })),
            );
        }
        let (m, imp) = (me.clone(), import.clone());
        let seed = me.clone();
        sheet.title(t!("New workflow")).size(px(460.)).child(
            v_flex()
                .gap_4()
                .child(ui::section(t!("From a template"), cx))
                .child(list)
                // Todos los sugeridos de una vez, apagados: se encienden cuando se quieran usar.
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().child(ui::muted(t!("Or create every suggested workflow this project is missing, turned off."), cx).text_xs()))
                        .child(Button::new("wf-seed").small().outline().icon(IconName::Plus).label(t!("Create suggested")).on_click(move |_, window, cx| {
                            seed.update(cx, |app, cx| {
                                let project = app.project.clone();
                                app.post("/api/workflows/suggested".into(), json!({ "project": project }), |app, res, cx| {
                                    // Los nuevos aparecen en la lista como «Apagado».
                                    if let Load::Failed(e) = res {
                                        app.action = Some(Load::Failed(e));
                                    }
                                    app.load_workflows(cx);
                                }, cx);
                            });
                            window.close_sheet(cx);
                        })),
                )
                .child(ui::section(t!("Import JSON"), cx))
                .child(ui::muted(t!("Paste an exported workflow (from this or another project)."), cx))
                .child(Input::new(&import).small())
                .child(h_flex().child(div().flex_1()).child(Button::new("wf-import").small().outline().icon(IconName::ArrowDown).label(t!("Import")).on_click(move |_, window, cx| {
                    let raw = imp.read(cx).value().to_string();
                    m.update(cx, |app, cx| {
                        let project = app.project.clone();
                        app.post("/api/workflows/import".into(), json!({ "project": project, "json": raw }), |app, res, cx| match res {
                            Load::Ready(w) => {
                                app.wf_selected = w["id"].as_str().map(str::to_string);
                                app.wf_draft = Some(w);
                                app.wf_dirty = false;
                                app.load_workflows(cx);
                            }
                            Load::Failed(e) => app.action = Some(Load::Failed(e)),
                            _ => {}
                        }, cx);
                    });
                    window.close_sheet(cx);
                }))),
        )
    });
}

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let t = Tokens::current(cx);
    // El JSON exportado llega por la lectura; se copia aquí, donde hay contexto de ventana.
    if let Some(json) = app.wf_clipboard.take() {
        cx.write_to_clipboard(ClipboardItem::new_string(json));
        app.notify("wf-export", "info", t!("Workflow copied"), t!("The workflow JSON is on the clipboard."), window, cx);
    }
    let data = match &app.wf_data {
        None => {
            app.load_workflows(cx);
            return super::empty(t!("Reading workflows…").to_string(), cx).into_any_element();
        }
        Some(Load::Loading) => return super::empty(t!("Reading workflows…").to_string(), cx).into_any_element(),
        Some(Load::Failed(e)) => return super::empty(e.clone(), cx).into_any_element(),
        Some(Load::Ready(v)) => v.clone(),
    };
    let list: Vec<Value> = data["workflows"].as_array().cloned().unwrap_or_default();
    let entry = app.wf_selected.as_ref().and_then(|id| list.iter().find(|w| w["workflow"]["id"] == id.as_str()).cloned());
    let problems: Vec<String> = entry.as_ref().and_then(|e| e["problems"].as_array().cloned()).unwrap_or_default().iter().filter_map(|p| p.as_str().map(str::to_string)).collect();
    let commit = app.inputs.entry("wf-commit".into()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).default_value("dev"))).clone();

    // Lista de flujos.
    let mut side = v_flex().w(px(240.)).flex_none().gap_1().p_2().border_r_1().border_color(t.card_border).child(
        h_flex().items_center().child(div().flex_1().child(ui::section(t!("Workflows"), cx))).child(Button::new("wf-new").xsmall().ghost().icon(IconName::Plus).label(t!("New")).on_click(cx.listener(|this, _, window, cx| open_new_sheet(this, window, cx)))),
    );
    for e in &list {
        let w = &e["workflow"];
        let id = w["id"].as_str().unwrap_or_default().to_string();
        let selected = app.wf_selected.as_deref() == Some(id.as_str());
        let last = e["lastRun"]["status"].as_str().map(|s| status_label(s).to_string()).unwrap_or_else(|| t!("no runs").into());
        let bad = e["problems"].as_array().is_some_and(|p| !p.is_empty());
        let off = w["enabled"] == false;
        side = side.child(
            v_flex()
                .id(SharedString::from(format!("wf-item-{id}")))
                .px_2()
                .py_1p5()
                .cursor_pointer()
                .when(selected, |d| d.bg(t.active))
                .hover(move |s| s.bg(t.hover))
                .child(div().text_color(t.fg).truncate().child(w["name"].as_str().unwrap_or_default().to_string()))
                .child(ui::muted(
                    if off {
                        t!("Off · {last}", last = last)
                    } else if bad {
                        t!("{last} · has problems", last = last)
                    } else {
                        last
                    },
                    cx,
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.wf_dirty && this.wf_selected.as_deref() != Some(id.as_str()) {
                        this.action = Some(Load::Failed(t!("The open workflow has unsaved changes: save or discard them").into()));
                        cx.notify();
                        return;
                    }
                    this.wf_selected = Some(id.clone());
                    this.wf_draft = None;
                    this.wf_run_sel = None;
                    this.wf_node_sel = None;
                    this.load_workflows(cx);
                    cx.notify();
                })),
        );
    }
    if list.is_empty() {
        side = side.child(ui::muted(t!("No workflows yet. “New” starts from a template or imports one."), cx));
    }

    let Some(draft) = app.wf_draft.clone() else {
        return h_flex().size_full().items_stretch().child(side).child(super::empty(t!("Choose or create a workflow.").to_string(), cx)).into_any_element();
    };
    let wid = draft["id"].as_str().unwrap_or_default().to_string();
    let run_detail = app.wf_run_sel.as_ref().and_then(|rid| app.wf_runs.iter().find(|r| r["id"] == rid.as_str()).cloned());
    let active_run = app.wf_runs.iter().find(|r| matches!(r["status"].as_str(), Some("running" | "waiting-approval" | "waiting-answer"))).cloned();
    let dirty = app.wf_dirty;
    let enabled = draft["enabled"] != false;

    // Barra: paleta, guardar, exportar, borrar y correr.
    let palette = |id: &'static str, kind: &'static str, label: &'static str, cx: &mut Context<DevtoolApp>| {
        Button::new(id).xsmall().ghost().icon(IconName::Plus).label(label).disabled(app.wf_run_sel.is_some()).on_click(cx.listener(move |this, _, _, cx| {
            this.wf_add_node(kind);
            cx.notify();
        }))
    };
    let (w_export, w_delete, w_run) = (wid.clone(), wid.clone(), wid.clone());
    let toolbar = h_flex()
        .gap_1()
        .items_center()
        .flex_wrap()
        .child(div().text_color(t.fg).mr_2().child(draft["name"].as_str().unwrap_or_default().to_string()))

        .child(palette("wf-add-recipe", "recipe", t!("Recipe"), cx))
        .child(palette("wf-add-wait", "wait", t!("Wait"), cx))
        .child(palette("wf-add-approval", "approval", t!("Approval"), cx))
        .child(palette("wf-add-check", "check", t!("Check"), cx))
        .child(palette("wf-add-notify", "notify", t!("Notice"), cx))
        .child(palette("wf-add-agent", "agent", t!("Agent"), cx))
        .child(palette("wf-add-ask", "ask", t!("Ask me"), cx))
        .child(div().flex_1())
        .when(dirty, |d| {
            d.child(Button::new("wf-discard").xsmall().ghost().label(t!("Discard")).on_click(cx.listener(|this, _, _, cx| {
                this.wf_dirty = false;
                this.wf_draft = None;
                this.load_workflows(cx);
            })))
        })
        .child(ui::brand_if(dirty, Button::new("wf-save").xsmall().icon(IconName::Check).label(if dirty { t!("Save") } else { t!("Saved") }).disabled(!dirty).on_click(cx.listener(|this, _, _, cx| this.wf_save(cx))), cx))
        .child(Button::new("wf-export").xsmall().ghost().icon(IconName::Copy).label(t!("Export")).tooltip(t!("Copies the workflow as JSON to the clipboard")).on_click(cx.listener(move |this, _, window, cx| {
            let id = w_export.clone();
            this.get(format!("/api/workflows/{id}/export"), |app, res| {
                if let Load::Ready(v) = res {
                    app.wf_clipboard = Some(serde_json::to_string_pretty(&v).unwrap_or_default());
                }
            }, cx);
            let _ = window;
        })))
        .child(Button::new("wf-delete").xsmall().ghost().icon(IconName::Delete).tooltip(t!("Delete workflow")).on_click(cx.listener(move |_, _, window, cx| {
            let id = w_delete.clone();
            let ent = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |alert, _, _| {
                let (ent, id) = (ent.clone(), id.clone());
                alert.title(t!("Delete this workflow?")).description(t!("Its past runs stay in the history.")).ok_text(t!("Delete")).cancel_text(t!("Cancel")).show_cancel(true).on_ok(move |_, _, cx| {
                    let id = id.clone();
                    let _ = ent.update(cx, |app, cx| {
                        app.delete(format!("/api/workflows/{id}"), |app, res, cx| {
                            if let Load::Failed(e) = res {
                                app.action = Some(Load::Failed(e));
                            }
                            app.wf_selected = None;
                            app.wf_draft = None;
                            app.wf_dirty = false;
                            app.load_workflows(cx);
                        }, cx);
                    });
                    true
                })
            });
        })))
        // Al final de la barra: encender o apagar sin borrarlo (apagado no se puede correr,
        // tampoco por el MCP).
        .child(div().w(px(8.)))
        .child(ui::muted(if enabled { t!("On") } else { t!("Off") }, cx).text_xs())
        .child(
            ui::toggle("wf-enabled", enabled, cx)
                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t!("A workflow that's off is kept, with its runs, but can't be run (not even by an agent through the MCP).")).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let was_dirty = this.wf_dirty;
                    let on = !enabled;
                    this.wf_mutate(|d| d["enabled"] = json!(on));
                    // Sin otros cambios pendientes, se guarda al momento.
                    if !was_dirty {
                        this.wf_save(cx);
                    }
                    cx.notify();
                })),
        );

    let can_run = problems.is_empty() && !dirty && active_run.is_none() && enabled;
    let run_bar = h_flex()
        .gap_2()
        .items_center()
        .when(!enabled, |h| h.child(ui::muted(t!("It's off: turn it on to run it."), cx)))
        .child(ui::muted(t!("Commit or branch"), cx))
        .child(div().w(px(160.)).child(Input::new(&commit).small()))
        .child(ui::brand(Button::new("wf-run").small().icon(IconName::Play).label(t!("Run")).disabled(!can_run).on_click(cx.listener(move |this, _, _, cx| {
            let id = w_run.clone();
            let c = this.inputs.get("wf-commit").map(|s| s.read(cx).value().to_string()).unwrap_or_else(|| "dev".into());
            this.post(format!("/api/workflows/{id}/run"), json!({ "commit": c }), |app, res, cx| match res {
                Load::Ready(run) => {
                    app.wf_run_sel = run["id"].as_str().map(str::to_string);
                    app.load_workflows(cx);
                }
                Load::Failed(e) => app.action = Some(Load::Failed(e)),
                _ => {}
            }, cx);
        })), cx))
        .child(ui::muted(
            if dirty {
                t!("Save the changes to run it.").to_string()
            } else if active_run.is_some() {
                t!("A run is already in progress.").to_string()
            } else if !problems.is_empty() {
                t!("Can't run: {a}", a = problems.join(" · "))
            } else {
                t!("Ready to run.").to_string()
            },
            cx,
        ));

    // El lienzo: el borrador o, con una corrida elegida, la foto del flujo al correrla.
    let shown = run_detail.as_ref().map(|r| r["snapshot"].clone()).unwrap_or_else(|| draft.clone());
    let canvas_box = ui::contained().relative().h(px(380.)).border_1().border_color(t.card_border).child(canvas_el(app, shown, run_detail.clone(), cx)).child(
        div().absolute().top_2().left_2().child(ui::muted(
            if app.wf_run_sel.is_some() { t!("Viewing a run: the workflow as it was when it ran (read-only).") } else { t!("Drag nodes · pull the right-hand dot onto another node to connect them · click a node to configure it · two fingers: pan · pinch: zoom") },
            cx,
        )),
    );

    // Corridas.
    let mut runs = v_flex().gap_1().child(h_flex().items_center().child(div().flex_1().child(ui::section(t!("Runs"), cx))).when(app.wf_run_sel.is_some(), |d| {
        d.child(Button::new("wf-back-edit").xsmall().ghost().label(t!("Back to editor")).on_click(cx.listener(|this, _, _, cx| {
            this.wf_run_sel = None;
            cx.notify();
        })))
    }));
    // Si la lista dice que tiene corridas y todavía no llegaron, se están leyendo.
    let has_runs = list.iter().any(|e| e["workflow"]["id"].as_str() == app.wf_selected.as_deref() && !e["lastRun"].is_null());
    if app.wf_runs.is_empty() {
        runs = runs.child(ui::muted(if has_runs { t!("Reading runs…") } else { t!("Hasn't run yet.") }, cx));
    }
    for r in app.wf_runs.clone() {
        let rid = r["id"].as_str().unwrap_or_default().to_string();
        let st = r["status"].as_str().unwrap_or_default().to_string();
        let selected = app.wf_run_sel.as_deref() == Some(rid.as_str());
        let when = r["createdAt"].as_i64().map(ui::ago).unwrap_or_default();
        let (r_cancel, r_retry, r_sel) = (rid.clone(), rid.clone(), rid.clone());
        let mut line = h_flex()
            .id(SharedString::from(format!("wf-run-{rid}")))
            .gap_2()
            .px_2()
            .h(px(30.))
            .items_center()
            .cursor_pointer()
            .when(selected, |d| d.bg(t.active))
            .hover(move |s| s.bg(t.hover))
            .child(ui::dot(match st.as_str() { "succeeded" => ui::Health::Ok, "failed" => ui::Health::Bad, "cancelled" => ui::Health::Unknown, _ => ui::Health::Warn }, cx))
            .child(div().w(px(140.)).child(status_label(&st).to_string()))
            .child(div().w(px(120.)).child(ui::mono(ui::txt(&r["commit"]))))
            .child(div().flex_1().min_w_0().truncate().child(ui::muted(format!("{when} · {}{}", ui::txt(&r["requestedBy"]), r["error"].as_str().map(|e| format!(" · {e}")).unwrap_or_default()), cx)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.wf_run_sel = Some(r_sel.clone());
                cx.notify();
            }));
        // Las corridas con agentes tienen su hilo (página Agentes).
        let has_agents = r["snapshot"]["nodes"].as_array().is_some_and(|l| l.iter().any(|n| matches!(n["kind"].as_str(), Some("agent" | "ask"))));
        if has_agents {
            let thread = rid.clone();
            line = line.child(Button::new(SharedString::from(format!("wf-thread-{rid}"))).xsmall().ghost().label(if st == "waiting-answer" { t!("Answer in its thread") } else { t!("Thread") }).on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.chat_open = Some(thread.clone());
                this.chat = None;
                this.open_page(Page::Agents, window, cx);
            })));
        }
        if matches!(st.as_str(), "running" | "waiting-approval" | "waiting-answer") {
            if st == "waiting-approval" {
                line = line.child(ui::brand(Button::new(SharedString::from(format!("wf-approve-{rid}"))).xsmall().icon(IconName::Check).label(t!("Go approve")).on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.go(Page::Approvals, cx);
                })), cx));
            }
            line = line.child(Button::new(SharedString::from(format!("wf-cancel-{rid}"))).xsmall().danger().label(t!("Cancel")).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.post(format!("/api/workflow-runs/{}/cancel", r_cancel), json!({}), |app, res, cx| {
                    if let Load::Failed(e) = res {
                        app.action = Some(Load::Failed(e));
                    }
                    app.load_workflows(cx);
                }, cx);
            })));
        }
        if matches!(st.as_str(), "failed" | "cancelled") {
            line = line.child(Button::new(SharedString::from(format!("wf-retry-{rid}"))).xsmall().outline().icon(IconName::RefreshCw).label(t!("Retry")).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.post(format!("/api/workflow-runs/{}/retry", r_retry), json!({}), |app, res, cx| {
                    if let Load::Failed(e) = res {
                        app.action = Some(Load::Failed(e));
                    }
                    app.load_workflows(cx);
                }, cx);
            })));
        }
        runs = runs.child(line);
    }
    // Detalle de la corrida elegida: cada nodo con su estado, su mensaje y su trabajo.
    if let Some(r) = &run_detail {
        let mut detail = v_flex().gap_0p5().p_2().border_1().border_color(t.card_border);
        for n in r["nodes"].as_array().cloned().unwrap_or_default() {
            let title = r["snapshot"]["nodes"].as_array().and_then(|l| l.iter().find(|x| x["id"] == n["id"]).map(|x| x["title"].as_str().unwrap_or_default().to_string())).unwrap_or_default();
            let st = n["status"].as_str().unwrap_or_default().to_string();
            let is_agent = r["snapshot"]["nodes"].as_array().and_then(|l| l.iter().find(|x| x["id"] == n["id"])).is_some_and(|x| x["kind"] == "agent");
            let job = n["job"].as_str().map(str::to_string).filter(|_| !is_agent);
            let agent_run = n["job"].as_str().map(str::to_string).filter(|_| is_agent);
            detail = detail.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(10.)).h(px(10.)).rounded_full().bg(status_color(&st, cx)))
                    .child(div().w(px(220.)).truncate().text_color(t.fg).child(title))
                    .child(div().w(px(130.)).child(ui::muted(status_label(&st), cx)))
                    .child(div().flex_1().min_w_0().truncate().child(ui::muted(ui::txt(&n["message"]), cx)))
                    .when_some(job, |d, job| {
                        d.child(Button::new(SharedString::from(format!("wf-job-{job}"))).xsmall().ghost().label(t!("View job")).on_click(cx.listener(move |this, _, _, cx| {
                            this.go(Page::Jobs, cx);
                            this.open_job(job.clone(), cx);
                        })))
                    })
                    .when_some(agent_run, |d, ar| {
                        d.child(Button::new(SharedString::from(format!("wf-agent-{ar}"))).xsmall().ghost().label(t!("Conversation")).on_click(cx.listener(move |this, _, window, cx| {
                            this.chat_open = Some(ar.clone());
                            this.chat = None;
                            this.open_page(Page::Agents, window, cx);
                        })))
                    }),
            );
            // El resultado de un agente, campo por campo.
            if is_agent && st == "succeeded" {
                let mut chips = h_flex().gap_1().flex_wrap().pl(px(18.));
                let mut fields: Vec<(String, String)> = n["outputs"].as_object().map(|o| o.iter().filter(|(k, _)| !matches!(k.as_str(), "report" | "result" | "_retried")).map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default();
                fields.sort();
                for (k, v) in fields {
                    chips = chips.child(div().px_1p5().py_0p5().bg(t.ground).border_1().border_color(t.card_border).text_size(px(11.)).text_color(t.muted).child(format!("{k}: {}", v.chars().take(90).collect::<String>())));
                }
                detail = detail.child(chips);
            }
        }
        runs = runs.child(detail);
    }

    // Qué eventos lo empiezan solo (events.rs del daemon). Cambiarlos es como editar el flujo:
    // hay que guardar.
    let triggers: Vec<Value> = draft["triggers"].as_array().cloned().unwrap_or_default();
    let mut trigger_row = h_flex().gap_1().items_center().flex_wrap().child(ui::muted(t!("Starts by itself on:"), cx));
    for (source, label) in [("convex-error", t!("Convex errors")), ("frontend-error", t!("Frontend errors")), ("user-report", t!("User reports")), ("mcp", t!("Agents (MCP)")), ("incident", t!("Incidents"))] {
        let on = triggers.iter().any(|t| t["source"] == source);
        let b = Button::new(SharedString::from(format!("wf-trig-{source}"))).xsmall().label(label).on_click(cx.listener(move |this, _, _, cx| {
            this.wf_mutate(|d| {
                let mut list: Vec<Value> = d["triggers"].as_array().cloned().unwrap_or_default();
                if on {
                    list.retain(|t| t["source"] != source);
                } else {
                    // El mismo filtro de texto que las demás, si lo hay.
                    let m = list.first().map(|t| t["match"].clone()).unwrap_or(Value::Null);
                    list.push(json!({ "source": source, "match": m }));
                }
                d["triggers"] = json!(list);
            });
            cx.notify();
        }));
        trigger_row = trigger_row.child(ui::brand_if(on, b, cx));
    }
    if !triggers.is_empty() {
        let current = triggers.first().and_then(|t| t["match"].as_str()).unwrap_or_default().to_string();
        let key = format!("wf-trig-match-{}", draft["id"].as_str().unwrap_or("new"));
        let st = app.inputs.entry(key.clone()).or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(t!("only if it contains… (optional)")).default_value(current))).clone();
        trigger_row = trigger_row.child(div().w(px(200.)).child(Input::new(&st).small())).child(Button::new("wf-trig-apply").xsmall().ghost().label(t!("Apply")).on_click(cx.listener(move |this, _, _, cx| {
            let m = this.inputs.get(&key).map(|s| s.read(cx).value().trim().to_string()).unwrap_or_default();
            this.wf_mutate(|d| {
                if let Some(list) = d["triggers"].as_array_mut() {
                    for t in list {
                        t["match"] = if m.is_empty() { Value::Null } else { json!(m) };
                    }
                }
            });
            cx.notify();
        })));
    } else {
        trigger_row = trigger_row.child(ui::muted(t!("nothing: you run it by hand."), cx).text_xs());
    }

    let main = v_flex().flex_1().min_w_0().gap_3().p_3().child(toolbar).child(run_bar).child(trigger_row).child(canvas_box).child(runs);
    h_flex().size_full().items_stretch().child(side).child(main).into_any_element()
}




/// La pestaña Flujos de un entorno: los flujos del proyecto que lo tocan (algún nodo con ese
/// entorno), con su última corrida y para ejecutarlos ahí mismo; debajo, lo que espera
/// aprobación en él. Reemplaza a la promoción fija de heskala-erp.
pub fn render_for_env(app: &mut DevtoolApp, env: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let list: Vec<Value> = match &app.wf_data {
        Some(Load::Ready(v)) => v["workflows"].as_array().cloned().unwrap_or_default(),
        Some(Load::Failed(e)) => return v_flex().p_4().child(ui::muted(e.to_string(), cx)).into_any_element(),
        _ => return v_flex().p_4().child(ui::muted(t!("Reading workflows…"), cx)).into_any_element(),
    };
    let touches = |e: &Value| e["workflow"]["nodes"].as_array().is_some_and(|ns| ns.iter().any(|n| n["params"]["env"] == env));
    let mine: Vec<Value> = list.into_iter().filter(touches).collect();
    let commit = app
        .inputs
        .entry("wf-env-commit".into())
        .or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder("dev").default_value("dev")))
        .clone();

    let mut col = v_flex().gap_3();
    if mine.is_empty() {
        col = col.child(
            ui::card(cx)
                .child(ui::muted(t!("No workflow deploys to this environment yet. A workflow chains the steps (checks, backups, deploys, waits and approvals) and runs them in order."), cx))
                .child(h_flex().child(ui::brand(Button::new("wf-env-new").small().icon(IconName::Plus).label(t!("New workflow")).on_click(cx.listener(|this, _, window, cx| {
                    this.open_page(Page::Workflows, window, cx);
                    open_new_sheet(this, window, cx);
                })), cx))),
        );
    } else {
        col = col.child(h_flex().gap_2().items_center().child(ui::muted(t!("Commit or branch"), cx)).child(div().w(px(160.)).child(Input::new(&commit).small())));
    }
    for e in &mine {
        col = col.child(env_card(e, cx));
    }
    col.into_any_element()
}

fn env_card(e: &Value, cx: &mut Context<DevtoolApp>) -> Div {
    let t = Tokens::current(cx);
    let w = &e["workflow"];
    let id = w["id"].as_str().unwrap_or_default().to_string();
    let name = ui::txt(&w["name"]);
    let problems: Vec<String> = e["problems"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(str::to_string)).collect();
    let last = &e["lastRun"];
    let active = matches!(last["status"].as_str(), Some("running" | "waiting-approval" | "waiting-answer"));

    // Los pasos en orden, cada uno con el estado que tuvo en la última corrida.
    let titles: std::collections::HashMap<&str, String> = w["nodes"].as_array().into_iter().flatten().filter_map(|n| Some((n["id"].as_str()?, ui::txt(&n["title"]).to_string()))).collect();
    let mut steps = h_flex().gap_1().flex_wrap().items_center();
    let order: Vec<&Value> = last["nodes"].as_array().map(|ns| ns.iter().collect()).unwrap_or_default();
    if order.is_empty() {
        for (i, n) in w["nodes"].as_array().into_iter().flatten().enumerate() {
            if i > 0 {
                steps = steps.child(ui::muted("→", cx));
            }
            steps = steps.child(div().text_xs().text_color(t.muted).child(ui::txt(&n["title"])));
        }
    } else {
        for (i, n) in order.iter().enumerate() {
            if i > 0 {
                steps = steps.child(ui::muted("→", cx));
            }
            let s = n["status"].as_str().unwrap_or("pending");
            let title = titles.get(n["id"].as_str().unwrap_or_default()).cloned().unwrap_or_default();
            steps = steps.child(h_flex().gap_1().items_center().child(div().size(px(7.)).rounded_full().bg(status_color(s, cx))).child(div().text_xs().text_color(t.fg).child(title)));
        }
    }

    let summary = match last["status"].as_str() {
        Some(s) => format!("{} · {} · {}", status_label(s), ui::txt(&last["commit"]), last["createdAt"].as_i64().map(ui::ago).unwrap_or_default()),
        None => t!("Hasn't run yet.").to_string(),
    };
    let (open_id, run_id) = (id.clone(), id.clone());
    let can_run = problems.is_empty() && !active;
    ui::card(cx)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(v_flex().flex_1().min_w_0().child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.fg).child(name)).child(ui::muted(summary, cx)))
                .child(Button::new(SharedString::from(format!("wf-env-open-{id}"))).xsmall().ghost().icon(IconName::ExternalLink).label(t!("Open")).on_click(cx.listener(move |this, _, window, cx| {
                    this.wf_selected = Some(open_id.clone());
                    this.wf_draft = None;
                    this.wf_run_sel = None;
                    this.open_page(Page::Workflows, window, cx);
                })))
                .child(ui::brand(
                    Button::new(SharedString::from(format!("wf-env-run-{id}"))).xsmall().icon(IconName::Play).label(t!("Run")).disabled(!can_run).on_click(cx.listener(move |this, _, _, cx| {
                        let id = run_id.clone();
                        let c = this.inputs.get("wf-env-commit").map(|s| s.read(cx).value().to_string()).filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "dev".into());
                        this.post(format!("/api/workflows/{id}/run"), json!({ "commit": c }), |app, res, cx| match res {
                            Load::Ready(_) => app.load_workflows(cx),
                            Load::Failed(e) => app.action = Some(Load::Failed(e)),
                            _ => {}
                        }, cx);
                    })),
                    cx,
                )),
        )
        .child(steps)
        .children((!problems.is_empty()).then(|| ui::muted(t!("Can't run: {a}", a = problems.join(" · ")), cx)))
        .children(last["error"].as_str().map(|err| ui::muted(err.to_string(), cx)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn agarra_nodo_y_punto_de_salida() {
        let nodes = vec![json!({ "id": "a", "x": 0., "y": 0. }), json!({ "id": "b", "x": 300., "y": 0. })];
        assert_eq!(super::hit(&nodes, (10., 10.)).as_deref(), Some("a"));
        assert_eq!(super::hit(&nodes, (310., 50.)).as_deref(), Some("b"));
        assert_eq!(super::hit(&nodes, (250., 10.)), None);
        // El punto de salida de «a» está en su borde derecho, a media altura.
        let port = (super::NODE_W, super::NODE_H / 2.);
        assert_eq!(super::hit_port(&nodes, port, 1.).as_deref(), Some("a"));
        assert_eq!(super::hit_port(&nodes, (port.0, port.1 + 30.), 1.), None);
    }
}
