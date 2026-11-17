//! Package version + release, as stored in `version` files.

use std::fmt;

/// A package version with its release number, e.g. version `1.3.1`, release `1`.
///
/// `version` files hold a single line `<version> <release>`
/// (see `docs/recipes.md`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VersionRel {
    pub version: String,
    pub release: String,
}

impl VersionRel {
    pub fn new(version: impl Into<String>, release: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            release: release.into(),
        }
    }

    /// Parse the single-line `<version> <release>` form.
    ///
    /// Exactly two non-empty fields (the shell version dies when the release
    /// field is missing; trailing fields make the line malformed).
    pub fn parse(line: &str) -> Option<Self> {
        let mut parts = line.trim().split_whitespace();
        let version = parts.next()?.to_string();
        let release = parts.next()?.to_string();
        if parts.next().is_some() {
            return None; // not "<version> <release>"
        }
        if version.is_empty() || release.is_empty() {
            return None;
        }
        Some(Self { version, release })
    }

    /// Split the version string into `(major, minor, patch, ident)` on the
    /// `.-+_` character set, mirroring the shell
    /// `IFS=.+-_ read -r repo_major repo_minor repo_patch repo_ident`.
    /// Missing trailing components are empty strings.
    pub fn components(&self) -> (String, String, String, String) {
        let mut it = self
            .version
            .split(|c: char| matches!(c, '.' | '+' | '-' | '_'));
        (
            it.next().unwrap_or("").to_string(),
            it.next().unwrap_or("").to_string(),
            it.next().unwrap_or("").to_string(),
            it.next().unwrap_or("").to_string(),
        )
    }

    /// The `version-release` display form used throughout the CLI output.
    pub fn display(&self) -> String {
        format!("{}-{}", self.version, self.release)
    }
}

impl fmt::Display for VersionRel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

// ---------------------------------------------------------------------------
// v1.1 version comparison (pacman `vercmp` semantics)
// ---------------------------------------------------------------------------

/// The three keys the solver and the repo index compare in order:
/// epoch (numeric) → version → release (pacman chunk comparisons).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct VersionKey {
    pub epoch: u64,
    pub version: String,
    pub release: String,
}

impl VersionKey {
    pub fn new(epoch: u64, version: impl Into<String>, release: impl Into<String>) -> Self {
        Self {
            epoch,
            version: version.into(),
            release: release.into(),
        }
    }
}

/// Parse pacman's `epoch:version-release` form, splitting at the last `-`.
pub fn parse_evr(s: &str) -> VersionKey {
    let epoch_end = s.bytes().take_while(u8::is_ascii_digit).count();
    let (epoch, remainder) = if s.as_bytes().get(epoch_end) == Some(&b':') {
        let epoch = if epoch_end == 0 {
            0
        } else {
            s[..epoch_end].parse::<u64>().unwrap_or(u64::MAX)
        };
        (epoch, &s[epoch_end + 1..])
    } else {
        (0, s)
    };
    let (version, release) = remainder.rsplit_once('-').unwrap_or((remainder, ""));
    VersionKey::new(epoch, version, release)
}

/// Pacman-style comparison: numeric epoch, then version, then release.
/// Release is compared only when both versions specify a non-empty release.
pub fn vercmp(a: &VersionKey, b: &VersionKey) -> std::cmp::Ordering {
    let o = a.epoch.cmp(&b.epoch);
    if o != std::cmp::Ordering::Equal {
        return o;
    }
    let o = pkgver_cmp(&a.version, &b.version);
    if o != std::cmp::Ordering::Equal {
        return o;
    }
    if !a.release.is_empty() && !b.release.is_empty() {
        pkgver_cmp(&a.release, &b.release)
    } else {
        std::cmp::Ordering::Equal
    }
}

/// Pacman's `rpmvercmp`: compare separator lengths and numeric/ASCII alpha runs.
/// Numeric runs are compared without conversion to a bounded integer.
pub fn pkgver_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    if a == b {
        return Ordering::Equal;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (start_a, start_b) = (i, j);
        while i < a.len() && !a[i].is_ascii_alphanumeric() {
            i += 1;
        }
        while j < b.len() && !b[j].is_ascii_alphanumeric() {
            j += 1;
        }
        if i == a.len() || j == b.len() {
            break;
        }
        let ordering = (i - start_a).cmp(&(j - start_b));
        if ordering != Ordering::Equal {
            return ordering;
        }

        let (mut start_a, mut start_b) = (i, j);
        let isnum = a[i].is_ascii_digit();
        if isnum {
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        } else {
            while i < a.len() && a[i].is_ascii_alphabetic() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
        }
        if j == start_b {
            return if isnum { Ordering::Greater } else { Ordering::Less };
        }
        if isnum {
            while start_a < i && a[start_a] == b'0' {
                start_a += 1;
            }
            while start_b < j && b[start_b] == b'0' {
                start_b += 1;
            }
            let ordering = (i - start_a).cmp(&(j - start_b));
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        let ordering = a[start_a..i].cmp(&b[start_b..j]);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }

    if i == a.len() && j == b.len() {
        return Ordering::Equal;
    }
    if (i == a.len() && !b[j].is_ascii_alphabetic())
        || (i < a.len() && a[i].is_ascii_alphabetic())
    {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacman_version_comparisons() {
        use std::cmp::Ordering::{Equal, Greater, Less};

        for (a, b, expected) in [
            ("1.5.0", "1.5.0", Equal),
            ("1.5.1", "1.5.0", Greater),
            ("1.5.1", "1.5", Greater),
            ("1.0", "1.0.1", Less),
            ("3.5", "3.5.1", Less),
            ("1.5.0-1", "1.5.0-2", Less),
            ("1.5.0-1", "1.5.1-1", Less),
            ("1.5.0-2", "1.5.1-1", Less),
            ("1.5-1", "1.5", Equal),
            ("1.1-1", "1.1", Equal),
            ("1.0a", "1.0alpha", Less),
            ("1.0alpha", "1.0b", Less),
            ("1.0b", "1.0beta", Less),
            ("1.0beta", "1.0rc", Less),
            ("1.0rc", "1.0", Less),
            ("3.5.1rc1", "3.5.1", Less),
            ("1.5.a", "1.5", Greater),
            ("1.5.b", "1.5.a", Greater),
            ("1.5.1", "1.5.b", Greater),
            ("1.0a", "1.0.1", Less),
            ("2.0", "2_0", Equal),
            ("2.0_a", "2_0.a", Equal),
            ("2.0a", "2.0.a", Less),
            ("2___a", "2_a", Greater),
            ("1.01", "1.1", Equal),
            ("1.9", "1.10", Less),
            ("0:1.0", "0:1.0", Equal),
            ("0:1.0", "0:1.1", Less),
            ("1:1.0", "0:1.0", Greater),
            ("1:1.0", "0:1.1", Greater),
            ("1:1.0", "2:1.1", Less),
            ("1:1.0", "0:1.0-1", Greater),
            ("1:1.0-1", "0:1.1-1", Greater),
            ("0:1.0", "1.0", Equal),
            ("0:1.1", "1.0", Greater),
            ("1:1.0", "1.0", Greater),
            ("1.1", "1:1.1", Less),
            ("99999999999999999999", "1", Greater),
        ] {
            assert_eq!(vercmp(&parse_evr(a), &parse_evr(b)), expected, "{a} vs {b}");
            assert_eq!(
                vercmp(&parse_evr(b), &parse_evr(a)),
                expected.reverse(),
                "{b} vs {a}"
            );
        }
    }

    #[test]
    fn parse_epoch_version_release() {
        for (input, epoch, version, release) in [
            ("2:1.0-beta-3", 2, "1.0-beta", "3"),
            ("001:1.0-", 1, "1.0", ""),
            ("abc:1.0-2", 0, "abc:1.0", "2"),
            ("1a:1.0-2", 0, "1a:1.0", "2"),
            (":1.0", 0, "1.0", ""),
            ("", 0, "", ""),
        ] {
            assert_eq!(parse_evr(input), VersionKey::new(epoch, version, release));
        }
    }

    #[test]
    fn numeric_runs_do_not_overflow_and_releases_use_chunk_rules() {
        use std::cmp::Ordering::{Equal, Less};

        assert_eq!(pkgver_cmp("99999999999999999998", "99999999999999999999"), Less);
        assert_eq!(vercmp(&parse_evr("1-1.9"), &parse_evr("1-1.10")), Less);
        assert_eq!(vercmp(&parse_evr("1-"), &parse_evr("1-2")), Equal);
    }

    #[test]
    fn parse_version_release() {
        let v = VersionRel::parse("1.3.1 1").unwrap();
        assert_eq!(v.version, "1.3.1");
        assert_eq!(v.release, "1");
        assert_eq!(v.display(), "1.3.1-1");
    }

    #[test]
    fn parse_tolerates_extra_whitespace_and_trailing_newline() {
        let v = VersionRel::parse("  2.0.0\t2\n").unwrap();
        assert_eq!(v.display(), "2.0.0-2");
    }

    #[test]
    fn parse_requires_release_field() {
        assert!(VersionRel::parse("1.0").is_none());
        assert!(VersionRel::parse("  ").is_none());
        assert!(VersionRel::parse("").is_none());
        // Three fields is not "<version> <release>".
        assert!(VersionRel::parse("not a version").is_none());
        assert!(VersionRel::parse("1.0 1 x").is_none());
    }

    #[test]
    fn components_split_on_dot_dash() {
        let v = VersionRel::new("1.3.1-1", "1");
        assert_eq!(v.components(), ("1".into(), "3".into(), "1".into(), "1".into()));

        let v = VersionRel::new("24.0", "1");
        assert_eq!(v.components(), ("24".into(), "0".into(), String::new(), String::new()));

        let v = VersionRel::new("1.2+git", "1");
        assert_eq!(v.components(), ("1".into(), "2".into(), "git".into(), String::new()));
    }

    // -----------------------------------------------------------------------
    // vercmp (v1.1)
    // -----------------------------------------------------------------------

    fn key(epoch: u64, version: &str, release: &str) -> VersionKey {
        VersionKey::new(epoch, version, release)
    }

    #[test]
    fn release_tags_sort_below_release() {
        use std::cmp::Ordering;
        assert_eq!(
            pkgver_cmp("3.5.1rc1", "3.5.1"),
            Ordering::Less,
            "3.5.1rc1 < 3.5.1"
        );
        assert_eq!(pkgver_cmp("3.5.1", "3.5.1rc1"), Ordering::Greater);
        assert_eq!(pkgver_cmp("1.0a1", "1.0"), Ordering::Less);
        assert_eq!(pkgver_cmp("1.0b2", "1.0rc1"), Ordering::Less);
        assert_eq!(pkgver_cmp("1.0alpha", "1.0beta"), Ordering::Less);
        assert_eq!(pkgver_cmp("3.5.1", "3.5.1"), Ordering::Equal);
    }

    #[test]
    fn numeric_chunks_compare_numerically() {
        use std::cmp::Ordering;
        assert_eq!(pkgver_cmp("1.9", "1.10"), Ordering::Less);
        assert_eq!(pkgver_cmp("1.0a2", "1.0a10"), Ordering::Less);
        assert_eq!(pkgver_cmp("2.0", "1.99"), Ordering::Greater);
    }

    #[test]
    fn epoch_beats_version() {
        use std::cmp::Ordering;
        // 1.0-1 with epoch 1 beats 0.9-2 with epoch 0.
        assert_eq!(
            vercmp(&key(1, "1.0", "1"), &key(0, "0.9", "2")),
            Ordering::Greater
        );
        assert_eq!(
            vercmp(&key(0, "0.9", "2"), &key(1, "1.0", "1")),
            Ordering::Less
        );
        // Equal epochs fall through to the version compare.
        assert_eq!(
            vercmp(&key(2, "0.9", "9"), &key(2, "1.0", "1")),
            Ordering::Less
        );
    }

    #[test]
    fn release_is_the_tiebreak() {
        use std::cmp::Ordering;
        assert_eq!(
            vercmp(&key(0, "3.5.1", "2"), &key(0, "3.5.1", "1")),
            Ordering::Greater
        );
        // Numeric, not lexical: release 10 > release 9.
        assert_eq!(
            vercmp(&key(0, "3.5.1", "10"), &key(0, "3.5.1", "9")),
            Ordering::Greater
        );
    }
}
