// condx: la app del devtool. Cliente delgado de condxd: si la cierras, el daemon sigue; el ícono
// de la barra de menú también. En el escritorio corre `run_desktop` (main.rs); compilada a
// WebAssembly (la demo de la web), `start` abre la misma app con un daemon de ejemplo: en vez de
// HTTP, `client` responde con datos inventados y no hay terminal, ícono de barra ni SQLite.

#[macro_use]
mod i18n;
mod app;
mod nav;
#[cfg(not(target_family = "wasm"))]
mod client;
#[cfg(target_family = "wasm")]
#[path = "web/client.rs"]
mod client;
mod ely;
#[cfg(not(target_family = "wasm"))]
mod prefs;
#[cfg(target_family = "wasm")]
#[path = "web/prefs.rs"]
mod prefs;
mod shell;
#[cfg(not(target_family = "wasm"))]
mod terminal;
#[cfg(target_family = "wasm")]
#[path = "web/terminal.rs"]
mod terminal;
mod theme;
mod touchid;
#[cfg(not(target_family = "wasm"))]
mod tray;
#[cfg(target_family = "wasm")]
#[path = "web/tray.rs"]
mod tray;
mod ui;
mod view;
mod views;

use gpui_kit::*;

actions!(devtool, [Quit]);

/// Abre la ventana principal, o la trae al frente si ya está abierta.
#[cfg(not(target_family = "wasm"))]
fn open_main_window(cx: &mut App) {
    cx.activate(true);
    if let Some(window) = cx.windows().first().copied() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
        return;
    }
    // DEVTOOL_WINDOW=760x700 abre la ventana de ese tamaño (para revisar pantallas chicas).
    let (w, h) = std::env::var("DEVTOOL_WINDOW")
        .ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))))
        .unwrap_or((1240., 820.));
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    // Barra de título integrada (la dibuja la app, como Zed): transparente, con los botones de macOS.
    let mut options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Más chica que esto, la barra de título y las pestañas ya no caben.
        window_min_size: Some(size(px(640.), px(480.))),
        ..component::TitleBar::window_options()
    };
    // Sin título, la ventana no aparece con nombre en Mission Control ni en los grabadores.
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some("Condx".into());
    }
    // El tema (colores del ERP en Kit y en Ely) lo aplica la app con la preferencia guardada.
    open_window(options, cx, |window, cx| cx.new(|cx| app::DevtoolApp::new(window, cx)))
    .expect("couldn't open the window");
}

/// El menú de la app (se vuelve a armar al cambiar de idioma).
pub fn set_app_menus(cx: &mut App) {
    cx.set_menus([Menu { name: "Condx".into(), items: vec![MenuItem::action(t!("Quit"), Quit)], disabled: false }]);
}

/// La app de escritorio (el binario `condx`).
#[cfg(not(target_family = "wasm"))]
pub fn run_desktop() {
    // El idioma antes que cualquier texto (menú, ícono de la barra, ventana).
    if let Ok(prefs) = prefs::Prefs::open() {
        i18n::set_lang(i18n::Lang::parse(&prefs.get("lang").unwrap_or_default()));
    }
    // Ely y GPUI Kit en una ventana: los archivos de Ely primero, el resto de Kit.
    application().with_assets(ely_core::Assets::before(assets::Assets)).run(|cx| {
        init(cx);
        ely_core::init(cx).expect("Ely resources failed to load");
        i18n::apply_ely(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        terminal::bind_keys(cx);
        nav::bind_keys(cx);
        set_app_menus(cx);
        if let Some(tray) = tray::Tray::new() {
            cx.set_global(tray);
        }
        tray::run(cx, client::Daemon::from_home().ok(), open_main_window);
        open_main_window(cx);
    });
}

/// La demo de la web: la misma app, con datos de ejemplo y sin daemon.
#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    gpui_kit::platform::web_init();
    i18n::set_lang(i18n::Lang::En);
    // Los íconos de GPUI Kit se piden por HTTP: con la URL entera (el cliente no acepta rutas
    // relativas), desde el origen de la página.
    let origin = web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default();
    gpui_kit::platform::application().with_assets(ely_core::Assets::before(gpui_kit::assets::Assets::new(origin))).run(|cx| {
        init(cx);
        ely_core::init(cx).expect("Ely resources failed to load");
        i18n::apply_ely(cx);
        nav::bind_keys(cx);
        open_window(WindowOptions::default(), cx, |window, cx| cx.new(|cx| app::DevtoolApp::new(window, cx))).expect("couldn't open the window");
    });
}
