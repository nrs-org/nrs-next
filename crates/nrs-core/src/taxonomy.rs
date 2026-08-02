//! Derives the `k_src` taxonomy from `Contains` relations (INFRA.md
//! §5.1: "the taxonomy for `k_src`... needs no separate table —
//! deepest-shared-ancestor is a graph query").
//!
//! `Contains`'s `from`/`to` direction is "borrower/lender" (`solve.rs`:
//! `from` borrows mass *from* `to`), not "child/parent". For structural
//! containment (a franchise or album nested around its works/tracks)
//! that means `from` is the *container* and `to` is the *child* — a
//! franchise has no impacts of its own, it borrows entirely from what it
//! contains. So taxonomically, `to`'s parent is `from`.
//!
//! Only `RelKind::Contains` counts: `RelKind::Credit` (a contributor
//! borrowing a share of a work they're credited on) uses the same edge
//! shape but isn't structural — see `RelKind`'s doc comment. Genuine
//! `Contains` edges are still expected to be a clean forest (each child
//! nested under exactly one container), but if a child somehow ends up
//! with more than one distinct container, it gets no derived parent at
//! all — same as today's empty-taxonomy fallback — rather than picking
//! one arbitrarily.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{RelKind, Relation, SourceId, Taxonomy};

pub fn derive_taxonomy(relations: &[Relation]) -> Taxonomy {
    let mut candidates: BTreeMap<SourceId, BTreeSet<SourceId>> = BTreeMap::new();
    for rel in relations {
        if rel.kind != RelKind::Contains {
            continue;
        }
        // to = child (lender), from = container (borrower); see module docs.
        candidates
            .entry(SourceId(rel.to.0.clone()))
            .or_default()
            .insert(SourceId(rel.from.0.clone()));
    }

    let parent = candidates
        .into_iter()
        .filter_map(|(child, mut containers)| {
            if containers.len() == 1 {
                Some((child, containers.pop_first().unwrap()))
            } else {
                None
            }
        })
        .collect();

    Taxonomy { parent }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EntryId, Transform};

    fn contains(from: &str, to: &str) -> Relation {
        Relation {
            kind: RelKind::Contains,
            from: EntryId(from.into()),
            to: EntryId(to.into()),
            transform: Transform::Scalar(1.0),
        }
    }

    fn credit(from: &str, to: &str) -> Relation {
        Relation {
            kind: RelKind::Credit,
            from: EntryId(from.into()),
            to: EntryId(to.into()),
            transform: Transform::Scalar(1.0),
        }
    }

    fn s(id: &str) -> SourceId {
        SourceId(id.into())
    }

    #[test]
    fn builds_a_chain_from_nested_contains() {
        // franchise Contains album, album Contains track.
        let relations = vec![contains("franchise", "album"), contains("album", "track")];
        let taxonomy = derive_taxonomy(&relations);
        assert_eq!(taxonomy.parent.get(&s("album")), Some(&s("franchise")));
        assert_eq!(taxonomy.parent.get(&s("track")), Some(&s("album")));
    }

    #[test]
    fn fractional_weight_contains_still_counts() {
        // <contains factor="0.2"> is still genuine structural
        // containment -- only `kind` decides, not the transform weight.
        let relations = vec![Relation {
            kind: RelKind::Contains,
            from: EntryId("container".into()),
            to: EntryId("member".into()),
            transform: Transform::Scalar(0.2),
        }];
        let taxonomy = derive_taxonomy(&relations);
        assert_eq!(taxonomy.parent.get(&s("member")), Some(&s("container")));
    }

    #[test]
    fn ignores_non_contains_relations() {
        let relations = vec![
            credit("artist", "track"),
            Relation {
                kind: RelKind::Remix,
                from: EntryId("a".into()),
                to: EntryId("b".into()),
                transform: Transform::Scalar(1.0),
            },
        ];
        let taxonomy = derive_taxonomy(&relations);
        assert!(taxonomy.parent.is_empty());
    }

    #[test]
    fn credit_relations_never_make_a_contributor_a_parent() {
        // An artist credited (even at full weight, e.g. a "total" role)
        // on many tracks must not become any of their taxonomic parent.
        let relations = vec![
            credit("artist", "track-a"),
            credit("artist", "track-b"),
            contains("album", "track-a"),
        ];
        let taxonomy = derive_taxonomy(&relations);
        assert_eq!(taxonomy.parent.get(&s("track-a")), Some(&s("album")));
        assert!(!taxonomy.parent.contains_key(&s("track-b")));
        assert!(!taxonomy.parent.contains_key(&s("artist")));
    }

    #[test]
    fn omits_a_child_with_conflicting_containers() {
        let relations = vec![contains("album-a", "track"), contains("album-b", "track")];
        let taxonomy = derive_taxonomy(&relations);
        assert!(!taxonomy.parent.contains_key(&s("track")));
    }

    #[test]
    fn repeated_identical_edges_still_resolve() {
        let relations = vec![contains("album", "track"), contains("album", "track")];
        let taxonomy = derive_taxonomy(&relations);
        assert_eq!(taxonomy.parent.get(&s("track")), Some(&s("album")));
    }
}
