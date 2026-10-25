// Disposición «por relaciones» del diagrama del esquema: capas de izquierda a
// derecha, cada tabla a la derecha de las que apunta. Adaptada de
// `layered_layout_sized` de GPUI Box (MIT, github.com/fran0220/gpui-box,
// crates/gpui-kit/src/canvas/layout.rs): los ciclos se condensan en grupos
// (componentes fuertemente conexas, Kosaraju) que comparten columna, y ocho
// barridos de baricentro ordenan cada capa quedándose con el orden de menos
// cruces. Para un esquema se agregan dos cosas: una capa demasiado alta se
// parte en varias columnas, y las tablas sin relaciones van aparte, en grilla.

use std::collections::BTreeSet;

/// Lugar de cada nodo (en el orden de `sizes`). `edges` va de padre a hijo:
/// el padre queda a la izquierda.
pub fn layered(sizes: &[(f32, f32)], edges: &[(usize, usize)], col_gap: f32, row_gap: f32, max_h: f32) -> Vec<(f32, f32)> {
    let n = sizes.len();
    let mut out = vec![BTreeSet::new(); n];
    let mut linked = vec![false; n];
    for &(a, b) in edges {
        if a != b && a < n && b < n {
            out[a].insert(b);
            linked[a] = true;
            linked[b] = true;
        }
    }
    let mut places = vec![(0f32, 0f32); n];

    // Grupos: los ciclos (a → b → a) quedan juntos en una columna.
    let comps = strongly_connected(&out);
    let mut comp_of = vec![0usize; n];
    for (c, members) in comps.iter().enumerate() {
        for &m in members {
            comp_of[m] = c;
        }
    }
    let g = comps.len();
    let mut g_out = vec![BTreeSet::new(); g];
    let mut g_nb = vec![BTreeSet::new(); g];
    for (a, targets) in out.iter().enumerate() {
        for &b in targets {
            let (ca, cb) = (comp_of[a], comp_of[b]);
            if ca != cb {
                g_out[ca].insert(cb);
                g_nb[ca].insert(cb);
                g_nb[cb].insert(ca);
            }
        }
    }
    let g_size: Vec<(f32, f32)> = comps
        .iter()
        .map(|m| {
            let w = m.iter().map(|&i| sizes[i].0).fold(0., f32::max);
            let h = m.iter().map(|&i| sizes[i].1).sum::<f32>() + row_gap * (m.len().saturating_sub(1)) as f32;
            (w, h)
        })
        .collect();
    let g_linked: Vec<bool> = comps.iter().map(|m| m.iter().any(|&i| linked[i])).collect();

    // Capas por camino más largo desde los que no tienen padre (Kahn).
    let mut indeg = vec![0usize; g];
    for targets in &g_out {
        for &b in targets {
            indeg[b] += 1;
        }
    }
    let mut layer = vec![0usize; g];
    let mut ready: Vec<usize> = (0..g).filter(|&c| indeg[c] == 0).collect();
    while let Some(c) = ready.pop() {
        for &b in &g_out[c] {
            layer[b] = layer[b].max(layer[c] + 1);
            indeg[b] -= 1;
            if indeg[b] == 0 {
                ready.push(b);
            }
        }
    }
    let depth = (0..g).filter(|&c| g_linked[c]).map(|c| layer[c]).max().map(|d| d + 1).unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![vec![]; depth];
    for c in 0..g {
        if g_linked[c] {
            layers[layer[c]].push(c);
        }
    }
    barycenter_order(&mut layers, &g_nb);

    // Columnas: cada capa de arriba abajo; si pasa de `max_h`, sigue en otra columna al lado.
    let mut x = 0f32;
    let mut columns: Vec<(Vec<(usize, f32)>, f32)> = vec![];
    for l in &layers {
        let mut col: Vec<(usize, f32)> = vec![];
        let (mut y, mut w) = (0f32, 0f32);
        for &c in l {
            let (cw, ch) = g_size[c];
            if y > 0. && y + ch > max_h {
                columns.push((std::mem::take(&mut col), y - row_gap));
                x += w + col_gap * 0.5;
                y = 0.;
                w = 0.;
            }
            col.push((c, x));
            w = w.max(cw);
            y += ch + row_gap;
        }
        if !col.is_empty() {
            columns.push((col, y - row_gap));
        }
        x += w + col_gap;
    }
    let tallest = columns.iter().map(|c| c.1).fold(0., f32::max);
    for (col, h) in &columns {
        // Columnas centradas en vertical: las líneas quedan más cortas.
        let mut y = (tallest - h) / 2.;
        for &(c, cx) in col {
            for &m in &comps[c] {
                places[m] = (cx, y);
                y += sizes[m].1 + row_gap;
            }
        }
    }

    // Sin relaciones: en grilla debajo, del ancho de lo de arriba.
    let loose: Vec<usize> = (0..n).filter(|&i| !linked[i]).collect();
    if !loose.is_empty() {
        let width = (x - col_gap).max(sizes.iter().map(|s| s.0).fold(0., f32::max));
        let cell = loose.iter().map(|&i| sizes[i].0).fold(0., f32::max) + row_gap;
        let cols = ((width / cell).floor() as usize).max(1);
        let top = if depth > 0 { tallest + col_gap } else { 0. };
        let mut col_y = vec![top; cols];
        for i in loose {
            let c = (0..cols).min_by(|a, b| col_y[*a].total_cmp(&col_y[*b])).unwrap_or(0);
            places[i] = (c as f32 * cell, col_y[c]);
            col_y[c] += sizes[i].1 + row_gap;
        }
    }
    places
}

/// Barridos de baricentro alternados (ida y vuelta); se queda con el orden de menos cruces.
fn barycenter_order(layers: &mut [Vec<usize>], nb: &[BTreeSet<usize>]) {
    let mut pos: Vec<Option<usize>> = vec![None; nb.len()];
    let mut best = layers.to_vec();
    let mut best_count = crossings(layers, nb, &mut pos);
    for pass in 0..8 {
        let order: Vec<usize> = if pass % 2 == 0 { (1..layers.len()).collect() } else { (0..layers.len().saturating_sub(1)).rev().collect() };
        for l in order {
            let prev = if pass % 2 == 0 { l - 1 } else { l + 1 };
            for (rank, &c) in layers[prev].iter().enumerate() {
                pos[c] = Some(rank);
            }
            let mut scored: Vec<(usize, f32)> = layers[l]
                .iter()
                .enumerate()
                .map(|(rank, &c)| {
                    let at: Vec<usize> = nb[c].iter().filter_map(|&o| pos[o]).collect();
                    (c, if at.is_empty() { rank as f32 } else { at.iter().sum::<usize>() as f32 / at.len() as f32 })
                })
                .collect();
            scored.sort_by(|a, b| a.1.total_cmp(&b.1));
            layers[l] = scored.into_iter().map(|(c, _)| c).collect();
            for &c in &layers[prev] {
                pos[c] = None;
            }
        }
        let count = crossings(layers, nb, &mut pos);
        if count < best_count {
            best_count = count;
            best.clone_from_slice(layers);
        }
    }
    layers.clone_from_slice(&best);
}

/// Cruces entre capas vecinas (inversiones, con un árbol de Fenwick).
fn crossings(layers: &[Vec<usize>], nb: &[BTreeSet<usize>], pos: &mut [Option<usize>]) -> u64 {
    let mut total = 0u64;
    for pair in layers.windows(2) {
        for (rank, &c) in pair[1].iter().enumerate() {
            pos[c] = Some(rank);
        }
        let mut tree = vec![0u64; pair[1].len() + 1];
        let mut seen = 0u64;
        for &s in &pair[0] {
            for rank in nb[s].iter().filter_map(|&o| pos[o]) {
                let (mut at, mut upto) = (rank + 1, 0);
                while at > 0 {
                    upto += tree[at];
                    at &= at - 1;
                }
                total += seen - upto;
            }
            for rank in nb[s].iter().filter_map(|&o| pos[o]) {
                seen += 1;
                let mut at = rank + 1;
                while at < tree.len() {
                    tree[at] += 1;
                    at += at.isolate_lowest_one();
                }
            }
        }
        for &c in &pair[1] {
            pos[c] = None;
        }
    }
    total
}

/// Componentes fuertemente conexas (Kosaraju, iterativo).
fn strongly_connected(out: &[BTreeSet<usize>]) -> Vec<Vec<usize>> {
    let n = out.len();
    let mut inc = vec![vec![]; n];
    for (a, t) in out.iter().enumerate() {
        for &b in t {
            inc[b].push(a);
        }
    }
    let mut seen = vec![false; n];
    let mut finished = Vec::with_capacity(n);
    for root in 0..n {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, out[root].iter())];
        while let Some((node, children)) = stack.last_mut() {
            if let Some(&next) = children.next() {
                if !seen[next] {
                    seen[next] = true;
                    stack.push((next, out[next].iter()));
                }
            } else {
                finished.push(*node);
                stack.pop();
            }
        }
    }
    seen.fill(false);
    let mut comps = vec![];
    for root in finished.into_iter().rev() {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut members = vec![];
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            members.push(node);
            for &next in &inc[node] {
                if !seen[next] {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        members.sort_unstable();
        comps.push(members);
    }
    comps.sort_by_key(|m| m[0]);
    comps
}

#[cfg(test)]
mod tests {
    #[test]
    fn padres_a_la_izquierda_ciclos_juntos_y_sueltas_aparte() {
        // 0 → 1 → 2, 2 ↔ 3 (ciclo), 4 suelta.
        let sizes = vec![(100., 50.); 5];
        let p = super::layered(&sizes, &[(0, 1), (1, 2), (2, 3), (3, 2)], 40., 10., 1000.);
        assert!(p[0].0 < p[1].0 && p[1].0 < p[2].0);
        assert_eq!(p[2].0, p[3].0, "el ciclo comparte columna");
        assert!(p[4].1 > p[0].1.max(p[1].1).max(p[2].1).max(p[3].1), "la suelta va debajo");
    }

    #[test]
    fn una_capa_alta_se_parte_en_columnas() {
        // Un padre con 10 hijos altos: la capa de los hijos no pasa de 300 de alto.
        let mut sizes = vec![(100., 100.)];
        sizes.extend(vec![(100., 100.); 10]);
        let edges: Vec<(usize, usize)> = (1..=10).map(|i| (0, i)).collect();
        let p = super::layered(&sizes, &edges, 40., 10., 300.);
        let xs: std::collections::BTreeSet<i32> = p[1..].iter().map(|q| q.0 as i32).collect();
        assert!(xs.len() >= 3);
        assert!(p[1..].iter().all(|q| q.1 < 330.));
    }

    #[test]
    fn el_baricentro_no_empeora_los_cruces() {
        let nb = vec![
            [2usize].into_iter().collect(),
            [3usize].into_iter().collect(),
            [0usize].into_iter().collect(),
            [1usize].into_iter().collect(),
        ];
        let mut layers = vec![vec![0, 1], vec![3, 2]];
        let mut pos = vec![None; 4];
        let before = super::crossings(&layers, &nb, &mut pos);
        super::barycenter_order(&mut layers, &nb);
        assert!(super::crossings(&layers, &nb, &mut pos) <= before);
        assert_eq!(super::crossings(&layers, &nb, &mut pos), 0);
    }
}
