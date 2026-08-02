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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::Hash;

use crate::measure::Measure;
use crate::types::{AnchorId, EntryId, Relation, RelationScaling, SourceId, Transform};

/// A node in the borrow graph, interned to a pair of small integers
/// (see `Interner`) rather than kept as `(EntryId, AnchorId)` directly:
/// this graph fans out to one node per `(entry, anchor)` touched by
/// *either* a measure cell or a relation edge, and a `Transform::Scalar`
/// relation (the common case) expands into one edge per active anchor
/// — so the naive `(EntryId, AnchorId)` keying cloned both `String`s on
/// every node lookup, redundant or not, which dominated this phase's
/// cost on the real dataset's relation-heavy snapshots.
type Node = (u32, u32);

/// Interns `T` to a small integer, cloning only on a genuinely new
/// value rather than on every lookup (`HashMap::get` takes `&T`, so a
/// redundant lookup — the common case once most values have been seen
/// — never allocates). `resolve` is only needed once, when converting
/// the solved result back to owned ids at the very end.
struct Interner<T: Eq + Hash + Clone> {
    id_of: HashMap<T, u32>,
    values: Vec<T>,
}

impl<T: Eq + Hash + Clone> Interner<T> {
    fn new() -> Self {
        Self {
            id_of: HashMap::new(),
            values: Vec::new(),
        }
    }

    fn intern(&mut self, value: &T) -> u32 {
        if let Some(&i) = self.id_of.get(value) {
            return i;
        }
        let i = self.values.len() as u32;
        self.id_of.insert(value.clone(), i);
        self.values.push(value.clone());
        i
    }

    fn resolve(&self, id: u32) -> &T {
        &self.values[id as usize]
    }
}

/// `RelationScaling::PowQ`: replace a raw weight `w` with `w^(1/q)`,
/// so that `w` comes out exactly linearly in an isolated borrowed
/// atom's contribution after the top nonlinearity, for every `q` (see
/// `RelationScaling`'s docs). Guards against `w <= 0` or `q <= 0`
/// (fractional powers of a non-positive base are undefined/complex)
/// by leaving the weight unscaled — relation weights are meant to be
/// borrow fractions in `(0, 1]`, so this never triggers on real data.
fn scale_weight(w: f32, q: f32, scaling: RelationScaling) -> f64 {
    // In f64: `w^(1/q)` at low q drives a borrow weight far below f32's
    // ~1e-38 floor (e.g. 0.04^(1/0.05) ≈ 1e-33, and chains multiply
    // those together during the solve), where f32 would flush it to an
    // exact zero and silently drop the borrow. f64's ~1e-308 range keeps
    // it a real number until the final quantisation back to f32.
    let w = f64::from(w);
    match scaling {
        RelationScaling::Fixed => w,
        RelationScaling::PowQ => {
            if w > 0.0 && q > 0.0 {
                w.powf(1.0 / f64::from(q))
            } else {
                w
            }
        }
    }
}

/// One relation's transform, expanded into `(dst_anchor_on_borrower,
/// src_anchor_on_lender, weight)` edges (anchors already interned to
/// `u32`), with `weight` already put through `scale_weight`.
/// `Transport`'s map is keyed `(src_anchor, dst_anchor)`: mass moves
/// *from* `src_anchor` (on the lender) *to* `dst_anchor` (on the
/// borrower).
fn transform_edges(
    transform: &Transform,
    active_anchors: &BTreeSet<u32>,
    anchors: &mut Interner<AnchorId>,
    q: f32,
    scaling: RelationScaling,
) -> Vec<(u32, u32, f64)> {
    let scale = |w: f32| scale_weight(w, q, scaling);
    match transform {
        Transform::Scalar(c) => active_anchors.iter().map(|&a| (a, a, scale(*c))).collect(),
        Transform::Regional(map) => map
            .iter()
            .filter(|(_, w)| **w != 0.0)
            .map(|(a, w)| {
                let a = anchors.intern(a);
                (a, a, scale(*w))
            })
            .collect(),
        Transform::Transport(map) => map
            .iter()
            .map(|((src, dst), w)| (anchors.intern(dst), anchors.intern(src), scale(*w)))
            .collect(),
    }
}

fn intern_node(node: Node, index: &mut HashMap<Node, usize>, nodes: &mut Vec<Node>) -> usize {
    if let Some(&i) = index.get(&node) {
        return i;
    }
    let i = nodes.len();
    index.insert(node, i);
    nodes.push(node);
    i
}

/// Tarjan's SCC algorithm. Returns components in the order they are
/// closed, which (since DFS closes a node only after everything it
/// points to) means: if there is an edge `u -> v`, `v`'s component
/// appears no later than `u`'s. That is exactly "resolve dependencies
/// before dependents", so no separate topological sort is needed.
fn tarjan_sccs(adj: &BTreeMap<usize, BTreeMap<usize, f64>>, node_count: usize) -> Vec<Vec<usize>> {
    struct State {
        counter: usize,
        index: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }

    fn strongconnect(v: usize, adj: &BTreeMap<usize, BTreeMap<usize, f64>>, st: &mut State) {
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
fn solve_dense(mat: &[Vec<f64>], rhs: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let k = mat.len();
    let m = rhs.first().map_or(0, Vec::len);
    let mut a: Vec<Vec<f64>> = mat.to_vec();
    let mut b: Vec<Vec<f64>> = rhs.to_vec();

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

    let mut x = vec![vec![0.0f64; m]; k];
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
/// fully-borrowed measure. `q`/`scaling` only affect how a raw
/// relation weight is reinterpreted (`RelationScaling::PowQ`); under
/// the default `Fixed`, this phase is exactly as `q`-independent as
/// INFRA.md's phase split intends.
pub(crate) fn solve_relations(
    measures: BTreeMap<EntryId, Measure>,
    relations: &[Relation],
    q: f32,
    scaling: RelationScaling,
) -> BTreeMap<EntryId, Measure> {
    let mut entries: Interner<EntryId> = Interner::new();
    let mut anchors: Interner<AnchorId> = Interner::new();
    let mut sources: Interner<SourceId> = Interner::new();

    let active_anchors: BTreeSet<u32> = measures
        .values()
        .flat_map(|m| m.keys().map(|(_, a)| a))
        .map(|a| anchors.intern(a))
        .collect();

    let mut node_index: HashMap<Node, usize> = HashMap::new();
    let mut nodes: Vec<Node> = Vec::new();
    // adjacency[u] = { v: accumulated weight of edges u -> v }
    let mut adj: BTreeMap<usize, BTreeMap<usize, f64>> = BTreeMap::new();

    for (entry, measure) in &measures {
        let e = entries.intern(entry);
        for (_, anchor) in measure.keys() {
            let a = anchors.intern(anchor);
            intern_node((e, a), &mut node_index, &mut nodes);
        }
    }
    for relation in relations {
        let from_entry = entries.intern(&relation.from);
        let to_entry = entries.intern(&relation.to);
        for (dst_anchor, src_anchor, weight) in
            transform_edges(&relation.transform, &active_anchors, &mut anchors, q, scaling)
        {
            if weight == 0.0 {
                continue;
            }
            let from = intern_node((from_entry, dst_anchor), &mut node_index, &mut nodes);
            let to = intern_node((to_entry, src_anchor), &mut node_index, &mut nodes);
            *adj.entry(from).or_default().entry(to).or_insert(0.0) += weight;
        }
    }

    // Own (pre-relation) mass at each node, broken out by source. Carried
    // in f64 through the whole borrow solve; only quantised back to the
    // measure's f32 at the very end. One pass per entry's measure
    // (rather than one pass *per node*, re-scanning the whole measure
    // and filtering by anchor each time): every `(entry, anchor)` node
    // touched here was already registered above, so the node index is a
    // direct lookup.
    let mut resolved: Vec<BTreeMap<u32, f64>> = vec![BTreeMap::new(); nodes.len()];
    for (entry, measure) in &measures {
        let e = entries.intern(entry);
        for ((source, anchor), mass) in measure {
            let a = anchors.intern(anchor);
            let node = node_index[&(e, a)];
            let s = sources.intern(source);
            resolved[node].insert(s, f64::from(*mass));
        }
    }

    for scc in tarjan_sccs(&adj, nodes.len()) {
        let k = scc.len();
        let local: BTreeMap<usize, usize> = scc.iter().enumerate().map(|(l, &g)| (g, l)).collect();

        let mut internal = vec![vec![0.0f64; k]; k];
        let mut rhs_map: Vec<BTreeMap<u32, f64>> =
            (0..k).map(|l| resolved[scc[l]].clone()).collect();

        for (l, &g) in scc.iter().enumerate() {
            if let Some(targets) = adj.get(&g) {
                for (&t, &w) in targets {
                    if let Some(&lt) = local.get(&t) {
                        internal[l][lt] += w;
                    } else {
                        for (&source, &mass) in &resolved[t] {
                            *rhs_map[l].entry(source).or_insert(0.0) += w * mass;
                        }
                    }
                }
            }
        }

        let scc_sources: Vec<u32> = {
            let mut set: BTreeSet<u32> = BTreeSet::new();
            for row in &rhs_map {
                set.extend(row.keys().copied());
            }
            set.into_iter().collect()
        };

        let mat: Vec<Vec<f64>> = (0..k)
            .map(|i| {
                (0..k)
                    .map(|j| (if i == j { 1.0 } else { 0.0 }) - internal[i][j])
                    .collect()
            })
            .collect();
        let rhs: Vec<Vec<f64>> = rhs_map
            .iter()
            .map(|row| {
                scc_sources
                    .iter()
                    .map(|s| row.get(s).copied().unwrap_or(0.0))
                    .collect()
            })
            .collect();

        let x = solve_dense(&mat, &rhs);

        for (l, &g) in scc.iter().enumerate() {
            let mut row = BTreeMap::new();
            for (c, &source) in scc_sources.iter().enumerate() {
                let v = x[l][c];
                if v != 0.0 {
                    row.insert(source, v);
                }
            }
            resolved[g] = row;
        }
    }

    let mut out: BTreeMap<EntryId, Measure> = measures
        .keys()
        .map(|e| (e.clone(), Measure::new()))
        .collect();
    for (i, &(e, a)) in nodes.iter().enumerate() {
        if resolved[i].is_empty() {
            continue;
        }
        let entry = entries.resolve(e).clone();
        let anchor = anchors.resolve(a);
        for (&s, mass) in &resolved[i] {
            // Quantise the f64 solve result back to the measure's f32. A
            // borrow smaller than f32's floor collapses to 0 here, but by
            // now it is genuinely negligible (its downstream contribution
            // `~mass^q` is also vanishing); the point of the f64 solve was
            // to keep it a real number *through* the elimination so it
            // wasn't lost to a premature underflow mid-computation.
            let mass = *mass as f32;
            if mass != 0.0 {
                out.entry(entry.clone())
                    .or_default()
                    .insert((sources.resolve(s).clone(), anchor.clone()), mass);
            }
        }
    }
    out
}
