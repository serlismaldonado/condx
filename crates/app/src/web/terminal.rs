// En la web (la demo) no hay terminal: las mismas piezas que en el escritorio, sin pty. Abrir
// una dice que la terminal es de la app de escritorio.

use std::path::Path;

use gpui_kit::*;

pub fn bind_keys(_: &mut App) {}

pub struct Pty;

impl Pty {
    pub fn open(_: &Path) -> anyhow::Result<Self> {
        anyhow::bail!("The terminal is part of the desktop app")
    }
}

pub struct TerminalView {
    pub title: SharedString,
    pub exited: std::sync::Arc<std::sync::atomic::AtomicBool>,
    focus: FocusHandle,
}

impl TerminalView {
    pub fn new(title: impl Into<SharedString>, _: Pty, _: &mut Window, cx: &mut Context<Self>) -> Self {
        Self { title: title.into(), exited: Default::default(), focus: cx.focus_handle() }
    }

    pub fn handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn send(&mut self, _: &[u8]) {}

    pub fn run_command(&mut self, _: &str) {}
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
