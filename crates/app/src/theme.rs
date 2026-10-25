// Tema plano y compacto, al estilo de Zed y Cursor: grises neutros, bordes
// sutiles y rectos, sin sombras, la tipografía de Zed (IBM Plex Sans, que
// trae Ely) y el acento solo en lo puntual (lo elegido, el botón principal).
// Se aplica a los dos temas, el de GPUI Kit y el de Ely, para que se vean
// como una sola cosa.

use ely_core::theme::{Mode as ElyMode, Palette, Theme as ElyTheme};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

/// Modo elegido en la app; `System` sigue a macOS.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    System,
    Light,
    Dark,
}

impl Choice {
    pub fn parse(s: &str) -> Self {
        match s {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Siguiente al tocar el botón de tema: sistema → oscuro → claro → sistema.
    pub fn next(self) -> Self {
        match self {
            Self::System => Self::Dark,
            Self::Dark => Self::Light,
            Self::Light => Self::System,
        }
    }
}

/// Fuente de la interfaz (la de Zed) y la monoespaciada, las dos incluidas en Ely.
pub const UI_FONT: &str = "IBM Plex Sans";
pub const MONO_FONT: &str = "JetBrains Mono";
/// Tamaño base del texto: compacto, como un editor.
pub const UI_SIZE: f32 = 13.;

/// Los tonos de la interfaz.
#[derive(Clone, Copy)]
pub struct Tokens {
    /// Barra lateral.
    pub ground: Hsla,
    /// El contenido.
    pub panel: Hsla,
    pub card: Hsla,
    pub card_border: Hsla,
    pub hover: Hsla,
    pub active: Hsla,
    pub fg: Hsla,
    pub muted: Hsla,
    pub subtle: Hsla,
    /// Amarillo de la marca, para rellenos (botón principal, resaltados).
    pub accent: Hsla,
    /// El acento como texto, línea o ícono sobre el fondo: amarillo en oscuro (9:1); en claro, un
    /// dorado oscuro (#A16207, 4,9:1), porque el amarillo sobre blanco no llega a 2:1.
    pub accent_ink: Hsla,
    /// Texto sobre el amarillo (9,5:1).
    pub on_accent: Hsla,
}

/// Amarillo de la marca (#F3B01C).
pub const BRAND_YELLOW: u32 = 0xF3B01C;

fn hex(v: u32) -> Hsla {
    rgb(v).into()
}

impl Tokens {
    pub fn of(dark: bool) -> Self {
        if dark {
            Self {
                ground: hex(0x181818),
                panel: hex(0x1c1c1c),
                card: hex(0x1c1c1c),
                card_border: hex(0x2b2b2b),
                hover: hex(0x262626),
                active: hex(0x2d2d2d),
                fg: hex(0xd6d6d6),
                muted: hex(0x8f8f8f),
                subtle: hex(0x6b6b6b),
                accent: hex(BRAND_YELLOW),
                accent_ink: hex(BRAND_YELLOW),
                on_accent: hex(0x1f1503),
            }
        } else {
            Self {
                ground: hex(0xf6f6f6),
                panel: hex(0xffffff),
                card: hex(0xffffff),
                card_border: hex(0xe4e4e4),
                hover: hex(0xefefef),
                active: hex(0xe7e7e7),
                fg: hex(0x1f1f1f),
                muted: hex(0x666666),
                subtle: hex(0x8c8c8c),
                accent: hex(BRAND_YELLOW),
                accent_ink: hex(0xa16207),
                on_accent: hex(0x1f1503),
            }
        }
    }

    pub fn current(cx: &App) -> Self {
        use gpui_kit::component::ActiveTheme;
        Self::of(cx.theme().is_dark())
    }
}

fn system_dark(window: &Window) -> bool {
    matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// Cambia el modo de los dos temas y les pone los colores del ERP.
pub fn apply(choice: Choice, window: &mut Window, cx: &mut App) {
    let dark = match choice {
        Choice::System => system_dark(window),
        Choice::Light => false,
        Choice::Dark => true,
    };
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
    let t = Tokens::of(dark);
    Theme::update(cx, |theme| {
        // Plano: bordes rectos, sin sombras, texto compacto.
        theme.radius = px(0.);
        theme.radius_lg = px(0.);
        theme.shadow = false;
        theme.font_family = UI_FONT.into();
        theme.font_size = px(UI_SIZE);
        theme.mono_font_family = MONO_FONT.into();
        theme.mono_font_size = px(12.);
        let c = &mut theme.colors;
        c.background = t.card;
        c.foreground = t.fg;
        c.border = t.card_border;
        c.muted = t.hover;
        c.muted_foreground = t.muted;
        c.popover = t.card;
        c.popover_foreground = t.fg;
        c.sidebar = t.ground;
        c.sidebar_border = t.card_border;
        c.sidebar_foreground = t.fg;
        c.sidebar_accent = t.hover;
        c.sidebar_accent_foreground = t.fg;
        c.list_hover = t.hover;
        c.list_active = t.active;
        c.secondary = t.hover;
        c.secondary_hover = t.active;
        c.secondary_foreground = t.fg;
        c.input = t.card_border;
        // Lo principal de Kit en el amarillo de la marca, con texto oscuro (también en oscuro).
        c.primary = t.accent;
        c.primary_hover = hex(0xe5a512);
        c.primary_active = hex(0xd4980e);
        c.primary_foreground = t.on_accent;
        c.button_primary = t.accent;
        c.button_primary_hover = c.primary_hover;
        c.button_primary_active = c.primary_active;
        c.button_primary_foreground = t.on_accent;
        // Lo demás que en Kit viene en azul y no es un estado: enlaces, selección, bordes de lo
        // activo, arrastre, interruptores, barras de progreso y deslizadores.
        c.link = t.accent_ink;
        c.link_hover = t.accent_ink;
        c.link_active = t.accent_ink;
        c.selection = t.accent.opacity(0.35);
        c.list_active_border = t.accent_ink;
        c.table_active_border = t.accent_ink;
        c.drag_border = t.accent_ink;
        c.drop_target = t.accent.opacity(0.15);
        // El fondo del interruptor apagado (encendido usa el primario): gris, para que se note.
        c.switch = t.subtle.opacity(0.7);
        c.progress_bar = t.accent;
        c.slider_bar = t.accent;
        c.slider_thumb = t.accent;
        c.sidebar_primary = t.accent;
        c.sidebar_primary_foreground = t.on_accent;
        c.tab_bar = t.panel;
        c.tab_active = t.panel;
        c.title_bar = t.ground;
        c.title_bar_border = t.card_border;
        c.ring = t.card_border;
        c.window_border = t.card_border;
    });
    let mut palette = if dark { Palette::dark(false) } else { Palette::light(false) };
    palette.bg = t.ground;
    palette.surface = t.card;
    palette.sunken = t.panel;
    palette.overlay = t.card;
    palette.hover = t.hover;
    palette.active = t.active;
    palette.border = t.card_border;
    palette.fg = t.fg;
    palette.fg_muted = t.muted;
    // Ely usa el acento como relleno y como texto: el tono legible de cada modo, y encima el
    // texto que contrasta con él (oscuro sobre amarillo, blanco sobre el dorado de modo claro).
    palette.accent = t.accent_ink;
    palette.on_accent = if dark { t.on_accent } else { hex(0xffffff) };
    // Compacto, como el resto de la app.
    ElyTheme::update(cx, |t| t.density = ely_core::theme::Density::Compact);
    let mode = if dark { ElyMode::Dark } else { ElyMode::Light };
    ElyTheme::set_palette(mode, Some(palette), cx);
    ElyTheme::set_mode_now(mode, cx);
    window.refresh();
}
