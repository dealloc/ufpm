//! Lenient ordering for `FoundryVTT` package version strings.
//!
//! Version strings in the wild are free-form: alongside plain `1.2.3` the
//! index contains `V1.0.1`, `2.0-beta.3`, `1..1`, `3+` and worse. This
//! module implements a Foundry-style piecewise comparison — split on dots,
//! compare numerically where both segments are numbers and
//! case-insensitively lexicographic otherwise — and an explicit
//! "different but not provably newer" outcome so callers never have to
//! pretend to know more than they do.

use std::cmp::Ordering;

/// How an available version relates to the installed one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    /// The version strings are identical.
    Same,

    /// The available version is newer under the lenient ordering.
    Newer,

    /// The versions differ, but the available one is not provably newer
    /// (incomparable strings, or the ordering says it is older). The index
    /// only ever holds the latest release, so this usually means the local
    /// version string is unusual rather than ahead.
    Changed,
}

/// Classifies an available version against the installed one.
#[must_use]
pub fn against_installed(installed: &str, available: &str) -> Comparison {
    if installed == available {
        return Comparison::Same;
    }
    match compare(installed, available) {
        Ordering::Less => Comparison::Newer,
        Ordering::Equal | Ordering::Greater => Comparison::Changed,
    }
}

/// Compares two version strings under the lenient piecewise ordering.
#[must_use]
pub fn compare(a: &str, b: &str) -> Ordering {
    let left: Vec<&str> = a.split('.').collect();
    let right: Vec<&str> = b.split('.').collect();

    for (l, r) in left.iter().zip(right.iter()) {
        let ordering = compare_segment(l, r);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }

    left.len().cmp(&right.len())
}

/// Compares one dot-separated segment: numerically when both sides are
/// numbers, case-insensitively lexicographic otherwise.
fn compare_segment(l: &str, r: &str) -> Ordering {
    match (l.parse::<u64>(), r.parse::<u64>()) {
        (Ok(l), Ok(r)) => l.cmp(&r),
        _ => l.to_lowercase().cmp(&r.to_lowercase()),
    }
}

#[cfg(test)]
mod tests {
    //! Ordering tests, including the corpus of weird real-world versions.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;

    /// Plain numeric versions order numerically, not lexicographically.
    #[test]
    fn numeric_versions_compare_numerically() {
        assert_eq!(compare("1.9.0", "1.10.0"), Ordering::Less);
        assert_eq!(compare("0.9", "0.10"), Ordering::Less);
        assert_eq!(compare("2.0.0", "2.0.0"), Ordering::Equal);
        assert_eq!(compare("10.0", "9.9"), Ordering::Greater);
    }

    /// More segments win when the shared prefix is equal.
    #[test]
    fn longer_versions_win_on_equal_prefix() {
        assert_eq!(compare("1.0", "1.0.0"), Ordering::Less);
        assert_eq!(compare("1.0.0", "1.0"), Ordering::Greater);
    }

    /// Non-numeric segments fall back to case-insensitive lexicographic.
    #[test]
    fn weird_real_world_versions_are_total() {
        assert_eq!(compare("V1.0.1", "V1.0.2"), Ordering::Less);
        assert_eq!(compare("v1.1", "V1.1"), Ordering::Equal);
        assert_eq!(compare("1..1", "1.0.1"), Ordering::Less);
        assert_eq!(compare("0.3.a", "0.3.b"), Ordering::Less);
        assert_eq!(compare("3+", "4"), Ordering::Less);
        assert_eq!(compare("2.4b", "2.4b"), Ordering::Equal);
    }

    /// Identical strings are the same version.
    #[test]
    fn identical_versions_are_same() {
        assert_eq!(against_installed("5.1.1", "5.1.1"), Comparison::Same);
    }

    /// A numerically higher available version is an update.
    #[test]
    fn higher_available_version_is_newer() {
        assert_eq!(against_installed("5.0.0", "5.1.1"), Comparison::Newer);
        assert_eq!(against_installed("V1.0.1", "V1.1"), Comparison::Newer);
        assert_eq!(against_installed("1.0", "1.0.0"), Comparison::Newer);
    }

    /// Different-but-not-newer versions are flagged as changed, never
    /// silently treated as up to date or as an update.
    #[test]
    fn incomparable_versions_are_changed() {
        assert_eq!(against_installed("2.0.0", "1.9.0"), Comparison::Changed);
        assert_eq!(against_installed("v1.1", "V1.1"), Comparison::Changed);
        assert_eq!(
            against_installed("2.0-beta.3", "2.0.0"),
            Comparison::Changed
        );
    }
}
