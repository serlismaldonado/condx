// Líneas en ángulo recto que esquivan las cajas. La idea es la del router de
// GPUI Box (MIT, github.com/fran0220/gpui-box, crates/gpui-kit/src/canvas/
// router.rs): A* sobre las coordenadas de los bordes de los obstáculos (nunca
// una grilla completa), con costo por largo y por cada giro, y un tope de
// trabajo por línea; si se agota, una línea en Z simple. Esta versión es más
// corta: solo mira los obstáculos cerca de los extremos.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// Rectángulo: x0, y0, x1, y1.
pub type Rect = (f32, f32, f32, f32);

/// Separación de una línea respecto de una caja ajena.
const CLEAR: f32 = 14.;
/// Lo que sale recta de una caja antes de poder girar.
const LEAD: f32 = 22.;
/// Costo de un giro, en unidades de largo: prefiere menos quiebres.
const BEND: f32 = 60.;
/// Alrededor de los extremos, cuánto se mira para rodear.
const MARGIN: f32 = 420.;
/// Tope de nodos expandidos por línea.
const WORK: usize = 6000;

#[derive(Clone, Copy, PartialEq)]
struct Open {
    cell: usize,
    heading: u8,
    cost: f32,
    estimate: f32,
}
impl Eq for Open {}
impl Ord for Open {
    fn cmp(&self, o: &Self) -> Ordering {
        o.estimate.total_cmp(&self.estimate).then_with(|| o.cost.total_cmp(&self.cost))
    }
}
impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

fn inside(p: (f32, f32), r: &Rect) -> bool {
    p.0 > r.0 && p.0 < r.2 && p.1 > r.1 && p.1 < r.3
}

/// Quita los puntos que no giran.
fn normalize(pts: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = vec![];
    for p in pts {
        if out.last().is_some_and(|l| (l.0 - p.0).abs() < 0.01 && (l.1 - p.1).abs() < 0.01) {
            continue;
        }
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let straight = ((a.0 - b.0).abs() < 0.01 && (b.0 - p.0).abs() < 0.01) || ((a.1 - b.1).abs() < 0.01 && (b.1 - p.1).abs() < 0.01);
            if straight {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// Z simple: sale, baja o sube por el medio, entra.
pub fn simple(a: (f32, f32), da: f32, b: (f32, f32), db: f32) -> Vec<(f32, f32)> {
    let (a1, b1) = ((a.0 + da * LEAD, a.1), (b.0 + db * LEAD, b.1));
    let mx = if da != db { (a1.0 + b1.0) / 2. } else if da > 0. { a1.0.max(b1.0) } else { a1.0.min(b1.0) };
    normalize(vec![a, a1, (mx, a1.1), (mx, b1.1), b1, b])
}

/// Camino de `a` (sale hacia `da`: 1 derecha, -1 izquierda) a `b` (entra desde `db`), sin
/// atravesar `rects` salvo las cajas de sus extremos (`owners`).
pub fn route(rects: &[Rect], owners: [usize; 2], a: (f32, f32), da: f32, b: (f32, f32), db: f32) -> Vec<(f32, f32)> {
    let (a1, b1) = ((a.0 + da * LEAD, a.1), (b.0 + db * LEAD, b.1));
    let env = (a1.0.min(b1.0) - MARGIN, a1.1.min(b1.1) - MARGIN, a1.0.max(b1.0) + MARGIN, a1.1.max(b1.1) + MARGIN);
    // Obstáculos cerca, inflados; las cajas de los extremos también, para no rozarlas al girar.
    let obs: Vec<Rect> = rects
        .iter()
        .enumerate()
        .map(|(i, r)| if owners.contains(&i) { (r.0 - CLEAR * 0.5, r.1 - CLEAR * 0.5, r.2 + CLEAR * 0.5, r.3 + CLEAR * 0.5) } else { (r.0 - CLEAR, r.1 - CLEAR, r.2 + CLEAR, r.3 + CLEAR) })
        .filter(|r| r.2 > env.0 && r.0 < env.2 && r.3 > env.1 && r.1 < env.3)
        .collect();
    if obs.iter().any(|r| inside(a1, r) || inside(b1, r)) {
        return simple(a, da, b, db);
    }
    let mut xs = vec![a1.0, b1.0, env.0, env.2];
    let mut ys = vec![a1.1, b1.1, env.1, env.3];
    for r in &obs {
        xs.extend([r.0, r.2]);
        ys.extend([r.1, r.3]);
    }
    for v in [&mut xs, &mut ys] {
        v.retain(|c| c.is_finite());
        v.sort_by(|p, q| p.total_cmp(q));
        v.dedup_by(|p, q| (*p - *q).abs() < 0.5);
    }
    let (nx, ny) = (xs.len(), ys.len());
    let idx = |x: usize, y: usize| x * ny + y;
    let find = |v: &[f32], c: f32| v.iter().position(|p| (p - c).abs() < 0.5);
    let (Some(sx), Some(sy), Some(tx), Some(ty)) = (find(&xs, a1.0), find(&ys, a1.1), find(&xs, b1.0), find(&ys, b1.1)) else {
        return simple(a, da, b, db);
    };
    let blocked = |p: (f32, f32)| obs.iter().any(|r| inside(p, r));
    // Direcciones: 0 derecha, 1 abajo, 2 izquierda, 3 arriba.
    let start_heading = if da > 0. { 0 } else { 2 };
    let end_heading = if db > 0. { 2 } else { 0 };
    let goal = idx(tx, ty);
    // Por celda y rumbo: el menor costo y de dónde se llegó.
    type Step = (usize, u8);
    let mut best: HashMap<Step, (f32, Option<Step>)> = HashMap::new();
    let mut heap = BinaryHeap::new();
    let h = |c: usize| {
        let (x, y) = (xs[c / ny], ys[c % ny]);
        (x - b1.0).abs() + (y - b1.1).abs()
    };
    let s = idx(sx, sy);
    best.insert((s, start_heading), (0., None));
    heap.push(Open { cell: s, heading: start_heading, cost: 0., estimate: h(s) });
    let mut work = 0;
    let mut found: Option<(usize, u8)> = None;
    while let Some(o) = heap.pop() {
        if best.get(&(o.cell, o.heading)).is_some_and(|b| b.0 < o.cost) {
            continue;
        }
        if o.cell == goal {
            found = Some((o.cell, o.heading));
            break;
        }
        work += 1;
        if work > WORK {
            break;
        }
        let (x, y) = (o.cell / ny, o.cell % ny);
        for d in 0u8..4 {
            if (d + 2) % 4 == o.heading {
                continue; // sin dar media vuelta
            }
            let (nx_, ny_) = match d {
                0 if x + 1 < nx => (x + 1, y),
                1 if y + 1 < ny => (x, y + 1),
                2 if x > 0 => (x - 1, y),
                3 if y > 0 => (x, y - 1),
                _ => continue,
            };
            let (p, q) = ((xs[x], ys[y]), (xs[nx_], ys[ny_]));
            // Entre coordenadas vecinas, el tramo cruza un obstáculo solo si su punto medio está adentro.
            if blocked(q) || blocked(((p.0 + q.0) / 2., (p.1 + q.1) / 2.)) {
                continue;
            }
            let c = idx(nx_, ny_);
            let mut cost = o.cost + (p.0 - q.0).abs() + (p.1 - q.1).abs() + if d != o.heading { BEND } else { 0. };
            if c == goal && d != end_heading {
                cost += BEND;
            }
            if best.get(&(c, d)).is_none_or(|b| cost < b.0) {
                best.insert((c, d), (cost, Some((o.cell, o.heading))));
                heap.push(Open { cell: c, heading: d, cost, estimate: cost + h(c) });
            }
        }
    }
    let Some(mut at) = found else { return simple(a, da, b, db) };
    let mut pts = vec![b, b1];
    while let Some((_, prev)) = best.get(&at).copied() {
        let Some(p) = prev else { break };
        pts.push((xs[p.0 / ny], ys[p.0 % ny]));
        at = p;
    }
    pts.push(a);
    pts.reverse();
    normalize(pts)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rodea_una_caja_en_el_medio() {
        // De la caja 0 (izquierda) a la 2 (derecha), con la 1 en el medio a la misma altura.
        let rects = vec![(0., 0., 100., 100.), (300., -50., 400., 150.), (600., 0., 700., 100.)];
        let path = super::route(&rects, [0, 2], (100., 50.), 1., (600., 50.), -1.);
        assert!(path.len() > 2);
        // Ningún tramo pasa por dentro de la caja del medio.
        for w in path.windows(2) {
            let m = ((w[0].0 + w[1].0) / 2., (w[0].1 + w[1].1) / 2.);
            assert!(!super::inside(m, &rects[1]), "tramo {w:?}");
            assert!(w[0].0 == w[1].0 || w[0].1 == w[1].1, "solo ángulos rectos");
        }
        assert_eq!(path.first(), Some(&(100., 50.)));
        assert_eq!(path.last(), Some(&(600., 50.)));
    }

    #[test]
    fn sin_obstaculos_sale_y_entra_derecho() {
        let rects = vec![(0., 0., 100., 100.), (400., 200., 500., 300.)];
        let path = super::route(&rects, [0, 1], (100., 50.), 1., (400., 250.), -1.);
        assert!(path.len() <= 6);
    }
}
