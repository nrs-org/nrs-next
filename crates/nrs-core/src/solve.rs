//! Phase 2 (INFRA.md §4.3, notes ch. "Putting it together" Prop.
//! "the borrowing solve decouples per anchor"): resolve every relation's
//! borrowing into each entry's final measure by solving `(I - M) f =
//! f_const`.
//!
//! Relations only ever scale or (rarely, via `Transform::Transport`)
//! redistribute mass across *anchors* — never across *sources*, which
//! is what lets a borrowed atom keep its original source (notes
//! §"Relations as linear operators on measures": "borrowed mass lands
//! on its source"). So the unknown at each node is a whole per-source
//! row, and one `(entry, anchor)` graph carries every source's system
//! at once; when every transform is a scalar field (the common case)
//! this graph is naturally block-diagonal per anchor, which is exactly
//! Prop. 8.2's decoupling — it falls out for free instead of needing a
//! separate code path. `Transform::Transport` just adds edges that
//! cross anchors, which the same solver still handles correctly.

use std::collections::{BTreeMap, BTreeSet};

use crate::measure::Measure;
use crate::types::{AnchorId, EntryId, Relation, SourceId, Transform};

type Node = (EntryId, AnchorId);

/// One relation's transform, expanded into `(dst_anchor_on_borrower,
/// src_anchor_on_lender, weight)` edges. `Transport`'s map is keyed
/// `(src_anchor, dst_anchor)`: mass moves *from* `src_anchor` (on the
/// lender) *to* `dst_anchor` (on the borrower).
fn transform_edges(
    transform: &Transform,
    active_anchors: &BTreeSet<AnchorId>,
) -> Vec<(AnchorId, AnchorId, f32)> {
    match transform {
        Transform::Scalar(c) => active_anchors
            .iter()
            .map(|a| (a.clone(), a.clone(), *c))
            .collect(),
        Transform::Regional(map) => map
            .iter()
            .filter(|(_, w)| **w != 0.0)
            .map(|(a, w)| (a.clone(), a.clone(), *w))
            .collect(),
        Transform::Transport(map) => map
            .iter()
            .map(|((src, dst), w)| (dst.clone(), src.clone(), *w))
            .collect(),
    }
}

fn intern(node: Node, index: &mut BTreeMap<Node, usize>, nodes: &mut Vec<Node>) -> usize {
    if let Some(&i) = index.get(&node) {
        return i;
    }
    let i = nodes.len();
    index.insert(node.clone(), i);
    nodes.push(node);
    i
}

/// Tarjan's SCC algorithm. Returns components in the order they are
/// closed, which (since DFS closes a node only after everything it
/// points to) means: if there is an edge `u -> v`, `v`'s component
/// appears no later than `u`'s. That is exactly "resolve dependencies
/// before dependents", so no separate topological sort is needed.
fn tarjan_sccs(adj: &BTreeMap<usize, BTreeMap<usize, f32>>, node_count: usize) -> Vec<Vec<usize>> {
    struct State {
        counter: usize,
        index: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }

    fn strongconnect(v: usize, adj: &BTreeMap<usize, BTreeMap<usize, f32>>, st: &mut State) {
        st.index[v] = Some(st.counter);
        st.lowlink[v] = st.counter;
        st.counter += 1;
        st.stack.push(v);
        st.on_stack[v] = true;

        if let Some(targets) = adj.get(&v) {
            for &w in targets.keys() {
                if st.index[w].is_none() {
                    strongconnect(w, adj, st);
                    st.lowlink[v] = st.lowlink[v].min(st.lowlink[w]);
                } else if st.on_stack[w] {
                    st.lowlink[v] = st.lowlink[v].min(st.index[w].unwrap());
                }
            }
        }

        if st.lowlink[v] == st.index[v].unwrap() {
            let mut component = Vec::new();
            loop {
                let w = st.stack.pop().unwrap();
                st.on_stack[w] = false;
                component.push(w);
                if w == v {
                    break;
                }
            }
            st.sccs.push(component);
        }
    }

    let mut st = State {
        counter: 0,
        index: vec![None; node_count],
        lowlink: vec![0; node_count],
        on_stack: vec![false; node_count],
        stack: Vec::new(),
        sccs: Vec::new(),
    };
    for v in 0..node_count {
        if st.index[v].is_none() {
            strongconnect(v, adj, &mut st);
        }
    }
    st.sccs
}

/// Solve `(I - mat) x = rhs` (`mat` is `k x k`, `rhs` is `k` rows by `m`
/// columns) via Gaussian elimination with partial pivoting. A singular
/// pivot (a borrow cycle with total gain >= 1, which the notes assume
/// away as a contraction) is left as a zero contribution rather than
/// panicking.
#[allow(clippy::needless_range_loop)] // row/col indices into two matrices at once
fn solve_dense(mat: &[Vec<f32>], rhs: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let k = mat.len();
    let m = rhs.first().map_or(0, Vec::len);
    let mut a: Vec<Vec<f32>> = mat.to_vec();
    let mut b: Vec<Vec<f32>> = rhs.to_vec();

    for col in 0..k {
        let mut pivot_row = col;
        let mut pivot_val = a[col][col].abs();
        for row in (col + 1)..k {
            if a[row][col].abs() > pivot_val {
                pivot_val = a[row][col].abs();
                pivot_row = row;
            }
        }
        if pivot_row != col {
            a.swap(col, pivot_row);
            b.swap(col, pivot_row);
        }
        let pivot = a[col][col];
        if pivot.abs() < 1e-9 {
            continue;
        }
        for row in (col + 1)..k {
            let factor = a[row][col] / pivot;
            if factor == 0.0 {
                continue;
            }
            for c in col..k {
                a[row][c] -= factor * a[col][c];
            }
            for c in 0..m {
                b[row][c] -= factor * b[col][c];
            }
        }
    }

    let mut x = vec![vec![0.0f32; m]; k];
    for row in (0..k).rev() {
        for c in 0..m {
            let mut sum = b[row][c];
            for col in (row + 1)..k {
                sum -= a[row][col] * x[col][c];
            }
            x[row][c] = if a[row][row].abs() > 1e-9 {
                sum / a[row][row]
            } else {
                0.0
            };
        }
    }
    x
}

/// Phase 2: resolve `(I - M) f = f_const` and return every entry's
/// fully-borrowed measure.
pub(crate) fn solve_relations(
    measures: BTreeMap<EntryId, Measure>,
    relations: &[Relation],
) -> BTreeMap<EntryId, Measure> {
    let active_anchors: BTreeSet<AnchorId> = measures
        .values()
        .flat_map(|m| m.keys().map(|(_, a)| a.clone()))
        .collect();

    let mut node_index: BTreeMap<Node, usize> = BTreeMap::new();
    let mut nodes: Vec<Node> = Vec::new();
    // adjacency[u] = { v: accumulated weight of edges u -> v }
    let mut adj: BTreeMap<usize, BTreeMap<usize, f32>> = BTreeMap::new();

    for (entry, measure) in &measures {
        for (_, anchor) in measure.keys() {
            intern((entry.clone(), anchor.clone()), &mut node_index, &mut nodes);
        }
    }
    for relation in relations {
        for (dst_anchor, src_anchor, weight) in
            transform_edges(&relation.transform, &active_anchors)
        {
            if weight == 0.0 {
                continue;
            }
            let from = intern(
                (relation.from.clone(), dst_anchor),
                &mut node_index,
                &mut nodes,
            );
            let to = intern(
                (relation.to.clone(), src_anchor),
                &mut node_index,
                &mut nodes,
            );
            *adj.entry(from).or_default().entry(to).or_insert(0.0) += weight;
        }
    }

    // Own (pre-relation) mass at each node, broken out by source.
    let mut resolved: Vec<BTreeMap<SourceId, f32>> = vec![BTreeMap::new(); nodes.len()];
    for (i, (entry, anchor)) in nodes.iter().enumerate() {
        if let Some(measure) = measures.get(entry) {
            for ((source, a), mass) in measure {
                if a == anchor {
                    resolved[i].insert(source.clone(), *mass);
                }
            }
        }
    }

    for scc in tarjan_sccs(&adj, nodes.len()) {
        let k = scc.len();
        let local: BTreeMap<usize, usize> = scc.iter().enumerate().map(|(l, &g)| (g, l)).collect();

        let mut internal = vec![vec![0.0f32; k]; k];
        let mut rhs_map: Vec<BTreeMap<SourceId, f32>> =
            (0..k).map(|l| resolved[scc[l]].clone()).collect();

        for (l, &g) in scc.iter().enumerate() {
            if let Some(targets) = adj.get(&g) {
                for (&t, &w) in targets {
                    if let Some(&lt) = local.get(&t) {
                        internal[l][lt] += w;
                    } else {
                        for (source, mass) in &resolved[t] {
                            *rhs_map[l].entry(source.clone()).or_insert(0.0) += w * mass;
                        }
                    }
                }
            }
        }

        let sources: Vec<SourceId> = {
            let mut set: BTreeSet<SourceId> = BTreeSet::new();
            for row in &rhs_map {
                set.extend(row.keys().cloned());
            }
            set.into_iter().collect()
        };

        let mat: Vec<Vec<f32>> = (0..k)
            .map(|i| {
                (0..k)
                    .map(|j| (if i == j { 1.0 } else { 0.0 }) - internal[i][j])
                    .collect()
            })
            .collect();
        let rhs: Vec<Vec<f32>> = rhs_map
            .iter()
            .map(|row| {
                sources
                    .iter()
                    .map(|s| row.get(s).copied().unwrap_or(0.0))
                    .collect()
            })
            .collect();

        let x = solve_dense(&mat, &rhs);

        for (l, &g) in scc.iter().enumerate() {
            let mut row = BTreeMap::new();
            for (c, source) in sources.iter().enumerate() {
                let v = x[l][c];
                if v != 0.0 {
                    row.insert(source.clone(), v);
                }
            }
            resolved[g] = row;
        }
    }

    let mut out: BTreeMap<EntryId, Measure> = measures
        .keys()
        .map(|e| (e.clone(), Measure::new()))
        .collect();
    for (i, (entry, anchor)) in nodes.into_iter().enumerate() {
        for (source, mass) in &resolved[i] {
            if *mass != 0.0 {
                out.entry(entry.clone())
                    .or_default()
                    .insert((source.clone(), anchor.clone()), *mass);
            }
        }
    }
    out
}
