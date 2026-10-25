// Rendimiento por función a partir de los logs de Convex que ya se leyeron (`convex logs --jsonl`):
// llamadas, errores y tiempos (p50, p95, máximo) de cada función, las que fallan y las lentas
// primero. Todo se calcula aquí, sin pedir nada más al backend.

use ely_core::data_display::Tone;
use ely_core::tables::{Cell, Column, DataTable, Row};
use gpui_kit::*;
use serde_json::Value;

use crate::ui;

#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub function: String,
    pub kind: String,
    pub calls: usize,
    pub errors: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub last_error: Option<String>,
}

/// El percentil `p` (0..=1) de una lista ya ordenada, por el rango más cercano.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

/// Lo que se va juntando de una función mientras se recorren los logs.
#[derive(Default)]
struct Acc {
    kind: String,
    times: Vec<f64>,
    calls: usize,
    errors: usize,
    last_error: Option<String>,
}

/// Una entrada por ejecución de función (las líneas sueltas de console.log no traen `identifier`).
pub fn stats(logs: &[Value]) -> Vec<Stat> {
    let mut by: std::collections::BTreeMap<String, Acc> = Default::default();
    for e in logs {
        let Some(function) = e["identifier"].as_str() else { continue };
        let acc = by.entry(function.to_string()).or_insert_with(|| Acc { kind: e["udfType"].as_str().unwrap_or_default().to_string(), ..Default::default() });
        acc.calls += 1;
        if let Some(t) = e["executionTime"].as_f64() {
            acc.times.push(t * 1000.);
        }
        if let Some(err) = e["error"].as_str() {
            acc.errors += 1;
            acc.last_error = Some(err.lines().next().unwrap_or(err).to_string());
        }
    }
    let mut out: Vec<Stat> = by
        .into_iter()
        .map(|(function, mut a)| {
            a.times.sort_by(|x, y| x.total_cmp(y));
            Stat {
                function,
                kind: a.kind,
                calls: a.calls,
                errors: a.errors,
                p50_ms: percentile(&a.times, 0.5),
                p95_ms: percentile(&a.times, 0.95),
                max_ms: a.times.last().copied(),
                last_error: a.last_error,
            }
        })
        .collect();
    out.sort_by(|a, b| b.errors.cmp(&a.errors).then(b.p95_ms.unwrap_or(0.).total_cmp(&a.p95_ms.unwrap_or(0.))).then(b.calls.cmp(&a.calls)));
    out
}

fn ms(v: Option<f64>) -> SharedString {
    match v {
        Some(x) if x >= 1000. => format!("{:.2} s", x / 1000.).into(),
        Some(x) => format!("{x:.0} ms").into(),
        None => "—".into(),
    }
}

/// La tabla, o nada si los logs no traen ejecuciones.
pub fn table(env: &str, logs: &[Value], cx: &App) -> Option<Div> {
    let list = stats(logs);
    if list.is_empty() {
        return None;
    }
    let total: usize = list.iter().map(|s| s.calls).sum();
    let failing = list.iter().filter(|s| s.errors > 0).count();
    let rows: Vec<Row> = list
        .iter()
        .map(|s| {
            let errors = if s.errors == 0 { Cell::Text("0".into()) } else { Cell::Tag(format!("{} / {}", s.errors, s.calls).into(), Tone::Danger) };
            Row::new(
                s.function.clone(),
                [
                    Cell::Text(s.function.clone().into()),
                    Cell::Text(s.kind.clone().into()),
                    Cell::Text(s.calls.to_string().into()),
                    errors,
                    Cell::Text(ms(s.p50_ms)),
                    Cell::Text(ms(s.p95_ms)),
                    Cell::Text(ms(s.max_ms)),
                    Cell::Text(s.last_error.clone().unwrap_or_default().into()),
                ],
            )
        })
        .collect();
    let height = (list.len().min(8) * 32 + 40) as f32;
    Some(
        ui::card(cx)
            .child(ui::section(t!("By function · {a} calls, {b} failing", a = total, b = failing), cx))
            .child(
                div().h(px(height)).child(
                    DataTable::new(SharedString::from(format!("insights-{env}")), [
                        Column::new("fn", t!("Function")),
                        Column::new("kind", t!("Type")).width(rems(6.)),
                        Column::new("calls", t!("Calls")).width(rems(4.5)),
                        Column::new("errors", t!("Errors")).width(rems(5.5)),
                        Column::new("p50", "p50").width(rems(5.)),
                        Column::new("p95", "p95").width(rems(5.)),
                        Column::new("max", t!("Max")).width(rems(5.)),
                        Column::new("last", t!("Last error")),
                    ])
                    .rows(rows),
                ),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::{percentile, stats};
    use serde_json::json;

    #[test]
    fn percentiles_por_rango() {
        let v = [10., 20., 30., 40., 50., 60., 70., 80., 90., 100.];
        assert_eq!(percentile(&v, 0.5), Some(50.));
        assert_eq!(percentile(&v, 0.95), Some(100.));
        assert_eq!(percentile(&[7.], 0.95), Some(7.));
        assert_eq!(percentile(&[], 0.5), None);
    }

    #[test]
    fn agrupa_por_funcion_con_errores_primero() {
        let logs = vec![
            json!({ "identifier": "tasks:list", "udfType": "Query", "executionTime": 0.004 }),
            json!({ "identifier": "tasks:list", "udfType": "Query", "executionTime": 0.120 }),
            json!({ "identifier": "comments:add", "udfType": "Mutation", "executionTime": 0.030, "error": "Uncaught Error: task not found\n  at handler" }),
            json!({ "identifier": "comments:add", "udfType": "Mutation", "executionTime": 0.010 }),
            json!({ "logLines": ["a console line without identifier"] }),
        ];
        let s = stats(&logs);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].function, "comments:add");
        assert_eq!((s[0].calls, s[0].errors), (2, 1));
        assert_eq!(s[0].last_error.as_deref(), Some("Uncaught Error: task not found"));
        assert_eq!(s[1].function, "tasks:list");
        assert_eq!(s[1].max_ms.map(|m| m.round()), Some(120.));
    }
}
