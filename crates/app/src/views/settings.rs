// Configuración de la app: idioma y tema (se guardan en ~/.condx/app), las reglas de datos del
// daemon (leer y cambiar por tipo de entorno) y dónde está el daemon.

use gpui_kit::component::button::Button;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::{h_flex, v_flex, Sizable};
use gpui_kit::*;

use crate::app::DevtoolApp;
use crate::i18n::Lang;
use crate::theme::Choice;
use crate::ui;

pub fn render(app: &mut DevtoolApp, window: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let current = crate::i18n::lang();
    let mut langs = h_flex().gap_2().flex_wrap();
    for lang in Lang::ALL {
        let button = Button::new(SharedString::from(format!("lang-{}", lang.code())))
            .small()
            .label(lang.native_name())
            .on_click(cx.listener(move |this, _, window, cx| this.set_language(lang, window, cx)));
        langs = langs.child(if lang == current { ui::brand(button, cx).into_any_element() } else { button.outline().into_any_element() });
    }

    let mut themes = h_flex().gap_2().flex_wrap();
    for (choice, label) in [(Choice::System, t!("System")), (Choice::Light, t!("Light")), (Choice::Dark, t!("Dark"))] {
        let button = Button::new(SharedString::from(format!("theme-{}", choice.as_str())))
            .small()
            .label(label)
            .on_click(cx.listener(move |this, _, window, cx| this.set_theme(choice, window, cx)));
        themes = themes.child(if app.theme == choice { ui::brand(button, cx).into_any_element() } else { button.outline().into_any_element() });
    }

    // Con ~ en vez de la carpeta del usuario, y cortada si no cabe.
    let mut daemon_home = crate::client::home().display().to_string();
    if let Some(user) = crate::ui::user_home() {
        if let Some(rest) = daemon_home.strip_prefix(&user) {
            daemon_home = format!("~{rest}");
        }
    }
    v_flex()
        .gap_3()
        .p_4()
        .max_w(px(720.))
        .child(agents(app, cx))
        .child(super::cloud::card(app, window, cx))
        .child(data_rules(app, cx))
        .child(
            ui::card(cx)
                .child(ui::section(t!("Language"), cx))
                .child(langs)
                .child(ui::muted(t!("Messages that come from the daemon (job steps, recipes, errors) are always in English."), cx).text_xs()),
        )
        .child(ui::card(cx).child(ui::section(t!("Theme"), cx)).child(themes))
        .child(
            ui::card(cx)
                .child(ui::section(t!("About"), cx))
                .child(ui::kv(t!("Version"), ui::mono(env!("CARGO_PKG_VERSION")), cx))
                .child(ui::kv(t!("Daemon data"), ui::mono(daemon_home).truncate(), cx))
                .child(ui::kv(t!("Daemon"), ui::mono(app.daemon.as_ref().map(|d| d.base.clone()).unwrap_or_else(|| t!("not connected").to_string())), cx)),
        )
}

const KINDS: [&str; 4] = ["devbox", "throwaway", "cloud-dev", "cloud-prod"];

/// Las reglas que exige el daemon a cada tipo de entorno: si la pestaña Datos (y el MCP) leen sus
/// registros, y cuánto se pide a las recetas que lo modifican. Solo endurecen lo que pide cada
/// receta, y solo las cambia la app.
fn data_rules(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) -> Div {
    let card = ui::card(cx)
        .child(ui::section(t!("Data and changes"), cx))
        .child(ui::muted(t!("Rules the daemon applies to each kind of environment. Only “No approval” loosens what a recipe asks (never in production); queries that only read don't ask for approval when reading is allowed. Only this app can change them (agents through the MCP can't)."), cx).text_xs());
    let Some(policy) = app.policy.clone() else {
        return card.child(ui::muted(if app.daemon.is_some() { t!("Loading…") } else { t!("not connected") }, cx));
    };
    let rules: [(&str, &'static str); 6] = [
        ("none", t!("No approval")),
        ("recipe", t!("As the recipe says")),
        ("approval", t!("Approval")),
        ("typed", t!("Type the name")),
        ("reinforced", t!("Touch ID")),
        ("blocked", t!("Blocked")),
    ];
    let mut card = card;
    for kind in KINDS {
        let readable = policy["read"][kind].as_bool().unwrap_or(kind != "cloud-prod");
        let write = policy["write"][kind].as_str().unwrap_or("recipe").to_string();
        let mut reads = h_flex().gap_1();
        for (value, label) in [(true, t!("Allowed")), (false, t!("Off"))] {
            let button = Button::new(SharedString::from(format!("read-{kind}-{value}")))
                .xsmall()
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_policy(kind, Some(value), None, cx)));
            reads = reads.child(ui::brand_if(readable == value, button, cx));
        }
        let mut writes = h_flex().gap_1().flex_wrap();
        // Producción siempre pide aprobación para cambiar algo.
        for (value, label) in rules.into_iter().filter(|(v, _)| kind != "cloud-prod" || *v != "none") {
            let button = Button::new(SharedString::from(format!("write-{kind}-{value}")))
                .xsmall()
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_policy(kind, None, Some(value), cx)));
            writes = writes.child(ui::brand_if(write == value, button, cx));
        }
        let mut row = v_flex()
            .gap_1p5()
            .pt_2()
            .child(ui::kind_tag(kind))
            .child(h_flex().gap_3().items_center().child(ui::muted(t!("Read data"), cx).text_xs().w_24()).child(reads))
            .child(h_flex().gap_3().items_start().child(ui::muted(t!("Changes"), cx).text_xs().w_24().pt_0p5()).child(writes));
        if kind == "cloud-prod" && readable {
            row = row.child(ui::muted(t!("Production records are readable in the Data tab and by agents through the MCP (read-only)."), cx).text_xs());
        }
        card = card.child(row);
    }
    if let Some(e) = &app.policy_error {
        card = card.child(ui::muted(t!("Couldn't save the rules: {e}", e = e), cx).text_xs());
    }
    card
}

/// Cómo conectar un agente de IA al MCP del daemon (Claude Code, Cursor, Codex o por HTTP), listo
/// para copiar, y si ya se usó. Por el MCP un agente mira, pide trabajos y arranca flujos, pero no
/// aprueba ni cambia estas reglas.
fn agents(app: &mut DevtoolApp, cx: &mut Context<DevtoolApp>) -> Div {
    let status = app.mcp_status.clone().unwrap_or_default();
    let command = status["command"].as_str().unwrap_or("~/.condx/daemon/bin/condxd").to_string();
    let url = status["url"].as_str().unwrap_or("http://127.0.0.1:4790/mcp").to_string();
    let token = status["tokenFile"].as_str().unwrap_or("~/.condx/daemon/token").to_string();
    let seen = match (status["lastAt"].as_i64(), status["client"].as_str()) {
        (Some(at), Some(c)) if !c.is_empty() => t!("Last used {a} by {b}.", a = ui::ago(at), b = c),
        (Some(at), _) => t!("Last used {a}.", a = ui::ago(at)),
        _ => t!("No agent has connected since the daemon started.").to_string(),
    };
    let snippets = [
        ("claude", "Claude Code", format!("claude mcp add condx -- {command} mcp")),
        ("cursor", t!("Cursor (~/.cursor/mcp.json)"), format!("{{\n  \"mcpServers\": {{\n    \"condx\": {{ \"command\": \"{command}\", \"args\": [\"mcp\"] }}\n  }}\n}}")),
        ("codex", t!("Codex (~/.codex/config.toml)"), format!("[mcp_servers.condx]\ncommand = \"{command}\"\nargs = [\"mcp\"]")),
        ("http", t!("Other clients over HTTP"), format!("POST {url}\nAuthorization: Bearer $(cat {token})")),
    ];
    let mut card = ui::card(cx)
        .child(ui::section(t!("Connect an agent"), cx))
        .child(ui::muted(t!("AI agents use Condx through its MCP server, on this Mac. Add it to your client with one of these; it needs the daemon running (condxd install)."), cx).text_xs())
        .child(h_flex().gap_2().items_center().child(ui::dot(if status["lastAt"].is_i64() { ui::Health::Ok } else { ui::Health::Unknown }, cx)).child(ui::muted(seen, cx).text_xs()));
    // Se ve con ~ (más corto); se copia con la ruta completa (Cursor y Codex no expanden ~).
    let user = crate::ui::user_home().unwrap_or_default();
    for (id, label, text) in snippets {
        let shown = if user.is_empty() { text.clone() } else { text.replace(&user, "~") };
        card = card.child(
            v_flex()
                .gap_1()
                .child(h_flex().items_center().child(div().flex_1().child(ui::muted(label.to_string(), cx).text_xs())).child(Clipboard::new(SharedString::from(format!("mcp-copy-{id}"))).value(text.clone()).tooltip(t!("Copy"))))
                .child(ui::contained().px_2().py_1().overflow_hidden().child(ui::mono(shown).text_xs().whitespace_normal())),
        );
    }
    card.child(ui::muted(t!("Through the MCP an agent can look at everything, request jobs and start workflows. It can't approve, change the rules below or save deploy keys: that's only done in this app."), cx).text_xs())
}
