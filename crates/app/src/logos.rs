// Los logos de los proveedores de los agentes (la marca que manda el daemon: `agents::logo`), en el
// catálogo de agentes y en los hilos. Los SVG son de simple-icons (CC0, assets/logos/README.md) y van
// embebidos: `WithLogos` los sirve antes que los recursos de Ely y GPUI Kit. Los que no están ahí
// (Groq, Mistral, MiniMax, OpenRouter, DeepSeek) van con su inicial en su color.

use std::borrow::Cow;

use gpui_kit::*;

const SVGS: &[(&str, &[u8])] = &[
    ("logos/claude.svg", include_bytes!("../assets/logos/claude.svg")),
    ("logos/openai.svg", include_bytes!("../assets/logos/openai.svg")),
    ("logos/gemini.svg", include_bytes!("../assets/logos/gemini.svg")),
    ("logos/x.svg", include_bytes!("../assets/logos/x.svg")),
    ("logos/ollama.svg", include_bytes!("../assets/logos/ollama.svg")),
];

/// Los recursos de la app con los logos delante.
pub struct WithLogos<A>(pub A);

impl<A: AssetSource> AssetSource for WithLogos<A> {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = SVGS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        self.0.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        self.0.list(path)
    }
}

/// Color de la marca (claro, oscuro).
fn color(logo: &str) -> Option<(u32, u32)> {
    Some(match logo {
        "claude" => (0xD97757, 0xD97757),
        "gemini" => (0x8E75B2, 0xA990D0),
        "openai" | "x" | "ollama" => (0x111111, 0xEDEDED),
        _ => return None,
    })
}

fn letter(logo: &str) -> Option<(&'static str, u32)> {
    Some(match logo {
        "groq" => ("G", 0xf55036),
        "mistral" => ("M", 0xfa520f),
        "minimax" => ("M", 0xe5446d),
        "openrouter" => ("O", 0x6467f2),
        "deepseek" => ("D", 0x4d6bfe),
        _ => return None,
    })
}

/// El logo de una marca en un círculo de `size` px, o None si no se conoce.
pub fn badge(logo: &str, size: f32, cx: &App) -> Option<AnyElement> {
    let t = crate::theme::Tokens::current(cx);
    let ring = div().size(px(size)).flex_none().rounded_full().flex().items_center().justify_center();
    if let Some((light, dark)) = color(logo) {
        use gpui_kit::component::ActiveTheme;
        let c = if cx.theme().is_dark() { dark } else { light };
        return Some(ring.bg(t.card).border_1().border_color(t.card_border).child(svg().path(format!("logos/{logo}.svg")).size(px(size * 0.6)).text_color(rgb(c))).into_any_element());
    }
    let (l, c) = letter(logo)?;
    Some(ring.bg(rgb(c)).text_size(px(size * 0.45)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(0xffffff)).child(l).into_any_element())
}
