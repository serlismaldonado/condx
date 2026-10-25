// Terminal integrada: un shell en un PTY (portable-pty), interpretado con
// vt100 y dibujado celda a celda con GPUI. Abre en el repo del ERP. Soporta
// colores, cursor, scrollback con la rueda, teclas de control y pegar con
// cmd-v. Vive mientras la app esté abierta (no en el daemon).

use std::cell::Cell;
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::ActiveTheme;
use gpui_kit::*;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

// Tab y shift-tab van al shell: sin estas acciones, Root las usa para mover
// el foco entre controles (su atajo gana a on_key_down).
actions!(terminal, [SendTab, SendBackTab]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("tab", SendTab, Some("Terminal")), KeyBinding::new("shift-tab", SendBackTab, Some("Terminal"))]);
}

const FONT_SIZE: f32 = 12.5;
const LINE_HEIGHT: f32 = 17.0;
const SCROLLBACK: usize = 5000;

pub struct TerminalView {
    pub title: SharedString,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn Child + Send + Sync>,
    pub exited: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    focus: FocusHandle,
    size: (u16, u16),
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    cell_width: Option<Pixels>,
    /// Líneas que se subió con la rueda (0 = abajo del todo).
    scroll: usize,
    wheel: f32,
}

fn shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into())
}

/// El shell ya abierto en su PTY (antes de tener una vista).
pub struct Pty {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    exited: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    size: (u16, u16),
}

impl Pty {
    pub fn open(cwd: &Path) -> anyhow::Result<Self> {
        let (rows, cols) = (30u16, 110u16);
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let mut cmd = CommandBuilder::new(shell());
        cmd.arg("-l");
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("HESKALA_DEVTOOL", "1");
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let (exited, dirty) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)));
        {
            let (parser, exited, dirty) = (parser.clone(), exited.clone(), dirty.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            parser.lock().unwrap().process(&buf[..n]);
                            dirty.store(true, Ordering::Release);
                        }
                    }
                }
                exited.store(true, Ordering::Release);
                dirty.store(true, Ordering::Release);
            });
        }
        Ok(Self { parser, writer, master: pair.master, child, exited, dirty, size: (rows, cols) })
    }
}

impl TerminalView {
    pub fn new(title: impl Into<SharedString>, pty: Pty, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Redibujar cuando llega salida (como mucho ~60 veces por segundo).
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            let alive = this
                .update(cx, |view, cx| {
                    if view.dirty.swap(false, Ordering::AcqRel) {
                        cx.notify();
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            title: title.into(),
            parser: pty.parser,
            writer: pty.writer,
            master: pty.master,
            _child: pty.child,
            exited: pty.exited,
            dirty: pty.dirty,
            focus,
            size: pty.size,
            bounds: Rc::new(Cell::new(None)),
            cell_width: None,
            scroll: 0,
            wheel: 0.0,
        }
    }

    pub fn handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Escribe en el shell (lo que se teclea o un comando de los atajos).
    pub fn send(&mut self, bytes: &[u8]) {
        if self.scroll > 0 {
            self.scroll = 0;
            self.parser.lock().unwrap().screen_mut().set_scrollback(0);
        }
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn run_command(&mut self, command: &str) {
        self.send(format!("{command}\r").as_bytes());
    }

    fn resize_to_bounds(&mut self, window: &mut Window) {
        let Some(bounds) = self.bounds.get() else { return };
        let cell_width = *self.cell_width.get_or_insert_with(|| {
            let font_id = window.text_system().resolve_font(&font(crate::theme::MONO_FONT));
            window.text_system().advance(font_id, px(FONT_SIZE), 'M').map(|s| s.width).unwrap_or(px(7.5))
        });
        let cols = ((bounds.size.width / cell_width).floor() as u16).clamp(20, 400);
        let rows = ((bounds.size.height / px(LINE_HEIGHT)).floor() as u16).clamp(5, 200);
        if (rows, cols) != self.size {
            self.size = (rows, cols);
            self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
            let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = k.modifiers;
        if m.platform {
            // cmd-v pega; los demás atajos de la app siguen funcionando.
            if k.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    let bracketed = self.parser.lock().unwrap().screen().bracketed_paste();
                    if bracketed {
                        self.send(format!("\x1b[200~{text}\x1b[201~").as_bytes());
                    } else {
                        self.send(text.as_bytes());
                    }
                }
                cx.stop_propagation();
            }
            return;
        }
        let app_cursor = self.parser.lock().unwrap().screen().application_cursor();
        let arrow = |c: char| if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") };
        let bytes: Option<Vec<u8>> = match k.key.as_str() {
            "enter" => Some(b"\r".to_vec()),
            "backspace" => Some(if m.alt { b"\x1b\x7f".to_vec() } else { b"\x7f".to_vec() }),
            "tab" => Some(if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
            "escape" => Some(b"\x1b".to_vec()),
            "up" => Some(arrow('A').into_bytes()),
            "down" => Some(arrow('B').into_bytes()),
            "right" => Some(if m.alt { b"\x1bf".to_vec() } else { arrow('C').into_bytes() }),
            "left" => Some(if m.alt { b"\x1bb".to_vec() } else { arrow('D').into_bytes() }),
            "home" => Some(b"\x1b[H".to_vec()),
            "end" => Some(b"\x1b[F".to_vec()),
            "pageup" => Some(b"\x1b[5~".to_vec()),
            "pagedown" => Some(b"\x1b[6~".to_vec()),
            "delete" => Some(b"\x1b[3~".to_vec()),
            "space" if m.control => Some(vec![0]),
            key if m.control && key.len() == 1 => {
                let c = key.as_bytes()[0].to_ascii_lowercase();
                c.is_ascii_lowercase().then(|| vec![c & 0x1f])
            }
            _ => k.key_char.as_ref().map(|s| {
                if m.alt {
                    let mut v = vec![0x1b];
                    v.extend_from_slice(s.as_bytes());
                    v
                } else {
                    s.as_bytes().to_vec()
                }
            }),
        };
        if let Some(b) = bytes {
            self.send(&b);
            cx.stop_propagation();
            window.prevent_default();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let dy = match event.delta {
            ScrollDelta::Lines(l) => l.y * 3.0,
            ScrollDelta::Pixels(p) => f32::from(p.y) / LINE_HEIGHT,
        };
        self.wheel += dy;
        let steps = self.wheel.trunc();
        if steps == 0.0 {
            return;
        }
        self.wheel -= steps;
        let mut parser = self.parser.lock().unwrap();
        let target = (self.scroll as i64 + steps as i64).max(0) as usize;
        parser.screen_mut().set_scrollback(target);
        self.scroll = parser.screen().scrollback();
        cx.notify();
    }
}

fn color(c: vt100::Color, default: Hsla) -> Hsla {
    // Paleta xterm: 16 básicos, cubo 6×6×6 y grises.
    const BASE: [u32; 16] = [
        0x1e1e1e, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666, 0xf14c4c, 0x23d18b, 0xf5f543,
        0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into(),
        vt100::Color::Idx(i) if i < 16 => rgb(BASE[i as usize]).into(),
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v as u32 * 40 };
            rgb((level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)).into()
        }
        vt100::Color::Idx(i) => {
            let v = 8 + (i as u32 - 232) * 10;
            rgb((v << 16) | (v << 8) | v).into()
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.resize_to_bounds(window);
        let (fg_default, bg_default) = (cx.theme().foreground, cx.theme().background);
        let focused = self.focus.is_focused(window);
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let (cur_row, cur_col) = screen.cursor_position();
        let show_cursor = focused && !screen.hide_cursor() && self.scroll == 0;
        let mut lines = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut spans: Vec<AnyElement> = vec![];
            let mut text = String::new();
            let mut style: Option<(Hsla, Option<Hsla>, bool)> = None;
            let flush = |spans: &mut Vec<AnyElement>, text: &mut String, style: Option<(Hsla, Option<Hsla>, bool)>| {
                if text.is_empty() {
                    return;
                }
                let (fg, bg, bold) = style.unwrap_or((fg_default, None, false));
                let mut span = div().text_color(fg).child(std::mem::take(text));
                if let Some(bg) = bg {
                    span = span.bg(bg);
                }
                if bold {
                    span = span.font_weight(FontWeight::BOLD);
                }
                spans.push(span.into_any_element());
            };
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = color(cell.fgcolor(), fg_default);
                let mut bg = match cell.bgcolor() {
                    vt100::Color::Default => None,
                    other => Some(color(other, bg_default)),
                };
                if cell.inverse() || (show_cursor && r == cur_row && c == cur_col) {
                    let swapped = bg.unwrap_or(bg_default);
                    bg = Some(fg);
                    fg = swapped;
                }
                let s = (fg, bg, cell.bold());
                if style != Some(s) {
                    flush(&mut spans, &mut text, style);
                    style = Some(s);
                }
                let content = cell.contents();
                text.push_str(if content.is_empty() { " " } else { content });
            }
            flush(&mut spans, &mut text, style);
            lines.push(div().h(px(LINE_HEIGHT)).flex().flex_row().whitespace_nowrap().children(spans));
        }
        drop(parser);
        let bounds = self.bounds.clone();
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|this, _: &SendTab, _, _| this.send(b"\t")))
            .on_action(cx.listener(|this, _: &SendBackTab, _, _| this.send(b"\x1b[Z")))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
            .size_full()
            .relative()
            .overflow_hidden()
            .font_family(crate::theme::MONO_FONT)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .bg(bg_default)
            .child(
                canvas(
                    move |b, window, _| {
                        if bounds.get() != Some(b) {
                            bounds.set(Some(b));
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .children(lines)
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
