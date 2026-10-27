// Ícono en la barra de menú (§11): el estado resumido de los entornos sin
// tener la ventana abierta. GPUI no trae ítems de estado; se usa tray-icon
// (NSStatusItem) desde el hilo principal.

use std::time::Duration;

use gpui_kit::*;
use serde_json::Value;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::client::Daemon;

pub const OPEN: &str = "abrir";
pub const QUIT: &str = "salir";

pub struct Tray {
    icon: TrayIcon,
    last: String,
}

impl Global for Tray {}

fn menu(lines: &[String]) -> Menu {
    let menu = Menu::new();
    for line in lines {
        let _ = menu.append(&MenuItem::new(line, false, None));
    }
    if !lines.is_empty() {
        let _ = menu.append(&PredefinedMenuItem::separator());
    }
    let _ = menu.append(&MenuItem::with_id(MenuId::new(OPEN), t!("Open Condx"), true, None));
    let _ = menu.append(&MenuItem::with_id(MenuId::new(QUIT), t!("Quit"), true, None));
    menu
}

/// La marca de Condx (una «C» con otra adentro, abiertas a la derecha, como `CONDX_MARK` de la app
/// del teléfono) en blanco y negro con alfa: macOS la usa de plantilla y la tiñe según la barra.
fn mark() -> Option<Icon> {
    const SIZE: u32 = 36;
    let scale = SIZE as f32 / 24.0;
    let (cx, cy) = (12.0 * scale, 12.0 * scale);
    let half = 1.1 * scale;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = (px * px + py * py).sqrt();
            // Abierta a la derecha: fuera del arco de ±45° alrededor del eje horizontal.
            let angle = py.atan2(px).abs().to_degrees();
            let ring = |r: f32| (half + 0.5 - (d - r * scale).abs()).clamp(0.0, 1.0);
            let cover = if angle < 45.0 { 0.0 } else { ring(9.0).max(ring(4.5)) };
            let i = ((y * SIZE + x) * 4) as usize;
            rgba[i + 3] = (cover * 255.0) as u8;
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

impl Tray {
    pub fn new() -> Option<Self> {
        let mut builder = TrayIconBuilder::new().with_tooltip("Condx");
        builder = match mark() {
            Some(icon) => builder.with_icon_templated(icon),
            None => builder.with_title("Condx"),
        };
        let icon = builder
            .with_menu(Box::new(menu(&[t!("Connecting to the daemon…").into()])))
            .build()
            .map_err(|e| eprintln!("no menu bar icon: {e}"))
            .ok()?;
        Some(Self { icon, last: String::new() })
    }

    /// Título y menú a partir de la vista general (o del error del daemon).
    pub fn update(&mut self, overview: Option<&Value>, error: Option<&str>) {
        let (title, lines) = summarize(overview, error);
        let key = format!("{title}\n{}", lines.join("\n"));
        if key == self.last {
            return;
        }
        // Solo el ícono; al lado, cuántas cosas mirar si hay alguna (el detalle va en el menú y el tooltip).
        self.icon.set_title(if title.is_empty() { None } else { Some(&title) });
        let _ = self.icon.set_tooltip(Some(format!("Condx · {}", lines.first().cloned().unwrap_or_default())));
        self.icon.set_menu(Some(Box::new(menu(&lines))));
        self.last = key;
    }
}

/// Lo que va al lado del ícono: nada si todo anda, «⚠ 2» (cosas que mirar) o «✕» (el daemon no
/// responde); más una línea por entorno para el menú. Un entorno local apagado no es un problema.
pub fn summarize(overview: Option<&Value>, error: Option<&str>) -> (String, Vec<String>) {
    let Some(o) = overview.filter(|_| error.is_none()) else {
        return ("✕".into(), vec![format!("Daemon: {}", error.unwrap_or(t!("no data")))]);
    };
    let mut lines = vec![];
    for env in o["environments"].as_array().into_iter().flatten() {
        let name = env["name"].as_str().unwrap_or("?");
        let ok = env["convex"]["reachable"] == true;
        let commit = env["deployed"]["commit"].as_str().unwrap_or(t!("unknown commit"));
        lines.push(format!("{} {name} · {}", if ok { "●" } else { "○" }, if ok { commit } else { t!("not responding") }));
    }
    let attention = o["attention"].as_array().map(Vec::len).unwrap_or(0);
    if let Some(phase) = o["deployer"]["phase"].as_str() {
        lines.push(format!("Deployer: {}", crate::ui::phase_label(phase)));
    }
    for a in o["attention"].as_array().into_iter().flatten().take(3) {
        lines.push(format!("⚠ {}", a.as_str().unwrap_or_default().chars().take(70).collect::<String>()));
    }
    let title = if attention > 0 { format!("⚠ {attention}") } else { String::new() };
    (title, lines)
}

/// Atiende los clics del menú (abrir la ventana o salir) y refresca el estado
/// cada 3 s por su cuenta: sigue vivo aunque se cierre la ventana.
pub fn run(cx: &mut App, daemon: Option<Daemon>, open: fn(&mut App)) {
    cx.spawn(async move |cx| {
        let mut ticks = 0u32;
        loop {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                let id = event.id().0.clone();
                cx.update(|cx| match id.as_str() {
                    OPEN => open(cx),
                    QUIT => cx.quit(),
                    _ => {}
                });
            }
            ticks += 1;
            if ticks % 20 != 1 {
                continue;
            }
            let (overview, error) = match &daemon {
                Some(d) => {
                    let d = d.clone();
                    match cx.background_executor().spawn(async move { d.get("/api/overview") }).await {
                        Ok(v) => (Some(v), None),
                        Err(e) => (None, Some(e.to_string())),
                    }
                }
                None => (None, Some(t!("condxd isn't installed").to_string())),
            };
            cx.update(|cx| {
                if cx.has_global::<Tray>() {
                    cx.update_global::<Tray, _>(|tray, _| tray.update(overview.as_ref(), error.as_deref()));
                }
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    // Sin `use super::*`: el glob de gpui_kit trae su propio `test`.
    use super::summarize;
    use serde_json::json;

    #[test]
    fn resume_el_estado() {
        assert_eq!(summarize(None, Some("no responde")).0, "✕");
        let o = json!({
            "environments": [
                { "name": "local", "convex": { "reachable": true }, "deployed": { "commit": "570c6a42" } },
                { "name": "prod", "convex": { "reachable": true }, "deployed": { "commit": null } }
            ],
            "attention": [],
            "deployer": { "phase": "idle" }
        });
        let (title, lines) = summarize(Some(&o), None);
        assert_eq!(title, "");
        assert_eq!(lines[0], "● local · 570c6a42");
        assert_eq!(lines[1], "● prod · unknown commit");
        let mut bad = o.clone();
        bad["environments"][1]["convex"]["reachable"] = json!(false);
        assert_eq!(summarize(Some(&bad), None).1[1], "○ prod · not responding");
        let mut warn = o;
        warn["attention"] = json!(["CI falló"]);
        assert_eq!(summarize(Some(&warn), None).0, "⚠ 1");
    }
}
