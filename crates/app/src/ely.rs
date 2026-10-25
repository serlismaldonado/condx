// Puente entre los datos del daemon y los componentes de Ely: líneas de log
// con su nivel para `LogViewer`, el log de un trabajo cortado por pasos para
// `CommandBlock`, opciones de `Select` y campos de búsqueda de Ely.

use ely_core::forms::{Choice, TextInput};
use ely_terminal::{LogLevel, LogLine};
use gpui_kit::*;

use crate::app::DevtoolApp;

/// Nivel de una línea según lo que dice (los logs del devtool, de Convex y de Vite no traen uno fijo).
fn level_of(line: &str) -> LogLevel {
    let low = line.to_lowercase();
    if line.contains('✖') || low.contains("error") || low.contains("failed") || low.contains("panic") || low.contains("uncaught") {
        LogLevel::Error
    } else if line.contains('⚠') || low.contains("warn") || low.contains("deprecat") {
        LogLevel::Warn
    } else if low.contains(" debug ") || low.starts_with("debug") || low.contains("[debug]") {
        LogLevel::Debug
    } else if low.contains(" trace ") || low.contains("[trace]") {
        LogLevel::Trace
    } else {
        LogLevel::Info
    }
}

/// Hora al principio de la línea (`2026-10-24T22:11:42.70Z …`, `12:04:31 …` o `10/24/2026, 3:28:54 PM …`).
fn split_time(line: &str) -> (String, &str) {
    let bytes = line.as_bytes();
    // RFC 3339.
    if line.len() > 19 && bytes.get(4) == Some(&b'-') && bytes.get(10) == Some(&b'T') {
        let end = line.find(' ').unwrap_or(line.len());
        return (line[11..19].to_string(), line[end..].trim_start());
    }
    // hh:mm:ss
    if line.len() > 8 && bytes.get(2) == Some(&b':') && bytes.get(5) == Some(&b':') && line[..8].chars().all(|c| c.is_ascii_digit() || c == ':') {
        return (line[..8].to_string(), line[8..].trim_start());
    }
    // Los logs de convex dev: «10/24/2026, 3:28:54 PM [CONVEX …]».
    if let Some(idx) = line.find(" PM ").or_else(|| line.find(" AM ")) {
        if idx < 24 && line.contains('/') {
            let time = line[..idx + 3].rsplit(", ").next().unwrap_or_default().to_string();
            return (time, line[idx + 4..].trim_start());
        }
    }
    (String::new(), line)
}

/// De dónde viene: la función de Convex (`[CONVEX Q(mod:fn)]`) o el módulo de un log de Rust (`INFO keybroker::broker:`).
fn target_of(rest: &str) -> (String, String) {
    if let Some(start) = rest.find("[CONVEX ") {
        if let Some(end) = rest[start..].find(']') {
            let inner = &rest[start + 8..start + end];
            let name = inner.split_once('(').map(|(_, f)| f.trim_end_matches(')')).unwrap_or(inner);
            let message = rest[start + end + 1..].trim_start().to_string();
            return (name.to_string(), message);
        }
    }
    for level in ["ERROR ", "WARN ", "INFO ", "DEBUG ", "TRACE "] {
        if let Some(r) = rest.trim_start().strip_prefix(level) {
            if let Some((target, msg)) = r.trim_start().split_once(": ") {
                if !target.contains(' ') {
                    return (target.to_string(), msg.to_string());
                }
            }
        }
    }
    (String::new(), rest.to_string())
}

/// Texto de log a líneas de `LogViewer`, con hora, nivel y origen cuando se reconocen.
pub fn log_lines(text: &str) -> Vec<LogLine> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (at, rest) = split_time(l);
            let (target, message) = target_of(rest);
            LogLine { at: at.into(), level: level_of(l), target: target.into(), message: message.into() }
        })
        .collect()
}

/// Ejecuciones de funciones de Convex (`get_function_logs`) a líneas: una
/// por cada `console.*` y una más con el error, si falló; la más vieja arriba.
pub fn function_log_lines(entries: &[serde_json::Value]) -> Vec<LogLine> {
    let mut out = vec![];
    for e in entries.iter().rev() {
        let at: SharedString = e["timestamp"].as_f64().map(|s| crate::ui::clock((s * 1000.0) as i64)).unwrap_or_default().into();
        let kind = e["udfType"].as_str().unwrap_or_default().chars().next().unwrap_or(' ');
        let target: SharedString = format!("{kind} {}", e["identifier"].as_str().unwrap_or_default()).trim().to_string().into();
        let lines: Vec<&str> = e["logLines"].as_array().into_iter().flatten().filter_map(|l| l.as_str()).collect();
        for l in &lines {
            out.push(LogLine { at: at.clone(), level: level_of(l), target: target.clone(), message: (*l).to_string().into() });
        }
        match e["error"].as_str().filter(|s| !s.is_empty()) {
            Some(err) => out.push(LogLine { at: at.clone(), level: LogLevel::Error, target: target.clone(), message: err.to_string().into() }),
            None if lines.is_empty() => out.push(LogLine { at: at.clone(), level: LogLevel::Debug, target: target.clone(), message: "ok".into() }),
            None => {}
        }
    }
    out
}

/// El log de un trabajo, cortado por sus pasos (`▶ nombre` abre cada uno).
pub fn step_outputs(log: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for line in log.lines() {
        if let Some(name) = line.strip_prefix("▶ ") {
            out.push((name.trim().to_string(), String::new()));
            continue;
        }
        if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    out
}

pub fn choice(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Choice {
    Choice::new(value, label)
}

pub fn choice_note(value: impl Into<SharedString>, label: impl Into<SharedString>, note: impl Into<SharedString>) -> Choice {
    let mut c = Choice::new(value, label);
    c.note = Some(note.into());
    c
}

impl DevtoolApp {
    /// Un campo de texto de Ely, guardado por clave (búsquedas de los visores).
    pub fn ely_input(&mut self, key: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextInput> {
        let placeholder = placeholder.to_string();
        self.ely_inputs.entry(key.to_string()).or_insert_with(|| cx.new(|cx| TextInput::new(window, cx).placeholder(placeholder))).clone()
    }

    /// Nivel mínimo elegido en un visor de logs.
    pub fn log_level(&self, key: &str) -> LogLevel {
        self.log_levels.get(key).copied().unwrap_or(LogLevel::Trace)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reconoce_hora_nivel_y_origen() {
        let l = super::log_lines("2026-10-24T22:11:42.700978Z  INFO keybroker::broker: Admin key accepted\n10/24/2026, 3:28:54 PM [CONVEX A(tasks:check)] [LOG] 'hola'\n✖ start: no respondió");
        assert_eq!(l[0].at.as_ref(), "22:11:42");
        assert_eq!(l[0].target.as_ref(), "keybroker::broker");
        assert_eq!(l[1].at.as_ref(), "3:28:54 PM");
        assert_eq!(l[1].target.as_ref(), "tasks:check");
        assert_eq!(l[2].level, ely_terminal::LogLevel::Error);
        let steps = super::step_outputs("Intento 1\n▶ Exportar\nuno\n✔ Exportar\n▶ Importar\ndos\n");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].1, "dos\n");
    }
}
