// Diagrama de tablas del esquema, pintado a mano en un solo elemento: cajas
// con sus campos, marcos por carpeta y líneas por referencia. Reemplaza al
// NodeGraph de Ely, que solo dejaba agarrar una caja por su título (6 px a 20 %
// de zoom), no la movía mientras se arrastraba y rehacía cientos de divs por
// cuadro. Aquí se pinta solo lo que se ve, el texto se omite cuando no se
// leería, y el arrastre (mover el lienzo o una caja) se escucha en la ventana.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::*;

use crate::app::DevtoolApp;
use crate::theme::Tokens;

/// Vista del lienzo: la esquina de arriba a la izquierda en unidades del lienzo y el zoom.
/// (La de Ely no baja de 10 %, y el esquema entero no cabe ahí.)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Viewport {
    pub const ZOOMS: (f32, f32) = (0.03, 4.0);

    pub fn new(x: f32, y: f32, zoom: f32) -> Self {
        Self { x, y, zoom: zoom.clamp(Self::ZOOMS.0, Self::ZOOMS.1) }
    }

    pub fn to_view(self, (x, y): (f32, f32)) -> (f32, f32) {
        ((x - self.x) * self.zoom, (y - self.y) * self.zoom)
    }

    pub fn to_canvas(self, (x, y): (f32, f32)) -> (f32, f32) {
        (x / self.zoom + self.x, y / self.zoom + self.y)
    }

    /// Acercado por `factor` sobre un punto de la vista, que queda en su lugar.
    pub fn zoomed(self, factor: f32, about: (f32, f32)) -> Self {
        let zoom = (self.zoom * factor).clamp(Self::ZOOMS.0, Self::ZOOMS.1);
        let (x, y) = self.to_canvas(about);
        Self::new(x - about.0 / zoom, y - about.1 / zoom, zoom)
    }

    /// Movido para que el contenido se deslice una distancia de la vista.
    pub fn panned(self, (dx, dy): (f32, f32)) -> Self {
        Self::new(self.x - dx / self.zoom, self.y - dy / self.zoom, self.zoom)
    }
}

pub const BOX_W: f32 = 260.;
pub const ROW_H: f32 = 22.;
pub const HEAD_H: f32 = 34.;
/// Margen de un marco de carpeta y alto de su etiqueta.
pub const PAD: f32 = 28.;
pub const LABEL_H: f32 = 44.;

pub struct ErField {
    pub name: SharedString,
    pub ty: SharedString,
    /// 0: campo, 1: clave propia, 2: referencia.
    pub kind: u8,
}

pub struct ErBox {
    pub key: String,
    /// La tabla principal de su carpeta (la que encabeza la rama): su nombre se ve siempre.
    pub head: bool,
    pub x: f32,
    pub y: f32,
    pub fields: Vec<ErField>,
}

impl ErBox {
    pub fn h(&self) -> f32 {
        HEAD_H + ROW_H * self.fields.len().max(1) as f32
    }
}

pub struct ErEdge {
    pub from: usize,
    pub row: usize,
    pub to: usize,
    /// Toca a la tabla elegida: va en otro color y encima.
    pub hot: bool,
    /// Camino en ángulo recto que esquiva las cajas (ver `er_route.rs`), en unidades del lienzo.
    pub route: Option<Rc<Vec<(f32, f32)>>>,
}

/// Extremos de una línea: sale del campo `row` de la caja en `a` por el lado que mira a `b` y
/// entra por el título de `b`. Devuelve punto y sentido (1 derecha, -1 izquierda) de cada uno.
pub fn ends(a: (f32, f32), row: usize, b: (f32, f32), same: bool) -> ((f32, f32), f32, (f32, f32), f32) {
    let ay = a.1 + HEAD_H + ROW_H * row as f32 + ROW_H / 2.;
    let by = b.1 + HEAD_H / 2.;
    if same {
        return ((a.0 + BOX_W, ay), 1., (a.0 + BOX_W, by), 1.);
    }
    if b.0 >= a.0 + BOX_W {
        ((a.0 + BOX_W, ay), 1., (b.0, by), -1.)
    } else if b.0 + BOX_W <= a.0 {
        ((a.0, ay), -1., (b.0 + BOX_W, by), 1.)
    } else {
        // Una sobre otra: sale y entra por la derecha.
        ((a.0 + BOX_W, ay), 1., (b.0 + BOX_W, by), 1.)
    }
}

/// Polilínea en ángulo recto con las esquinas redondeadas, en píxeles de la vista.
fn polyline(pts: &[(f32, f32)], radius: f32, o: Point<Pixels>, width: f32) -> Option<Path<Pixels>> {
    if pts.len() < 2 {
        return None;
    }
    let at = |p: (f32, f32)| o + point(px(p.0), px(p.1));
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(at(pts[0]));
    for i in 1..pts.len() - 1 {
        let (a, b, c) = (pts[i - 1], pts[i], pts[i + 1]);
        let (d1, d2) = (((b.0 - a.0).abs() + (b.1 - a.1).abs()), ((c.0 - b.0).abs() + (c.1 - b.1).abs()));
        let r = radius.min(d1 / 2.).min(d2 / 2.);
        let toward = |from: (f32, f32), to: (f32, f32), d: f32| {
            let len = ((to.0 - from.0).abs() + (to.1 - from.1).abs()).max(0.001);
            (from.0 + (to.0 - from.0) / len * d, from.1 + (to.1 - from.1) / len * d)
        };
        path.line_to(at(toward(b, a, r)));
        path.curve_to(at(toward(b, c, r)), at(b));
    }
    path.line_to(at(pts[pts.len() - 1]));
    path.build().ok()
}

/// Minimapa: abajo a la derecha. Devuelve su rectángulo (en la vista), la esquina del contenido
/// y la escala, o nada si todo cabe en la vista.
/// El minimapa: su rectángulo en la vista, la esquina del contenido y la escala.
type Minimap = ((f32, f32, f32, f32), (f32, f32), f32);

fn minimap(scene: &Scene, view: Viewport, (vw, vh): (f32, f32)) -> Option<Minimap> {
    if scene.boxes.is_empty() || vw < 320. || vh < 220. {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for b in &scene.boxes {
        x0 = x0.min(b.x);
        y0 = y0.min(b.y);
        x1 = x1.max(b.x + BOX_W);
        y1 = y1.max(b.y + b.h());
    }
    let (cw, ch) = ((x1 - x0).max(1.), (y1 - y0).max(1.));
    // Si casi todo se ve, no hace falta (y taparía una esquina del diagrama).
    let (vx0, vy0) = view.to_canvas((0., 0.));
    let (vx1, vy1) = view.to_canvas((vw, vh));
    let seen = ((vx1.min(x1) - vx0.max(x0)).max(0.) * (vy1.min(y1) - vy0.max(y0)).max(0.)) / (cw * ch);
    if seen > 0.85 {
        return None;
    }
    let (mw, mh) = (200f32, 130f32);
    let scale = (mw / cw).min(mh / ch);
    let (w, h) = (cw * scale, ch * scale);
    Some(((vw - w - 12., vh - h - 12., w, h), (x0, y0), scale))
}

/// Marco de una carpeta: rodea a sus tablas y a los marcos de sus subcarpetas.
pub struct ErGroup {
    pub label: SharedString,
    pub depth: usize,
    /// Cajas propias.
    pub direct: Vec<usize>,
    /// Marcos de las subcarpetas (índices en `groups`).
    pub children: Vec<usize>,
}

#[derive(Default)]
pub struct Scene {
    pub boxes: Vec<ErBox>,
    pub edges: Vec<ErEdge>,
    pub groups: Vec<ErGroup>,
    pub index: HashMap<String, usize>,
    /// Ramas del árbol de carpetas: de la principal de una carpeta a cada una de las suyas.
    pub links: Vec<(usize, usize)>,
}

/// Lo que hace el puntero presionado.
#[derive(Clone)]
pub enum Grab {
    Pan { start: (f32, f32), view: Viewport },
    /// Arrastrando sobre el minimapa: la vista sigue al puntero.
    Mini,
    Move { key: String, start: (f32, f32), at: (f32, f32), now: (f32, f32), moved: bool, places_key: String },
}

/// Lo que el lienzo necesita saber de la vista de la app.
pub struct Ctx {
    pub view_key: String,
    pub places_key: String,
    pub env: String,
    /// En Foco, elegir otra tabla recentra el diagrama: se elige al soltar sin mover.
    pub select_on_release: bool,
    /// En Carpeta, la carpeta que se ve: se fija al elegir una tabla, para que no cambie con ella.
    pub folder: Option<String>,
}

impl Scene {
    fn at(&self, i: usize, grab: &Option<Grab>) -> (f32, f32) {
        let b = &self.boxes[i];
        match grab {
            Some(Grab::Move { key, now, .. }) if *key == b.key => *now,
            _ => (b.x, b.y),
        }
    }

    /// La caja bajo un punto del lienzo (la de más arriba).
    fn hit(&self, p: (f32, f32), grab: &Option<Grab>) -> Option<usize> {
        (0..self.boxes.len()).rev().find(|&i| {
            let (x, y) = self.at(i, grab);
            p.0 >= x && p.0 <= x + BOX_W && p.1 >= y && p.1 <= y + self.boxes[i].h()
        })
    }
}

fn local(p: Point<Pixels>, b: Bounds<Pixels>) -> (f32, f32) {
    (f32::from(p.x - b.origin.x), f32::from(p.y - b.origin.y))
}

/// Tamaño de letra a medio píxel: el caché de líneas de GPUI acierta más mientras se hace zoom.
fn q(size: f32) -> f32 {
    (size * 2.).round() / 2.
}

/// Texto recortado con «…» por ancho medio de letra: medir cada línea costaría más que pintarla.
fn clip(s: &str, size: f32, mono: bool, max_w: f32) -> SharedString {
    let per = size * if mono { 0.6 } else { 0.55 };
    let fit = (max_w / per).floor() as usize;
    if s.chars().count() > fit {
        let mut t: String = s.chars().take(fit.saturating_sub(1)).collect();
        t.push('…');
        t.into()
    } else {
        SharedString::from(s.to_string())
    }
}

fn shape(shown: SharedString, size: f32, color: Hsla, weight: FontWeight, mono: bool, window: &mut Window) -> ShapedLine {
    let mut f = font(if mono { crate::theme::MONO_FONT } else { crate::theme::UI_FONT });
    f.weight = weight;
    let run = TextRun { len: shown.len(), font: f, color, background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_line(shown, px(size), &[run], None)
}

#[allow(clippy::too_many_arguments)]
fn text(s: &str, size: f32, color: Hsla, weight: FontWeight, mono: bool, origin: (f32, f32), max_w: f32, window: &mut Window, cx: &mut App) {
    if max_w < size || s.is_empty() {
        return;
    }
    let line = shape(clip(s, size, mono, max_w), size, color, weight, mono, window);
    let _ = line.paint(point(px(origin.0), px(origin.1)), px(size * 1.3), TextAlign::Left, None, window, cx);
}

/// Por debajo de este tamaño las letras ya no se dibujan (saldrían en blanco): el texto va como
/// una raya de su largo y su color, igual que una foto reducida.
const GREEK_PX: f32 = 3.;

/// Texto dentro de una caja (título, campo o tipo), del tamaño que le toca con el zoom: centrado
/// de alto en su franja y, si `right`, alineado a la derecha de `x`. El largo se estima como el
/// recorte de `text`.
#[allow(clippy::too_many_arguments)]
fn box_text(s: &str, fs: f32, color: Hsla, weight: FontWeight, mono: bool, (x, row_y): (f32, f32), row_h: f32, max_w: f32, right: bool, window: &mut Window, cx: &mut App) {
    if s.is_empty() || max_w <= 0. {
        return;
    }
    let est = (s.chars().count() as f32 * fs * if mono { 0.6 } else { 0.55 }).min(max_w);
    let x = if right { x - est } else { x };
    if fs >= GREEK_PX {
        let fs = q(fs);
        text(s, fs, color, weight, mono, (x, row_y + (row_h - fs * 1.3) / 2.), max_w, window, cx);
    } else {
        let h = (fs * 0.6).max(0.5);
        let b = Bounds::new(point(px(x), px(row_y + (row_h - h) / 2.)), size(px(est.max(0.5)), px(h)));
        window.paint_quad(fill(b, color.opacity(0.7)));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(scene: &Scene, view: Viewport, selected: Option<usize>, grab: &Option<Grab>, bounds: Bounds<Pixels>, t: &Tokens, window: &mut Window, cx: &mut App) {
    let o = bounds.origin;
    let (vw, vh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let z = view.zoom;
    window.paint_quad(fill(bounds, t.panel));

    // Puntos de fondo: dan la sensación de movimiento. Se espacian al alejar.
    let mut step = 32. * z;
    while step < 18. {
        step *= 2.;
    }
    let (ox, oy) = view.to_view((0., 0.));
    let dot = t.card_border;
    let mut y = oy.rem_euclid(step);
    while y < vh {
        let mut x = ox.rem_euclid(step);
        while x < vw {
            window.paint_quad(fill(Bounds::new(o + point(px(x), px(y)), size(px(1.5), px(1.5))), dot));
            x += step;
        }
        y += step;
    }

    let to_view = |p: (f32, f32)| view.to_view(p);
    let visible = |x: f32, y: f32, w: f32, h: f32| x + w >= 0. && y + h >= 0. && x <= vw && y <= vh;

    // Marcos de carpeta: cada uno rodea sus cajas y los marcos de sus hijas (se calculan de
    // adentro hacia afuera y se pintan al revés). Siguen a una caja mientras se arrastra.
    let mut frames: Vec<Option<(f32, f32, f32, f32)>> = vec![None; scene.groups.len()];
    let mut order: Vec<usize> = (0..scene.groups.len()).collect();
    order.sort_by_key(|&g| std::cmp::Reverse(scene.groups[g].depth));
    for &gi in &order {
        let g = &scene.groups[gi];
        let mut f: Option<(f32, f32, f32, f32)> = None;
        let mut grow = |r: (f32, f32, f32, f32)| {
            f = Some(match f {
                None => r,
                Some(a) => (a.0.min(r.0), a.1.min(r.1), a.2.max(r.2), a.3.max(r.3)),
            });
        };
        for &i in &g.direct {
            let (x, y) = scene.at(i, grab);
            grow((x, y, x + BOX_W, y + scene.boxes[i].h()));
        }
        for &c in &g.children {
            if let Some(r) = frames.get(c).copied().flatten() {
                grow(r);
            }
        }
        frames[gi] = f.map(|(x0, y0, x1, y1)| (x0 - PAD, y0 - PAD - LABEL_H, x1 + PAD, y1 + PAD));
    }
    for &gi in order.iter().rev() {
        let Some((x0, y0, x1, y1)) = frames[gi] else { continue };
        let g = &scene.groups[gi];
        let (gx, gy) = to_view((x0, y0));
        let (gw, gh) = ((x1 - x0) * z, (y1 - y0) * z);
        if !visible(gx, gy, gw, gh) {
            continue;
        }
        let b = Bounds::new(o + point(px(gx), px(gy)), size(px(gw), px(gh)));
        window.paint_quad(quad(b, px(8. * z), t.ground.opacity(0.35), px(1.), t.card_border, BorderStyle::default()));
        // La etiqueta se encoge con el zoom como los títulos de las tablas (ver `box_text`).
        let (fx, fy) = (f32::from(o.x) + gx, f32::from(o.y) + gy);
        box_text(&g.label, 15. * z, t.muted, FontWeight::MEDIUM, false, (fx + 10. * z, fy), LABEL_H * z, gw - 20. * z, false, window, cx);
    }

    // Las líneas se adelgazan al alejar (con cientos juntas, 1 px fijo tapa todo), sin bajar de
    // medio píxel; las de la elegida siguen más gruesas y en otro color.
    let thin = (1. * z.sqrt()).clamp(0.5, 1.);
    let thick = (1.4 * z.sqrt()).clamp(0.9, 1.4);
    let faint = 0.5 + 0.05 * z.min(1.);

    // Ramas del árbol (punteadas): de la derecha de la principal, por un tronco a media columna,
    // a la izquierda de cada una de las suyas.
    for &(p, c) in &scene.links {
        let (a, b) = (scene.at(p, grab), scene.at(c, grab));
        if b.0 <= a.0 + BOX_W {
            continue;
        }
        let (ya, yb) = (a.1 + HEAD_H / 2., b.1 + HEAD_H / 2.);
        let mid = a.0 + BOX_W + (b.0 - a.0 - BOX_W) * 0.35;
        let pts: Vec<(f32, f32)> = [(a.0 + BOX_W, ya), (mid, ya), (mid, yb), (b.0, yb)].iter().map(|p| to_view(*p)).collect();
        let (miny, maxy) = (pts[1].1.min(pts[2].1), pts[1].1.max(pts[2].1));
        if !visible(pts[0].0, miny, pts[3].0 - pts[0].0, maxy - miny + 1.) {
            continue;
        }
        let mut path = PathBuilder::stroke(px(thin)).dash_array(&[px(4.), px(3.)]);
        path.move_to(o + point(px(pts[0].0), px(pts[0].1)));
        for q in &pts[1..] {
            path.line_to(o + point(px(q.0), px(q.1)));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, t.muted.opacity(0.7));
        }
    }

    // Líneas: primero las comunes, tenues; encima las de la elegida. Cada una sigue su ruta en
    // ángulo recto; la de una caja que se arrastra va en Z simple hasta soltarla.
    let moving = match grab {
        Some(Grab::Move { key, .. }) => scene.index.get(key).copied(),
        _ => None,
    };
    for hot in [false, true] {
        for e in scene.edges.iter().filter(|e| e.hot == hot) {
            let live;
            let pts: &[(f32, f32)] = match (&e.route, moving) {
                (Some(r), m) if m != Some(e.from) && m != Some(e.to) => r,
                _ => {
                    let (a, da, b, db) = ends(scene.at(e.from, grab), e.row, scene.at(e.to, grab), e.from == e.to);
                    live = super::er_route::simple(a, da, b, db);
                    &live
                }
            };
            let view_pts: Vec<(f32, f32)> = pts.iter().map(|p| to_view(*p)).collect();
            let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for p in &view_pts {
                minx = minx.min(p.0);
                miny = miny.min(p.1);
                maxx = maxx.max(p.0);
                maxy = maxy.max(p.1);
            }
            if !visible(minx - 4., miny - 4., maxx - minx + 8., maxy - miny + 8.) {
                continue;
            }
            let (width, color) = if hot { (thick, t.accent_ink) } else { (thin, t.subtle.opacity(faint)) };
            if let Some(path) = polyline(&view_pts, 10. * z, o, width) {
                window.paint_path(path, color);
            }
            // Punta: un punto donde entra.
            if hot || z > 0.35 {
                let end = view_pts[view_pts.len() - 1];
                let d = (2.5 * z.sqrt()).clamp(1., 3.);
                window.paint_quad(quad(Bounds::new(o + point(px(end.0 - d), px(end.1 - d)), size(px(d * 2.), px(d * 2.))), px(d), color, px(0.), color, BorderStyle::default()));
            }
        }
    }

    // Cajas. El título y los campos se encogen con la caja como en una foto y no desaparecen
    // nunca (ver `box_text`): todas las tablas guardan la misma proporción entre sí.
    let row_px = ROW_H * z;
    let fs = 11.5 * z;
    for (i, b) in scene.boxes.iter().enumerate() {
        let (x, y) = to_view(scene.at(i, grab));
        let (w, h) = (BOX_W * z, b.h() * z);
        if !visible(x, y, w, h) {
            continue;
        }
        let chosen = Some(i) == selected;
        let r = Bounds::new(o + point(px(x), px(y)), size(px(w), px(h)));
        window.paint_quad(quad(r, px(6. * z), t.card, px(if chosen { 2. } else { 1. }), if chosen { t.accent_ink } else { t.card_border }, BorderStyle::default()));
        let head = Bounds::new(r.origin + point(px(1.), px(1.)), size(px((w - 2.).max(0.)), px((HEAD_H * z - 1.).max(0.))));
        let head_bg = if chosen { t.active } else if b.head { t.accent_ink.opacity(0.14) } else { t.hover };
        window.paint_quad(quad(head, Corners { top_left: px(5. * z), top_right: px(5. * z), bottom_left: px(0.), bottom_right: px(0.) }, head_bg, px(0.), t.card_border, BorderStyle::default()));
        let (ax, ay) = (f32::from(o.x) + x, f32::from(o.y) + y);
        let title = if chosen { t.accent_ink } else { t.fg };
        box_text(&b.key, 13. * z, title, FontWeight::SEMIBOLD, false, (ax + 10. * z, ay), HEAD_H * z, w - 20. * z, false, window, cx);
        let (top, bottom) = (f32::from(o.y), f32::from(o.y) + vh);
        for (r, f) in b.fields.iter().enumerate() {
            let ry = ay + HEAD_H * z + row_px * r as f32;
            if ry > bottom || ry + row_px < top {
                continue;
            }
            let color = match f.kind {
                1 => t.accent_ink,
                2 => t.fg,
                _ => t.fg.opacity(0.85),
            };
            box_text(&f.name, fs, color, FontWeight::NORMAL, true, (ax + 10. * z, ry), row_px, w * 0.55, false, window, cx);
            let ty_color = if f.kind == 2 { t.accent_ink.opacity(0.85) } else { t.muted };
            box_text(&f.ty, fs, ty_color, FontWeight::NORMAL, true, (ax + w - 10. * z, ry), row_px, w * 0.4, true, window, cx);
        }
    }
}

/// Minimapa encima de todo: las cajas en pequeño y el rectángulo de lo que se ve.
fn paint_minimap(scene: &Scene, view: Viewport, selected: Option<usize>, bounds: Bounds<Pixels>, t: &Tokens, window: &mut Window) {
    let (vw, vh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let Some(((mx, my, mw, mh), (cx0, cy0), k)) = minimap(scene, view, (vw, vh)) else { return };
    let o = bounds.origin;
    let r = Bounds::new(o + point(px(mx - 6.), px(my - 6.)), size(px(mw + 12.), px(mh + 12.)));
    window.paint_quad(quad(r, px(6.), t.ground.opacity(0.92), px(1.), t.card_border, BorderStyle::default()));
    for (i, b) in scene.boxes.iter().enumerate() {
        let (x, y) = (mx + (b.x - cx0) * k, my + (b.y - cy0) * k);
        let c = if Some(i) == selected { t.accent_ink } else { t.subtle.opacity(0.6) };
        window.paint_quad(fill(Bounds::new(o + point(px(x), px(y)), size(px((BOX_W * k).max(1.)), px((b.h() * k).max(1.)))), c));
    }
    let (a, b) = (view.to_canvas((0., 0.)), view.to_canvas((vw, vh)));
    let (x0, y0) = ((mx + (a.0 - cx0) * k).max(mx), (my + (a.1 - cy0) * k).max(my));
    let (x1, y1) = ((mx + (b.0 - cx0) * k).min(mx + mw), (my + (b.1 - cy0) * k).min(my + mh));
    if x1 > x0 && y1 > y0 {
        let v = Bounds::new(o + point(px(x0), px(y0)), size(px(x1 - x0), px(y1 - y0)));
        window.paint_quad(quad(v, px(2.), t.accent_ink.opacity(0.12), px(1.), t.accent_ink, BorderStyle::default()));
    }
}

/// Vista centrada en el punto del minimapa bajo `p` (en la vista).
fn minimap_jump(scene: &Scene, view: Viewport, size: (f32, f32), p: (f32, f32)) -> Option<Viewport> {
    let ((mx, my, mw, mh), (cx0, cy0), k) = minimap(scene, view, size)?;
    if p.0 < mx - 6. || p.1 < my - 6. || p.0 > mx + mw + 6. || p.1 > my + mh + 6. {
        return None;
    }
    let c = (cx0 + (p.0 - mx).clamp(0., mw) / k, cy0 + (p.1 - my).clamp(0., mh) / k);
    Some(Viewport::new(c.0 - size.0 / 2. / view.zoom, c.1 - size.1 / 2. / view.zoom, view.zoom))
}

/// El lienzo: pinta la escena y atiende rueda, pellizco y arrastre.
pub fn element(app: &DevtoolApp, scene: Rc<Scene>, view: Viewport, selected: &str, ctx: Rc<Ctx>, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let t = Tokens::current(cx);
    let bounds = app.schema_bounds.clone();
    let grab = app.schema_grab.clone();
    let sel = scene.index.get(selected).copied();
    let me = cx.entity();
    let (s_paint, s_down, c_down, c_wheel, c_pinch) = (scene.clone(), scene.clone(), ctx.clone(), ctx.clone(), ctx.clone());
    let (b_down, b_wheel, b_pinch) = (bounds.clone(), bounds.clone(), bounds.clone());
    let surface = canvas(
        move |b, _, _| bounds.set(b),
        move |b, _, window, cx| {
            window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                paint(&s_paint, view, sel, &grab, b, &t, window, cx);
                paint_minimap(&s_paint, view, sel, b, &t, window);
            });
            if grab.is_none() {
                return;
            }
            // Mientras hay algo agarrado, el movimiento se escucha en toda la ventana.
            let (mv, up) = (me.clone(), me.clone());
            let (s_up, c_up) = (s_paint.clone(), ctx.clone());
            let (c_mv, s_mv) = (ctx.clone(), s_paint.clone());
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                mv.update(cx, |app, cx| {
                    let p = local(ev.position, app.schema_bounds.get());
                    match app.schema_grab.as_mut() {
                        Some(Grab::Pan { start, view }) => {
                            let v = view.panned((p.0 - start.0, p.1 - start.1));
                            app.schema_views.insert(c_mv.view_key.clone(), (v.x, v.y, v.zoom));
                        }
                        Some(Grab::Mini) => {
                            let b = app.schema_bounds.get();
                            let size = (f32::from(b.size.width), f32::from(b.size.height));
                            let Some(v) = app.schema_views.get(&c_mv.view_key).map(|v| Viewport::new(v.0, v.1, v.2)) else { return };
                            let Some(next) = minimap_jump(&s_mv, v, size, p) else { return };
                            app.schema_views.insert(c_mv.view_key.clone(), (next.x, next.y, next.zoom));
                        }
                        Some(Grab::Move { start, at, now, moved, .. }) => {
                            let z = app.schema_views.get(&c_mv.view_key).map(|v| v.2).unwrap_or(1.);
                            let (dx, dy) = (p.0 - start.0, p.1 - start.1);
                            *moved |= dx.abs() + dy.abs() > 3.;
                            *now = (at.0 + dx / z, at.1 + dy / z);
                        }
                        None => return,
                    }
                    cx.notify();
                });
            });
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                up.update(cx, |app, cx| {
                    match app.schema_grab.take() {
                        Some(Grab::Pan { .. }) | Some(Grab::Mini) => {
                            if let Some(v) = app.schema_views.get(&c_up.view_key).copied() {
                                app.remember_view(&c_up.view_key, v);
                            }
                        }
                        Some(Grab::Move { key, now, moved, places_key, .. }) => {
                            if moved {
                                app.save_place(&places_key, &key, now);
                                app.schema_cache = None;
                            } else if c_up.select_on_release && s_up.index.contains_key(&key) {
                                app.schema_table.insert(c_up.env.clone(), key);
                            }
                        }
                        None => return,
                    }
                    cx.notify();
                });
            });
        },
    )
    .size_full();

    div()
        .id("er-canvas")
        .relative()
        .size_full()
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, cx.listener(move |app, ev: &MouseDownEvent, _, cx| {
            let p = local(ev.position, b_down.get());
            let Some(v) = app.schema_views.get(&c_down.view_key).map(|v| Viewport::new(v.0, v.1, v.2)) else { return };
            let b = b_down.get();
            let size = (f32::from(b.size.width), f32::from(b.size.height));
            if let Some(next) = minimap_jump(&s_down, v, size, p) {
                app.remember_view(&c_down.view_key, (next.x, next.y, next.zoom));
                app.schema_grab = Some(Grab::Mini);
                cx.notify();
                return;
            }
            let c = v.to_canvas(p);
            app.schema_grab = Some(match s_down.hit(c, &None) {
                Some(i) => {
                    let b = &s_down.boxes[i];
                    if !c_down.select_on_release {
                        if let Some(f) = &c_down.folder {
                            app.schema_folder.insert(c_down.env.clone(), f.clone());
                        }
                        app.schema_table.insert(c_down.env.clone(), b.key.clone());
                    }
                    Grab::Move { key: b.key.clone(), start: p, at: (b.x, b.y), now: (b.x, b.y), moved: false, places_key: c_down.places_key.clone() }
                }
                None => Grab::Pan { start: p, view: v },
            });
            cx.notify();
        }))
        .on_scroll_wheel(cx.listener(move |app, ev: &ScrollWheelEvent, window, cx| {
            cx.stop_propagation();
            let Some(v) = app.schema_views.get(&c_wheel.view_key).map(|v| Viewport::new(v.0, v.1, v.2)) else { return };
            let (dx, dy) = match ev.delta {
                ScrollDelta::Pixels(d) => (f32::from(d.x), f32::from(d.y)),
                ScrollDelta::Lines(l) => {
                    let line = f32::from(window.line_height());
                    (l.x * line, l.y * line)
                }
            };
            if dx == 0. && dy == 0. {
                return;
            }
            let next = if ev.modifiers.platform || ev.modifiers.control {
                v.zoomed((dy / 240.).exp(), local(ev.position, b_wheel.get()))
            } else {
                v.panned((dx, dy))
            };
            app.remember_view(&c_wheel.view_key, (next.x, next.y, next.zoom));
            cx.notify();
        }))
        .on_pinch(cx.listener(move |app, ev: &PinchEvent, _, cx| {
            cx.stop_propagation();
            let Some(v) = app.schema_views.get(&c_pinch.view_key).map(|v| Viewport::new(v.0, v.1, v.2)) else { return };
            let next = v.zoomed(1. + ev.delta, local(ev.position, b_pinch.get()));
            app.remember_view(&c_pinch.view_key, (next.x, next.y, next.zoom));
            cx.notify();
        }))
        .child(surface)
}

#[cfg(test)]
mod tests {
    use super::{ErBox, Grab, Scene};

    #[test]
    fn agarra_la_caja_de_arriba() {
        let mut s = Scene::default();
        s.boxes.push(ErBox { key: "a".into(), head: false, x: 0., y: 0., fields: vec![] });
        s.boxes.push(ErBox { key: "b".into(), head: false, x: 100., y: 10., fields: vec![] });
        assert_eq!(s.hit((150., 20.), &None), Some(1));
        assert_eq!(s.hit((10., 20.), &None), Some(0));
        assert_eq!(s.hit((10., 500.), &None), None);
        let g = Some(Grab::Move { key: "a".into(), start: (0., 0.), at: (0., 0.), now: (1000., 0.), moved: true, places_key: String::new() });
        assert_eq!(s.hit((10., 20.), &g), None);
    }
}
