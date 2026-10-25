// Idiomas de la app. Los textos se escriben en inglés en el código (`t!("Save")`) y
// `locales/es.json` trae su versión en español (clave = el texto en inglés). Lo que no
// está traducido se muestra en inglés.
//
//   t!("Save")                         -> &'static str
//   t!("{n} jobs running", n = count)  -> String (marcadores `{nombre}`, solo Display)

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Es,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Es];

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
        }
    }

    /// Siempre en su propio idioma, para que se encuentre aunque no se entienda el actual.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Es => "Español",
        }
    }

    pub fn parse(code: &str) -> Lang {
        match code {
            "es" => Lang::Es,
            _ => Lang::En,
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 {
        Lang::Es
    } else {
        Lang::En
    }
}

pub fn set_lang(lang: Lang) {
    LANG.store(if lang == Lang::Es { 1 } else { 0 }, Ordering::Relaxed);
}

fn spanish() -> &'static HashMap<String, String> {
    static ES: OnceLock<HashMap<String, String>> = OnceLock::new();
    ES.get_or_init(|| serde_json::from_str(include_str!("../locales/es.json")).expect("locales/es.json ilegible"))
}

/// El texto en el idioma actual (el inglés si no hay traducción).
pub fn tr(en: &'static str) -> &'static str {
    match lang() {
        Lang::En => en,
        Lang::Es => spanish().get(en).map(String::as_str).unwrap_or(en),
    }
}

/// Reemplaza los `{nombre}` de la plantilla; `{{` y `}}` quedan como llaves.
pub fn fill(template: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if let Some(end) = tail.strip_prefix('{').and_then(|t| t.find('}')) {
            let name = &tail[1..1 + end];
            if let Some((_, v)) = args.iter().find(|(k, _)| *k == name) {
                out.push_str(&v.to_string());
                rest = &tail[end + 2..];
                continue;
            }
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// Textos de los componentes de Ely (constructores de filtro y orden de las tablas) en el idioma
/// de la app. Ely no trae español: su catálogo en-US se cubre con el nuestro.
const ELY_ES: &[(&str, &str)] = &[
    ("builder.add_rule", "Agregar regla"),
    ("builder.add_sort", "Agregar orden"),
    ("builder.all", "Cumplir todas"),
    ("builder.any", "Cumplir alguna"),
    ("builder.column", "Columna"),
    ("builder.down", "Descendente"),
    ("builder.remove", "Quitar"),
    ("builder.test", "Condición"),
    ("builder.up", "Ascendente"),
    ("builder.value", "Valor"),
    ("builder.test.contains", "contiene"),
    ("builder.test.equals", "es"),
    ("builder.test.not_equals", "no es"),
    ("builder.test.above", "es mayor que"),
    ("builder.test.below", "es menor que"),
    ("builder.test.empty", "está vacío"),
];

pub fn apply_ely(cx: &mut gpui_kit::App) {
    let i18n = ely_core::i18n::I18n::new("en-US");
    cx.set_global(match lang() {
        Lang::En => i18n,
        Lang::Es => i18n.catalog("en-US", ELY_ES),
    });
}

#[macro_export]
macro_rules! t {
    ($s:literal) => {
        $crate::i18n::tr($s)
    };
    ($s:literal, $($k:ident = $v:expr),+ $(,)?) => {
        $crate::i18n::fill($crate::i18n::tr($s), &[$((stringify!($k), &$v as &dyn ::std::fmt::Display)),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rellena_marcadores_y_llaves() {
        assert_eq!(fill("{n} jobs, {{literal}}", &[("n", &3)]), "3 jobs, {literal}");
        assert_eq!(fill("use {run.commit}", &[("n", &1)]), "use {run.commit}");
    }

    #[test]
    fn el_espanol_traduce() {
        // Sin tocar el idioma global (las demás pruebas corren en paralelo y esperan inglés).
        assert_eq!(spanish().get("Save").map(String::as_str), Some("Guardar"));
        assert_eq!(fill(spanish()["{n} nodes"].as_str(), &[("n", &4)]), "4 nodos");
    }

    #[test]
    fn el_espanol_tiene_los_mismos_marcadores() {
        let re = regex_lite_placeholders;
        for (en, es) in spanish() {
            assert_eq!(re(en), re(es), "marcadores distintos en «{en}» → «{es}»");
        }
    }

    fn regex_lite_placeholders(s: &str) -> Vec<String> {
        // Solo los marcadores de `fill` (identificadores); lo demás entre llaves es texto.
        let mut v: Vec<String> = s
            .split('{')
            .skip(1)
            .filter_map(|p| p.split_once('}').map(|(n, _)| n.to_string()))
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .collect();
        v.sort();
        v
    }
}
