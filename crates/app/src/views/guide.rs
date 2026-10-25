// Guía de uso dentro de la app: capítulos en markdown
// (tools/devtool/docs/guia/*.md), compilados en el binario.

use gpui_kit::component::button::Button;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::app::{DevtoolApp, Page};
use crate::ui;

pub const CHAPTERS: &[(&str, &str)] = &[
    ("Pieces and environments", include_str!("../../../../docs/guia/01-piezas.md")),
    ("Checking status", include_str!("../../../../docs/guia/02-estado.md")),
    ("Local environment and terminal", include_str!("../../../../docs/guia/10-local-terminal.md")),
    ("Other Convex projects", include_str!("../../../../docs/guia/11-proyectos.md")),
    ("Requesting a job", include_str!("../../../../docs/guia/03-trabajos.md")),
    ("Approve", include_str!("../../../../docs/guia/04-aprobar.md")),
    ("Day to day", include_str!("../../../../docs/guia/05-dia-a-dia.md")),
    ("Promotion to production", include_str!("../../../../docs/guia/06-promocion.md")),
    ("Deploy workflows", include_str!("../../../../docs/guia/12-flujos.md")),
    ("Data", include_str!("../../../../docs/guia/13-datos.md")),
    ("When something goes wrong", include_str!("../../../../docs/guia/07-problemas.md")),
    ("AI agents", include_str!("../../../../docs/guia/08-agentes.md")),
    ("Maintenance", include_str!("../../../../docs/guia/09-mantenimiento.md")),
];


pub fn render(_: &mut DevtoolApp, chapter: usize, _: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let chapter = chapter.min(CHAPTERS.len() - 1);
    let mut index = v_flex().gap_0p5().w(px(230.)).flex_none().child(ui::section(t!("Guide"), cx).px_2().pb_1());
    for (i, (title, _)) in CHAPTERS.iter().enumerate() {
        let title = crate::i18n::tr(title);
        index = index.child(
            div()
                .id(("guide-chapter", i))
                .px_2()
                .py_1()
                
                .text_sm()
                .cursor_pointer()
                .when(i == chapter, |d| d.bg(cx.theme().accent).text_color(cx.theme().accent_foreground))
                .hover(|d| d.bg(cx.theme().muted))
                .on_click(cx.listener(move |this, _, _, cx| this.go(Page::Guide(i), cx)))
                .child(format!("{}. {title}", i + 1)),
        );
    }

    let nav = h_flex()
        .gap_2()
        .pt_4()
        .children((chapter > 0).then(|| {
            Button::new("guide-prev")
                .small()
                .outline()
                .label(format!("← {}", crate::i18n::tr(CHAPTERS[chapter - 1].0)))
                .on_click(cx.listener(move |this, _, _, cx| this.go(Page::Guide(chapter - 1), cx)))
        }))
        .child(div().flex_1())
        .children((chapter + 1 < CHAPTERS.len()).then(|| {
            Button::new("guide-next")
                .small()
                .outline()
                .label(format!("{} →", crate::i18n::tr(CHAPTERS[chapter + 1].0)))
                .on_click(cx.listener(move |this, _, _, cx| this.go(Page::Guide(chapter + 1), cx)))
        }));

    h_flex()
        .items_start()
        .gap_6()
        .p_4()
        .child(index)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .max_w(px(820.))
                .p_5()
                
                .border_1()
                .border_color(cx.theme().border)
                .child(TextView::markdown(("guide", chapter), CHAPTERS[chapter].1).selectable(true))
                .child(nav),
        )
}
