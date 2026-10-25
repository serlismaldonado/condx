// Pestaña «Esquema» de un entorno: el árbol de carpetas de `convex/` con sus
// tablas (de dónde importa cada una el schema.ts), el diagrama de tablas con
// sus referencias y los índices de la tabla elegida.
//
// El diagrama (`er.rs`) pinta cada tabla como una caja con sus campos, cada
// referencia como una línea y cada carpeta como un marco con sus subcarpetas
// dentro, dispuestas a lo ancho. Las líneas de la tabla elegida van en otro
// color; un clic en cualquier parte de una caja la elige y arrastrarla la
// mueve. Tres modos: foco (la elegida, las que apunta y las que la apuntan),
// carpeta (la carpeta elegida con sus subcarpetas) y todas. El lienzo se mueve
// con dos dedos o arrastrando el fondo y se acerca pellizcando o con ⌘ + rueda.
// Alto ajustable, pantalla completa, espaciado; lo que se mueve, la vista y el
// alto se guardan en las preferencias.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use ely_core::forms::{SearchInput, Select};
use ely_core::lists::{Tree, TreeNode};
use ely_core::primitives::IconName as Ico;
use ely_core::tables::{Cell, Column, DataTable, Row};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;

use super::er::{self, ErBox, Viewport, ErEdge, ErField, ErGroup, Scene, BOX_W, HEAD_H, LABEL_H, PAD, ROW_H};
use crate::app::{DevtoolApp, Load};
use crate::ui;

/// Proporción buscada para cada carpeta y sus subcarpetas: más ancha que alta.
const ASPECT: f32 = 1.8;
/// Alto del diagrama: mínimo, por defecto y máximo.
const HEIGHT: (f32, f32, f32) = (280., 560., 1600.);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Focus,
    Folder,
    All,
}

impl Mode {
    fn key(self) -> &'static str {
        match self {
            Mode::Focus => "focus",
            Mode::Folder => "folder",
            Mode::All => "all",
        }
    }
}

/// Lo que se calcula del esquema para dibujarlo; se recalcula solo si cambia algo de lo que depende.
pub struct Derived {
    tree: Vec<TreeNode>,
    open: Vec<String>,
    tree_count: usize,
    scene: Rc<Scene>,
    places: HashMap<String, (f32, f32)>,
    shown: usize,
    blocks: usize,
}

fn box_h(table: &Value) -> f32 {
    HEAD_H + ROW_H * table["fields"].as_array().map(Vec::len).unwrap_or(1) as f32
}

/// Grupo por el nombre, para proyectos sin carpetas: lo anterior al primer `_`
/// (`ai_runs` → `ai`) o las minúsculas iniciales (`authAccounts` → `auth`).
fn prefix(name: &str) -> String {
    if let Some((p, _)) = name.split_once('_') {
        return p.to_lowercase();
    }
    let head: String = name.chars().take_while(|c| !c.is_uppercase()).collect();
    if head.is_empty() { name.to_lowercase() } else { head }
}

/// Carpeta de cada tabla: la de su módulo en `convex/`, o su prefijo si el daemon no la supo.
fn folders(tables: &[Value]) -> HashMap<String, String> {
    tables
        .iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?.to_string();
            let folder = t["module"].as_str().map(str::to_string).unwrap_or_else(|| format!("({})", prefix(&name)));
            Some((name, folder))
        })
        .collect()
}

fn refs_of(table: &Value) -> Vec<String> {
    let mut out: Vec<String> = table["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|f| f["refs"].as_array().cloned().unwrap_or_default())
        .filter_map(|r| r.as_str().map(str::to_string))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Foco: la tabla al centro, a la derecha las que apunta, a la izquierda las que la apuntan, en
/// columnas cortas para que el conjunto crezca a lo ancho.
fn focus_layout(tables: &[Value], center: &str, gap: f32) -> (Vec<String>, HashMap<String, (f32, f32)>) {
    let by_name: HashMap<&str, &Value> = tables.iter().filter_map(|t| t["name"].as_str().map(|n| (n, t))).collect();
    let Some(c) = by_name.get(center) else { return (vec![], HashMap::new()) };
    let outgoing: Vec<String> = refs_of(c).into_iter().filter(|r| r != center && by_name.contains_key(r.as_str())).collect();
    let incoming: Vec<String> = tables
        .iter()
        .filter(|t| t["name"] != center && refs_of(t).iter().any(|r| r == center))
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .filter(|n| !outgoing.contains(n))
        .collect();
    let mut places = HashMap::new();
    places.insert(center.to_string(), (0., 0.));
    let column = |names: &[String], dir: f32, places: &mut HashMap<String, (f32, f32)>| {
        let per = ((names.len() as f32 / ASPECT).sqrt().ceil() as usize).clamp(2, 6);
        for (ci, chunk) in names.chunks(per).enumerate() {
            let total: f32 = chunk.iter().map(|n| by_name.get(n.as_str()).map(|t| box_h(t)).unwrap_or(60.) + gap).sum();
            let mut y = -total / 2. + box_h(c) / 2.;
            let x = dir * ((BOX_W + gap * 2.) * (ci as f32 + 1.));
            for n in chunk {
                places.insert(n.clone(), (x, y));
                y += by_name.get(n.as_str()).map(|t| box_h(t)).unwrap_or(60.) + gap;
            }
        }
    };
    column(&outgoing, 1., &mut places);
    column(&incoming, -1., &mut places);
    let mut names = vec![center.to_string()];
    names.extend(outgoing);
    names.extend(incoming);
    (names, places)
}

/// Marcos: (etiqueta, profundidad, tablas propias, índices de los marcos hijos).
type Groups = Vec<(String, usize, Vec<String>, Vec<usize>)>;

/// Lo que devuelve una disposición: tablas, lugares, marcos, principales de cada carpeta y
/// las ramas del árbol (de la principal a cada una de las suyas).
struct Laid {
    names: Vec<String>,
    places: HashMap<String, (f32, f32)>,
    groups: Groups,
    heads: HashSet<String>,
    links: Vec<(String, String)>,
}

/// Carpetas con sus tablas, desde una raíz (vacía: `convex/`).
#[derive(Default)]
struct Dir {
    tables: Vec<String>,
    dirs: BTreeMap<String, Dir>,
}

fn dir_tree(names: &[String], folder_of: &HashMap<String, String>, root: &str) -> Dir {
    let mut top = Dir::default();
    for n in names {
        let folder = folder_of.get(n).cloned().unwrap_or_default();
        let rel = if root.is_empty() { folder.as_str() } else { folder.strip_prefix(root).unwrap_or("") };
        let mut d = &mut top;
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            d = d.dirs.entry(part.to_string()).or_default();
        }
        d.tables.push(n.clone());
    }
    top
}

/// La tabla principal de una carpeta: la que se llama como ella (`companies`, o `company` en
/// `companies/`) o, si no hay, la más apuntada desde dentro de la carpeta.
fn head_of(dir: &Dir, name: &str, refs_in: &HashMap<String, usize>) -> Option<String> {
    let norm = |s: &str| s.to_lowercase().replace(['_', '-'], "");
    let n = norm(name);
    let singular = |s: &str| -> String {
        if let Some(b) = s.strip_suffix("ies") {
            format!("{b}y")
        } else {
            s.strip_suffix('s').unwrap_or(s).to_string()
        }
    };
    if let Some(t) = dir.tables.iter().find(|t| norm(t) == n || singular(&norm(t)) == singular(&n)) {
        return Some(t.clone());
    }
    dir.tables.iter().filter(|t| refs_in.get(*t).copied().unwrap_or(0) > 0).max_by_key(|t| (refs_in.get(*t).copied().unwrap_or(0), std::cmp::Reverse((*t).clone()))).cloned()
}

/// Tablas en columnas de alto parejo desde (0, 0); devuelve lugares, ancho y alto.
fn grid(names: &[String], cols: usize, by_name: &HashMap<&str, &Value>, gap: f32) -> (HashMap<String, (f32, f32)>, f32, f32) {
    let cols = cols.clamp(1, names.len().max(1));
    let mut col_y = vec![0f32; cols];
    let mut places = HashMap::new();
    for n in names {
        let c = (0..cols).min_by(|a, b| col_y[*a].total_cmp(&col_y[*b])).unwrap_or(0);
        places.insert(n.clone(), (c as f32 * (BOX_W + gap), col_y[c]));
        col_y[c] += by_name.get(n.as_str()).map(|t| box_h(t)).unwrap_or(60.) + gap;
    }
    (places, cols as f32 * (BOX_W + gap) - gap, col_y.iter().cloned().fold(0., f32::max) - gap)
}

/// Un bloque dispuesto, con lugares relativos a su esquina.
struct Blk {
    places: HashMap<String, (f32, f32)>,
    groups: Groups,
    heads: HashSet<String>,
    links: Vec<(String, String)>,
    /// Lo que encabeza el bloque (para unirlo con la principal de arriba): su principal o, si no
    /// tiene, sus tablas de la primera columna.
    leads: Vec<String>,
    w: f32,
    h: f32,
}

/// Árbol a lo ancho: la principal de cada carpeta a la izquierda y, a su derecha, lo suyo (sus
/// tablas y los bloques de sus subcarpetas, cada uno igual: su principal a la izquierda y lo suyo
/// a la derecha). Lo de la derecha se acomoda en filas para que el conjunto quede más ancho que
/// alto. Cada carpeta lleva su marco.
fn tree_layout(names: &[String], folder_of: &HashMap<String, String>, root: &str, by_name: &HashMap<&str, &Value>, gap: f32) -> Laid {
    let dir = dir_tree(names, folder_of, root);
    // Cuántas veces apuntan a cada tabla las de su misma carpeta (o de más adentro).
    let mut refs_in: HashMap<String, usize> = HashMap::new();
    for n in names {
        let from = folder_of.get(n).cloned().unwrap_or_default();
        for r in by_name.get(n.as_str()).map(|t| refs_of(t)).unwrap_or_default() {
            let to = folder_of.get(&r).cloned().unwrap_or_default();
            if r != *n && (from == to || from.starts_with(&format!("{to}/"))) {
                *refs_in.entry(r).or_default() += 1;
            }
        }
    }
    // La proporción que más se acerca a ASPECT entre las cantidades por fila (o columnas de la grilla).
    fn best_count(n: usize, size_of: impl Fn(usize) -> (f32, f32)) -> usize {
        (1..=n.max(1))
            .min_by(|a, b| {
                let s = |k: usize| {
                    let (w, h) = size_of(k);
                    (w / h.max(1.) / ASPECT).ln().abs()
                };
                s(*a).total_cmp(&s(*b))
            })
            .unwrap_or(1)
    }
    fn block(dir: &Dir, label: Option<String>, depth: usize, by_name: &HashMap<&str, &Value>, refs_in: &HashMap<String, usize>, gap: f32) -> Blk {
        let head = label.as_deref().and_then(|l| head_of(dir, l, refs_in));
        let sp = gap * 1.2;
        let mut own: Vec<String> = dir.tables.iter().filter(|t| Some(*t) != head.as_ref()).cloned().collect();
        own.sort();
        // Lo de la derecha: la grilla de las tablas propias y un bloque por subcarpeta.
        let mut items: Vec<Blk> = vec![];
        if !own.is_empty() {
            let cols = best_count(own.len(), |c| {
                let (_, w, h) = grid(&own, c, by_name, gap * 0.5);
                (w, h)
            });
            let (places, w, h) = grid(&own, cols, by_name, gap * 0.5);
            let leads = places.iter().filter(|(_, p)| p.0 == 0.).map(|(n, _)| n.clone()).collect();
            items.push(Blk { places, groups: vec![], heads: HashSet::new(), links: vec![], leads, w, h });
        }
        for (name, sub) in &dir.dirs {
            items.push(block(sub, Some(name.clone()), depth + 1, by_name, refs_in, gap));
        }
        let per_row = best_count(items.len(), |k| {
            let (mut w, mut h) = (0f32, 0f32);
            for row in items.chunks(k) {
                w = w.max(row.iter().map(|l| l.w + sp).sum::<f32>() - sp);
                h += row.iter().map(|l| l.h).fold(0., f32::max) + sp;
            }
            (w, h - sp)
        });
        let x0 = if head.is_some() { BOX_W + gap * 2. } else { 0. };
        let (mut x, mut y, mut row_h, mut w) = (x0, 0f32, 0f32, x0);
        let mut out = Blk { places: HashMap::new(), groups: vec![], heads: HashSet::new(), links: vec![], leads: vec![], w: 0., h: 0. };
        let mut kids = vec![];
        let mut firsts = vec![];
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 && i % per_row == 0 {
                x = x0;
                y += row_h + sp;
                row_h = 0.;
            }
            for (n, (px_, py_)) in item.places {
                out.places.insert(n, (px_ + x, py_ + y));
            }
            // Solo la primera columna se une a la principal: las líneas no cruzan cajas.
            if x == x0 {
                firsts.extend(item.leads);
            }
            let base = out.groups.len();
            let had = !item.groups.is_empty();
            for (l, d, o, k) in item.groups {
                out.groups.push((l, d, o, k.into_iter().map(|c| c + base).collect()));
            }
            if had {
                kids.push(out.groups.len() - 1);
            }
            out.heads.extend(item.heads);
            out.links.extend(item.links);
            w = w.max(x + item.w);
            row_h = row_h.max(item.h);
            x += item.w + sp;
        }
        let mut h = y + row_h;
        match &head {
            Some(hd) => {
                out.places.insert(hd.clone(), (0., 0.));
                out.heads.insert(hd.clone());
                for f in &firsts {
                    out.links.push((hd.clone(), f.clone()));
                }
                h = h.max(by_name.get(hd.as_str()).map(|t| box_h(t)).unwrap_or(60.));
                w = w.max(BOX_W);
                out.leads = vec![hd.clone()];
            }
            None => out.leads = firsts,
        }
        if let Some(l) = label {
            for p in out.places.values_mut() {
                p.0 += PAD;
                p.1 += PAD + LABEL_H;
            }
            w += PAD * 2.;
            h += PAD * 2. + LABEL_H;
            let mut members = own;
            if let Some(hd) = &head {
                members.push(hd.clone());
            }
            out.groups.push((l, depth, members, kids));
        }
        out.w = w;
        out.h = h;
        out
    }
    // En Carpeta, la raíz es la carpeta elegida, con su nombre; en Todas, `convex/`, sin marco.
    let label = (!root.is_empty()).then(|| root.rsplit('/').next().unwrap_or(root).to_string());
    let b = block(&dir, label, 0, by_name, &refs_in, gap);
    let mut order: Vec<String> = b.places.keys().cloned().collect();
    order.sort();
    Laid { names: order, places: b.places, groups: b.groups, heads: b.heads, links: b.links }
}

type Routes = HashMap<(String, usize, String), Rc<Vec<(f32, f32)>>>;

/// Por relaciones: capas de izquierda a derecha (cada tabla a la derecha de las que apunta), sin marcos.
fn relations_layout(names: &[String], by_name: &HashMap<&str, &Value>, gap: f32) -> Laid {
    let mut names = names.to_vec();
    names.sort();
    let index: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    let sizes: Vec<(f32, f32)> = names.iter().map(|n| (BOX_W, by_name.get(n.as_str()).map(|t| box_h(t)).unwrap_or(60.))).collect();
    let mut edges = vec![];
    for (i, n) in names.iter().enumerate() {
        if let Some(t) = by_name.get(n.as_str()) {
            for r in refs_of(t) {
                if let Some(&j) = index.get(r.as_str()) {
                    edges.push((j, i));
                }
            }
        }
    }
    let area: f32 = sizes.iter().map(|(w, h)| (w + gap) * (h + gap)).sum();
    let max_h = (area / ASPECT).sqrt().max(600.);
    let pos = super::er_layout::layered(&sizes, &edges, gap * 2.2, gap * 0.6, max_h);
    let places = names.iter().cloned().zip(pos).collect();
    Laid { names, places, groups: vec![], heads: HashSet::new(), links: vec![] }
}

fn scene(tables: &[Value], laid: &Laid, selected: &str, wire: impl Fn(&str, &str) -> bool, routes: &mut Routes) -> Scene {
    let (names, places, groups) = (&laid.names, &laid.places, &laid.groups);
    let by_name: HashMap<&str, &Value> = tables.iter().filter_map(|t| t["name"].as_str().map(|n| (n, t))).collect();
    let mut sc = Scene::default();
    for n in names {
        let (Some(t), Some(at)) = (by_name.get(n.as_str()), places.get(n)) else { continue };
        let fields = t["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                let fk = f["refs"].as_array().is_some_and(|r| !r.is_empty());
                let kind = if f["primary"] == true { 1 } else if fk { 2 } else { 0 };
                let ty = f["type"].as_str().unwrap_or_default();
                ErField { name: ui::txt(&f["name"]), ty: if kind == 1 { format!("{ty} · PK").into() } else { ty.to_string().into() }, kind }
            })
            .collect();
        sc.index.insert(n.clone(), sc.boxes.len());
        sc.boxes.push(ErBox { key: n.clone(), head: laid.heads.contains(n), x: at.0, y: at.1, fields });
    }
    for (i, b) in sc.boxes.iter().enumerate() {
        let t = by_name[b.key.as_str()];
        for (row, f) in t["fields"].as_array().into_iter().flatten().enumerate() {
            let Some(r) = f["refs"].as_array().and_then(|r| r.first()).and_then(|r| r.as_str()) else { continue };
            let Some(&to) = sc.index.get(r) else { continue };
            if wire(&b.key, r) {
                sc.edges.push(ErEdge { from: i, row, to, hot: b.key == selected || r == selected, route: None });
            }
        }
    }
    let rects: Vec<super::er_route::Rect> = sc.boxes.iter().map(|b| (b.x, b.y, b.x + BOX_W, b.y + b.h())).collect();
    for e in sc.edges.iter_mut() {
        let (fb, tb) = (&sc.boxes[e.from], &sc.boxes[e.to]);
        let key = (fb.key.clone(), e.row, tb.key.clone());
        let r = routes
            .entry(key)
            .or_insert_with(|| {
                let (a, da, b, db) = er::ends((fb.x, fb.y), e.row, (tb.x, tb.y), e.from == e.to);
                Rc::new(super::er_route::route(&rects, [e.from, e.to], a, da, b, db))
            })
            .clone();
        e.route = Some(r);
    }
    sc.links = laid.links.iter().filter_map(|(a, b)| Some((*sc.index.get(a)?, *sc.index.get(b)?))).collect();
    for (label, depth, own, kids) in groups {
        let direct = own.iter().filter_map(|n| sc.index.get(n).copied()).collect();
        sc.groups.push(ErGroup { label: label.clone().into(), depth: *depth, direct, children: kids.clone() });
    }
    sc
}

/// Vista que encuadra todas las cajas en un área de `size` píxeles.
fn fit(places: &HashMap<String, (f32, f32)>, by_name: &HashMap<&str, &Value>, size: (f32, f32)) -> Viewport {
    if places.is_empty() {
        return Viewport::new(0., 0., 1.);
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (n, (x, y)) in places {
        let h = by_name.get(n.as_str()).map(|t| box_h(t)).unwrap_or(60.);
        x0 = x0.min(*x);
        y0 = y0.min(*y);
        x1 = x1.max(x + BOX_W);
        y1 = y1.max(y + h);
    }
    let (w, h) = ((x1 - x0).max(1.), (y1 - y0).max(1.));
    let (by_w, by_h) = (size.0 / w * 0.92, size.1 / h * 0.92);
    // Muy alto para lo ancho (el árbol de carpetas): se encuadra a lo ancho desde arriba, para que
    // se lea, y se baja con dos dedos. Encuadrar todo dejaría cajas de pocos píxeles.
    if by_h < by_w * 0.4 {
        let zoom = by_w.clamp(Viewport::ZOOMS.0, 0.8);
        return Viewport::new(x0 - (size.0 / zoom - w) / 2., y0 - LABEL_H - 20., zoom);
    }
    let zoom = by_w.min(by_h).clamp(Viewport::ZOOMS.0, 1.2);
    Viewport::new(x0 - (size.0 / zoom - w) / 2., y0 - (size.1 / zoom - h) / 2., zoom)
}

/// Árbol de carpetas con sus tablas; las claves son `d:carpeta` y `t:tabla`.
fn folder_tree(tables: &[Value], folder_of: &HashMap<String, String>, query: &str) -> (Vec<TreeNode>, usize) {
    #[derive(Default)]
    struct Dir {
        dirs: BTreeMap<String, Dir>,
        tables: Vec<(String, usize)>,
    }
    fn total(d: &Dir) -> usize {
        d.tables.len() + d.dirs.values().map(total).sum::<usize>()
    }
    fn build(d: &Dir, path: &str) -> Vec<TreeNode> {
        let mut out = vec![];
        for (name, sub) in &d.dirs {
            let p = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
            out.push(TreeNode::new(format!("d:{p}"), name.clone()).icon(Ico::Folder).note(total(sub).to_string()).children(build(sub, &p)));
        }
        let mut tables = d.tables.clone();
        tables.sort();
        for (t, n) in tables {
            out.push(TreeNode::new(format!("t:{t}"), t.clone()).icon(Ico::Table).note(t!("{n} fields", n = n)));
        }
        out
    }
    let mut root = Dir::default();
    let mut count = 0;
    for t in tables {
        let name = t["name"].as_str().unwrap_or_default();
        if !query.is_empty() && !name.to_lowercase().contains(query) {
            continue;
        }
        count += 1;
        let folder = folder_of.get(name).cloned().unwrap_or_default();
        let mut dir = &mut root;
        for part in folder.split('/').filter(|p| !p.is_empty()) {
            dir = dir.dirs.entry(part.to_string()).or_default();
        }
        dir.tables.push((name.to_string(), t["fields"].as_array().map(Vec::len).unwrap_or(0)));
    }
    (build(&root, ""), count)
}

/// Las carpetas que hay que abrir para ver `folder` (y ella misma).
fn ancestors(folder: &str) -> Vec<String> {
    let mut out = vec![];
    let mut acc = String::new();
    for part in folder.split('/').filter(|p| !p.is_empty()) {
        acc = if acc.is_empty() { part.to_string() } else { format!("{acc}/{part}") };
        out.push(format!("d:{acc}"));
    }
    out
}

impl DevtoolApp {
    pub fn load_schema(&mut self, env: String, cx: &mut Context<Self>) {
        self.schema.insert(env.clone(), Load::Loading);
        self.get(format!("/api/environments/{env}/schema"), move |app, res| {
            if let (Load::Ready(v), None) = (&res, app.schema_table.get(&env)) {
                // Por defecto, la tabla con más relaciones.
                let tables = v["tables"].as_array().cloned().unwrap_or_default();
                if let Some(best) = tables.iter().max_by_key(|t| refs_of(t).len()).and_then(|t| t["name"].as_str()) {
                    app.schema_table.insert(env.clone(), best.to_string());
                }
            }
            app.schema_rev += 1;
            app.schema.insert(env, res);
        }, cx);
    }

    fn schema_places(&self, key: &str) -> HashMap<String, (f32, f32)> {
        self.prefs.get(key).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
    }

    pub fn save_place(&self, key: &str, table: &str, at: (f32, f32)) {
        let mut map = self.schema_places(key);
        map.insert(table.to_string(), at);
        self.prefs.set(key, &serde_json::to_string(&map).unwrap_or_default());
    }

    fn schema_gap(&self) -> f32 {
        self.prefs.get("schema.gap").and_then(|g| g.parse().ok()).unwrap_or(90.)
    }

    fn diagram_height(&mut self) -> f32 {
        if self.schema_height == 0. {
            self.schema_height = self.prefs.get("schema.height").and_then(|h| h.parse().ok()).unwrap_or(HEIGHT.1);
        }
        self.schema_height
    }

    /// Vista actual (la que movió el lienzo, o `seed` si aún no hay).
    fn live_view(&self, key: &str, seed: Viewport) -> Viewport {
        self.schema_views.get(key).map(|(x, y, z)| Viewport::new(*x, *y, *z)).unwrap_or(seed)
    }

    /// Guarda la vista en memoria y, cada segundo como mucho, en las preferencias.
    pub fn remember_view(&mut self, key: &str, v: (f32, f32, f32)) {
        self.schema_views.insert(key.to_string(), v);
        let now = ui::now_ms();
        if now - self.schema_saved_at > 1000 {
            self.schema_saved_at = now;
            for (k, v) in &self.schema_views {
                self.prefs.set(k, &serde_json::to_string(v).unwrap_or_default());
            }
        }
    }
}

pub fn render(app: &mut DevtoolApp, env: &str, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let e = env.to_string();
    let mode = app.schema_mode;
    let mode_button = |id: &'static str, label: &'static str, m: Mode, cx: &mut Context<DevtoolApp>| {
        let b = Button::new(id).xsmall().label(label).on_click(cx.listener(move |this, _, _, cx| {
            this.schema_mode = m;
            cx.notify();
        }));
        ui::brand_if(mode == m, b, cx)
    };
    let gap = app.schema_gap();
    let this = cx.entity().downgrade();
    let spacing = Select::new("schema-gap", [crate::ely::choice("40", t!("Compact")), crate::ely::choice("90", t!("Normal")), crate::ely::choice("160", t!("Wide"))])
        .selected(format!("{gap:.0}"))
        .on_change(move |v, _, cx| {
            let v = v.to_string();
            let _ = this.update(cx, |app, cx| {
                app.prefs.set("schema.gap", &v);
                cx.notify();
            });
        });
    let only_rel = app.schema_only_rel;
    // Disposición en Carpeta y Todas: por carpetas (marcos) o por relaciones (capas de izquierda a derecha).
    let by_rel = mode != Mode::Focus && app.prefs.get("schema.arrange").as_deref() == Some("relations");
    let this_a = cx.entity().downgrade();
    let arrange = Select::new("schema-arrange", [crate::ely::choice("folders", t!("By folder")), crate::ely::choice("relations", t!("By relations"))])
        .selected(if by_rel { "relations" } else { "folders" })
        .on_change(move |v, _, cx| {
            let v = v.to_string();
            let _ = this_a.update(cx, |app, cx| {
                app.prefs.set("schema.arrange", &v);
                cx.notify();
            });
        });
    // El panel de tablas se puede plegar para dar más espacio al diagrama; se recuerda.
    let tree_hidden = app.prefs.get("schema.tree").as_deref() == Some("hidden");
    let fold = Button::new(if tree_hidden { "schema-tree-show" } else { "schema-tree-hide" })
        .xsmall()
        .ghost()
        .icon(IconName::PanelLeft)
        .tooltip(if tree_hidden { t!("Show tables panel") } else { t!("Hide tables panel") })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.prefs.set("schema.tree", if tree_hidden { "shown" } else { "hidden" });
            cx.notify();
        }));
    let fold = if tree_hidden { fold.label(t!("Tables")) } else { fold };
    let mut toolbar = h_flex()
        .gap_2()
        .items_center()
        .child(fold)
        .child(mode_button("schema-focus", t!("Focus"), Mode::Focus, cx))
        .child(mode_button("schema-folder", t!("Folder"), Mode::Folder, cx))
        .child(mode_button("schema-all", t!("All"), Mode::All, cx))
        .child(div().w(px(8.)))
        .child(ui::muted(t!("Spacing"), cx))
        .child(div().w(px(110.)).child(spacing));
    if mode != Mode::Focus {
        toolbar = toolbar.child(div().w(px(130.)).child(arrange));
        let b = Button::new("schema-only-rel").xsmall().label(t!("Only the selected one's")).on_click(cx.listener(|this, _, _, cx| {
            this.schema_only_rel = !this.schema_only_rel;
            cx.notify();
        }));
        toolbar = toolbar.child(ui::brand_if(only_rel, b, cx));
    }
    toolbar = toolbar.child(div().flex_1());

    match app.schema.get(env) {
        None => {
            app.load_schema(env.to_string(), cx);
            return v_flex().gap_2().child(toolbar).child(ui::muted(t!("Reading the schema…"), cx)).into_any_element();
        }
        Some(Load::Loading) => return v_flex().gap_2().child(toolbar).child(ui::muted(t!("Reading the schema…"), cx)).into_any_element(),
        Some(Load::Failed(err)) => return v_flex().gap_2().child(toolbar).child(ui::muted(err.clone(), cx).text_color(cx.theme().danger)).into_any_element(),
        Some(Load::Ready(_)) => {}
    };
    // El esquema, compartido: copiarlo en cada cuadro (se dibuja uno por movimiento) costaba.
    let fresh = !matches!(&app.schema_data, Some((k, r, _)) if k == env && *r == app.schema_rev);
    if fresh {
        if let Some(Load::Ready(v)) = app.schema.get(env) {
            app.schema_data = Some((env.to_string(), app.schema_rev, Rc::new(v.clone())));
        }
    }
    let data = app.schema_data.as_ref().map(|d| d.2.clone()).unwrap_or_default();
    let empty = vec![];
    let tables: &Vec<Value> = data["tables"].as_array().unwrap_or(&empty);
    let by_name: HashMap<&str, &Value> = tables.iter().filter_map(|t| t["name"].as_str().map(|n| (n, t))).collect();
    let folder_of = folders(&tables);
    let selected = app.schema_table.get(env).cloned().unwrap_or_default();
    let sel_folder = folder_of.get(&selected).cloned().unwrap_or_default();
    let folder = app.schema_folder.get(env).cloned().unwrap_or_else(|| sel_folder.clone());
    let source = if data["source"] == "checkout" {
        t!("Schema from the checkout (local backend “{a}”), not from the deploy: the CLI can't read a cloud environment's schema.", a = ui::txt(&data["fromEnvironment"]))
    } else {
        t!("Schema deployed on this backend.").to_string()
    };
    let search = app.ely_input(&format!("schema-search-{env}"), t!("Search tables…"), window, cx);
    let query = search.read(cx).text().to_lowercase();
    let scope = match mode {
        Mode::Focus => selected.clone(),
        Mode::Folder => folder.clone(),
        Mode::All => t!("all").into(),
    };
    let lay = if by_rel { ".rel" } else { "" };
    let places_key = format!("schema.places.{}{lay}.{env}.{scope}.{gap:.0}", mode.key());
    let view_key = format!("schema.view.{}{lay}.{env}.{scope}.{gap:.0}", mode.key());
    let fullscreen = app.schema_fullscreen;
    // Tampoco al achicar la ventana: el alto guardado se recorta al espacio que queda.
    let top = f32::from(app.schema_bounds.get().origin.y);
    let room = f32::from(window.viewport_size().height) - top - 28.;
    let height = if top > 0. { app.diagram_height().min(room.max(HEIGHT.0)) } else { app.diagram_height() };

    // Lo caro, solo si cambió algo de lo que depende (no en cada movimiento del lienzo).
    let cache_key = format!("{env}|{}|{}|{scope}|{gap}|{selected}|{only_rel}|{query}|{by_rel}", app.schema_rev, mode.key());
    let derived = match &app.schema_cache {
        Some((k, d)) if *k == cache_key => d.clone(),
        _ => {
            let mut laid = match mode {
                Mode::Focus => {
                    let (names, places) = focus_layout(&tables, &selected, gap);
                    Laid { names, places, groups: vec![], heads: HashSet::new(), links: vec![] }
                }
                Mode::Folder if by_rel => {
                    let names: Vec<String> =
                        folder_of.iter().filter(|(_, f)| **f == folder || f.starts_with(&format!("{folder}/"))).map(|(n, _)| n.clone()).collect();
                    relations_layout(&names, &by_name, gap)
                }
                Mode::All if by_rel => relations_layout(&folder_of.keys().cloned().collect::<Vec<_>>(), &by_name, gap),
                Mode::Folder => {
                    // La carpeta como árbol: su principal a la izquierda y lo suyo a la derecha.
                    let names: Vec<String> =
                        folder_of.iter().filter(|(_, f)| **f == folder || f.starts_with(&format!("{folder}/"))).map(|(n, _)| n.clone()).collect();
                    tree_layout(&names, &folder_of, &folder, &by_name, gap)
                }
                Mode::All => tree_layout(&folder_of.keys().cloned().collect::<Vec<_>>(), &folder_of, "", &by_name, gap),
            };
            let blocks = laid.groups.len().max(1);
            for (n, at) in app.schema_places(&places_key) {
                if laid.places.contains_key(&n) {
                    laid.places.insert(n, at);
                }
            }
            let (names, places) = (laid.names.clone(), laid.places.clone());
            // Qué referencias se dibujan: en Foco y en Carpeta todas (también entre subcarpetas); en
            // Todas, las de dentro de cada carpeta y las de la elegida. Con «solo las de la elegida»,
            // solo esas. Con todas juntas, 155 tablas eran una maraña.
            let wire = |from: &str, to: &str| -> bool {
                let rel = from == selected || to == selected;
                match mode {
                    _ if only_rel && mode != Mode::Focus => rel,
                    Mode::Focus | Mode::Folder => true,
                    Mode::All => rel || folder_of.get(from) == folder_of.get(to),
                }
            };
            // Rutas en ángulo recto: se guardan mientras las cajas no se muevan (elegir otra tabla no
            // las recalcula).
            let mut sig: Vec<(&String, (i32, i32))> = places.iter().map(|(n, p)| (n, (p.0 as i32, p.1 as i32))).collect();
            sig.sort();
            let sig = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                sig.hash(&mut h);
                h.finish()
            };
            if app.schema_routes.0 != sig {
                app.schema_routes = (sig, HashMap::new());
            }
            let t0 = web_time::Instant::now();
            let scene = Rc::new(scene(&tables, &laid, &selected, wire, &mut app.schema_routes.1));
            if std::env::var_os("DEVTOOL_TRACE").is_some() {
                eprintln!("esquema: {} cajas, {} líneas en {:?}", scene.boxes.len(), scene.edges.len(), t0.elapsed());
            }
            let (tree, tree_count) = folder_tree(&tables, &folder_of, &query);
            let open = if query.is_empty() {
                let mut o = ancestors(&sel_folder);
                o.extend(ancestors(&folder));
                o
            } else {
                // Con búsqueda, todas las carpetas abiertas para ver lo encontrado.
                folder_of.values().flat_map(|f| ancestors(f)).collect::<HashSet<_>>().into_iter().collect()
            };
            let d = Rc::new(Derived { tree, open, tree_count, scene, places, shown: names.len(), blocks });
            app.schema_cache = Some((cache_key, d.clone()));
            d
        }
    };

    // Vista: la viva (la mueve el lienzo) o, la primera vez, la guardada o una de arranque.
    let measured = app.schema_bounds.get().size;
    let area = if measured.width > px(50.) {
        (f32::from(measured.width), f32::from(measured.height))
    } else {
        (if fullscreen { 1100. } else { 780. }, if fullscreen { 760. } else { height })
    };
    let fitted = fit(&derived.places, &by_name, area);
    let start = match (mode, by_name.get(selected.as_str())) {
        (Mode::Focus, Some(t)) => {
            let z = 0.6;
            Viewport::new(BOX_W / 2. - area.0 / 2. / z, (box_h(t) / 2. - area.1 / 2. / z).min(-40.), z)
        }
        _ => fitted,
    };
    if !app.schema_views.contains_key(&view_key) {
        let saved = app.prefs.get(&view_key).and_then(|v| serde_json::from_str::<(f32, f32, f32)>(&v).ok());
        app.schema_views.insert(view_key.clone(), saved.unwrap_or((start.x, start.y, start.zoom)));
    }
    let live = app.live_view(&view_key, start);

    // Botones de Kit: los de Ely no responden a la accesibilidad de macOS (ni a quien la use para probar).
    let zoom_by = |id: &'static str, icon: IconName, tip: &'static str, factor: f32, cx: &mut Context<DevtoolApp>| {
        let key = view_key.clone();
        Button::new(id).xsmall().ghost().icon(icon).tooltip(tip).on_click(cx.listener(move |this, _, _, cx| {
            let v = this.live_view(&key, start).zoomed(factor, (area.0 / 2., area.1 / 2.));
            this.remember_view(&key, (v.x, v.y, v.zoom));
            cx.notify();
        }))
    };
    let fit_key = view_key.clone();
    toolbar = toolbar
        .child(ui::muted(format!("{:.0}%", live.zoom * 100.), cx))
        .child(zoom_by("schema-zoom-out", IconName::Minus, t!("Zoom out"), 1. / 1.25, cx))
        .child(zoom_by("schema-zoom-in", IconName::Plus, t!("Zoom in"), 1.25, cx))
        .child(Button::new("schema-fit").xsmall().ghost().label(t!("Fit")).on_click(cx.listener(move |this, _, _, cx| {
            this.remember_view(&fit_key, (fitted.x, fitted.y, fitted.zoom));
            cx.notify();
        })))
        // Entrar y salir fijan el estado (no lo alternan): un clic doble no lo deja igual.
        .child(
            Button::new(if fullscreen { "schema-full-exit" } else { "schema-full-enter" })
                .xsmall()
                .ghost()
                .icon(IconName::Maximize)
                .label(if fullscreen { t!("Exit full screen") } else { t!("Full screen") })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.schema_fullscreen = !fullscreen;
                    cx.notify();
                })),
        )
        .child(Button::new("schema-reload").xsmall().ghost().icon(IconName::RefreshCw).label(t!("Reload")).on_click(cx.listener(move |this, _, _, cx| this.load_schema(e.clone(), cx))));

    // El diagrama: un solo elemento pintado (ver `er.rs`).
    let ctx = Rc::new(er::Ctx { view_key: view_key.clone(), places_key: places_key.clone(), env: env.to_string(), select_on_release: mode == Mode::Focus, folder: (mode == Mode::Folder).then(|| folder.clone()) });
    let diagram_box = ui::contained().relative().flex_1().min_w_0().h_full().child(er::element(app, derived.scene.clone(), live, &selected, ctx, cx));

    // Árbol de carpetas.
    let this = cx.entity().downgrade();
    let e2 = env.to_string();
    let tree_sel = if mode == Mode::Folder { vec![format!("d:{folder}")] } else { vec![format!("t:{selected}")] };
    let tree = Tree::new(SharedString::from(format!("schema-tree-{env}")), derived.tree.clone()).open(derived.open.clone()).selected(tree_sel).on_select(move |keys, _, cx| {
        let Some(k) = keys.first().map(|k| k.to_string()) else { return };
        let env = e2.clone();
        let _ = this.update(cx, |app, cx| {
            if let Some(t) = k.strip_prefix("t:") {
                app.schema_table.insert(env.clone(), t.to_string());
                app.schema_folder.remove(&env);
            } else if let Some(d) = k.strip_prefix("d:") {
                // Una carpeta: el modo Carpeta la muestra.
                app.schema_folder.insert(env, d.to_string());
                app.schema_mode = Mode::Folder;
            }
            cx.notify();
        });
    });

    let t = crate::theme::Tokens::current(cx);
    let shown_label = match mode {
        Mode::Focus => t!("“{selected}” and its relations ({a} tables)", selected = selected, a = derived.shown),
        Mode::Folder if by_rel => t!("folder “{folder}”, {a} tables by relations", folder = folder, a = derived.shown),
        Mode::All if by_rel => t!("{a} tables by relations (each to the right of the ones it points to)", a = derived.shown),
        Mode::Folder => t!("folder “{folder}”, {a} tables in {b} folders", folder = folder, a = derived.shown, b = derived.blocks),
        Mode::All => t!("{a} tables in {b} folders", a = derived.shown, b = derived.blocks),
    };
    // Borde inferior para ajustar el alto (arrastrar).
    let handle = div()
        .id("schema-resize")
        .h(px(7.))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_row_resize()
        .hover(move |s| s.bg(t.hover))
        .child(div().w(px(36.)).h(px(2.)).bg(t.subtle))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
            this.schema_drag = Some((f32::from(ev.position.y), height));
            cx.notify();
        }));
    // Mientras se arrastra, el movimiento se escucha en toda la ventana: el
    // puntero sale del borde en cuanto se mueve un poco.
    let dragging = app.schema_drag.is_some();
    let me = cx.entity();
    let handle = handle.when(dragging, |d| {
        d.child(canvas(|_, _, _| {}, move |_, _, window, _| {
            let mv = me.clone();
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let win_h = f32::from(window.viewport_size().height);
                mv.update(cx, |this, cx| {
                    let Some((y0, h0)) = this.schema_drag else { return };
                    if !ev.dragging() {
                        this.schema_drag = None;
                        cx.notify();
                        return;
                    }
                    // El borde no baja de la ventana: queda a la vista para poder volver a tomarlo.
                    let room = win_h - f32::from(this.schema_bounds.get().origin.y) - 28.;
                    let h = (h0 + f32::from(ev.position.y) - y0).clamp(HEIGHT.0, HEIGHT.2.min(room.max(HEIGHT.0)));
                    if (h - this.schema_height).abs() >= 1. {
                        this.schema_height = h;
                        cx.notify();
                    }
                });
            });
            let up = me.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                up.update(cx, |this, cx| {
                    if this.schema_drag.take().is_some() {
                        this.prefs.set("schema.height", &format!("{:.0}", this.schema_height));
                        cx.notify();
                    }
                });
            });
        }).absolute().size_0())
    });

    let tree_panel = v_flex()
        .w(px(300.))
        .flex_none()
        .border_r_1()
        .border_color(t.card_border)
        .child(div().p_2().border_b_1().border_color(t.card_border).child(SearchInput::new("schema-search", &search)))
        .child(div().px_2().py_1().child(ui::muted(t!("{a} tables in convex/", a = derived.tree_count), cx)))
        // El árbol de Ely dibuja solo las filas visibles: necesita un alto concreto.
        .child(ui::contained().flex_1().min_h_0().child(tree.size_full()));
    let body = h_flex().items_stretch().border_1().border_color(t.card_border).when(!tree_hidden, |b| b.child(tree_panel)).child(diagram_box);
    let body = if fullscreen { body.flex_1().min_h_0() } else { body.h(px(height)) };
    let mut col = v_flex()
        .gap_2()
        .child(toolbar)
        .child(ui::muted(t!("{source} In the diagram: {shown_label}. Two fingers: pan · pinch or ⌘ + wheel: zoom · click a box to select it.", source = source, shown_label = shown_label), cx))
        .child(body);
    if fullscreen {
        // En pantalla completa el bloque ocupa todo el alto disponible.
        col = col.size_full().min_h_0();
    } else {
        col = col.child(handle);
        if let Some(tab) = by_name.get(selected.as_str()) {
            col = col.child(table_card(env, &selected, &sel_folder, tab, cx));
        }
    }
    col.into_any_element()
}

/// Ficha de la tabla elegida: carpeta, campos, a qué apunta y sus índices.
fn table_card(env: &str, selected: &str, folder: &str, t: &Value, cx: &mut Context<DevtoolApp>) -> Div {
    let mut rows = vec![];
    for i in t["indexes"].as_array().into_iter().flatten() {
        let fields: Vec<&str> = i["fields"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).collect();
        rows.push(Row::new(format!("i-{}", ui::txt(&i["name"])), [Cell::Text(ui::txt(&i["name"])), Cell::Text(t!("index").into()), Cell::Text(fields.join(", ").into())]));
    }
    for i in t["searchIndexes"].as_array().into_iter().flatten() {
        let filters: Vec<&str> = i["filterFields"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).collect();
        let what = t!("text: {a}{b}", a = ui::txt(&i["searchField"]), b = if filters.is_empty() { String::new() } else { format!(" · filtros: {}", filters.join(", ")) });
        rows.push(Row::new(format!("s-{}", ui::txt(&i["name"])), [Cell::Text(ui::txt(&i["name"])), Cell::Text(t!("search").into()), Cell::Text(what.into())]));
    }
    for i in t["vectorIndexes"].as_array().into_iter().flatten() {
        let what = t!("{a} · {b} dimensions", a = ui::txt(&i["vectorField"]), b = ui::txt(&i["dimensions"]));
        rows.push(Row::new(format!("v-{}", ui::txt(&i["name"])), [Cell::Text(ui::txt(&i["name"])), Cell::Text(t!("vector").into()), Cell::Text(what.into())]));
    }
    let refs = refs_of(t);
    let card = ui::card(cx).child(
        h_flex().gap_2().items_center().child(ui::section(t!("Table · {selected}", selected = selected), cx)).child(ui::muted(
            t!("convex/{folder} · {a} fields · points to {b}", folder = folder, a = t["fields"].as_array().map(Vec::len).unwrap_or(0), b = if refs.is_empty() { t!("none").to_string() } else { refs.join(", ") }),
            cx,
        )),
    );
    if rows.is_empty() {
        card.child(ui::muted(t!("No indexes (besides _creationTime)."), cx))
    } else {
        card.child(
            DataTable::new(SharedString::from(format!("indexes-{env}-{selected}")), [Column::new("name", t!("Index")).width(rems(14.)), Column::new("kind", t!("Type")).width(rems(7.)), Column::new("fields", t!("Fields"))])
                .rows(rows),
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn foco_carpetas_y_relaciones() {
        let tables = vec![
            json!({ "name": "users", "module": "system/users", "fields": [{ "name": "_id" }] }),
            json!({ "name": "posts", "module": "companies/posts", "fields": [{ "name": "_id" }, { "name": "author", "refs": ["users"] }] }),
            json!({ "name": "likes", "module": "companies/posts/likes", "fields": [{ "name": "_id" }, { "name": "post", "refs": ["posts"] }] }),
        ];
        let (names, places) = super::focus_layout(&tables, "posts", 90.);
        assert_eq!(names, vec!["posts", "users", "likes"]);
        assert!(places["users"].0 > 0. && places["likes"].0 < 0.);
        let folders = super::folders(&tables);
        assert_eq!(folders["likes"], "companies/posts/likes");
        let (tree, count) = super::folder_tree(&tables, &folders, "");
        assert_eq!(count, 3);
        assert_eq!(tree.len(), 2); // companies y system
        assert_eq!(super::ancestors("companies/posts"), vec!["d:companies", "d:companies/posts"]);
        let all: Vec<String> = ["users", "posts", "likes"].iter().map(|s| s.to_string()).collect();
        let laid = super::Laid { names: all.clone(), places: places.clone(), groups: vec![], heads: Default::default(), links: vec![] };
        let sc = super::scene(&tables, &laid, "posts", |_, _| true, &mut Default::default());
        assert_eq!(sc.boxes.len(), 3);
        assert_eq!(sc.edges.len(), 2);
        assert!(sc.edges.iter().all(|e| e.hot));
    }

    #[test]
    fn arbol_por_carpetas() {
        // companies/ con su tabla principal, dos tablas sueltas y una subcarpeta con la suya.
        let t = |name: &str, module: &str, refs: &[&str]| {
            let mut fields = vec![json!({ "name": "_id" })];
            for r in refs {
                fields.push(json!({ "name": format!("{r}Id"), "refs": [r] }));
            }
            json!({ "name": name, "module": module, "fields": fields })
        };
        let tables = vec![
            t("companies", "companies", &[]),
            t("branches", "companies/branches", &["companies"]),
            t("clients", "companies", &["companies"]),
            t("invoices", "modules/invoices", &["companies", "clients"]),
            t("invoice_items", "modules/invoices", &["invoices"]),
            t("users", "system", &[]),
        ];
        let folders = super::folders(&tables);
        let by: std::collections::HashMap<&str, &serde_json::Value> = tables.iter().map(|t| (t["name"].as_str().unwrap(), t)).collect();
        let names: Vec<String> = folders.keys().cloned().collect();
        let laid = super::tree_layout(&names, &folders, "", &by, 90.);
        let p = &laid.places;
        // companies a la izquierda y todo lo suyo a su derecha; lo de invoices, a la derecha de invoices.
        assert!(laid.heads.contains("companies") && laid.heads.contains("invoices"));
        for n in ["clients", "invoices", "branches", "invoice_items"] {
            assert!(p[n].0 >= p["companies"].0 + super::BOX_W, "{n} a la derecha de companies");
        }
        assert!(p["invoice_items"].0 >= p["invoices"].0 + super::BOX_W);
        assert!(laid.links.contains(&("companies".into(), "clients".into())));
        assert!(laid.links.contains(&("invoices".into(), "invoice_items".into())));
        let labels: Vec<&str> = laid.groups.iter().map(|g| g.0.as_str()).collect();
        assert!(labels.contains(&"companies") && labels.contains(&"invoices"));
        // En Carpeta, la raíz es la carpeta: companies sigue a la izquierda.
        let sub: Vec<String> = names.iter().filter(|n| folders[*n].starts_with("companies")).cloned().collect();
        let laid = super::tree_layout(&sub, &folders, "companies", &by, 90.);
        assert!(laid.places.values().all(|q| q.0 >= laid.places["companies"].0));

        // Muchas tablas en varias carpetas: el conjunto queda más ancho que alto.
        let many: Vec<serde_json::Value> = (0..80)
            .map(|i| json!({ "name": format!("t{i}"), "module": format!("m{}/s{}", i % 4, i % 5), "fields": [{ "name": "_id" }, { "name": "a" }, { "name": "b" }] }))
            .collect();
        let f = super::folders(&many);
        let by: std::collections::HashMap<&str, &serde_json::Value> = many.iter().map(|t| (t["name"].as_str().unwrap(), t)).collect();
        let names: Vec<String> = f.keys().cloned().collect();
        let p = super::tree_layout(&names, &f, "", &by, 90.).places;
        let w = p.values().map(|a| a.0).fold(0., f32::max) + super::BOX_W;
        let h = p.values().map(|a| a.1).fold(0., f32::max) + 100.;
        assert!(w > h, "ancho {w} alto {h}");
    }
}
