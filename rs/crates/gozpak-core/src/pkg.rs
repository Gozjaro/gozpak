//! Package and dependency records.
//!
//! v1.1: dependency entries are constraint strings (pacman-style), parsed
//! into [`DepReq`]. The constraint syntax is:
//!
//! - `"name"` — any version satisfies
//! - `"name>=1.3"` — operator in `{ =, <, >, <=, >= }` followed by a version
//!
//! Legacy `depends` files additionally allow a trailing ` make` marker
//! (build-only dep), handled by [`DepReq::parse_legacy_line`].

/// A version constraint operator (pacman-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    /// `=` — exactly this version.
    Eq,
    /// `<` — strictly older.
    Lt,
    /// `>` — strictly newer.
    Gt,
    /// `<=` — this version or older.
    Le,
    /// `>=` — this version or newer.
    Ge,
}

impl Op {
    /// Two-char symbol (for display/round-trip).
    pub fn symbol(&self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Lt => "<",
            Op::Gt => ">",
            Op::Le => "<=",
            Op::Ge => ">=",
        }
    }
}

/// One dependency requirement: a package name plus an optional version
/// constraint. Parsed from TOML `depends`/`build_depends`/`provides`/
/// `conflicts`/`replaces` entries, or legacy `depends` file lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DepReq {
    /// Package name (`[a-z0-9-]+`).
    pub name: String,
    /// Constraint operator; `None` = any version satisfies.
    pub op: Option<Op>,
    /// The constrained version (present iff `op` is `Some`).
    pub version: Option<String>,
    /// Build-only dependency (legacy ` make` suffix / TOML `build_depends`).
    pub build_only: bool,
    /// The raw input string, kept for round-trip and display.
    pub raw: String,
}

impl DepReq {
    /// Whether the candidate version meets this dependency's constraint.
    pub fn satisfied_by(&self, candidate: &crate::version::VersionKey) -> bool {
        use crate::version::{parse_evr, vercmp};
        use std::cmp::Ordering;

        let Some(op) = self.op else {
            return true;
        };
        let Some(version) = self.version.as_deref() else {
            return false;
        };
        let ordering = vercmp(candidate, &parse_evr(version));
        match op {
            Op::Eq => ordering == Ordering::Equal,
            Op::Lt => ordering == Ordering::Less,
            Op::Gt => ordering == Ordering::Greater,
            Op::Le => ordering != Ordering::Greater,
            Op::Ge => ordering != Ordering::Less,
        }
    }

    /// Parse a v1.1 constraint string (TOML `depends` entry).
    ///
    /// `build_only` marks the entry as coming from `build_depends`.
    pub fn parse_constraint(raw: &str, build_only: bool) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("empty dependency entry".into());
        }
        // Find the leftmost constraint operator. Two-char operators beat
        // the one-char form at the same position.
        let mut pos: Option<usize> = None;
        for (i, c) in raw.char_indices() {
            match c {
                '<' | '>' | '=' => {
                    pos = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let (name, op, version) = match pos {
            None => (raw, None, None),
            Some(i) => {
                let name = &raw[..i];
                let rest = &raw[i..];
                let (sym, ver) = if rest.starts_with("<=") || rest.starts_with(">=") {
                    (&rest[..2], rest[2..].trim_start())
                } else {
                    (&rest[..1], rest[1..].trim_start())
                };
                let op = match sym {
                    "=" => Op::Eq,
                    "<" => Op::Lt,
                    ">" => Op::Gt,
                    "<=" => Op::Le,
                    _ => Op::Ge,
                };
                if ver.is_empty() {
                    return Err(format!(
                        "dependency '{raw}': constraint '{sym}' needs a version"
                    ));
                }
                (name, Some(op), Some(ver.to_string()))
            }
        };
        if !is_valid_name(name) {
            return Err(format!(
                "dependency '{raw}': invalid package name '{name}' (want [a-z0-9-]+)"
            ));
        }
        Ok(Self {
            name: name.to_string(),
            op,
            version: version.map(String::from),
            build_only,
            raw: raw.to_string(),
        })
    }

    /// Parse a legacy `depends` file line: `<constraint>` or
    /// `<constraint> make` (build-only). Returns `None` for blank lines
    /// and lines that are not valid constraints (legacy is lenient;
    /// malformed lines are skipped, matching the shell reader).
    pub fn parse_legacy_line(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let (core, build_only) = if let Some(c) = line.strip_suffix("make") {
            if c.ends_with(' ') {
                (c.trim_end(), true)
            } else {
                (line, false)
            }
        } else {
            (line, false)
        };
        // A constraint name can only contain [a-z0-9-]; a line whose name
        // part holds other characters (e.g. a stray comment) is skipped.
        Self::parse_constraint(core, build_only).ok()
    }

    /// A plain name with no constraint (TOML helper).
    pub fn simple(name: impl Into<String>, build_only: bool) -> Self {
        let name = name.into();
        Self {
            raw: name.clone(),
            name,
            op: None,
            version: None,
            build_only,
        }
    }

    /// Render back in display form (`name`, `name op version`, plus
    /// ` make` for legacy build-only round-trips).
    pub fn display(&self) -> String {
        let mut out = self.name.clone();
        if let (Some(op), Some(v)) = (&self.op, &self.version) {
            out.push_str(op.symbol());
            out.push_str(v);
        }
        if self.build_only {
            out.push_str(" make");
        }
        out
    }
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// An installed-package record: directory name plus its `version` file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pkg {
    pub name: String,
    pub version: crate::VersionRel,
}

impl Pkg {
    /// Build from an installed-db entry (directory name + version line).
    /// Returns `None` when the version line is missing/invalid.
    pub fn from_db_entry(name: &str, version_line: Option<&str>) -> Option<Self> {
        let line = version_line?;
        Some(Self {
            name: name.to_string(),
            version: crate::VersionRel::parse(line)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_constraint_satisfaction() {
        use crate::version::parse_evr;

        for (requirement, candidate, expected) in [
            ("zlib>=1.3", "1.3.1-1", true),
            ("zlib>=1.3", "1.2.13-1", false),
            ("zlib>=1.3", "1.3-5", true),
            ("zlib<1.3", "1.2.13", true),
            ("zlib<1.3", "1.3", false),
            ("zlib=1.3-2", "1.3-2", true),
            ("zlib=1.3-2", "1.3-1", false),
            ("zlib=1.3", "1.3-7", true),
            ("foo>=1:2.0", "0:9.9", false),
            ("foo>=1:2.0", "1:2.0-1", true),
            ("bar", "anything", true),
            ("zlib>1.3", "1.3.1", true),
            ("zlib>1.3", "1.3", false),
            ("zlib<=1.3", "1.2.13", true),
            ("zlib<=1.3", "1.3", true),
            ("zlib<=1.3", "1.3.1", false),
        ] {
            let dependency = DepReq::parse_constraint(requirement, false).unwrap();
            assert_eq!(
                dependency.satisfied_by(&parse_evr(candidate)),
                expected,
                "{requirement} by {candidate}"
            );
        }
    }

    #[test]
    fn parse_plain_dep() {
        let d = DepReq::parse_constraint("openssl", false).unwrap();
        assert_eq!(d.name, "openssl");
        assert!(d.op.is_none());
        assert!(d.version.is_none());
        assert!(!d.build_only);
        assert_eq!(d.display(), "openssl");
    }

    #[test]
    fn parse_all_operators() {
        for (raw, op, ver) in [
            ("zlib>=1.3", Op::Ge, "1.3"),
            ("zlib<=1.3", Op::Le, "1.3"),
            ("zlib>2", Op::Gt, "2"),
            ("zlib<2", Op::Lt, "2"),
            ("libssl=3", Op::Eq, "3"),
        ] {
            let d = DepReq::parse_constraint(raw, false).unwrap();
            assert_eq!(d.op, Some(op), "{raw}");
            assert_eq!(d.version.as_deref(), Some(ver), "{raw}");
            assert_eq!(d.name, raw.split(|c| c == '<' || c == '>' || c == '=').next().unwrap());
        }
    }

    #[test]
    fn two_char_operator_wins_at_same_position() {
        let d = DepReq::parse_constraint("foo<=1.2", false).unwrap();
        assert_eq!(d.op, Some(Op::Le));
        assert_eq!(d.version.as_deref(), Some("1.2"));
    }

    #[test]
    fn legacy_make_suffix() {
        let d = DepReq::parse_legacy_line("cmake make").unwrap();
        assert!(d.build_only);
        assert_eq!(d.name, "cmake");
        assert_eq!(d.display(), "cmake make");
        // A package literally named "make" stays a plain dep.
        let d = DepReq::parse_legacy_line("make").unwrap();
        assert!(!d.build_only);
        assert_eq!(d.name, "make");
    }

    #[test]
    fn legacy_blank_and_garbage() {
        assert!(DepReq::parse_legacy_line("").is_none());
        assert!(DepReq::parse_legacy_line("   ").is_none());
        assert!(DepReq::parse_legacy_line("UPPER case").is_none());
    }

    #[test]
    fn constraint_without_version_is_error() {
        assert!(DepReq::parse_constraint("zlib>=", false).is_err());
        assert!(DepReq::parse_constraint(">=1", false).is_err());
    }

    #[test]
    fn pkg_from_db_entry() {
        let p = Pkg::from_db_entry("zlib", Some("1.3.1 1")).unwrap();
        assert_eq!(p.name, "zlib");
        assert_eq!(p.version.display(), "1.3.1-1");
        assert!(Pkg::from_db_entry("zlib", None).is_none());
        assert!(Pkg::from_db_entry("zlib", Some("garbage")).is_none());
    }
}
