pub mod approvals;
pub mod cloud;
pub mod data;
pub mod env;
pub mod er;
pub mod er_layout;
pub mod er_route;
pub mod guide;
pub mod insights;
pub mod jobs;
pub mod keys;
pub mod local;
pub mod live;
pub mod overview;
pub mod palette;
pub mod agent_outcomes;
pub mod agent_roles;
pub mod agent_settings;
pub mod agents;
pub mod frontend;
pub mod incidents;
pub mod notify_settings;
pub mod repo;
pub mod schema;
pub mod sdk;
pub mod connections;
pub mod schema_diff;
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
