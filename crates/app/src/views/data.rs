// Pestaña Datos de un entorno: a la izquierda sus tablas con cuántos documentos tiene cada
// una; a la derecha, los documentos de la elegida en un DataTable de Ely (búsqueda, filtros,
// orden, columnas visibles, densidad, selección, totales al pie, copiar como CSV) y el documento
// seleccionado en el editor de código de Ely. Solo lectura.
//
// Es una vista propia (`DataView`), una por entorno, que la app conserva: volver a la pestaña o
// al entorno no vuelve a leer nada, y la app la dibuja con `cached`, así que los redibujos de
// la app (el sondeo del daemon, otras pestañas) no la reconstruyen. Por eso todo lo que cambia
// lo que se ve notifica a esta vista: sus lecturas, sus campos de texto y el editor.

use std::rc::Rc;

use ely_core::forms::TextInput;
use ely_core::tables::{to_csv, Aggregate, Cell, Column, DataTable, FilterBuilder, FilterRule, Row, SortBuilder, SortKey, TableToolbar};
use ely_core::theme::Density;
use ely_core::data_display::Tone;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::pagination::Pagination;
use gpui_kit::component::{h_flex, v_flex, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;
use std::collections::HashMap;

use crate::app::Load;
use crate::client::Daemon;
use crate::theme::Tokens;
use crate::ui;

/// Como mucho, documentos por lectura al saltar a una página lejana (el tope del daemon).
const MAX_FETCH: usize = 500;
/// Filas por página de la tabla.
const PAGE_ROWS: usize = 50;

/// Los documentos leídos de una tabla.
struct Page {
    table: String,
    docs: Vec<Value>,
    cursor: Option<String>,
    done: bool,
    total: Option<u64>,
    loading: bool,
    error: Option<String>,
    /// La CLI llegó a su tope de documentos (cloud).
    capped: bool,
    /// La página que se ve (desde 0), de `PAGE_ROWS` filas.
    ix: usize,
}

/// La pestaña Datos de un entorno.
pub struct DataView {
    env: String,
    kind: String,
    daemon: Option<Daemon>,
    tables: Option<Load>,
    page: Option<Page>,
    /// En cloud (sin tamaños), mientras el daemon busca la primera tabla con documentos.
    finding: bool,
    rules: (Vec<FilterRule>, bool),
    sorts: Vec<SortKey>,
    hidden: Vec<SharedString>,
    density: Density,
    builders: bool,
    selected: Vec<SharedString>,
    copied: Option<usize>,
    /// Si las reglas de datos (Configuración) dejan leer los registros de este tipo de entorno.
    readable: bool,
    filter: Entity<InputState>,
    search: Entity<TextInput>,
    /// El documento seleccionado en el editor de código y su texto.
    doc: Option<(Entity<EditorState>, String)>,
    /// Las relaciones del esquema: tabla → campo → tabla a la que apunta su `v.id(...)`.
    refs: HashMap<String, HashMap<String, String>>,
    /// Teñir las columnas numéricas según dónde cae cada valor (casilla «Colores»).
    tint: bool,
    /// Foco y scroll de la lista de tablas (flechas arriba y abajo para recorrerla).
    tables_focus: FocusHandle,
    tables_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl DataView {
    pub fn new(env: String, kind: String, readable: bool, daemon: Option<Daemon>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search tables…")));
        let search = cx.new(|cx| TextInput::new(window, cx).placeholder(t!("Search documents…")));
        // Los campos son entidades propias: escribir en ellos no notifica a esta vista.
        let subscriptions = vec![cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify()), cx.observe(&search, |_, _, cx| cx.notify())];
        let mut view = Self {
            env,
            kind,
            readable,
            daemon,
            tables: None,
            page: None,
            finding: false,
            rules: (vec![], false),
            sorts: vec![],
            hidden: vec![],
            density: Density::Compact,
            builders: false,
            selected: vec![],
            copied: None,
            filter,
            search,
            doc: None,
            refs: HashMap::new(),
            tint: false,
            tables_focus: cx.focus_handle(),
            tables_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        // Si las reglas no dejan leer este tipo de entorno, no se pide nada.
        if view.readable {
            view.load_tables(cx);
            view.load_refs(cx);
        }
        view
    }

    /// Una lectura al daemon; `store` recibe el resultado con el contexto de la vista.
    fn fetch(&mut self, path: String, store: impl FnOnce(&mut Self, Load, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else { return };
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { daemon.get(&path) }).await;
            this.update(cx, |view, cx| {
                store(view, res.into(), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Lee del esquema a qué tabla apunta cada campo `v.id(...)`, para marcar esas celdas.
    fn load_refs(&mut self, cx: &mut Context<Self>) {
        let env = self.env.clone();
        self.fetch(format!("/api/environments/{env}/schema"), |view, res, _| {
            if let Load::Ready(v) = res {
                view.refs = refs_of(&v);
            }
        }, cx);
    }

    /// Lee las tablas y, si no hay una abierta, abre la primera con documentos.
    fn load_tables(&mut self, cx: &mut Context<Self>) {
        self.tables = Some(Load::Loading);
        let env = self.env.clone();
        self.fetch(format!("/api/environments/{env}/tables"), |view, res, cx| {
            view.tables = Some(res);
            if view.page.is_none() {
                view.open_first(cx);
            }
        }, cx);
    }

    /// Los nombres de las tablas visibles (con el filtro), en el orden de la lista.
    fn visible_names(&self, cx: &App) -> Vec<String> {
        let Some(Load::Ready(v)) = &self.tables else { return vec![] };
        let q = self.filter.read(cx).value().to_string();
        Self::ordered(v).into_iter().filter_map(|x| x["name"].as_str()).filter(|n| table_matches(n, &q)).map(str::to_string).collect()
    }

    /// Flecha arriba o abajo en la lista de tablas: abre la anterior o la siguiente y la deja a
    /// la vista.
    fn step_table(&mut self, delta: isize, cx: &mut Context<Self>) {
        let names = self.visible_names(cx);
        let current = self.page.as_ref().and_then(|p| names.iter().position(|n| *n == p.table));
        if let Some(ix) = crate::nav::step(current, delta, names.len()) {
            if Some(ix) != current {
                self.open_table(names[ix].clone(), cx);
            }
            self.tables_scroll.scroll_to_item(ix);
        }
    }

    /// Las tablas en el orden en que se muestran: las que tienen documentos primero.
    fn ordered(v: &Value) -> Vec<&Value> {
        let mut list: Vec<&Value> = v["tables"].as_array().map(|l| l.iter().collect()).unwrap_or_default();
        list.sort_by_key(|x| x["count"].as_u64().unwrap_or(0) == 0);
        list
    }

    /// La primera tabla con documentos. En local sale de los tamaños; en cloud la CLI no los da y
    /// el daemon prueba por orden. Si no hay ninguna, la primera de la lista.
    fn open_first(&mut self, cx: &mut Context<Self>) {
        let Some(Load::Ready(v)) = &self.tables else { return };
        let first = Self::ordered(v).first().and_then(|x| x["name"].as_str()).map(str::to_string);
        if v["counts"] != false {
            if let Some(table) = first {
                self.open_table(table, cx);
            }
            return;
        }
        self.finding = true;
        let env = self.env.clone();
        self.fetch(format!("/api/environments/{env}/first-table"), move |view, res, cx| {
            view.finding = false;
            let found = match &res {
                Load::Ready(v) => v["table"].as_str().map(str::to_string),
                _ => None,
            };
            if let (None, Some(table)) = (&view.page, found.or(first)) {
                view.open_table(table, cx);
            }
        }, cx);
    }

    /// Copia un `_id` y lo avisa con una notificación que se va sola.
    fn copy_id(&mut self, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()));
        window.push_notification(Notification::success(t!("Copied {a}", a = id)).id1::<DataView>("copied-id"), cx);
    }

    /// Va a la página `ix` (desde 0); si todavía no se leyeron sus documentos, los lee.
    fn go_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(p) = self.page.as_mut() else { return };
        p.ix = ix;
        let need = (ix + 1) * PAGE_ROWS;
        if p.docs.len() < need && !p.done && !p.loading {
            self.load_until(need, cx);
        }
        cx.notify();
    }

    /// Abre una tabla: borra lo que se veía de la anterior y lee su primera página.
    fn open_table(&mut self, table: String, cx: &mut Context<Self>) {
        self.rules = (vec![], false);
        self.sorts = vec![];
        self.hidden = vec![];
        self.selected = vec![];
        self.copied = None;
        self.doc = None;
        self.page = Some(Page { table, docs: vec![], cursor: None, done: false, total: None, loading: true, error: None, capped: false, ix: 0 });
        self.load_until(PAGE_ROWS, cx);
        cx.notify();
    }

    /// Lee documentos de la tabla abierta hasta tener `want` (o que no haya más): de a una
    /// lectura de hasta `MAX_FETCH`, siguiendo el cursor.
    fn load_until(&mut self, want: usize, cx: &mut Context<Self>) {
        let Some(p) = self.page.as_mut() else { return };
        p.loading = true;
        let (env, table) = (self.env.clone(), p.table.clone());
        let cursor = p.cursor.clone().unwrap_or_default();
        let limit = want.saturating_sub(p.docs.len()).clamp(PAGE_ROWS, MAX_FETCH);
        self.fetch(format!("/api/environments/{env}/documents?table={table}&limit={limit}&cursor={cursor}"), move |view, res, cx| {
            // Una respuesta de otra tabla (se eligió otra mientras tanto) se descarta.
            let Some(p) = view.page.as_mut().filter(|p| p.table == table) else { return };
            p.loading = false;
            match res {
                Load::Ready(v) => {
                    p.docs.extend(v["documents"].as_array().cloned().unwrap_or_default());
                    p.cursor = v["cursor"].as_str().map(str::to_string);
                    p.done = v["isDone"] != false;
                    p.total = v["total"].as_u64();
                    p.capped = v["capped"] == true;
                    p.error = None;
                    if p.docs.len() < want && !p.done {
                        view.load_until(want, cx);
                    }
                }
                Load::Failed(e) => p.error = Some(e),
                Load::Loading => {}
            }
        }, cx);
    }

    /// Elige filas; con una sola, su documento va al editor de código.
    fn select(&mut self, keys: Vec<SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = keys;
        let doc = (self.selected.len() == 1)
            .then(|| self.page.as_ref()?.docs.iter().find(|x| x["_id"].as_str() == Some(self.selected[0].as_ref())).cloned())
            .flatten();
        match doc {
            None => self.doc = None,
            Some(doc) => {
                let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
                match &mut self.doc {
                    Some((editor, shown)) => {
                        *shown = text.clone();
                        editor.update(cx, |ed, cx| ed.set_value(text, window, cx));
                    }
                    None => {
                        // El editor de código de GPUI Kit: corta las líneas largas al ancho del
                        // panel (el de Ely no) y se dibuja de solo lectura.
                        let t = text.clone();
                        let editor = cx.new(|cx| EditorState::new(window, cx).language("json").line_number(true).soft_wrap(true).default_value(t));
                        self.doc = Some((editor, text));
                    }
                }
            }
        }
        cx.notify();
    }

    fn render_tables(&self, cx: &mut Context<Self>) -> Div {
        let t = Tokens::current(cx);
        let q = self.filter.read(cx).value().to_string();
        let mut side = v_flex().w_56().flex_none().h_full().gap_1().child(Input::new(&self.filter).small());
        match &self.tables {
            Some(Load::Ready(v)) => {
                let all = Self::ordered(v);
                let with_docs = all.iter().filter(|x| x["count"].as_u64().unwrap_or(0) > 0).count();
                // En cloud la CLI no da tamaños: solo los nombres.
                let note = if v["counts"] == false { t!("{a} tables · read with your Convex CLI session (for record counts, save a deploy key: condxd secret set dev-deploy-key)", a = all.len()) } else { t!("{a} tables · {b} with documents", a = all.len(), b = with_docs) };
                side = side.child(ui::muted(note, cx).text_xs().pb_1());
                let current = self.page.as_ref().map(|p| p.table.as_str());
                // Con el foco en la lista, las flechas recorren las tablas.
                let mut items = v_flex()
                    .id("data-tables")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.tables_scroll)
                    .track_focus(&self.tables_focus)
                    .key_context(crate::nav::TABLES)
                    .on_action(cx.listener(|this, _: &crate::nav::SelectPrev, _, cx| this.step_table(-1, cx)))
                    .on_action(cx.listener(|this, _: &crate::nav::SelectNext, _, cx| this.step_table(1, cx)));
                for x in all.into_iter().filter(|x| table_matches(x["name"].as_str().unwrap_or_default(), &q)) {
                    let name = x["name"].as_str().unwrap_or_default().to_string();
                    let count = x["count"].as_u64();
                    let selected = current == Some(name.as_str());
                    let n = name.clone();
                    items = items.child(
                        h_flex()
                            .id(SharedString::from(format!("data-t-{name}")))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .when(selected, |d| d.bg(t.hover))
                            .hover(|d| d.bg(t.hover))
                            .child(div().flex_1().min_w_0().truncate().text_sm().text_color(if count.unwrap_or(0) > 0 { t.fg } else { t.subtle }).child(name))
                            .child(div().text_xs().text_color(t.subtle).child(count.map(|c| c.to_string()).unwrap_or_else(|| "—".into())))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                // El clic deja el foco en la lista: después se sigue con las flechas.
                                window.focus(&this.tables_focus, cx);
                                this.open_table(n.clone(), cx)
                            })),
                    );
                }
                side = side.child(items);
            }
            Some(Load::Failed(e)) => side = side.child(ui::muted(e.clone(), cx)),
            _ => side = side.child(ui::muted(t!("Reading tables…"), cx)),
        }
        side
    }

    fn render_documents(&mut self, cx: &mut Context<Self>) -> Div {
        let Some(p) = self.page.as_ref() else {
            let note = if self.finding { t!("Looking for a table with documents…") } else { t!("Choose a table to see its documents. Read-only.") };
            return v_flex().flex_1().child(ui::card(cx).child(ui::muted(note, cx)));
        };
        let query = self.search.read(cx).text().to_string();
        let keys = columns_of(&p.docs);
        let dates: Vec<bool> = keys.iter().map(|k| is_date(k, &p.docs)).collect();
        // Campos que apuntan a otra tabla: los del esquema (`v.id("tabla")`) o, sin esquema, los
        // que solo traen ids de Convex.
        let table_refs = self.refs.get(&p.table);
        let links: Vec<Option<String>> = keys
            .iter()
            .map(|k| {
                if k == "_id" {
                    return None;
                }
                match table_refs.and_then(|r| r.get(k)) {
                    Some(t) => Some(t.clone()),
                    None => is_id_column(k, &p.docs).then(String::new),
                }
            })
            .collect();
        let tint = self.tint;
        let columns: Vec<Column> = keys
            .iter()
            .zip(&dates)
            .zip(&links)
            .map(|((k, date), link)| {
                let numeric = !date && !p.docs.is_empty() && p.docs.iter().all(|d| d[k.as_str()].is_number() || d[k.as_str()].is_null()) && p.docs.iter().any(|d| d[k.as_str()].is_number());
                let title = match link.as_deref() {
                    Some(t) if !t.is_empty() => format!("{k} → {t}"),
                    _ => k.clone(),
                };
                let c = Column::new(k.clone(), title).width(rems(if k == "_id" || link.is_some() { 15. } else if *date { 11. } else { 10. }));
                // Números: alineados, con su promedio al pie y, con «Colores», teñidos según dónde
                // cae cada uno entre el menor y el mayor de la columna.
                match (numeric, tint) {
                    (true, true) => c.end().aggregate(Aggregate::Average).scale(),
                    (true, false) => c.end().aggregate(Aggregate::Average),
                    _ => c,
                }
            })
            .collect();
        let all_rows: Rc<Vec<Row>> = Rc::new(
            p.docs
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let key = d["_id"].as_str().map(str::to_string).unwrap_or_else(|| i.to_string());
                    Row::new(
                        key,
                        keys.iter().zip(&dates).zip(&links).map(|((k, date), link)| match (&d[k.as_str()], link) {
                            // Un id de otra tabla, como etiqueta: se distingue del texto.
                            (Value::String(id), Some(_)) => Cell::Tag(id.clone().into(), Tone::Info),
                            (v, _) => cell_of(v, *date),
                        }),
                    )
                })
                .collect(),
        );
        let titles: Vec<(SharedString, SharedString)> = keys.iter().map(|k| (SharedString::from(k.clone()), SharedString::from(k.clone()))).collect();
        let me = cx.entity();
        let (export_cols, export_rows) = (columns.clone(), all_rows.clone());
        // Con búsqueda, filtros u orden, la tabla trabaja sobre lo leído (con su propio
        // paginador); si no, se pagina sobre el total de la tabla y se lee lo que falte.
        let local = !query.trim().is_empty() || !self.rules.0.is_empty() || !self.sorts.is_empty();
        let pages = page_count(p.total, p.docs.len(), p.done || p.capped);
        let ix = p.ix.min(pages - 1);
        let rows: Rc<Vec<Row>> = if local { all_rows.clone() } else { Rc::new(all_rows.iter().skip(ix * PAGE_ROWS).take(PAGE_ROWS).cloned().collect()) };
        let toolbar = TableToolbar::new("data-tools")
            .search(&self.search)
            .filter(self.rules.0.len(), {
                let me = me.clone();
                move |_, cx| me.update(cx, |v, cx| {
                    v.builders = !v.builders;
                    cx.notify();
                })
            })
            .columns(titles.clone(), self.hidden.clone(), {
                let me = me.clone();
                move |keys, _, cx| me.update(cx, |v, cx| {
                    v.hidden = keys.to_vec();
                    cx.notify();
                })
            })
            .density(self.density, {
                let me = me.clone();
                move |density, _, cx| me.update(cx, |v, cx| {
                    v.density = density;
                    cx.notify();
                })
            })
            .export({
                let me = me.clone();
                move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(to_csv(&export_cols, &export_rows)));
                    let n = export_rows.len();
                    me.update(cx, |v, cx| {
                        v.copied = Some(n);
                        cx.notify();
                    });
                }
            });
        let builders = (self.builders && !titles.is_empty()).then(|| {
            let (m1, m2) = (me.clone(), me.clone());
            h_flex()
                .gap_6()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(Tokens::current(cx).card_border)
                .child(div().flex_1().child(FilterBuilder::new("data-filter", titles.clone()).rules(self.rules.0.clone(), self.rules.1).on_change(move |rules, any, _, cx| {
                    m1.update(cx, |v, cx| {
                        v.rules = (rules.to_vec(), any);
                        cx.notify();
                    })
                })))
                .child(div().w_96().child(SortBuilder::new("data-sort", titles.clone()).keys(self.sorts.clone()).on_change(move |keys, _, cx| {
                    m2.update(cx, |v, cx| {
                        v.sorts = keys.to_vec();
                        cx.notify();
                    })
                })))
        });

        let (loaded, total) = (p.docs.len(), p.total);
        let (loading, capped) = (p.loading, p.capped);
        let header = h_flex()
            .gap_2()
            .items_center()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(p.table.clone()))
            .child(ui::muted(
                match total {
                    Some(n) => t!("{a} documents", a = n),
                    None => t!("{a} documents loaded", a = loaded),
                },
                cx,
            ))
            .when(!self.selected.is_empty(), |h| h.child(ui::muted(t!("· {a} selected", a = self.selected.len()), cx)))
            .when_some(self.copied, |h, n| h.child(ui::muted(t!("· copied {a} rows as CSV", a = n), cx)))
            .when(capped && local, |h| h.child(ui::muted(t!("· the Convex CLI reads at most the newest {a}", a = 2000), cx)))
            .when(local && !p.done && total.is_some_and(|n| n as usize > loaded), |h| h.child(ui::muted(t!("· searching the {a} documents read", a = loaded), cx)))
            .child(div().flex_1())
            .child(Checkbox::new("data-tint").label(t!("Colors")).checked(self.tint).tooltip(t!("Tint numbers by where they fall between the column's lowest (red) and highest (green)")).on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.tint = *checked;
                cx.notify();
            })))
            .child(Button::new("data-reload").xsmall().ghost().icon(IconName::RefreshCw).label(t!("Reload")).on_click(cx.listener(|this, _, _, cx| {
                if let Some(table) = this.page.as_ref().map(|p| p.table.clone()) {
                    this.open_table(table, cx);
                }
            })));

        let mut col = v_flex().flex_1().min_w_0().h_full().gap_2().child(header).child(toolbar).children(builders);
        if let Some(e) = &p.error {
            col = col.child(ui::muted(e.clone(), cx));
        }
        if p.docs.is_empty() {
            return col.child(ui::card(cx).child(ui::muted(if p.loading { t!("Reading documents…") } else { t!("This table has no documents.") }, cx)));
        }
        // La tabla ocupa el alto que queda y desplaza sus filas por dentro (virtualizada): el
        // paginador, que va dentro de ese alto, siempre queda a la vista.
        let table_el = DataTable::new("data-table", columns)
            .rows(rows.clone())
            .query(query)
            .filters(self.rules.0.clone(), self.rules.1)
            .sorts(self.sorts.clone())
            .hidden(self.hidden.clone())
            .when(local, |t| t.paged(PAGE_ROWS))
            .virtualized()
            .density(self.density)
            .selected(self.selected.clone())
            .on_select(cx.listener(|this, keys: &[SharedString], window, cx| this.select(keys.to_vec(), window, cx)))
            .size_full();
        // Con una sola fila elegida, el documento entero al lado y un botón para copiarlo.
        let panel = self.doc.as_ref().map(|(editor, text)| {
            let text = text.clone();
            v_flex()
                .w_96()
                .flex_none()
                .h_full()
                .rounded_md()
                .border_1()
                .border_color(Tokens::current(cx).card_border)
                .overflow_hidden()
                .child(
                    h_flex()
                        .px_3()
                        .py_1p5()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().min_w_0().child(ui::section(t!("Document"), cx)))
                        .child(Clipboard::new("data-doc-copy").value(text).tooltip(t!("Copy JSON"))),
                )
                .child(div().flex_1().min_h_0().child(Editor::new(editor).readonly(true).bordered(false).h_full()))
        });
        // Doble clic en una fila copia su `_id` (la clave de la fila), como en el dashboard de
        // Convex. El primer clic ya la eligió; la tabla de Ely no dice en qué celda fue.
        let table = div().flex_1().min_h_0().min_w_0().on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, window, cx| {
            if e.click_count == 2 {
                if let [id] = this.selected.as_slice() {
                    this.copy_id(id.clone(), window, cx);
                }
            }
        }));
        // El paginador del total (sin búsqueda ni filtros): páginas de `PAGE_ROWS`, contadas del
        // tamaño de la tabla; ir a una que no se leyó la lee.
        let pager = (!local).then(|| {
            let first = ix * PAGE_ROWS;
            let shown = rows.len();
            h_flex()
                .gap_3()
                .items_center()
                .child(ui::muted(
                    match total {
                        Some(n) => t!("{a}–{b} of {c}", a = (first + 1).min(n as usize), b = first + shown, c = n),
                        None => t!("{a}–{b}", a = first + 1, b = first + shown),
                    },
                    cx,
                ))
                .when(loading, |h| h.child(ui::muted(t!("Reading…"), cx)))
                .when(capped, |h| h.child(ui::muted(t!("· the Convex CLI reads at most the newest {a}", a = 2000), cx)))
                .child(div().flex_1())
                .child(Pagination::new("data-pages").current_page(ix + 1).total_pages(pages).visible_pages(7).on_click(cx.listener(|this, page: &usize, _, cx| this.go_to(page.saturating_sub(1), cx))))
        });
        let main = v_flex().flex_1().min_w_0().h_full().gap_2().child(table.child(table_el)).children(pager);
        col.child(h_flex().flex_1().min_h_0().items_stretch().gap_3().child(main).children(panel))
    }
}

impl Render for DataView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // DEVTOOL_TRACE=1: cada render, para comprobar que la caché evita los de más.
        if std::env::var_os("DEVTOOL_TRACE").is_some() {
            eprintln!("render DataView {}", self.env);
        }
        // Las reglas de datos (Configuración) no dejan leer este tipo de entorno.
        if !self.readable {
            let (title, body) = if self.kind == "cloud-prod" {
                (t!("Production data isn't shown"), t!("Reading production records is off in Settings (Data and changes). To look at real data safely, restore a production backup into a throwaway environment (the rehearsal does it) and open its Data tab."))
            } else {
                (t!("This environment's data isn't shown"), t!("Reading records of this kind of environment is off in Settings (Data and changes)."))
            };
            return div()
                .px_4()
                .pt_3()
                .child(ui::card(cx).max_w(rems(45.)).child(div().font_weight(FontWeight::SEMIBOLD).child(title)).child(ui::muted(body, cx)))
                .into_any_element();
        }
        let side = self.render_tables(cx);
        let main = self.render_documents(cx);
        h_flex().size_full().items_stretch().gap_4().px_4().pt_3().pb_4().child(side).child(main).into_any_element()
    }
}

/// Un campo que guarda un instante en ms (`_creationTime`, `…At`, `…Time`, `…Date`).
/// Milisegundos entre 2000 y 2100: ningún contador común cae ahí.
const MS_RANGE: std::ops::Range<f64> = 946_684_800_000.0..4_102_444_800_000.0;
/// Lo mismo en segundos (solo se cree si el nombre lo dice).
const S_RANGE: std::ops::Range<f64> = 946_684_800.0..4_102_444_800.0;

/// Si una columna es de fechas: todos sus valores son números en milisegundos entre 2000 y 2100
/// (con cualquier nombre), o en segundos si el nombre dice que es una fecha (`…At`, `…_at`,
/// `…Time`, `timestamp`…).
fn is_date(key: &str, docs: &[Value]) -> bool {
    let values: Vec<&Value> = docs.iter().map(|d| &d[key]).filter(|v| !v.is_null()).collect();
    if values.is_empty() || !values.iter().all(|v| v.is_number()) {
        return false;
    }
    let nums: Vec<f64> = values.iter().filter_map(|v| v.as_f64()).collect();
    if nums.iter().all(|n| MS_RANGE.contains(n)) {
        return true;
    }
    let lower = key.to_lowercase();
    let named = key.ends_with("At") || key.ends_with("Time") || key.ends_with("Date") || ["_at", "_time", "_date", "timestamp", "expires", "expiry"].iter().any(|s| lower.ends_with(s));
    named && nums.iter().all(|n| S_RANGE.contains(n))
}

/// Si un texto parece un id de Convex: 31 a 37 caracteres del alfabeto base32 de Convex, con al
/// menos una letra después de la `f` (así un hash hexadecimal no pasa por id).
fn looks_like_id(s: &str) -> bool {
    const ALPHABET: &str = "0123456789abcdefghjkmnpqrstvwxyz";
    (31..=37).contains(&s.len()) && s.chars().all(|c| ALPHABET.contains(c)) && s.chars().any(|c| c > 'f')
}

/// Una columna que solo trae ids de Convex (al menos uno).
fn is_id_column(key: &str, docs: &[Value]) -> bool {
    let values: Vec<&Value> = docs.iter().map(|d| &d[key]).filter(|v| !v.is_null()).collect();
    !values.is_empty() && values.iter().all(|v| v.as_str().is_some_and(looks_like_id))
}

/// Del esquema simplificado del daemon: tabla → campo → tabla a la que apunta (el primer
/// `v.id(...)` del campo).
fn refs_of(schema: &Value) -> HashMap<String, HashMap<String, String>> {
    let mut out = HashMap::new();
    for t in schema["tables"].as_array().into_iter().flatten() {
        let Some(name) = t["name"].as_str() else { continue };
        let fields: HashMap<String, String> = t["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["name"] != "_id")
            .filter_map(|f| Some((f["name"].as_str()?.to_string(), f["refs"].as_array()?.first()?.as_str()?.to_string())))
            .collect();
        out.insert(name.to_string(), fields);
    }
    out
}

/// Columnas: `_id` primero, luego en el orden en que aparecen, `_creationTime` al final.
pub fn columns_of(docs: &[Value]) -> Vec<String> {
    let mut keys: Vec<String> = vec![];
    for d in docs {
        for k in d.as_object().into_iter().flat_map(|o| o.keys()) {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
    }
    keys.retain(|k| k != "_id" && k != "_creationTime");
    keys.insert(0, "_id".into());
    keys.push("_creationTime".into());
    keys
}

fn date_text(ms: f64) -> String {
    jiff::Timestamp::from_millisecond(ms as i64)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Texto de una sola línea: la celda se dibuja como una línea y GPUI no acepta saltos en ella
/// (entra en pánico). Los saltos se ven como «↵»; el documento entero está en el panel.
fn one_line(text: &str) -> String {
    if text.contains(['\n', '\r']) {
        text.replace("\r\n", "↵ ").replace(['\n', '\r'], "↵ ")
    } else {
        text.to_string()
    }
}

/// Cuántas páginas de `PAGE_ROWS` tiene la tabla: del total si se sabe; si no (la CLI), las
/// leídas y una más mientras queden documentos.
fn page_count(total: Option<u64>, loaded: usize, done: bool) -> usize {
    let n = match total {
        Some(n) => n as usize,
        None if done => loaded,
        None => loaded + 1,
    };
    n.div_ceil(PAGE_ROWS).max(1)
}

/// La búsqueda de tablas: cada palabra tiene que estar en el nombre, en cualquier parte y orden,
/// sin distinguir mayúsculas ni `_`/`-` («rate limit» encuentra `authRateLimits`).
fn table_matches(name: &str, query: &str) -> bool {
    let plain = |s: &str| s.to_lowercase().replace(['_', '-'], "");
    let name = plain(name);
    query.split_whitespace().all(|w| name.contains(&plain(w)))
}

/// Una celda: números como números (se suman y se ordenan), fechas legibles, el resto como texto.
pub fn cell_of(v: &Value, date: bool) -> Cell {
    match v {
        Value::Null => Cell::Empty,
        // En segundos si no llega a milisegundos.
        Value::Number(n) if date => {
            let n = n.as_f64().unwrap_or(0.);
            Cell::Text(date_text(if n < 1e11 { n * 1000. } else { n }).into())
        }
        Value::Number(n) => Cell::Number(n.as_f64().unwrap_or(0.)),
        Value::Bool(b) => Cell::Text(if *b { "true".into() } else { "false".into() }),
        Value::String(s) => Cell::Text(one_line(s).into()),
        // Un objeto o una lista en una línea (su JSON compacto ya no tiene saltos, salvo dentro
        // de sus textos, que van escapados).
        other => Cell::Text(one_line(&other.to_string()).into()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn columnas_y_celdas() {
        let docs = vec![json!({ "_id": "a", "_creationTime": 1792906246883.0, "n": 3.0, "name": "x" }), json!({ "_id": "b", "_creationTime": 1792906246884.0, "tags": ["p"], "n": 5.0 })];
        assert_eq!(super::columns_of(&docs), ["_id", "n", "name", "tags", "_creationTime"]);
        assert!(super::is_date("_creationTime", &docs));
        assert!(!super::is_date("n", &docs));
        assert_eq!(super::cell_of(&json!(["p"]), false), ely_core::tables::Cell::Text("[\"p\"]".into()));
        assert_eq!(super::cell_of(&json!(null), false), ely_core::tables::Cell::Empty);
        assert_eq!(super::cell_of(&json!(5.0), false), ely_core::tables::Cell::Number(5.0));
        // Saltos de línea: la celda es de una línea (GPUI entra en pánico con «\n»).
        assert_eq!(super::cell_of(&json!("uno\ndos\r\ntres"), false), ely_core::tables::Cell::Text("uno↵ dos↵ tres".into()));
    }

    #[test]
    fn fechas_e_ids() {
        let docs = vec![json!({ "expiresAt": 1792906246, "created_at": 1792906246883.0, "count": 3, "user": "kx7156mze9233v3ax02y8qnsxx8gzb12", "hash": "d41d8cd98f00b204e9800998ecf8427e" })];
        assert!(super::is_date("expiresAt", &docs));
        assert!(super::is_date("created_at", &docs));
        assert!(!super::is_date("count", &docs));
        assert!(super::is_id_column("user", &docs));
        assert!(!super::is_id_column("hash", &docs));
        let schema = json!({ "tables": [{ "name": "posts", "fields": [{ "name": "_id", "refs": [] }, { "name": "author", "refs": ["users"] }, { "name": "title", "refs": [] }] }] });
        let refs = super::refs_of(&schema);
        assert_eq!(refs["posts"].get("author").map(String::as_str), Some("users"));
        assert!(refs["posts"].get("title").is_none());
    }

    #[test]
    fn paginas_del_total() {
        assert_eq!(super::page_count(Some(240), 50, false), 5);
        assert_eq!(super::page_count(Some(0), 0, true), 1);
        assert_eq!(super::page_count(None, 100, false), 3);
        assert_eq!(super::page_count(None, 100, true), 2);
    }

    #[test]
    fn busqueda_por_contiene() {
        assert!(super::table_matches("authRateLimits", "rate"));
        assert!(super::table_matches("authRateLimits", "limit RATE"));
        assert!(super::table_matches("live_pulses", "livepul"));
        assert!(super::table_matches("live_pulses", ""));
        assert!(!super::table_matches("live_pulses", "rate"));
    }

    #[test]
    fn orden_de_tablas_con_documentos_primero() {
        let v = json!({ "tables": [{ "name": "a", "count": 0 }, { "name": "b", "count": 3 }, { "name": "c", "count": null }] });
        let names: Vec<&str> = super::DataView::ordered(&v).iter().filter_map(|x| x["name"].as_str()).collect();
        assert_eq!(names, ["b", "a", "c"]);
    }
}
