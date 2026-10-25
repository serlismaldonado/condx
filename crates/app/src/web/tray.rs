// En la web (la demo) no hay ícono en la barra de menú.

use gpui_kit::*;
use serde_json::Value;

pub struct Tray;

impl Global for Tray {}

impl Tray {
    pub fn update(&mut self, _: Option<&Value>, _: Option<&str>) {}
}
