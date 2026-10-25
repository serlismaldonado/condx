// Piezas de interfaz compartidas por las vistas, planas y compactas: bloques
// con borde sutil y recto, etiquetas de sección pequeñas, cifras sin adornos,
// estados como punto + texto, filas clave/valor y formato de tiempos. El
// color queda para lo que lo necesita (estados, producción, lo elegido).

use gpui_kit::component::{h_flex, v_flex, ActiveTheme};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;

pub fn now_ms() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// «hace 12 s», «hace 3 min», «hace 2 h», «hace 4 d».
pub fn ago(ms: i64) -> String {
    let s = ((now_ms() - ms) / 1000).max(0);
    match s {
        0..=59 => t!("{s}s ago", s = s),
        60..=3599 => t!("{a} min ago", a = s / 60),
        3600..=86_399 => t!("{a} h ago", a = s / 3600),
        _ => t!("{a} d ago", a = s / 86_400),
    }
}

/// Hora local «HH:MM:SS» de un instante en ms.
pub fn clock(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Texto de un valor JSON, o «—» si falta.
pub fn txt(v: &Value) -> SharedString {
    match v {
        Value::Null => "—".into(),
        Value::String(s) if s.is_empty() => "—".into(),
        Value::String(s) => s.clone().into(),
        other => other.to_string().into(),
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Health {
    Ok,
    Warn,
    Bad,
    Unknown,
}

/// La carpeta del usuario (para mostrar rutas con ~). En la demo de la web no hay.
pub fn user_home() -> Option<String> {
    #[cfg(not(target_family = "wasm"))]
    {
        dirs::home_dir().map(|h| h.display().to_string())
    }
    #[cfg(target_family = "wasm")]
    {
        None
    }
}

/// Interruptor chico (22×12; el de Kit no baja de 28×16): amarillo con el punto a la derecha
/// encendido, gris con el punto a la izquierda apagado. El dueño le pone su `on_click`.
pub fn toggle(id: impl Into<ElementId>, on: bool, cx: &App) -> Stateful<Div> {
    let t = crate::theme::Tokens::current(cx);
    div()
        .id(id)
        .flex_none()
        .w(px(22.))
        .h(px(12.))
        .px(px(2.))
        .flex()
        .items_center()
        .when(on, |d| d.justify_end())
        .rounded_full()
        .cursor_pointer()
        .bg(if on { t.accent } else { t.subtle.opacity(0.7) })
        .child(div().size(px(8.)).rounded_full().bg(if on { t.on_accent } else { t.fg.opacity(0.85) }))
}

pub fn dot(health: Health, cx: &App) -> Div {
    let t = cx.theme();
    let color = match health {
        Health::Ok => t.success,
        Health::Warn => t.warning,
        Health::Bad => t.danger,
        Health::Unknown => t.muted_foreground,
    };
    div().size(px(8.)).rounded_full().bg(color).flex_none()
}

/// Estado como punto de color y texto apagado.
pub fn status_tag(health: Health, label: impl Into<SharedString>) -> Div {
    h_flex().gap_1p5().items_center().flex_none().child(DotOnly(health)).child(Label(label.into()))
}

/// El punto de `status_tag` (necesita el tema al dibujarse).
#[derive(IntoElement)]
struct DotOnly(Health);

impl RenderOnce for DotOnly {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        dot(self.0, cx).size(px(7.))
    }
}

#[derive(IntoElement)]
struct Label(SharedString);

impl RenderOnce for Label {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div().text_size(px(12.)).text_color(crate::theme::Tokens::current(cx).muted).child(self.0)
    }
}

/// Tipo de entorno: texto pequeño con borde; solo producción lleva color.
pub fn kind_tag(kind: &str) -> Div {
    let label = match kind {
        "cloud-prod" => t!("production"),
        "cloud-dev" => t!("cloud dev"),
        "devbox" => "devbox",
        "throwaway" => t!("throwaway"),
        other => other,
    };
    h_flex().child(KindLabel(label.to_string().into(), kind == "cloud-prod"))
}

#[derive(IntoElement)]
struct KindLabel(SharedString, bool);

impl RenderOnce for KindLabel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = crate::theme::Tokens::current(cx);
        let color: Hsla = if self.1 { rgb(0xe5756e).into() } else { t.muted };
        div().px_1p5().h(px(18.)).flex().items_center().border_1().border_color(if self.1 { color.opacity(0.5) } else { t.card_border }).text_size(px(11.)).text_color(color).child(self.0)
    }
}

/// Contenedor de algo con scroll propio (logs, JSON, listas, el diagrama):
/// la rueda mueve solo eso, no la página de alrededor. GPUI desplaza el
/// contenedor de adentro y sigue pasando el evento hacia afuera.
pub fn contained() -> Div {
    div().on_scroll_wheel(|_, _, cx| cx.stop_propagation())
}

/// Bordes de una lista interna: (arriba, abajo, se sabe). Lo escribe quien mide, lo lee la rueda.
type EdgeCell = std::rc::Rc<std::cell::Cell<(bool, bool, bool)>>;

fn edges_of(off: Point<Pixels>, max: Point<Pixels>) -> (bool, bool, bool) {
    // Sin nada que desplazar, la rueda es de la página.
    if max.y <= px(0.5) {
        (true, true, true)
    } else {
        (off.y >= px(-0.5), -off.y >= max.y - px(0.5), true)
    }
}

/// El contenedor: mientras la lista pueda moverse en la dirección de la rueda, la página no se
/// mueve; en el borde (arriba del todo y hacia arriba, abajo del todo y hacia abajo), sí.
fn chain(at: EdgeCell) -> Div {
    div().relative().on_scroll_wheel(move |ev, window, cx| {
        let (top, bottom, known) = at.get();
        let dy = f32::from(ev.delta.pixel_delta(window.line_height()).y);
        // En GPUI, rueda hacia arriba = delta positivo.
        if !(known && ((dy > 0. && top) || (dy < 0. && bottom))) {
            cx.stop_propagation();
        }
    })
}

/// Una lista nuestra con su `ScrollHandle` (la del hijo con `track_scroll`).
pub fn chained(handle: ScrollHandle) -> Div {
    let at: EdgeCell = Default::default();
    let probe = at.clone();
    chain(at).child(canvas(move |_, _, _| probe.set(edges_of(handle.offset(), handle.max_offset())), |_, _, _, _| {}).absolute().size_0())
}

/// El `LogViewer` de Ely con id `id` (u otro contenido mientras no hay líneas). Ely guarda su
/// scroll en el estado `(id, "scroll")`; `Watch` lo lee al dibujarse, en la misma fase y con la
/// misma ruta de ids que el visor, así que obtiene el mismo.
pub fn chained_log(id: impl Into<ElementId>, child: AnyElement) -> Div {
    let at: EdgeCell = Default::default();
    chain(at.clone()).child(Watch { key: id.into(), at, child })
}

#[derive(IntoElement)]
struct Watch {
    key: ElementId,
    at: EdgeCell,
    child: AnyElement,
}

impl RenderOnce for Watch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // GPUI dibuja cada componente bajo el nombre de su tipo; el visor queda dentro de este,
        // así que su estado está en [.., Watch, LogViewer, (id, "scroll")].
        let viewer = ElementId::from(std::any::type_name::<ely_terminal::LogViewer>());
        let h = window.with_id(viewer, |window| window.use_keyed_state((self.key, "scroll"), cx, |_, _| UniformListScrollHandle::new()).read(cx).clone());
        let st = h.0.borrow();
        self.at.set(if st.last_item_size.is_some() { edges_of(st.base_handle.offset(), st.base_handle.max_offset()) } else { (false, false, false) });
        drop(st);
        div().size_full().child(self.child)
    }
}

/// Botón principal: el amarillo de la marca, con texto oscuro (9,5:1, igual en claro y en oscuro
/// porque el botón lleva su propio fondo). Es el `primary` de Kit, que el tema pone en amarillo
/// (`theme::apply`); la variante `custom` de Kit no sirve: pinta el color al 20 %.
pub fn brand(button: gpui_kit::component::button::Button, _cx: &App) -> gpui_kit::component::button::Button {
    use gpui_kit::component::button::ButtonVariants;
    button.primary()
}

/// El principal de la marca si `primary`; si no, con contorno.
pub fn brand_if(primary: bool, button: gpui_kit::component::button::Button, cx: &App) -> AnyElement {
    if primary {
        brand(button, cx).into_any_element()
    } else {
        button.outline().into_any_element()
    }
}

pub fn card(cx: &App) -> Div {
    let t = crate::theme::Tokens::current(cx);
    v_flex().gap_2().p_3().border_1().border_color(t.card_border).bg(t.card)
}

/// Etiqueta de sección: versalitas pequeñas y espaciadas (como «TOTAL FACTURAS» en el ERP).
pub fn section(title: impl Into<SharedString>, cx: &App) -> Div {
    let t = crate::theme::Tokens::current(cx);
    div().text_size(px(10.5)).text_color(t.subtle).child(title.into().to_uppercase())
}

/// Tonos de los íconos en círculo de las tarjetas de cifras.
#[derive(Clone, Copy)]
pub enum Hue {
    /// El amarillo de la marca (lo decorativo; los estados usan los demás).
    Brand,
    Green,
    Amber,
    Red,
    Violet,
}

impl Hue {
    pub fn color(self) -> Hsla {
        rgb(match self {
            Hue::Brand => crate::theme::BRAND_YELLOW,
            Hue::Green => 0x10b981,
            Hue::Amber => 0xf59e0b,
            Hue::Red => 0xef4444,
            Hue::Violet => 0x8b5cf6,
        })
        .into()
    }
}

/// Ícono de una tarjeta, apagado (el tono queda para quien lo pida).
pub fn icon_circle(icon: ely_core::primitives::IconName, hue: Hue, cx: &App) -> Div {
    let _ = hue.color();
    let t = crate::theme::Tokens::current(cx);
    div().flex_none().child(ely_core::primitives::Icon::new(icon).size(ely_core::theme::IconSize::Sm).color(t.subtle))
}

/// Tarjeta de cifra del tablero del ERP: número grande, etiqueta en versalitas,
/// un rótulo opcional y el ícono a la derecha.
pub fn stat_card(
    value: impl Into<SharedString>,
    label: impl Into<SharedString>,
    caption: Option<&str>,
    icon: ely_core::primitives::IconName,
    hue: Hue,
    cx: &App,
) -> Div {
    let t = crate::theme::Tokens::current(cx);
    let mut head = h_flex().gap_1p5().items_center().child(icon_circle(icon, hue, cx)).child(div().text_size(px(11.)).text_color(t.muted).truncate().child(label.into()));
    if let Some(c) = caption {
        head = head.child(div().text_size(px(11.)).text_color(t.subtle).truncate().child(format!("· {c}")));
    }
    card(cx).flex_1().min_w(px(150.)).gap_1().child(head).child(div().text_size(px(20.)).text_color(t.fg).font_family(crate::theme::MONO_FONT).child(value.into()))
}

pub fn kv(label: impl Into<SharedString>, value: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .gap_3()
        .child(div().w(px(130.)).flex_none().text_color(crate::theme::Tokens::current(cx).muted).child(label.into()))
        .child(div().flex_1().min_w_0().overflow_hidden().child(value))
}

pub fn mono(text: impl Into<SharedString>) -> Div {
    div().font_family(crate::theme::MONO_FONT).text_size(px(12.)).whitespace_nowrap().child(text.into())
}

pub fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_size(px(12.)).text_color(crate::theme::Tokens::current(cx).muted).child(text.into())
}

/// Secciones plegables como el `Accordion` de Ely, pero con lo abierto en la app
/// (`DevtoolApp::folds`, clave `id:item`) para que se recuerde al volver y al reabrir.
pub struct Folds {
    id: SharedString,
    items: Vec<(&'static str, SharedString, AnyElement)>,
    first_open: bool,
}

impl Folds {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self { id: id.into(), items: vec![], first_open: false }
    }

    /// `key` identifica la sección aunque cambien su título o las que hay antes.
    pub fn item(mut self, key: &'static str, title: impl Into<SharedString>, body: impl IntoElement) -> Self {
        self.items.push((key, title.into(), body.into_any_element()));
        self
    }

    /// La primera abierta mientras no se haya tocado.
    pub fn first_open(mut self) -> Self {
        self.first_open = true;
        self
    }

    pub fn render(self, app: &crate::app::DevtoolApp, cx: &mut Context<crate::app::DevtoolApp>) -> impl IntoElement {
        let theme = ely_core::theme::ActiveTheme::theme(&**cx);
        let (border, muted) = (theme.colors.border, theme.colors.fg_muted);
        let title_size = theme.text_size(ely_core::theme::TextSize::Base);
        let id = self.id.clone();
        let rows: Vec<_> = self
            .items
            .into_iter()
            .enumerate()
            .map(|(ix, (key, title, body))| {
                let fold = format!("{id}:{key}");
                let open = app.folds.get(&fold).copied().unwrap_or(self.first_open && ix == 0);
                let chevron = ely_core::primitives::Disclosure::new(SharedString::from(format!("{id}-chevron-{key}")), open)
                    .size(ely_core::theme::IconSize::Sm)
                    .color(muted);
                div()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .id(SharedString::from(format!("{id}-head-{key}")))
                            .flex()
                            .items_center()
                            .justify_between()
                            .py_3()
                            .px_1()
                            .cursor_pointer()
                            .text_size(title_size)
                            .font_weight(FontWeight::MEDIUM)
                            .child(title)
                            .child(chevron)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.folds.insert(fold.clone(), !open);
                                cx.notify();
                            })),
                    )
                    .child(
                        ely_core::layout::Collapsible::new(SharedString::from(format!("{id}-body-{key}")), open)
                            .child(div().pb_4().px_1().text_color(muted).child(body)),
                    )
            })
            .collect();
        div().id(self.id).flex().flex_col().border_t_1().border_color(border).children(rows)
    }
}

/// Fase del deployer en el idioma de la app (el daemon la manda en inglés).
pub fn phase_label(phase: &str) -> String {
    match phase {
        "idle" => t!("idle").to_string(),
        "syncing" | "deploying" => t!("deploying").to_string(),
        "starting" => t!("starting").to_string(),
        "error" => t!("error").to_string(),
        "stopped" => t!("stopped").to_string(),
        "unknown" => t!("unknown").to_string(),
        other => other.to_string(),
    }
}
