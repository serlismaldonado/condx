pub mod approvals;
pub mod data;
pub mod env;
pub mod er;
pub mod er_layout;
pub mod er_route;
pub mod guide;
pub mod jobs;
pub mod keys;
pub mod local;
pub mod live;
pub mod overview;
pub mod repo;
pub mod schema;
pub mod settings;
pub mod terminal;
pub mod welcome;
pub mod workflows;

use gpui_kit::component::v_flex;
use gpui_kit::*;

use crate::app::DevtoolApp;
use crate::ui;

/// Estado vacío centrado (sin datos todavía o sin conexión).
pub fn empty(message: impl Into<SharedString>, cx: &mut Context<DevtoolApp>) -> Div {
    v_flex().size_full().p_6().items_center().justify_center().child(ui::muted(message, cx))
}
