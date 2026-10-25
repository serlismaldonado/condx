// Página «Terminal»: pestañas de terminales en el repo y atajos para los
// comandos más usados (los escribe en la terminal activa y presiona Enter).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, IconName, Sizable};
use gpui_kit::*;

use crate::app::DevtoolApp;
use crate::ui;

const QUICK: &[(&str, &str)] = &[
    ("git status", "git status -sb"),
    ("Types", "npx tsc --noEmit"),
    ("Lint", "bun run lint"),
    ("Tests", "bun run test"),
    ("Convex logs", "npx convex logs"),
    ("Condx status", "~/.condx/daemon/bin/condxd status | head -40"),
];

pub fn render(app: &mut DevtoolApp, _: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let mut tabs = TabBar::new("terminal-tabs").selected_index(app.term_active).on_click(cx.listener(|this, ix: &usize, window, cx| {
        this.term_active = *ix;
        if let Some(t) = this.terminals.get(*ix) {
            { let h = t.read(cx).handle(); window.focus(&h, cx); }
        }
        cx.notify();
    }));
    for t in &app.terminals {
        let view = t.read(cx);
        let exited = view.exited.load(std::sync::atomic::Ordering::Acquire);
        tabs = tabs.child(Tab::new().label(if exited { t!("{a} (exited)", a = view.title) } else { view.title.to_string() }));
    }
    let has = !app.terminals.is_empty();
    let header = h_flex()
        .gap_2()
        .items_center()
        .child(tabs)
        .child(div().flex_1())
        .child(
            Button::new("term-new")
                .small()
                .outline()
                .icon(IconName::Plus)
                .label(t!("New"))
                .on_click(cx.listener(|this, _, window, cx| this.new_terminal(window, cx))),
        )
        .child(
            Button::new("term-close")
                .small()
                .ghost()
                .icon(IconName::Close)
                .label(t!("Close"))
                .disabled(!has)
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.term_active < this.terminals.len() {
                        // Al soltar la vista se cierra el PTY y el shell recibe SIGHUP.
                        this.terminals.remove(this.term_active);
                        this.term_active = this.term_active.min(this.terminals.len().saturating_sub(1));
                        if let Some(t) = this.terminals.get(this.term_active) {
                            { let h = t.read(cx).handle(); window.focus(&h, cx); }
                        }
                        cx.notify();
                    }
                })),
        );

    let mut quick = h_flex().gap_1().flex_wrap().items_center().child(ui::muted(t!("Shortcuts:"), cx).text_xs());
    for (i, (label, command)) in QUICK.iter().enumerate() {
        let command = command.to_string();
        quick = quick.child(
            Button::new(("quick", i))
                .xsmall()
                .ghost()
                .label(crate::i18n::tr(label))
                .tooltip(command.clone())
                .disabled(!has)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(t) = this.terminals.get(this.term_active).cloned() {
                        t.update(cx, |view, _| view.run_command(&command));
                        { let h = t.read(cx).handle(); window.focus(&h, cx); }
                    }
                })),
        );
    }

    let body: AnyElement = match app.terminals.get(app.term_active) {
        Some(t) => t.clone().into_any_element(),
        None => v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(ui::muted(t!("No open terminals: “New” opens one in the repo."), cx))
            .into_any_element(),
    };

    let col = v_flex().size_full().gap_2().px_5().pb_4().child(header).child(quick);
    col.child(
        div()
            .flex_1()
            .min_h_0()
            .p_2()
            
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(body),
    )
}
