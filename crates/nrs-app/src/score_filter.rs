//! The scores view's filter language -- what the search box above the
//! table accepts. Space-separated terms, all of which must match:
//!
//! - a bare word or `"quoted phrase"` matches title, id, or store
//! - `type:anime` (`t:` for short; repeated `type:` terms mean any of
//!   them; `type:music` is albums, tracks, and artists), `store:mal`
//! - `v>10`, `rank<=100`, `delta>=5`, with `>` `>=` `<` `<=` `=`
//! - `is:moved`, `is:up`, `is:down` -- rank changes, only meaningful
//!   while comparing two runs
//! - a leading `-` negates any term
//!
//! Unrecognized `key:value` terms are plain words, since titles like
//! `Re:STAGE!` contain colons. Terms that look like filters but can't
//! be used (a bad number, an unknown type) are dropped with a warning
//! rather than silently matching nothing.

use crate::{kind_alias, kind_covers};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl Cmp {
    fn test(self, left: f64, right: f64) -> bool {
        match self {
            Cmp::Lt => left < right,
            Cmp::Le => left <= right,
            Cmp::Eq => (left - right).abs() < 1e-9,
            Cmp::Ge => left >= right,
            Cmp::Gt => left > right,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Field {
    Value,
    Rank,
    Delta,
}

#[derive(Debug, PartialEq)]
enum Test {
    /// Lowercased; matched against `Candidate::haystack`.
    Text(String),
    Kind(&'static str),
    Store(String),
    Compare(Field, Cmp, f64),
    Moved,
    Up,
    Down,
}

#[derive(Debug, PartialEq)]
struct Term {
    negated: bool,
    test: Test,
    /// The term as written, so the UI can remove exactly it.
    source: String,
}

/// What a filter is checked against: one row of a ranking.
pub(crate) struct Candidate<'a> {
    /// Lowercased title, id, and store, for word terms.
    pub(crate) haystack: &'a str,
    pub(crate) store: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) value: f32,
    pub(crate) rank: usize,
    /// Places gained since the compared run; `None` when not comparing,
    /// or for an entry the compared run didn't have.
    pub(crate) delta: Option<i64>,
}

#[derive(Debug, Default)]
pub(crate) struct ScoreFilter {
    terms: Vec<Term>,
    pub(crate) warnings: Vec<String>,
}

impl ScoreFilter {
    /// `comparing` says whether rank changes exist to filter on.
    pub(crate) fn parse(query: &str, comparing: bool) -> Self {
        let mut filter = ScoreFilter::default();
        for (token, quoted) in tokens(query) {
            let source = if quoted {
                match token.strip_prefix('-') {
                    Some(rest) => format!("-\"{rest}\""),
                    None => format!("\"{token}\""),
                }
            } else {
                token.clone()
            };
            let (negated, body) = match token.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest),
                _ => (false, token.as_str()),
            };
            let test = if quoted {
                Some(Test::Text(body.to_lowercase()))
            } else {
                filter.term(body, comparing)
            };
            if let Some(test) = test {
                filter.terms.push(Term {
                    negated,
                    test,
                    source,
                });
            }
        }
        filter
    }

    /// One unquoted term, or `None` (with a warning) when it looked
    /// like a filter but can't be used.
    fn term(&mut self, body: &str, comparing: bool) -> Option<Test> {
        let needs_comparison = |filter: &mut Self| {
            if !comparing {
                filter.warnings.push(format!(
                    "`{body}` needs a comparison -- edit something, then compare against the previous version"
                ));
            }
            comparing
        };

        if let Some((key, value)) = body.split_once(':') {
            match key.to_lowercase().as_str() {
                "type" | "t" | "kind" => {
                    return match kind_alias(value) {
                        Some(kind) => Some(Test::Kind(kind)),
                        None => {
                            self.warnings.push(format!("unknown type `{value}`"));
                            None
                        }
                    };
                }
                "store" | "s" => return Some(Test::Store(value.to_lowercase())),
                "is" => {
                    let test = match value.to_lowercase().as_str() {
                        "moved" => Test::Moved,
                        "up" => Test::Up,
                        "down" => Test::Down,
                        _ => {
                            self.warnings.push(format!("unknown `is:{value}`"));
                            return None;
                        }
                    };
                    return needs_comparison(self).then_some(test);
                }
                _ => {}
            }
        }

        if let Some(at) = body.find(['<', '>', '=']) {
            let field = match body[..at].to_lowercase().as_str() {
                "v" | "value" => Some(Field::Value),
                "rank" | "r" | "#" => Some(Field::Rank),
                "delta" | "d" | "Δ" | "δ" => Some(Field::Delta),
                _ => None,
            };
            if let Some(field) = field {
                let rest = &body[at..];
                let (cmp, number) = [
                    (">=", Cmp::Ge),
                    ("<=", Cmp::Le),
                    (">", Cmp::Gt),
                    ("<", Cmp::Lt),
                    ("=", Cmp::Eq),
                ]
                .iter()
                .find_map(|(op, cmp)| rest.strip_prefix(op).map(|number| (*cmp, number)))?;
                let Ok(number) = number.parse::<f64>() else {
                    self.warnings.push(format!("`{body}` needs a number"));
                    return None;
                };
                if field == Field::Delta && !needs_comparison(self) {
                    return None;
                }
                return Some(Test::Compare(field, cmp, number));
            }
        }

        Some(Test::Text(body.to_lowercase()))
    }

    /// Whether `candidate` passes. `type:` terms match if any of them
    /// does; every other term must. `skip_kinds` leaves the `type:`
    /// terms out entirely, to count what each type would show.
    /// `kind`'s `type:` terms as written, and whether they exclude it.
    /// The type chips use this to show their state and to toggle off
    /// whatever spelling turned them on (`t:song` for music, ...).
    pub(crate) fn kind_terms(&self, kind: &str) -> (Vec<&str>, Option<bool>) {
        let mut sources = Vec::new();
        let mut excluded = None;
        for term in &self.terms {
            if matches!(term.test, Test::Kind(selected) if kind_covers(selected, kind)) {
                sources.push(term.source.as_str());
                excluded = Some(term.negated);
            }
        }
        (sources, excluded)
    }

    pub(crate) fn matches(&self, candidate: &Candidate<'_>, skip_kinds: bool) -> bool {
        let mut wanted_kind = None;
        for term in &self.terms {
            if let (Test::Kind(kind), false) = (&term.test, term.negated) {
                if skip_kinds {
                    continue;
                }
                let hit = kind_covers(kind, candidate.kind);
                wanted_kind = Some(wanted_kind.unwrap_or(false) || hit);
                continue;
            }
            if skip_kinds && matches!(term.test, Test::Kind(_)) {
                continue;
            }
            if test(&term.test, candidate) == term.negated {
                return false;
            }
        }
        wanted_kind.unwrap_or(true)
    }
}

fn test(test: &Test, candidate: &Candidate<'_>) -> bool {
    match test {
        Test::Text(text) => candidate.haystack.contains(text.as_str()),
        Test::Kind(kind) => kind_covers(kind, candidate.kind),
        Test::Store(store) => candidate.store.to_lowercase().contains(store.as_str()),
        Test::Compare(field, cmp, number) => {
            let left = match field {
                Field::Value => f64::from(candidate.value),
                Field::Rank => candidate.rank as f64,
                Field::Delta => match candidate.delta {
                    Some(delta) => delta as f64,
                    None => return false,
                },
            };
            cmp.test(left, *number)
        }
        // An entry the compared run didn't have counts as moved.
        Test::Moved => candidate.delta != Some(0),
        Test::Up => candidate.delta.is_some_and(|delta| delta > 0),
        Test::Down => candidate.delta.is_some_and(|delta| delta < 0),
    }
}

/// Splits on whitespace, keeping `"quoted phrases"` (optionally
/// `-"negated"`) together; the flag says whether the token was quoted.
fn tokens(query: &str) -> Vec<(String, bool)> {
    let mut tokens = Vec::new();
    let mut chars = query.chars().peekable();
    while let Some(&next) = chars.peek() {
        if next.is_whitespace() {
            chars.next();
            continue;
        }
        let mut token = String::new();
        if next == '-' {
            token.push('-');
            chars.next();
        }
        if chars.peek() == Some(&'"') {
            chars.next();
            let phrase: String = chars.by_ref().take_while(|c| *c != '"').collect();
            token.push_str(&phrase);
            if !phrase.is_empty() {
                tokens.push((token, true));
            }
            continue;
        }
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            token.push(c);
            chars.next();
        }
        tokens.push((token, false));
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate<'a>(
        haystack: &'a str,
        kind: &'a str,
        value: f32,
        rank: usize,
        delta: Option<i64>,
    ) -> Candidate<'a> {
        Candidate {
            haystack,
            store: "none",
            kind,
            value,
            rank,
            delta,
        }
    }

    #[test]
    fn words_phrases_and_negation() {
        let row = candidate(
            "re:stage! dream days f-vgmdb-7059 none",
            "franchise",
            1.0,
            1,
            None,
        );
        assert!(ScoreFilter::parse("dream", false).matches(&row, false));
        assert!(ScoreFilter::parse("re:stage!", false).matches(&row, false));
        assert!(ScoreFilter::parse("\"dream days\"", false).matches(&row, false));
        assert!(!ScoreFilter::parse("\"days dream\"", false).matches(&row, false));
        assert!(!ScoreFilter::parse("-dream", false).matches(&row, false));
        assert!(!ScoreFilter::parse("-\"dream days\"", false).matches(&row, false));
    }

    #[test]
    fn type_terms_are_any_of_and_negations_are_all_of() {
        let anime = candidate("x", "anime", 1.0, 1, None);
        let track = candidate("x", "track", 1.0, 1, None);
        let album = candidate("x", "album", 1.0, 1, None);
        let filter = ScoreFilter::parse("type:anime t:song", false);
        assert!(filter.matches(&anime, false) && filter.matches(&track, false));
        assert!(!filter.matches(&album, false));
        let filter = ScoreFilter::parse("-type:music", false);
        assert!(filter.matches(&anime, false));
        assert!(!filter.matches(&track, false) && !filter.matches(&album, false));
        let filter = ScoreFilter::parse("type:music", false);
        assert!(filter.matches(&album, false) && !filter.matches(&anime, false));
        let filter = ScoreFilter::parse("type:anime", false);
        assert!(!filter.matches(&track, false));
        assert!(
            filter.matches(&track, true),
            "skip_kinds ignores type terms"
        );
    }

    #[test]
    fn comparisons() {
        let row = candidate("x", "anime", 12.5, 40, Some(-3));
        assert!(ScoreFilter::parse("v>10 rank<=40", true).matches(&row, false));
        assert!(!ScoreFilter::parse("v>=13", true).matches(&row, false));
        assert!(ScoreFilter::parse("delta<0 is:down is:moved", true).matches(&row, false));
        assert!(!ScoreFilter::parse("is:up", true).matches(&row, false));
        assert!(ScoreFilter::parse("-rank<10", true).matches(&row, false));
    }

    #[test]
    fn kind_terms_report_the_spelling_used() {
        let filter = ScoreFilter::parse("t:song -type:anime \"t:song\"", false);
        assert_eq!(filter.kind_terms("track"), (vec!["t:song"], Some(false)));
        assert_eq!(
            filter.kind_terms("anime"),
            (vec!["-type:anime"], Some(true))
        );
        assert_eq!(filter.kind_terms("game"), (vec![], None));
        // The `music` group lights each of its kinds.
        let group = ScoreFilter::parse("type:music", false);
        assert_eq!(group.kind_terms("album"), (vec!["type:music"], Some(false)));
        assert_eq!(group.kind_terms("anime"), (vec![], None));
    }

    #[test]
    fn unusable_terms_warn_instead_of_matching_nothing() {
        let row = candidate("x", "anime", 1.0, 1, None);
        let filter = ScoreFilter::parse("type:podcast v>abc is:up", false);
        assert_eq!(filter.warnings.len(), 3, "{:?}", filter.warnings);
        assert!(filter.matches(&row, false));
    }
}
