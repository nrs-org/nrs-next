//! The considered palette (`../../ANCHOR_PALETTE_PROPOSAL.md`), replacing
//! the legacy `nrs-lib-ts` 6-box valence/arousal grid + 3 craft factors
//! (`DAH_factors.ts`) this crate started as a literal migration target
//! of. Mined from the real dataset, not designed in the abstract -- see
//! the proposal doc for the evidence behind each split.

use nrs_core::AnchorId;

// -- feeling anchors, replacing the old AP/MP/CU boxes --------------

pub fn comedy() -> AnchorId {
    AnchorId("comedy".into())
}
pub fn hype() -> AnchorId {
    AnchorId("hype".into())
}
pub fn tragedy() -> AnchorId {
    AnchorId("tragedy".into())
}
pub fn wistful() -> AnchorId {
    AnchorId("wistful".into())
}
pub fn cozy() -> AnchorId {
    AnchorId("cozy".into())
}
pub fn heartwarming() -> AnchorId {
    AnchorId("heartwarming".into())
}
pub fn hate() -> AnchorId {
    AnchorId("hate".into())
}
pub fn discomfort() -> AnchorId {
    AnchorId("discomfort".into())
}

// -- previously-conflated special-case impacts, now their own anchors --

pub fn waifu() -> AnchorId {
    AnchorId("waifu".into())
}
pub fn meme() -> AnchorId {
    AnchorId("meme".into())
}
pub fn hooked() -> AnchorId {
    AnchorId("hooked".into())
}
pub fn worldview() -> AnchorId {
    AnchorId("worldview".into())
}
pub fn gateway() -> AnchorId {
    AnchorId("gateway".into())
}

// -- craft factors, kept (renamed from AL/AV/AM) ---------------------

pub fn writing() -> AnchorId {
    AnchorId("writing".into())
}
pub fn visual() -> AnchorId {
    AnchorId("visual".into())
}
pub fn music() -> AnchorId {
    AnchorId("music".into())
}

// -- scalar extras, kept --------------------------------------------

pub fn boredom() -> AnchorId {
    AnchorId("boredom".into())
}
pub fn additional() -> AnchorId {
    AnchorId("additional".into())
}

// -- default pooling exponents ---------------------------------------

/// The legacy box an anchor descends from, with that box's exponent
/// (`nrs-lib-ts`'s `factorWeight`), and the group the box pooled in.
fn legacy_box(anchor: &str) -> Option<(&'static str, f32, &'static str, f32)> {
    const FEELING: (&str, f32) = ("feeling", 0.6);
    const CRAFT: (&str, f32) = ("craft", 0.7);
    let (b, w, (g, gw)) = match anchor {
        "comedy" | "hype" | "meme" => ("AP", 0.4, FEELING),
        "tragedy" | "wistful" => ("CU", 0.4, FEELING),
        "cozy" => ("CP", 0.5, FEELING),
        "heartwarming" | "waifu" | "hooked" => ("MP", 0.35, FEELING),
        "hate" => ("AU", 0.3, FEELING),
        "discomfort" => ("MU", 0.35, FEELING),
        "writing" => ("AL", 0.4, CRAFT),
        "visual" => ("AV", 0.1, CRAFT),
        "music" => ("AM", 0.3, CRAFT),
        "boredom" => ("B", 0.05, ("boredom", 0.05)),
        "worldview" | "gateway" | "additional" => ("A", 1.0, ("additional", 1.0)),
        _ => return None,
    };
    Some((b, w, g, gw))
}

/// Every palette anchor.
pub fn all() -> Vec<AnchorId> {
    vec![
        comedy(),
        hype(),
        tragedy(),
        wistful(),
        cozy(),
        heartwarming(),
        hate(),
        discomfort(),
        waifu(),
        meme(),
        hooked(),
        worldview(),
        gateway(),
        writing(),
        visual(),
        music(),
        boredom(),
        additional(),
    ]
}

/// The default pooling-exponent matrix `K` (`nrs_core::KFeatMatrix`),
/// reproducing the legacy box/group pooling: each anchor's exponent is
/// its old box's, anchors of one box pool at that box's exponent, boxes
/// of one group at the group's, and everything else adds. `nrs-store`'s
/// schema V6 seeds the same rows into `kfeat`.
pub fn legacy_k_feat() -> nrs_core::KFeatMatrix {
    let mut k = nrs_core::KFeatMatrix::default();
    let anchors = all();
    for (i, a) in anchors.iter().enumerate() {
        let (box_a, w_a, group_a, w_g) = legacy_box(&a.0).expect("palette anchor");
        if w_a < 1.0 {
            k.pairs.insert((a.clone(), a.clone()), w_a);
        }
        for b in &anchors[i + 1..] {
            let (box_b, _, group_b, _) = legacy_box(&b.0).expect("palette anchor");
            let level = if box_a == box_b {
                w_a
            } else if group_a == group_b {
                w_g
            } else {
                1.0
            };
            if level < 1.0 {
                k.pairs.insert((a.clone(), b.clone()), level);
            }
        }
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_k_has_the_seeded_shape() {
        let k = legacy_k_feat();
        assert_eq!(k.pairs.len(), 73);
        assert_eq!(k.exponent(&waifu()), 0.35);
        assert_eq!(k.get(&heartwarming(), &waifu()), 0.35);
        assert_eq!(k.get(&comedy(), &waifu()), 0.6);
        assert_eq!(k.get(&music(), &visual()), 0.7);
        assert_eq!(k.get(&music(), &comedy()), 1.0);
        assert_eq!(k.exponent(&gateway()), 1.0);
    }
}
