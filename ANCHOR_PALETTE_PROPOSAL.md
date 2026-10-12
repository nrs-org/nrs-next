# A manageable palette, mined from `NRSImplMain.xml`

> Status: proposal, not a decision. Written from a close read of the real dataset (1573
> entries), not designed in the abstract. See `../INFRA.md` and `../redesign-tex/nrs-redesign.tex`
> ("Why not a fixed plane of emotions", "Migration by distillation") for the design
> rationale this responds to.

## What the current 11 anchors actually are

Cross-referencing `crates/nrs-import/src/anchors.rs` against
`nrs-lib-ts/exts/DAH_factors.ts` confirms the current anchor set (`AU/AP/MU/MP/CU/CP` +
`AL/AV/AM` + `Boredom`/`Additional`) is a literal **activated/moderate/calming ×
unpleasant/pleasant 6-box grid**, plus three craft factors and two scalar extras:

```
AU = ActivatedUnpleasant   AP = ActivatedPleasant
MU = ModerateUnpleasant    MP = ModeratePleasant
CU = CalmingUnpleasant     CP = CalmingPleasant
AL = Language (craft)  AV = Visual (craft)  AM = Music (craft)
```

This is exactly "the six emotion boxes" the redesign notes call out by name as the wrong
upgrade path ("Why not a fixed plane of emotions") — a valence/arousal plane, not a
considered palette.

## Method

1. Grepped every XML comment immediately preceding an `emotions="..."` attribute across
   the whole dataset, grouped by code, to find recurring themes hiding inside each box.
2. Scored the full dataset (`nrs_core::score` with legacy-equivalent default params) and
   ranked all 1573 entries, excluding **franchises** (`F-`/`GF-` id prefix — inflated,
   they only borrow via `Contains`) and **artists** (`M-VGMDB-AR-*` — also inflated, pure
   `Credit`-borrowing aggregators with no impacts of their own; confirmed by inspection,
   e.g. `M-VGMDB-AR-14655` "HoneyWorks" is a bare `<entry .../>` with zero direct content).
3. Read the top ~40 of what's left in full (not just comments near `emotions=`) — this
   surfaced structural issues the broad low-score sample didn't, because they're not about
   *which feeling*, they're about *impact kinds forced into the wrong anchor by the
   legacy→anchor mapping itself*.
4. Went back into the excluded franchises — score is inflated there, but content and tag
   *frequency* aren't, and counting every distinct XML tag used dataset-wide turned up the
   single strongest piece of evidence in this doc (`osuSong`, Finding 4).

## Finding 1: the broad sample — boxes that each hide 2+ distinct feelings

- **`AP` (82 annotated) is almost entirely comedy**, not general activation: *"this was
  funny af"*, *"comedy was weak ngl"*, *"the memes were funny"*. Non-comedy excitement
  barely registers as its own signal — it's riding along in the same bucket.
- **`CU` (75 annotated) splits into grief/tragedy vs. wistful nostalgia**: *"Kyoko's
  backstory... very tragic"*, *"this made me cry"* (loss-in-the-work) vs. *"When
  Ayumu-era ends, I felt depressed"* (an era ending — different feeling entirely).
- **`CP` (55) skews cozy/atmospheric**, **`MP` (48) skews warm/wholesome character
  moments** — related but not the same: *"atmospheric farming"*, *"the girls are comfy
  too"* (CP) vs. *"cute daughter widepeepohappy"*, *"heartwarming af"* (MP).
- **`AU`/`MU` (21-22 each) — too little clean signal in the comments alone** to tell them
  apart (mostly in-jokes); the split below (`hate`/`discomfort`) comes from direct
  clarification, not the sample — see Finding 4.

## Finding 2: two real impact kinds are structurally forced into the wrong anchor

From `crates/nrs-import/src/legacy.rs`:

```rust
pub fn waifu(source: SourceId, days: f32) -> Vec<Atom> {
    let base = 1.2 * (days / 90.0).powf(0.35);
    flat_atom(source, anchors::mp(), base)      // character attachment -> MP
}
pub fn meme(source: SourceId, strength: f32, days: f32) -> Vec<Atom> {
    let base = strength * (days / 120.0).powf(0.4) * 4.0;
    flat_atom(source, anchors::ap(), base)      // cultural currency -> AP
}
```

Both are **time-decay functions of days-since**, not intensity ratings like the direct
emotional tags (`nei`/`cry`/`aei`) — structurally different phenomena sharing an anchor
identity with "this scene made me feel warm" / "this made me laugh". The XML comments
back this up directly: *"the only acceptable girl"*, *"X my fucking beloved"* (waifu) vs.
*"ksdgk"*, *"tea party"* in-group references (meme) — neither reads as a direct reaction
to content.

## Finding 3 (from the top-20% deep dive): two more conflations, and they're worse

**`epi`/`ehi` ("legendary moment" markers) dump into `AP` at 3.5–4.5 magnitude** — 5-10x
a normal impact (`nei`/`aei` bases in the data run 0.1-1.0) — regardless of what the
moment actually felt like:

```rust
pub fn epi(source: SourceId, factor: f32) -> Vec<Atom> {
    let base = remap(factor, 0.0, 1.0, 3.5, 4.5);
    emotion_atoms(source, base, &[(anchors::ap(), 1.0)])   // always AP, any content
}
```

Real example, `V-VNDB-17516` (a visual novel, one of the highest-scoring non-franchise
entries): `<!-- the twists --><epi base="1.0" />` sits next to a comment about "the
realization that the main girl didn't love mc" — that's tragic/bittersweet content, not
comedy, but it's scored as if it were pure `AP`. This isn't a missing anchor so much as
**a migration-mapping bug**: `epi`/`ehi` should route to whichever anchor actually matches
the moment (as chosen per-instance), not be hardcoded to `AP`.

**`sleepless_night` dumps into `MP` at a fixed 4.0** — again huge relative to normal
impacts — right next to "cute daughter" and "wholesome song":

```rust
pub fn sleepless_night(source: SourceId) -> Vec<Atom> {
    emotion_atoms(source, 4.0, &[(anchors::mp(), 1.0)])
}
```

"Couldn't put it down, stayed up all night" is a real, recurring, and *distinct* feeling
— it's about intensity of absorption, not warmth, and it can attach to something dark just
as easily as something wholesome. It deserves its own anchor, not a spot in "heartwarming".

**`interest_field`** ("this got me into a new hobby/field") is currently just a bump in
the `Additional` catch-all (`1.0`, or `2.0` if it's a genuinely new field). That's a real,
distinct kind of value — a work changing what you're *into*, not just how you felt about
it — worth naming rather than burying.

**The single highest-scoring non-franchise, non-artist entry in the whole dataset**
(`M-20240815T042406`, "CTB Girls' Dormitory", score 49.5) turned out to be an original
self-insert fan music/lore project — years of tracks with an elaborate personal narrative
("An idol who hates every fan of hers... a girl excluded by the world..."), heavy
long-running `meme` accumulation (`from`/`to` date ranges spanning 2023-2025, `strength`
explicitly modeled as "initial run" vs. "legacy"), and comments like *"this was when blud
was accepted into the tea party"* — literal self-insertion into the narrative.

**Caveat, per feedback**: this entry's rank is somewhat out-of-distribution and shouldn't
be read as strong quantitative validation on its own — every track has `<music base="0.0"
/>` and there's essentially no `<visual>` signal (self-published fan content, not
professionally produced), so unlike almost everything else in the dataset its score is
built *entirely* from `nei`/`aei` + sustained `meme` accumulation with zero craft
counterweight pulling the other way. That inflates its absolute rank relative to
content that also carries real `writing`/`visual`/`music` mass. The **qualitative**
point still holds independent of this entry's rank, though — `waifu()`/`meme()` are
structurally time-decayed and hardcoded to `MP`/`AP` regardless of content, confirmed
directly from `legacy.rs`, not inferred from where this one entry happened to land.

## Finding 4: ambiguous impacts (per feedback: "a lot of dormslop is good because I resonate with it perfectly, since author=grader")

**Retracted, per feedback**: this was wrong. "Self-insert" isn't a feeling at all — it's
the grader making art that intentionally resonates with themselves, which is authorial
*intent*, not a distinct axis parallel to `tragedy`/`comedy`/`heartwarming`. The actual
felt content in a self-insert piece is still whatever primary feeling it is — *"my
self-insert... want to kms"* isn't "self-insert energy," it's despair (close to `tragedy`,
possibly its own thing — thin evidence, not confident enough to split it out on one
example), and *"this was when blud was accepted into the tea party"* reads as
`heartwarming`. Self-insertion is why those feelings land unusually hard for this specific
grader, not a different feeling in itself — closer in role to a craft dimension (how
precisely something achieves its emotional target, like `writing` already rates) than to
a WHAT-did-I-feel dimension. It doesn't need its own anchor: a self-insert moment that
lands hard already shows up as a *high magnitude* on whichever real anchor it is, and
that's the correct place for it to live — inventing a parallel anchor for it would have
double-classified the same content instead of just weighting it honestly.

One more genuinely ambiguous case, smaller but real: `Koikake.xml` has *"eroge love ig"*
commented out once (`<!-- <aei base="0.5" emotions="MP" /> -->`, left ungraded) and scored
for real a few lines later (`<aei base="0.8" emotions="MP" />`) — the grader visibly
hesitated on whether general romantic/dating-sim-immersion enjoyment (not a crush on one
specific character) belongs with `waifu` or just `heartwarming`. No clean answer from the
data alone; flagging it rather than resolving it.

**`AU`/`MU`, clarified rather than mined**: the comment sample was too thin to split these
with confidence (Finding 1), but per direct clarification: `AU` is closer to *hate*, `MU`
is the broader *discomfort/hurt* — and `hate` is typically downstream of `discomfort` (you
hate a thief because being stolen from made you uncomfortable), not a separate root
feeling. Splitting them below as `hate`/`discomfort` rather than one `distress` bucket, to
stay consistent with how `tragedy`/`wistful` and `cozy`/`heartwarming` were split despite
also being causally/experientially close — with a nonzero `k_feat` between them reflecting
that `hate` usually *follows from* `discomfort`, same shape as the other two pairs.

## Finding 5: the highest-ranked franchises (score is inflated, content isn't)

Franchise-level *scores* are excluded from evidence throughout (they're aggregate,
inflated by `Contains`), but their *content* — franchise-level comments and, more
usefully, tag frequency across the whole dataset — is not, and turned up the single
biggest piece of evidence in this whole investigation:

**`<osuSong personal="..." community="...">` appears 171 times** — more than `nei` (153)
or `aei` (134). A whole file, `franchises/OsuGame.xml`, is basically dedicated to it (94
occurrences alone), and it shows up throughout the top franchises too (`KokuhakuJikkouIinkai.xml`
×14, `Bandori.xml` ×5, `LoveLive.xml` ×2, etc.). It's rhythm-game (osu!) engagement with a
track, and — same shape as `waifu`/`meme` — it's currently forced entirely into `AP`:

```rust
pub fn osu_song(source: SourceId, personal: f32, community: f32) -> Vec<Atom> {
    let personal_factor = remap(personal, 0.0, 1.0, 0.0, 0.5);
    let community_factor = remap(community, 0.0, 1.0, 0.0, 0.2);
    flat_atom(source, anchors::ap(), personal_factor + community_factor)
}
```

Unlike `waifu`/`meme`, this one doesn't need a new anchor at all — it already splits
cleanly into exactly the two anchors already proposed: `personal` (played/mastered this
specific map — the same "invested in this specific thing" feeling as `hooked`) and
`community` (how big a deal this map/song is in the osu community — literally the same
shape as `meme`). This is the strongest evidence yet for `hooked`+`meme` as a real split,
not a footnote — it's a third, independently-discovered instance of the exact same
personal/community conflation, and by frequency it's the single most common special-case
impact in the dataset.

**Correction on `<politics>` (10 uses, → `Additional`), per feedback**: not fandom
tribalism as first read — it's the work shifting the grader's actual real-world worldview
or lifestyle, marked with a coined "-ism" each time: *"Hikism-Yukism"* (Oregairu, next to
`<politics><contributor id="A-MAL-14813".../><contributor id="A-MAL-23847"/></politics>`),
*"ingsoc and colorism"* / *"literally the inspiration for colorism's name"* (LoveLive),
*"ayumuism"*, *"observism and the COP"*, *"localism"*. Each is a personal philosophy/way
of living the grader adopted *from* the work, not a debate *about* it — a genuinely
different thing from `meme` (cultural currency) or `gateway` (new hobby/interest): this is
about changed *beliefs*, not changed *habits* or *social currency*. Renamed to `worldview`
below; kept distinct from `gateway` rather than merged into it.

**Walking back `gateway`'s evidence**: `interestField` (the tag) is used exactly *once* in
the whole dataset, and `gateOpen`/`GateOpen` — the concept of "this franchise opened the
door to a new gacha game/hobby" — appears **only inside XML comments**, several of them
literally commented-out (`<!-- <gateOpen id="..." /> -->`). The desire is clearly real and
recurring (shows up unprompted in `MadokaMagica.xml` twice, `ReStage.xml`, `ImoutoIreba.xml`)
but the mechanism is effectively unimplemented — closer to "a thing you've wanted to track
and haven't gotten around to" than an established pattern like `osuSong`/`meme`/`waifu`.
Keeping it in the proposal, but flagged as the weakest-evidenced anchor here by far.

## Proposed palette (18 graded anchors — up from 11)

| anchor | replaces / fixes | evidence |
|---|---|---|
| `comedy` | AP's dominant use | *"funny af"*, *"made me laugh my ass off"* |
| `hype` | AP's minority use | plot/moment excitement — real and independently graded, per feedback; close to `comedy` (and to what `meme` amplifies) but not reducible to either |
| `tragedy` | CU's dominant use | *"Kyoko's backstory"*, *"this made me cry"* |
| `wistful` | CU's other use | *"When Ayumu-era ends, I felt depressed"* |
| `cozy` | CP's dominant use | *"atmospheric farming"*, *"comfy"* |
| `heartwarming` | MP's dominant use | *"heartwarming af"*, *"cute daughter"* |
| `waifu` | MP (forced, `waifu()`) | character attachment; already its own function, just not its own anchor |
| `meme` | AP (forced, `meme()`, `osuSong`'s `community`) | cultural/community currency; `osuSong` alone confirms this shape 171 times — but see below, this one is structurally secondary, not a peer primary feeling |
| `hooked` | MP (forced, `sleepless_night()`/`jumpscare()`) + AP (`osuSong`'s `personal`) | obsessive absorption/personal investment, orthogonal to valence |
| `worldview` | Additional (`politics()`) | a work reshaping real beliefs/lifestyle — *"Hikism-Yukism"*, *"ayumuism"*, *"localism"* |
| `gateway` | Additional (`interest_field()`) | "opened a new interest/hobby/field" — real but barely used (1 tag; mostly commented-out `gateOpen`s) |
| `hate` | AU | per feedback: sharper, more reactive than `discomfort` |
| `discomfort` | MU | per feedback: the broader, root feeling — being hurt/bothered; `hate` usually follows from it (hating a thief because being stolen from made you uncomfortable), not a separate primary axis |
| `writing` | AL | kept, well-evidenced |
| `visual` | AV | kept |
| `music` | AM | kept |
| `boredom` | Boredom | kept, structural penalty not a feeling |
| `additional` | Additional | kept as catch-all (now smaller) |

Not a new anchor, but a fix needed regardless of which palette ships: **`epi`/`ehi`
should stop hardcoding `AP`** and instead take an explicit anchor argument (or blend) set
per-instance to whatever the moment actually was.

## Mechanism: can a feature be derived from others? (per feedback — corrected)

First pass here was wrong to make `hype` the derived one. On reflection (and per
feedback): `hype` is its own real, independently-felt thing — it just sits *close* to
`comedy` and to whatever `meme` amplifies, the same relationship `tragedy` has to
`wistful`. Keeping it graded directly, with `k_feat` doing the "these overlap" work
instead of collapsing it away. Same lesson from `self-insert`'s retraction (Finding 4):
not everything that *feels* like it might be a derived combination actually is one —
`writing`/`visual`/`music` aren't derivable from the feelings they enable either, they're
independent craft judgments about how well the piece achieves whatever it's going for.

**`meme` is the one that's actually structurally secondary, per feedback** — and this
holds up mechanically, not just by feel. Its formula (`strength * (days/120)^0.4`) isn't
a felt-intensity rating like `comedy`/`hype`/`tragedy` are; it's a function of *elapsed
time*. `meme` is never a primary reaction — it's always parasitic on something that
already had one (usually `comedy` or `hype`) and then kept mattering. `nrs-core` already
has a first-class concept for exactly this shape: `TimeSupport::Tail { peak, lambda,
cutoff }`, which integrates `peak * exp(-lambda*t)` over time (`crates/nrs-core/src/measure.rs`)
— and `legacy.rs` explicitly comments `meme()`/`waifu()` as *"Duration-decayed impacts
(already-collapsed magnitudes)"*, i.e. pre-computed by the importer instead of using the
time-integration the engine already has. CTB Girls' Dormitory's "initial run" vs. "legacy"
`strength` values for the *same* meme are a hand-rolled piecewise stand-in for a curve
`Tail` (or a growth-shaped sibling of it — `Tail` decays from a peak, `meme`'s formula
currently *grows* with sustained days, so it's not a literal drop-in) could plausibly
express natively.

This is a real "derived" case, just not the `explain()`-time read I originally proposed
for `hype` — `meme`'s `strength`/`days` are still independent grader input (you still
decide how significant something's become), so it can't be computed with zero new
information the way I'd wrongly claimed for `hype`. What it *can* do is stop being a
free-floating anchor identity and instead become a time-based amplifier attached to
whichever primary anchor it's actually amplifying — closer in shape to `Contains`/`Credit`
(borrowing a share of something else's value) than to a peer feeling like `comedy`. That's
a real engine question (does an atom's mass get a time-support-driven multiplier tied to
another anchor's contribution, rather than living at its own anchor identity?), not
something to resolve in this pass — flagged below rather than designed here.

In the meantime, without new engine machinery: keep `meme` as its own graded anchor as
proposed, but make sure `k_feat(meme, comedy)` and `k_feat(meme, hype)` are meaningfully
nonzero — since `meme` mass usually really is "more of the comedy/hype that was already
there," the redundancy discount should reflect that overlap explicitly rather than
double-counting it as unrelated mass.

## `k_feat` observations, for free

- `tragedy` ↔ `wistful` and `cozy` ↔ `heartwarming`: constantly co-occur in the data
  (e.g. Clannad's own `CU-0.5:CP-0.5` split, annotated *"it's not as good as After Story...
  but yeah this is still somewhat emotional"*) — good candidates for a moderate nonzero
  `k_feat`, not the default-zero off-diagonal.
- `worldview` ↔ `gateway`: both "real-life impact beyond the viewing experience" — plausibly
  related enough for a small nonzero `k_feat`, without merging them (belief change vs.
  habit change are still different enough to keep separately graded, per the correction
  above).
- `hate` ↔ `discomfort`: per the clarification above, `hate` usually *follows from*
  `discomfort` rather than arising independently — a nonzero `k_feat` here is less "these
  two happen to co-occur" and more "one is causally downstream of the other," same
  reasoning as `tragedy`/`wistful` and `cozy`/`heartwarming`.
- `hype` ↔ `comedy`, and `meme` ↔ (`hype`, `comedy`): per the correction above, `hype` is
  close to `comedy` without being reducible to it, and `meme` is close to both without
  being a peer of either — all three pairs want a real nonzero `k_feat`, not the
  default-zero off-diagonal, with `meme`'s edges probably the strongest of the three given
  it's structurally an amplifier of one of the other two, not an independent reaction.

## Migration mapping (INFRA.md's "re-embed for free" move)

Every old factor still has a home, so old impacts port over by lookup with no regrading:

```
AU -> hate               MU -> discomfort
AP -> comedy  (default; epi/ehi, meme/waifu, and osuSong need per-instance routing)
MP -> heartwarming (default; waifu/sleepless_night/jumpscare need routing)
CU -> tragedy  (default; some historical CU was wistful, needs a pass to re-split)
CP -> cozy
AL -> writing   AV -> visual   AM -> music
Boredom -> boredom
Additional -> additional  (minus interest_field -> gateway, politics -> worldview)
```

The parenthetical caveats are exactly where "re-embed for free" breaks down a little:
`AP`/`MP`/`CU`/`Additional` were overloaded, so a straight lookup re-embeds the *box*
correctly but loses which sub-feeling was meant — those need a one-time manual pass (or a
`nrs-fit` warm-start against old rankings, since the box-level re-embed already preserves
ordering) rather than a pure mechanical migration.

## Open questions for you

- `hate`/`discomfort` are split above for consistency with the other causally-linked pairs,
  but unlike those pairs there was near-zero comment evidence either way — does the split
  actually match how you'd grade in practice, or does "hate" only ever show up already
  mixed with discomfort, making the two-anchor version pure overhead?
- Is `meme`-as-time-amplifier-of-another-anchor (see above) worth a real engine mechanism
  (an atom whose mass is tied to another anchor's contribution via a time-support curve,
  rather than a free-floating anchor identity), or is a strong `k_feat(meme, comedy/hype)`
  edge good enough in practice and the deeper rework isn't worth it?
- The ranking already needed franchise- and artist-entries excluded as inflated; this
  session's caveat suggests a third category — self-published/fan content with ~zero
  craft signal — inflates similarly by having no `writing`/`visual`/`music` counterweight.
  Worth a `k_feat`-style discount, or is raw score simply not comparable across that line
  and no palette fix should try to paper over it?
