// Condiciones en las aristas de un flujo (docs/design/agent-flows.md, §2.4): una arista puede decir
// cuándo se pasa por ella, con las salidas de los nodos anteriores. Un nodo con aristas de entrada
// corre si al menos una se cumple; si ninguna, se salta (y dice por qué).
//
//   risk.risk >= high and risk.immediate_fix == true
//   review.verdict == approve
//   triage.confidence != low or risk.risk == critical
//   deploy.status == failed
//
// Comparaciones: == != >= <= > < y `contains`. Los valores van tal cual o entre comillas. `>=`
// ordena números, y las opciones de un agente en su orden (low < medium < high < critical); sin
// orden conocido, compara texto. `and` va antes que `or`. `<nodo>.status` es el estado del nodo.

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq)]
struct Cmp {
    node: String,
    field: String,
    op: String,
    value: String,
}

/// Lo que la condición necesita saber: el valor de `nodo.campo` y, si lo hay, el orden de las
/// opciones de ese campo (para `>=`).
pub trait Facts {
    fn value(&self, node: &str, field: &str) -> Option<String>;
    fn order(&self, node: &str, field: &str) -> Option<Vec<String>>;
}

const OPS: [&str; 7] = [">=", "<=", "==", "!=", ">", "<", " contains "];

fn parse_cmp(text: &str) -> Result<Cmp> {
    let t = text.trim();
    let (at, op) = OPS.iter().filter_map(|op| t.find(op).map(|i| (i, *op))).min_by_key(|(i, _)| *i).ok_or_else(|| anyhow::anyhow!("“{t}”: missing a comparison (==, !=, >=, <=, >, <, contains)"))?;
    let left = t[..at].trim();
    let value = t[at + op.len()..].trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    let Some((node, field)) = left.split_once('.') else { bail!("“{left}”: write it as node.field (e.g. risk.risk)") };
    if node.trim().is_empty() || field.trim().is_empty() || value.is_empty() {
        bail!("“{t}”: incomplete");
    }
    Ok(Cmp { node: node.trim().into(), field: field.trim().into(), op: op.trim().into(), value })
}

/// `or` de grupos `and`.
fn parse(text: &str) -> Result<Vec<Vec<Cmp>>> {
    let mut out = vec![];
    for group in split_word(text, "or") {
        let mut all = vec![];
        for c in split_word(&group, "and") {
            all.push(parse_cmp(&c)?);
        }
        out.push(all);
    }
    Ok(out)
}

/// Parte por la palabra (sin distinguir mayúsculas), fuera de comillas.
fn split_word(text: &str, word: &str) -> Vec<String> {
    let mut parts = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let tokens: Vec<&str> = text.split(' ').collect();
    for (i, tok) in tokens.iter().enumerate() {
        for c in tok.chars() {
            if c == '"' || c == '\'' {
                quote = match quote {
                    Some(q) if q == c => None,
                    None => Some(c),
                    q => q,
                };
            }
        }
        if quote.is_none() && tok.eq_ignore_ascii_case(word) && i > 0 {
            parts.push(std::mem::take(&mut cur));
            continue;
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(tok);
    }
    parts.push(cur);
    parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// Revisa la sintaxis (para avisar al guardar el flujo).
pub fn check(text: &str) -> Result<()> {
    parse(text).map(|_| ())
}

fn holds(c: &Cmp, facts: &dyn Facts) -> bool {
    let Some(actual) = facts.value(&c.node, &c.field) else { return false };
    let (a, b) = (actual.trim().to_lowercase(), c.value.to_lowercase());
    let rank = |order: &Option<Vec<String>>, v: &str| order.as_ref().and_then(|o| o.iter().position(|x| x.eq_ignore_ascii_case(v)));
    let ordering = || -> Option<std::cmp::Ordering> {
        if let (Ok(x), Ok(y)) = (a.parse::<f64>(), b.parse::<f64>()) {
            return x.partial_cmp(&y);
        }
        let order = facts.order(&c.node, &c.field);
        match (rank(&order, &a), rank(&order, &b)) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => Some(a.cmp(&b)),
        }
    };
    use std::cmp::Ordering::*;
    match c.op.as_str() {
        "==" => a == b,
        "!=" => a != b,
        "contains" => a.contains(&b),
        ">=" => matches!(ordering(), Some(Greater | Equal)),
        "<=" => matches!(ordering(), Some(Less | Equal)),
        ">" => matches!(ordering(), Some(Greater)),
        "<" => matches!(ordering(), Some(Less)),
        _ => false,
    }
}

/// ¿Se cumple? Una condición mal escrita no se cumple (y `check` lo avisa antes).
pub fn eval(text: &str, facts: &dyn Facts) -> bool {
    match parse(text) {
        Ok(groups) => groups.iter().any(|all| all.iter().all(|c| holds(c, facts))),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct F(HashMap<(String, String), String>);
    impl Facts for F {
        fn value(&self, n: &str, f: &str) -> Option<String> {
            self.0.get(&(n.to_string(), f.to_string())).cloned()
        }
        fn order(&self, _: &str, f: &str) -> Option<Vec<String>> {
            (f == "risk").then(|| ["low", "medium", "high", "critical"].iter().map(|s| s.to_string()).collect())
        }
    }

    fn facts(pairs: &[(&str, &str, &str)]) -> F {
        F(pairs.iter().map(|(n, f, v)| ((n.to_string(), f.to_string()), v.to_string())).collect())
    }

    #[test]
    fn compara_con_orden_numeros_y_texto() {
        let f = facts(&[("risk", "risk", "high"), ("risk", "immediate_fix", "true"), ("review", "verdict", "approve"), ("t", "count", "12"), ("t", "summary", "Null card in charge")]);
        assert!(eval("risk.risk >= high", &f));
        assert!(eval("risk.risk >= medium and risk.immediate_fix == true", &f));
        assert!(!eval("risk.risk >= critical", &f));
        assert!(eval("risk.risk >= critical or review.verdict == approve", &f));
        assert!(eval("risk.risk < critical", &f));
        assert!(eval("t.count > 9", &f), "12 > 9 as numbers, not text");
        assert!(eval("t.summary contains \"null card\"", &f));
        assert!(eval("review.verdict != changes", &f));
        assert!(!eval("missing.field == x", &f), "an unknown field never holds");
        assert!(eval("RISK.risk >= High AND risk.immediate_fix == TRUE", &facts(&[("RISK", "risk", "high"), ("risk", "immediate_fix", "true")])));
    }

    #[test]
    fn avisa_si_esta_mal_escrita() {
        assert!(check("risk.risk >= high and review.verdict == approve").is_ok());
        assert!(check("risk >= high").is_err());
        assert!(check("risk.risk high").is_err());
        assert!(check("risk.risk ==").is_err());
        assert!(!eval("risk.risk high", &facts(&[])));
    }
}
