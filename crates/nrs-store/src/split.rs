//! How a shared impact is divided: a tree of groups over its members.
//!
//! The flat `impact_member` shares are what scoring uses; a split tree is
//! the intent behind them ("0.1 between these six, evenly"; "the rest over
//! the tracks, by weight"). A group's children get portions of the group's
//! own amount by one rule:
//!
//! - `explicit`: each child has its own fraction; any rest is unassigned.
//! - `even`: equal parts.
//! - `weighted`: in proportion to each child's weight.
//!
//! The root is a group whose amount is the whole impact. A member's share
//! is the product of portions down its path. Trees edited in the app are
//! stored (`impact_split`); otherwise `infer` derives one from the flat
//! shares whenever it is shown.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use nrs_core::EntryId;

use crate::shared::ImpactMember;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitRule {
    Explicit,
    Even,
    Weighted,
}

/// A node of a split tree. `share` is used under an `explicit` parent,
/// `weight` under a `weighted` one; under `even` neither matters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SplitNode {
    Member {
        entry: EntryId,
        #[serde(default)]
        share: f64,
        #[serde(default)]
        weight: f64,
    },
    Group {
        rule: SplitRule,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default)]
        share: f64,
        #[serde(default)]
        weight: f64,
        children: Vec<SplitNode>,
    },
}

impl SplitNode {
    fn share(&self) -> f64 {
        match self {
            SplitNode::Member { share, .. } | SplitNode::Group { share, .. } => *share,
        }
    }

    fn weight(&self) -> f64 {
        match self {
            SplitNode::Member { weight, .. } | SplitNode::Group { weight, .. } => *weight,
        }
    }
}

/// Each child's portion of its group's amount.
fn portions(rule: SplitRule, children: &[SplitNode]) -> Vec<f64> {
    match rule {
        SplitRule::Explicit => children.iter().map(SplitNode::share).collect(),
        SplitRule::Even => vec![1.0 / children.len().max(1) as f64; children.len()],
        SplitRule::Weighted => {
            let total: f64 = children.iter().map(SplitNode::weight).sum();
            children
                .iter()
                .map(|c| if total > 0.0 { c.weight() / total } else { 0.0 })
                .collect()
        }
    }
}

/// The flat member shares a tree stands for, in tree order.
pub fn flatten(root: &SplitNode) -> Vec<ImpactMember> {
    fn walk(node: &SplitNode, amount: f64, out: &mut Vec<ImpactMember>) {
        match node {
            SplitNode::Member { entry, .. } => out.push(ImpactMember {
                contributor: entry.clone(),
                factor: amount as f32,
            }),
            SplitNode::Group { rule, children, .. } => {
                for (child, portion) in children.iter().zip(portions(*rule, children)) {
                    walk(child, amount * portion, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 1.0, &mut out);
    out
}

/// Checks a tree before it is stored: a group at the root, every member a
/// distinct entry, finite non-negative numbers, explicit portions summing
/// to at most 1, no empty groups.
pub fn validate(root: &SplitNode) -> Result<(), String> {
    fn walk(node: &SplitNode, seen: &mut BTreeSet<EntryId>) -> Result<(), String> {
        let (share, weight) = (node.share(), node.weight());
        if !share.is_finite() || share < 0.0 || !weight.is_finite() || weight < 0.0 {
            return Err("shares and weights must be finite numbers >= 0".into());
        }
        match node {
            SplitNode::Member { entry, .. } => {
                if !seen.insert(entry.clone()) {
                    return Err(format!("{} appears twice in the split", entry.0));
                }
            }
            SplitNode::Group { rule, children, .. } => {
                if children.is_empty() {
                    return Err("a group needs at least one member".into());
                }
                if *rule == SplitRule::Explicit {
                    let total: f64 = children.iter().map(SplitNode::share).sum();
                    if total > 1.0 + 1e-6 {
                        return Err(format!("explicit shares add up to {total:.3}, more than 1"));
                    }
                }
                for child in children {
                    walk(child, seen)?;
                }
            }
        }
        Ok(())
    }
    if !matches!(root, SplitNode::Group { .. }) {
        return Err("the split must be a group".into());
    }
    walk(root, &mut BTreeSet::new())
}

/// Takes `entry` out of the tree. Even and weighted groups rebalance by
/// their rule; an explicit group leaves the freed share unassigned. Groups
/// left empty disappear. Returns whether `entry` was found.
pub fn remove_member(root: &mut SplitNode, entry: &EntryId) -> bool {
    fn walk(node: &mut SplitNode, entry: &EntryId) -> bool {
        let SplitNode::Group { children, .. } = node else {
            return false;
        };
        if let Some(i) = children
            .iter()
            .position(|c| matches!(c, SplitNode::Member { entry: e, .. } if e == entry))
        {
            children.remove(i);
            return true;
        }
        for i in 0..children.len() {
            if walk(&mut children[i], entry) {
                if matches!(&children[i], SplitNode::Group { children: c, .. } if c.is_empty()) {
                    children.remove(i);
                }
                return true;
            }
        }
        false
    }
    walk(root, entry)
}

/// A share that reads as a deliberate decimal (0.25, 0.05) rather than the
/// remainder of a division or a script's normalisation.
fn is_round(factor: f64) -> bool {
    ((factor * 100.0) - (factor * 100.0).round()).abs() < 1e-3
}

/// A split tree for flat shares that have none stored:
/// - the "non-round" shares (remainders of a division, a script's
///   normalisation) are one pool when there are 3 or more: an even group
///   if they are all equal (`0.1/6` six times), else a weighted group (a
///   script's proportional pool, `1/51` next to `3/51`),
/// - 3+ identical round shares form an even group,
/// - everything else is an explicit member.
///
/// A single group covering the whole impact becomes the root itself.
pub fn infer(members: &[ImpactMember]) -> SplitNode {
    let factors: Vec<f64> = members.iter().map(|m| f64::from(m.factor)).collect();
    let key = |f: f64| (f * 1e6).round() as i64;
    let member = |i: usize, share: f64, weight: f64| SplitNode::Member {
        entry: members[i].contributor.clone(),
        share,
        weight,
    };
    let group = |rule: SplitRule, share: f64, children: Vec<SplitNode>| SplitNode::Group {
        rule,
        label: None,
        share,
        weight: 0.0,
        children,
    };

    let mut grouped = vec![false; members.len()];
    // (position of first member, node), so the tree keeps member order.
    let mut placed: Vec<(usize, SplitNode)> = Vec::new();

    let pool: Vec<usize> = (0..members.len()).filter(|&i| !is_round(factors[i])).collect();
    if pool.len() >= 3 {
        let share: f64 = pool.iter().map(|&i| factors[i]).sum();
        let all_equal = pool.iter().all(|&i| key(factors[i]) == key(factors[pool[0]]));
        let node = if all_equal {
            group(SplitRule::Even, share, pool.iter().map(|&i| member(i, 0.0, 0.0)).collect())
        } else {
            group(
                SplitRule::Weighted,
                share,
                pool.iter().map(|&i| member(i, 0.0, factors[i] / share)).collect(),
            )
        };
        placed.push((pool[0], node));
        for &i in &pool {
            grouped[i] = true;
        }
    }

    // Identical round shares, in first-seen order.
    let mut clusters: Vec<(i64, Vec<usize>)> = Vec::new();
    for i in (0..members.len()).filter(|&i| !grouped[i]) {
        match clusters.iter_mut().find(|(k, _)| *k == key(factors[i])) {
            Some((_, idx)) => idx.push(i),
            None => clusters.push((key(factors[i]), vec![i])),
        }
    }
    for (_, idx) in clusters.iter().filter(|(_, idx)| idx.len() >= 3) {
        let share: f64 = idx.iter().map(|&i| factors[i]).sum();
        placed.push((
            idx[0],
            group(SplitRule::Even, share, idx.iter().map(|&i| member(i, 0.0, 0.0)).collect()),
        ));
        for &i in idx {
            grouped[i] = true;
        }
    }

    for i in (0..members.len()).filter(|&i| !grouped[i]) {
        placed.push((i, member(i, factors[i], 0.0)));
    }
    placed.sort_by_key(|(i, _)| *i);
    let mut children: Vec<SplitNode> = placed.into_iter().map(|(_, n)| n).collect();

    if children.len() == 1 {
        if let SplitNode::Group { share, .. } = &children[0] {
            if (share - 1.0).abs() < 1e-4 {
                let mut only = children.remove(0);
                if let SplitNode::Group { share, .. } = &mut only {
                    *share = 0.0;
                }
                return only;
            }
        }
    }
    group(SplitRule::Explicit, 0.0, children)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(shares: &[(&str, f32)]) -> Vec<ImpactMember> {
        shares
            .iter()
            .map(|(e, f)| ImpactMember {
                contributor: EntryId((*e).into()),
                factor: *f,
            })
            .collect()
    }

    fn close(a: &[ImpactMember], b: &[ImpactMember]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                x.contributor == y.contributor && (x.factor - y.factor).abs() < 1e-6
            })
    }

    #[test]
    fn infers_an_even_group_from_repeated_fractions() {
        // Love Live: 0.3, 0.6, and 0.1 over six seasons.
        let mut shares = vec![("a", 0.3), ("b", 0.6)];
        let sixth = 0.1f32 / 6.0;
        let ids = ["s1", "s2", "s3", "s4", "s5", "s6"];
        shares.extend(ids.iter().map(|id| (*id, sixth)));
        let m = members(&shares);
        let tree = infer(&m);
        let SplitNode::Group { rule, children, .. } = &tree else { panic!() };
        assert_eq!(*rule, SplitRule::Explicit);
        assert_eq!(children.len(), 3);
        assert!(matches!(&children[2], SplitNode::Group { rule: SplitRule::Even, children, .. } if children.len() == 6));
        assert!(close(&flatten(&tree), &m));
    }

    #[test]
    fn infers_a_weighted_pool_beside_round_shares() {
        let m = members(&[("anime", 0.25), ("film", 0.2), ("t1", 0.0274), ("t2", 0.0137), ("t3", 0.0411)]);
        let tree = infer(&m);
        let SplitNode::Group { children, .. } = &tree else { panic!() };
        assert!(matches!(&children[2], SplitNode::Group { rule: SplitRule::Weighted, .. }));
        assert!(close(&flatten(&tree), &m));
    }

    #[test]
    fn mixed_fractions_become_one_weighted_pool() {
        // 1/51 x3 and 3/51 x2 beside an explicit 0.5: weights 1 : 3.
        let (one, three) = (1.0f32 / 51.0, 3.0f32 / 51.0);
        let m = members(&[("x", 0.5), ("a", one), ("b", one), ("c", three), ("d", one), ("e", three)]);
        let tree = infer(&m);
        let SplitNode::Group { children, .. } = &tree else { panic!() };
        assert_eq!(children.len(), 2);
        let SplitNode::Group { rule: SplitRule::Weighted, children: pool, .. } = &children[1] else { panic!("{tree:?}") };
        let weights: Vec<f64> = pool.iter().map(SplitNode::weight).collect();
        assert!((weights[2] / weights[0] - 3.0).abs() < 1e-4, "{weights:?}");
        assert!(close(&flatten(&tree), &m));
    }

    #[test]
    fn a_group_covering_everything_becomes_the_root() {
        let m = members(&[("a", 0.25), ("b", 0.25), ("c", 0.25), ("d", 0.25)]);
        let tree = infer(&m);
        assert!(matches!(&tree, SplitNode::Group { rule: SplitRule::Even, children, .. } if children.len() == 4));
        assert!(close(&flatten(&tree), &m));
    }

    #[test]
    fn removing_a_member_rebalances_even_groups_only() {
        let mut tree = infer(&members(&[
            ("x", 0.4),
            ("a", 0.2),
            ("b", 0.2),
            ("c", 0.2),
        ]));
        assert!(remove_member(&mut tree, &EntryId("b".into())));
        let flat = flatten(&tree);
        assert!(close(&flat, &members(&[("x", 0.4), ("a", 0.3), ("c", 0.3)])), "{flat:?}");
        assert!(remove_member(&mut tree, &EntryId("x".into())));
        assert!(close(&flatten(&tree), &members(&[("a", 0.3), ("c", 0.3)])));
    }

    #[test]
    fn validation_rejects_bad_trees() {
        let dup = SplitNode::Group {
            rule: SplitRule::Even,
            label: None,
            share: 0.0,
            weight: 0.0,
            children: vec![
                SplitNode::Member { entry: EntryId("a".into()), share: 0.0, weight: 0.0 },
                SplitNode::Member { entry: EntryId("a".into()), share: 0.0, weight: 0.0 },
            ],
        };
        assert!(validate(&dup).is_err());
        let over = infer(&members(&[("a", 0.7), ("b", 0.7)]));
        assert!(validate(&over).is_err());
        assert!(validate(&infer(&members(&[("a", 0.7), ("b", 0.2)]))).is_ok());
    }
}
