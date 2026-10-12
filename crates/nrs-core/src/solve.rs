//! Phase 2 (INFRA.md §4.3): resolve relation borrowing into each entry's
//! final embedded measure.
//!
//! Relations borrow **substance**, not score. For every `(entry, anchor)`
//! node `X` and every node `o` that holds own embedded mass, `phi_X(o)`
//! is the fraction of `o`'s own mass present at `X`:
//!
//! ```text
//! phi_X(o) = min(1, [X = o] + sum over edges X -> Y of w * phi_Y(o))
//! e_X      = sum over o of phi_X(o) * e_own(o)
//! ```
//!
//! The cap is what stops double counting: a franchise that contains an
//! album and also each of its tracks still holds each track once, and an
//! entry borrowing back its own mass through a remix cycle never holds
//! more than all of it. Two halves of one impact reaching an entry along
//! different paths still add up to the whole.
//!
//! The map is monotone and 1-Lipschitz, so whenever the borrow graph is a
//! contraction (spectral radius < 1, as the old linear solve required)
//! the fixed point is unique. Strongly connected components are resolved
//! dependencies-first; acyclic nodes take one exact step, and a cycle is
//! iterated to convergence. Relation weights act on embedded mass.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::Hash;

use crate::measure::Measure;
use crate::types::{AnchorId, EntryId, Relation, Transform};

/// A node in the borrow graph: `(entry, anchor)` interned to small
/// integers, so node lookups never clone the id `String`s.
type Node = (u32, u32);

/// Origin fractions at one node: origin node index -> `phi`.
type Fractions = BTreeMap<usize, f64>;

/// Interns `T` to a small integer, cloning only on a genuinely new value.
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

/// One relation's transform, expanded into `(dst_anchor_on_borrower,
/// src_anchor_on_lender, weight)` edges. `Transport`'s map is keyed
/// `(src_anchor, dst_anchor)` and moves embedded mass across anchors.
fn transform_edges(
    transform: &Transform,
    active_anchors: &BTreeSet<u32>,
    anchors: &mut Interner<AnchorId>,
) -> Vec<(u32, u32, f64)> {
    match transform {
        Transform::Scalar(c) => active_anchors
            .iter()
            .map(|&a| (a, a, f64::from(*c)))
            .collect(),
        Transform::Regional(map) => map
            .iter()
            .filter(|(_, w)| **w != 0.0)
            .map(|(a, w)| {
                let a = anchors.intern(a);
                (a, a, f64::from(*w))
            })
            .collect(),
        Transform::Transport(map) => map
            .iter()
            .map(|((src, dst), w)| (anchors.intern(dst), anchors.intern(src), f64::from(*w)))
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

/// Tarjan's SCC algorithm. Components come out in the order they close,
/// so for an edge `u -> v`, `v`'s component appears no later than `u`'s:
/// dependencies before dependents.
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

fn add_scaled(acc: &mut Fractions, from: &Fractions, w: f64) {
    for (&o, &phi) in from {
        *acc.entry(o).or_insert(0.0) += w * phi;
    }
}

fn cap(mut phi: Fractions) -> Fractions {
    phi.retain(|_, v| {
        *v = v.min(1.0);
        *v != 0.0
    });
    phi
}

/// A cycle's fractions converge geometrically at the borrow graph's
/// spectral radius; this bound only matters for a non-contracting cycle,
/// where the cap still keeps the iteration bounded and monotone.
const MAX_CYCLE_ITERATIONS: usize = 10_000;
const CYCLE_TOLERANCE: f64 = 1e-12;

/// Phase 2: return every entry's fully-borrowed embedded measure.
pub(crate) fn solve_relations(
    measures: BTreeMap<EntryId, Measure>,
    relations: &[Relation],
) -> BTreeMap<EntryId, Measure> {
    let mut entries: Interner<EntryId> = Interner::new();
    let mut anchors: Interner<AnchorId> = Interner::new();

    let active_anchors: BTreeSet<u32> = measures
        .values()
        .flat_map(|m| m.keys())
        .map(|a| anchors.intern(a))
        .collect();

    let mut node_index: HashMap<Node, usize> = HashMap::new();
    let mut nodes: Vec<Node> = Vec::new();
    // adjacency[u] = { v: accumulated weight of edges u -> v }
    let mut adj: BTreeMap<usize, BTreeMap<usize, f64>> = BTreeMap::new();

    let mut own: Vec<f64> = Vec::new();
    for (entry, measure) in &measures {
        let e = entries.intern(entry);
        for (anchor, &mass) in measure {
            let a = anchors.intern(anchor);
            let i = intern_node((e, a), &mut node_index, &mut nodes);
            own.resize(nodes.len(), 0.0);
            own[i] += mass;
        }
    }
    for relation in relations {
        let from_entry = entries.intern(&relation.from);
        let to_entry = entries.intern(&relation.to);
        for (dst_anchor, src_anchor, weight) in
            transform_edges(&relation.transform, &active_anchors, &mut anchors)
        {
            if weight == 0.0 {
                continue;
            }
            let from = intern_node((from_entry, dst_anchor), &mut node_index, &mut nodes);
            let to = intern_node((to_entry, src_anchor), &mut node_index, &mut nodes);
            *adj.entry(from).or_default().entry(to).or_insert(0.0) += weight;
        }
    }
    own.resize(nodes.len(), 0.0);

    let mut phi: Vec<Fractions> = vec![Fractions::new(); nodes.len()];
    for scc in tarjan_sccs(&adj, nodes.len()) {
        let members: BTreeSet<usize> = scc.iter().copied().collect();
        // Everything not inside this component is already resolved.
        let base: Vec<Fractions> = scc
            .iter()
            .map(|&g| {
                let mut acc = Fractions::new();
                if own[g] != 0.0 {
                    acc.insert(g, 1.0);
                }
                if let Some(targets) = adj.get(&g) {
                    for (&t, &w) in targets {
                        if !members.contains(&t) {
                            add_scaled(&mut acc, &phi[t], w);
                        }
                    }
                }
                acc
            })
            .collect();
        let cyclic = scc.len() > 1
            || adj
                .get(&scc[0])
                .is_some_and(|targets| targets.contains_key(&scc[0]));
        if !cyclic {
            phi[scc[0]] = cap(base.into_iter().next().unwrap());
            continue;
        }
        for (l, &g) in scc.iter().enumerate() {
            phi[g] = cap(base[l].clone());
        }
        for _ in 0..MAX_CYCLE_ITERATIONS {
            let mut change = 0.0f64;
            let next: Vec<Fractions> = scc
                .iter()
                .enumerate()
                .map(|(l, &g)| {
                    let mut acc = base[l].clone();
                    for (&t, &w) in &adj[&g] {
                        if members.contains(&t) {
                            add_scaled(&mut acc, &phi[t], w);
                        }
                    }
                    cap(acc)
                })
                .collect();
            for (l, &g) in scc.iter().enumerate() {
                for (o, &v) in &next[l] {
                    change = change.max((v - phi[g].get(o).copied().unwrap_or(0.0)).abs());
                }
                phi[g] = next[l].clone();
            }
            if change < CYCLE_TOLERANCE {
                break;
            }
        }
    }

    let mut out: BTreeMap<EntryId, Measure> = measures
        .keys()
        .map(|e| (e.clone(), Measure::new()))
        .collect();
    for (i, &(e, a)) in nodes.iter().enumerate() {
        let mass: f64 = phi[i].iter().map(|(&o, &f)| f * own[o]).sum();
        if mass != 0.0 {
            out.entry(entries.resolve(e).clone())
                .or_default()
                .insert(anchors.resolve(a).clone(), mass);
        }
    }
    out
}
