//! The music-credit role calculus, ported from the *fixed* version in
//! `nrs-org/monorepo`'s `packages/ext-dah-entry-roles` (not the
//! original `nrs-lib-ts`'s `DAH_entry_roles.ts`, which has a bug: it
//! computed `arrange`/`compose` as fractions of the whole
//! `inst_total` - including the performance share - so
//! `compose + arrange + inst_perform` summed to `4/3` of `inst_total`
//! instead of exactly `inst_total`. The fix inserts an intermediate
//! `inst_writing = inst_total - inst_perform` node so `arrange`/
//! `compose` only divide up the writing pool once the performance
//! share has been removed from it; see `CompositeRole::InstWriting`.
//!
//! A `<role id=".." roles="expr"/>` on a track entry auto-generates a
//! `Contains`-shaped relation from the credited artist back to the
//! track, weighted by a role-shaped fraction of the old 11-factor
//! space.
//!
//! The original represents these fractions as `ScalarMatrix` /
//! `DiagonalMatrix` (never a full cross-factor `RegularMatrix` - none
//! of the role formulas below ever produce one), so a plain `[f32; 11]`
//! dense vector (a "scalar" is just a vector with all 11 entries
//! equal) is enough to replicate every `add`/`scale`/`mul` used here,
//! without reimplementing the old matrix type hierarchy.
//!
//! The recursive role definitions form a DAG (`total` is the only
//! base case), so plain recursion terminates without needing the
//! original's memoisation cache - that was an optimisation there, not
//! a correctness requirement.

use std::collections::BTreeMap;

use nrs_core::AnchorId;

use crate::anchors;

const N: usize = 11;
const IDX_WRITING: usize = 6;
const IDX_VISUAL: usize = 7;
const IDX_MUSIC: usize = 8;

type Vec11 = [f32; N];

/// Positions are historical (the old 11-factor space), not the new
/// palette's own ordering -- `hate`/`comedy`/`discomfort`/
/// `heartwarming`/`tragedy`/`cozy` sit at the old `AU`/`AP`/`MU`/`MP`/
/// `CU`/`CP` box-default positions (see `snapshot::anchor_for_code`),
/// so a role's weight vector still lands on the same defaults
/// everything else does; `AP`/`MP`/`CU`'s ambiguity applies here too.
fn anchor_order() -> [AnchorId; N] {
    [
        anchors::hate(),
        anchors::comedy(),
        anchors::discomfort(),
        anchors::heartwarming(),
        anchors::tragedy(),
        anchors::cozy(),
        anchors::writing(),
        anchors::visual(),
        anchors::music(),
        anchors::boredom(),
        anchors::additional(),
    ]
}

fn scalar(c: f32) -> Vec11 {
    [c; N]
}
fn zero() -> Vec11 {
    [0.0; N]
}
fn add(a: Vec11, b: Vec11) -> Vec11 {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: Vec11, b: Vec11) -> Vec11 {
    std::array::from_fn(|i| a[i] - b[i])
}
fn scale(a: Vec11, c: f32) -> Vec11 {
    std::array::from_fn(|i| a[i] * c)
}
fn mul(a: Vec11, b: Vec11) -> Vec11 {
    std::array::from_fn(|i| a[i] * b[i])
}
fn at(idx: usize, v: f32) -> Vec11 {
    let mut r = zero();
    r[idx] = v;
    r
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AtomicRole {
    Total,
    Arrange,
    Compose,
    InstPerform,
    Image,
    ImageFeat,
    Vocal,
    Lyrics,
    Mv,
    AlbumArt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompositeRole {
    MusicTotal,
    ImageTotal,
    VocalLyrics,
    InstTotal,
    /// `inst_total - inst_perform`: the "writing" (compose+arrange)
    /// pool, with the performance share already removed. Fixes a bug
    /// in the original `DAH_entry_roles.ts` where `arrange`/`compose`
    /// were computed as fractions of the *whole* `inst_total`
    /// (including the performance share), so
    /// `compose + arrange + inst_perform` summed to `4/3` of
    /// `inst_total` instead of exactly `inst_total`. Fixed upstream in
    /// `nrs-org/monorepo`'s `packages/ext-dah-entry-roles`; ported
    /// here rather than the original.
    InstWriting,
    Inst,
    Perform,
    Prod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoleType {
    Atomic(AtomicRole),
    Composite(CompositeRole),
}

fn parse_role_name(s: &str) -> Option<RoleType> {
    use AtomicRole::*;
    use CompositeRole::*;
    use RoleType::*;
    Some(match s {
        "total" => Atomic(Total),
        "arrange" => Atomic(Arrange),
        "compose" => Atomic(Compose),
        "inst_perform" => Atomic(InstPerform),
        "image" => Atomic(Image),
        "image_feat" => Atomic(ImageFeat),
        "vocal" => Atomic(Vocal),
        "lyrics" => Atomic(Lyrics),
        "mv" => Atomic(Mv),
        "albumart" => Atomic(AlbumArt),
        "music_total" => Composite(MusicTotal),
        "image_total" => Composite(ImageTotal),
        "vocal_lyrics" => Composite(VocalLyrics),
        "inst_total" => Composite(InstTotal),
        "inst_writing" => Composite(InstWriting),
        "inst" => Composite(Inst),
        "perform" => Composite(Perform),
        "prod" => Composite(Prod),
        _ => return None,
    })
}

/// The fully-flattened atomic children of a composite role
/// (`getComposingAtomicRoleTypes`), used only for expanding a role
/// expression before merging - **not** the same as that composite's
/// own `calc` formula, which several composites override rather than
/// defining as a sum of these (see `calc` below).
fn composing_atomic_roles(role: CompositeRole) -> &'static [AtomicRole] {
    use AtomicRole::*;
    use CompositeRole::*;
    match role {
        MusicTotal => &[
            Compose,
            Arrange,
            Lyrics,
            InstPerform,
            Vocal,
            Image,
            ImageFeat,
        ],
        ImageTotal => &[Image, ImageFeat],
        VocalLyrics => &[Vocal, Lyrics],
        InstTotal => &[Compose, Arrange, InstPerform],
        InstWriting => &[Compose, Arrange],
        Inst => &[Compose, Arrange],
        Perform => &[InstPerform, Vocal],
        Prod => &[Compose, Arrange, Lyrics],
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MusicVars {
    pub vocallyrics: f32,
    pub lyricsmusic: f32,
    pub emolyrics: f32,
    /// Share of the writing pool (`inst_writing = inst_total -
    /// inst_perform`) that's arranging rather than composing.
    pub arrange: f32,
    /// Share of `inst_total` that's performing rather than writing.
    pub instperform: f32,
    pub feat: bool,
}

impl Default for MusicVars {
    fn default() -> Self {
        MusicVars {
            vocallyrics: 0.5,
            lyricsmusic: 0.1,
            emolyrics: 0.2,
            arrange: 0.5,
            instperform: 1.0 / 3.0,
            feat: false,
        }
    }
}

/// `AtomicRoleTypes`/`CompositeRoleTypes` `calcFactor` (`DAH_entry_roles.ts`
/// lines ~317-454), evaluated recursively.
fn calc(role: RoleType, vars: &MusicVars) -> Vec11 {
    use AtomicRole::*;
    use CompositeRole::*;
    use RoleType::*;
    match role {
        Atomic(Total) => scalar(1.0),
        Atomic(Arrange) => scale(calc(Composite(InstWriting), vars), vars.arrange),
        Atomic(Compose) => sub(
            calc(Composite(InstWriting), vars),
            calc(Atomic(Arrange), vars),
        ),
        Atomic(InstPerform) => scale(calc(Composite(InstTotal), vars), vars.instperform),
        Atomic(Image) => scale(
            calc(Composite(ImageTotal), vars),
            if vars.feat { 0.7 } else { 1.0 },
        ),
        Atomic(ImageFeat) => sub(calc(Composite(ImageTotal), vars), calc(Atomic(Image), vars)),
        Atomic(Vocal) => sub(
            calc(Composite(VocalLyrics), vars),
            calc(Atomic(Lyrics), vars),
        ),
        Atomic(Lyrics) => mul(
            calc(Composite(VocalLyrics), vars),
            add(
                add(
                    scalar(vars.emolyrics),
                    at(IDX_WRITING, 1.0 - vars.emolyrics),
                ),
                at(IDX_MUSIC, vars.lyricsmusic - vars.emolyrics),
            ),
        ),
        Atomic(Mv) => at(IDX_VISUAL, 1.0),
        Atomic(AlbumArt) => at(IDX_VISUAL, 1.0),
        Composite(MusicTotal) => calc(Atomic(Total), vars),
        Composite(ImageTotal) => scale(calc(Composite(MusicTotal), vars), 0.2),
        Composite(VocalLyrics) => mul(
            sub(
                calc(Composite(MusicTotal), vars),
                calc(Composite(ImageTotal), vars),
            ),
            add(
                scalar(vars.vocallyrics),
                at(IDX_WRITING, 1.0 - vars.vocallyrics),
            ),
        ),
        Composite(InstTotal) => sub(
            sub(
                calc(Composite(MusicTotal), vars),
                calc(Composite(ImageTotal), vars),
            ),
            calc(Composite(VocalLyrics), vars),
        ),
        Composite(InstWriting) => sub(
            calc(Composite(InstTotal), vars),
            calc(Atomic(InstPerform), vars),
        ),
        Composite(Inst) => add(calc(Atomic(Compose), vars), calc(Atomic(Arrange), vars)),
        Composite(Perform) => add(calc(Atomic(InstPerform), vars), calc(Atomic(Vocal), vars)),
        Composite(Prod) => add(calc(Composite(Inst), vars), calc(Atomic(Lyrics), vars)),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RoleTerm {
    pub role: RoleType,
    pub multiplier: f32,
}

/// `parseRoleExpressionString` + `#parseRoleComponent`: `"vocal+image/2"`
/// -> `[vocal*1.0, image*0.5]`. `"role*a*b/c"` chains left to right.
pub fn parse_role_expression(expr: &str) -> Result<Vec<RoleTerm>, String> {
    expr.split('+')
        .map(|component| {
            let op_pos = component.find(['*', '/']).unwrap_or(component.len());
            let (name, mut rest) = component.split_at(op_pos);
            let role =
                parse_role_name(name).ok_or_else(|| format!("unknown role type '{name}'"))?;
            let mut multiplier = 1.0f32;
            while !rest.is_empty() {
                let op = rest.as_bytes()[0] as char;
                let next = rest[1..]
                    .find(['*', '/'])
                    .map(|p| p + 1)
                    .unwrap_or(rest.len());
                let num_str = &rest[1..next];
                let num: f32 = num_str
                    .parse()
                    .map_err(|_| format!("bad factor '{num_str}' in '{expr}'"))?;
                multiplier *= if op == '/' { 1.0 / num } else { num };
                rest = &rest[next..];
            }
            Ok(RoleTerm { role, multiplier })
        })
        .collect()
}

fn expand_to_atomic(term: &RoleTerm) -> Vec<(AtomicRole, f32)> {
    match term.role {
        RoleType::Atomic(a) => vec![(a, term.multiplier)],
        RoleType::Composite(c) => composing_atomic_roles(c)
            .iter()
            .map(|&a| (a, term.multiplier))
            .collect(),
    }
}

/// Whether `AtomicRole::ImageFeat` appears anywhere in `terms` -
/// contributes to `defaultMusicVars`'s `feat` default alongside the
/// entry title containing `"feat."`/`"ft."`.
pub fn mentions_image_feat(terms: &[RoleTerm]) -> bool {
    terms.iter().any(|t| {
        expand_to_atomic(t)
            .iter()
            .any(|(a, _)| *a == AtomicRole::ImageFeat)
    })
}

/// `#calculateFactors`, for one contributor: expand every term to its
/// atomic roles, merge by atomic type (summing multipliers - this is
/// where duplicate roles across several `<role>` tags for the same id
/// combine), then sum each atomic role's own `calc` scaled by its
/// merged multiplier.
pub fn weight_vector(terms: &[RoleTerm], vars: &MusicVars) -> Vec11 {
    let mut atomic_totals: BTreeMap<AtomicRole, f32> = BTreeMap::new();
    for term in terms {
        for (atomic, mult) in expand_to_atomic(term) {
            *atomic_totals.entry(atomic).or_insert(0.0) += mult;
        }
    }
    let mut total = zero();
    for (atomic, mult) in atomic_totals {
        total = add(total, scale(calc(RoleType::Atomic(atomic), vars), mult));
    }
    total
}

/// Collapse to `nrs_core::Transform`: a uniform vector becomes
/// `Scalar`, otherwise a sparse `Regional` map of the nonzero entries.
pub fn vector_to_transform(v: Vec11) -> nrs_core::Transform {
    let first = v[0];
    if v.iter().all(|&x| (x - first).abs() < 1e-6) {
        return nrs_core::Transform::Scalar(first);
    }
    let names = anchor_order();
    let mut map = BTreeMap::new();
    for i in 0..N {
        if v[i].abs() > 1e-6 {
            map.insert(names[i].clone(), v[i]);
        }
    }
    nrs_core::Transform::Regional(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_is_a_full_uniform_borrow() {
        let terms = parse_role_expression("total").unwrap();
        let v = weight_vector(&terms, &MusicVars::default());
        assert!(v.iter().all(|&x| (x - 1.0).abs() < 1e-6));
        assert_eq!(vector_to_transform(v), nrs_core::Transform::Scalar(1.0));
    }

    #[test]
    fn mv_and_albumart_are_visual_only() {
        let terms = parse_role_expression("mv").unwrap();
        let v = weight_vector(&terms, &MusicVars::default());
        for (i, &x) in v.iter().enumerate() {
            if i == IDX_VISUAL {
                assert!((x - 1.0).abs() < 1e-6);
            } else {
                assert!(x.abs() < 1e-6, "index {i} should be 0, got {x}");
            }
        }
    }

    #[test]
    fn expression_with_multiplier_scales_result() {
        let terms = parse_role_expression("mv*2").unwrap();
        let v = weight_vector(&terms, &MusicVars::default());
        assert!((v[IDX_VISUAL] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn division_operator_parses() {
        let terms = parse_role_expression("image/2").unwrap();
        assert_eq!(terms.len(), 1);
        assert!((terms[0].multiplier - 0.5).abs() < 1e-6);
    }

    #[test]
    fn expanding_a_default_sum_composite_agrees_with_its_own_calc() {
        // "inst"'s calcFactor has no override - it's literally defined
        // as compose + arrange (its children), so weight_vector's
        // expand-to-atomic-then-sum path must agree with calling
        // calc() on the composite directly. (This still does not hold
        // for composites with a genuinely different override, like
        // music_total/image_total/vocal_lyrics - only inst_total's
        // partition was the actual bug.)
        let vars = MusicVars::default();
        let terms = parse_role_expression("inst").unwrap();
        let expanded = weight_vector(&terms, &vars);
        let direct = calc(RoleType::Composite(CompositeRole::Inst), &vars);
        for i in 0..N {
            assert!(
                (expanded[i] - direct[i]).abs() < 1e-5,
                "index {i}: {} vs {}",
                expanded[i],
                direct[i]
            );
        }
    }

    #[test]
    fn compose_arrange_inst_perform_exactly_partition_inst_total() {
        // The actual fix: with inst_writing = inst_total - inst_perform
        // interposed, compose + arrange (which divide inst_writing) plus
        // inst_perform (which divides inst_total directly) now sum to
        // exactly inst_total, for any vars - not 4/3 of it.
        for arrange in [0.0f32, 0.3, 0.5, 0.75, 1.0] {
            for instperform in [0.0f32, 1.0 / 3.0, 0.5, 0.9] {
                let vars = MusicVars {
                    arrange,
                    instperform,
                    ..MusicVars::default()
                };
                let inst_total = calc(RoleType::Composite(CompositeRole::InstTotal), &vars);
                let sum = add(
                    add(
                        calc(RoleType::Atomic(AtomicRole::Compose), &vars),
                        calc(RoleType::Atomic(AtomicRole::Arrange), &vars),
                    ),
                    calc(RoleType::Atomic(AtomicRole::InstPerform), &vars),
                );
                for i in 0..N {
                    assert!(
                        (inst_total[i] - sum[i]).abs() < 1e-4,
                        "arrange={arrange} instperform={instperform} index {i}: {} vs {}",
                        inst_total[i],
                        sum[i]
                    );
                }
            }
        }
    }

    #[test]
    fn duplicate_atomic_roles_across_terms_add() {
        let terms_once = parse_role_expression("mv").unwrap();
        let terms_twice = parse_role_expression("mv+mv").unwrap();
        let once = weight_vector(&terms_once, &MusicVars::default());
        let twice = weight_vector(&terms_twice, &MusicVars::default());
        assert!((twice[IDX_VISUAL] - 2.0 * once[IDX_VISUAL]).abs() < 1e-6);
    }

    #[test]
    fn unknown_role_name_errors() {
        assert!(parse_role_expression("not_a_role").is_err());
    }
}
