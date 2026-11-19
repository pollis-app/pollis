//! Fitting foreign emoji names to Pollis shortcodes, with an explicit
//! collision policy.
//!
//! Pollis shortcodes are `[a-z0-9_]{2,32}` ([`shortcode_is_valid`]). Discord
//! allows upper case, so `Pepe` and `pepe` are two emoji there and one name
//! here; Slack allows `-` and `+`. The policy, decided once and shared by every
//! source (#1179, #1180):
//!
//! 1. lower-case; map every other disallowed character to `_`; collapse runs
//!    of `_`; trim `_` from both ends; cut to 32 bytes;
//! 2. a result shorter than 2 has no legal target and is skipped, reported;
//! 3. a name already taken (by an earlier emoji in the same import, or by an
//!    emoji the group already has) gets the first free `_2`, `_3`, ... suffix,
//!    cutting the stem so the whole stays within 32; reported as renamed.
//!
//! Emoji are processed in a stable order (most used first, then by id), so a
//! re-run assigns the same names.

use std::collections::BTreeSet;

pub use pollis_api::emoji::{shortcode_is_valid, SHORTCODE_MAX, SHORTCODE_MIN};

/// The outcome of fitting one name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fitted {
    /// Usable as is (possibly after case and character mapping).
    Ok(String),
    /// Collided with a taken name and was suffixed.
    Renamed { shortcode: String, collided_with: String },
    /// No legal shortcode exists for it.
    Skipped { reason: String },
}

/// Step 1 of the policy, without collision handling.
pub fn normalise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_underscore = false;
    for c in name.chars() {
        let mapped = if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '_'
        };
        if mapped == '_' {
            if last_underscore {
                continue;
            }
            last_underscore = true;
        } else {
            last_underscore = false;
        }
        out.push(mapped);
    }
    let trimmed = out.trim_matches('_');
    let mut cut = trimmed.to_string();
    cut.truncate(SHORTCODE_MAX);
    cut.trim_end_matches('_').to_string()
}

/// Assigns shortcodes against a growing set of taken names.
#[derive(Debug, Default)]
pub struct Allocator {
    taken: BTreeSet<String>,
}

impl Allocator {
    /// Start with the names the target group already uses.
    pub fn with_existing<I: IntoIterator<Item = String>>(existing: I) -> Self {
        Self {
            taken: existing.into_iter().collect(),
        }
    }

    pub fn fit(&mut self, name: &str) -> Fitted {
        let base = normalise(name);
        if base.len() < SHORTCODE_MIN {
            return Fitted::Skipped {
                reason: format!("\"{name}\" has no valid shortcode (needs 2 to 32 of a-z, 0-9, _)"),
            };
        }
        if self.taken.insert(base.clone()) {
            return Fitted::Ok(base);
        }
        for n in 2u32.. {
            let suffix = format!("_{n}");
            let mut stem = base.clone();
            stem.truncate(SHORTCODE_MAX - suffix.len());
            let candidate = format!("{}{suffix}", stem.trim_end_matches('_'));
            if self.taken.insert(candidate.clone()) {
                return Fitted::Renamed {
                    shortcode: candidate,
                    collided_with: base,
                };
            }
        }
        unreachable!("an unbounded suffix search always finds a free name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_case_and_characters() {
        assert_eq!(normalise("PepeHands"), "pepehands");
        assert_eq!(normalise("party-parrot+1"), "party_parrot_1");
        assert_eq!(normalise("__a--b__"), "a_b");
        assert_eq!(normalise(&"x".repeat(40)).len(), SHORTCODE_MAX);
    }

    #[test]
    fn every_fitted_name_is_valid() {
        let mut alloc = Allocator::default();
        for name in ["Pepe", "pepe", "PEPE", "a-b", "Ünïcödé_dragon", &"Long".repeat(20)] {
            match alloc.fit(name) {
                Fitted::Ok(s) | Fitted::Renamed { shortcode: s, .. } => {
                    assert!(shortcode_is_valid(&s), "{name:?} became invalid {s:?}");
                }
                Fitted::Skipped { .. } => {}
            }
        }
    }

    #[test]
    fn collisions_get_suffixes_in_order() {
        let mut alloc = Allocator::default();
        assert_eq!(alloc.fit("Pepe"), Fitted::Ok("pepe".into()));
        assert_eq!(
            alloc.fit("pepe"),
            Fitted::Renamed { shortcode: "pepe_2".into(), collided_with: "pepe".into() }
        );
        assert_eq!(
            alloc.fit("PEPE"),
            Fitted::Renamed { shortcode: "pepe_3".into(), collided_with: "pepe".into() }
        );
    }

    #[test]
    fn existing_group_emoji_count_as_taken() {
        let mut alloc = Allocator::with_existing(["dragon".to_string()]);
        assert!(matches!(alloc.fit("Dragon"), Fitted::Renamed { .. }));
    }

    #[test]
    fn a_suffix_never_overflows_the_limit() {
        let mut alloc = Allocator::default();
        let long = "d".repeat(40);
        alloc.fit(&long);
        match alloc.fit(&long) {
            Fitted::Renamed { shortcode, .. } => {
                assert!(shortcode.len() <= SHORTCODE_MAX);
                assert!(shortcode.ends_with("_2"));
                assert!(shortcode_is_valid(&shortcode));
            }
            other => panic!("expected a rename, got {other:?}"),
        }
    }

    #[test]
    fn a_one_character_name_is_skipped() {
        let mut alloc = Allocator::default();
        assert!(matches!(alloc.fit("x"), Fitted::Skipped { .. }));
        assert!(matches!(alloc.fit("!!"), Fitted::Skipped { .. }));
    }
}
