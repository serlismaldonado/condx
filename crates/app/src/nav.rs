// Navegación con el teclado en las listas: flecha arriba y abajo mueven la selección en la
// barra lateral y en la lista de tablas de Datos cuando tienen el foco. Cada lista declara su
// contexto de teclas y atiende estas acciones; un comando, una sola acción (guías de GPUI Kit).

use gpui_kit::*;

actions!(condx_nav, [SelectPrev, SelectNext, OpenPalette]);

/// Contexto de teclas de la barra lateral.
pub const SIDEBAR: &str = "CondxSidebar";
/// Contexto de teclas de la lista de tablas de Datos.
pub const TABLES: &str = "CondxTables";

pub fn bind_keys(cx: &mut App) {
    let mut keys = vec![];
    for context in [SIDEBAR, TABLES] {
        keys.push(KeyBinding::new("up", SelectPrev, Some(context)));
        keys.push(KeyBinding::new("down", SelectNext, Some(context)));
    }
    // La paleta de comandos, desde cualquier lugar de la ventana.
    keys.push(KeyBinding::new("secondary-k", OpenPalette, None));
    cx.bind_keys(keys);
}

/// La posición siguiente en una lista de `len` elementos, sin dar la vuelta.
pub fn step(current: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        None => if delta < 0 { len - 1 } else { 0 },
        Some(ix) => (ix as isize + delta).clamp(0, len as isize - 1) as usize,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn avanza_sin_dar_la_vuelta() {
        assert_eq!(super::step(None, 1, 3), Some(0));
        assert_eq!(super::step(None, -1, 3), Some(2));
        assert_eq!(super::step(Some(2), 1, 3), Some(2));
        assert_eq!(super::step(Some(0), -1, 3), Some(0));
        assert_eq!(super::step(Some(1), 1, 3), Some(2));
        assert_eq!(super::step(Some(0), 1, 0), None);
    }
}
