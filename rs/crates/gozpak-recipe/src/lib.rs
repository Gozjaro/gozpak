//! `gozpak-recipe`: package recipe parsing (M1, recipe format v1.1).
//!
//! Two on-disk formats:
//!
//! - **TOML (new primary)** — a single `recipe.toml` in the recipe
//!   directory (`[package]` / `[source]` / `[build]` / `[install]` /
//!   `[meta]`); parsed in [`toml_recipe`].
//! - **Legacy (kiss-style)** — separate `version`, `build`, `depends`,
//!   `sources`, `meta`, `checksums` files; parsed in [`legacy`] via
//!   [`Recipe::load_legacy`] (powers the M3 migration tool).
//!
//! [`Recipe::load`] tries `recipe.toml` first, then falls back to the
//! legacy layout, and records which kind was used in [`Recipe::kind`]
//! so the CLI can warn "legacy recipe" where it matters.
//!
//! M1 is read-only: the `build` section is parsed (TOML `steps[]`,
//! legacy shell script) but never executed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gozpak_core::{paths, DepReq, VersionRel};
use serde::{Deserialize, Serialize};

pub mod glob;
pub mod legacy;
pub mod subst;
pub mod toml_recipe;

/// Which on-disk format a recipe was loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeKind {
    /// New primary format: `recipe.toml`.
    Toml,
    /// Legacy kiss-style 3-file format (`version`/`build`/...).
    Legacy,
}

impl RecipeKind {
    pub fn label(&self) -> &'static str {
        match self {
            RecipeKind::Toml => "toml",
            RecipeKind::Legacy => "legacy",
        }
    }
}

/// One downloadable source file — v1.1 `[source].urls` entry.
///
/// `dest` is the legacy `sources` second column (cache subdirectory);
/// TOML recipes leave it `None`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceFile {
    pub url: String,
    /// Checksum; REQUIRED for non-git sources (M1: a warning when missing).
    pub sha256: Option<String>,
    pub dest: Option<String>,
    /// `None` = default (tarballs strip the top-level dir, M2a);
    /// `Some(false)` keeps the archive's top-level dir in place.
    pub strip_topdir: Option<bool>,
}

/// One source entry after normalizing both formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// One or more downloadable files (v1.1 `[source].urls`).
    Files { files: Vec<SourceFile> },
    /// A git repository, `repo` with the `git+` prefix stripped and
    /// `pin` the ref after `#` (TOML v1.1 requires a *pinned* ref —
    /// commit or tag, never a branch; legacy `git+` lines may have none).
    Git {
        repo: String,
        pin: Option<String>,
    },
    /// Local file or directory; relative paths resolve against the
    /// recipe directory (TOML `patches[]`, legacy local source lines).
    Local { path: PathBuf },
}

impl Source {
    pub fn is_git(&self) -> bool {
        matches!(self, Source::Git { .. })
    }

    /// A short display form for log lines.
    pub fn display(&self) -> String {
        match self {
            Source::Files { files } => files
                .iter()
                .map(|f| f.url.clone())
                .collect::<Vec<_>>()
                .join(" "),
            Source::Git { repo, pin } => match pin {
                Some(p) => format!("{repo}#{p}"),
                None => repo.clone(),
            },
            Source::Local { path } => path.display().to_string(),
        }
    }
}

/// A validated post-install hook (v1.1 `[install].post`).
///
/// `hook` is always a registry name (registry: ldconfig,
/// update-mime-database, gdk-pixbuf-query-loaders, desktop-database,
/// fontconfig, shared-mime-info); `match_filter` is the optional path
/// glob (used from M2 on; carried in M1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookSpec {
    pub hook: String,
    pub match_filter: Option<String>,
}

/// The mixed TOML `[install].post` entry: a plain string (registry hook
/// name) or an inline table `{ hook = "...", match = "..." }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum HookEntry {
    Simple(String),
    Named {
        hook: String,
        /// TOML key `match` (a keyword-ish word; renamed in Rust).
        #[serde(default, rename = "match")]
        match_filter: Option<String>,
    },
}

/// The hook names the installer knows (v1.1 registry).
pub const HOOK_REGISTRY: [&str; 6] = [
    "ldconfig",
    "update-mime-database",
    "gdk-pixbuf-query-loaders",
    "desktop-database",
    "fontconfig",
    "shared-mime-info",
];

/// A parsed recipe (unified across both on-disk formats).
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub kind: RecipeKind,
    /// Package name: TOML `[package].name`, else the recipe directory name.
    pub name: String,
    /// v1.1 `[package].epoch` (0 for legacy recipes).
    pub epoch: u64,
    pub version: VersionRel,
    /// The recipe directory.
    pub dir: PathBuf,
    /// TOML `[package].arch` (defaults to `"x86_64"`); `None` for legacy.
    pub arch: Option<String>,
    /// TOML `[package].desc` / legacy `meta` `description=`.
    pub desc: Option<String>,
    /// TOML `[package].license` / legacy `meta` `license=`.
    pub license: Option<String>,
    /// TOML `[package].url` (homepage); `None` for legacy.
    pub homepage: Option<String>,
    /// Runtime dependencies (v1.1 constraint strings, e.g. `zlib>=1.3`).
    pub depends: Vec<DepReq>,
    /// Build-only dependencies (legacy ` make` suffix / TOML `build_depends`).
    pub build_depends: Vec<DepReq>,
    /// v1.1 virtual packages / conflicts / replacements.
    pub provides: Vec<DepReq>,
    pub conflicts: Vec<DepReq>,
    pub replaces: Vec<DepReq>,
    /// Sources in file order.
    pub sources: Vec<Source>,
    /// Legacy `checksums` file lines (TOML recipes keep their checksums
    /// in `[source].urls[].sha256` instead, so this stays empty for them).
    pub checksums: Vec<String>,
    /// Free-form metadata: legacy `meta` key=value lines / TOML `[meta]`.
    pub meta: BTreeMap<String, String>,
    // TOML-only sections (empty for legacy recipes):
    /// `[build].system` — `"raw" | "autotools" | "meson" | "cmake" | "make"`.
    pub build_system: Option<String>,
    /// `[build].steps` — for `system = "raw"` this is ONE shell script
    /// (single-`sh -eu` semantics, documented only in M1); the legacy
    /// shell `build` file content is stored here verbatim (one string).
    pub build_steps: Vec<String>,
    /// `[build].meson_args` / `[build].cmake_args` / `[build].configure_args`.
    pub meson_args: Vec<String>,
    pub cmake_args: Vec<String>,
    pub configure_args: Vec<String>,
    /// `[build].make_target` (for `system = "make"`).
    pub make_target: Option<String>,
    /// `[install].post` — validated registry hook specs.
    pub install_post: Vec<HookSpec>,
    /// `[install].files` — `"auto"` or an explicit list (M1: stored only).
    pub install_files: String,
    /// `[install].backup` — user-modified config paths (M1: carried only).
    pub install_backup: Vec<String>,
    /// Non-fatal validation notes (M1), e.g. `arch = "any"`, missing
    /// sha256 for a non-git source. The CLI prints these as warnings.
    pub warnings: Vec<String>,
}

/// Errors returned by [`Recipe::load`] / [`Recipe::load_legacy`].
#[derive(Debug)]
pub enum RecipeError {
    Io(std::io::Error),
    NotADirectory(PathBuf),
    /// Legacy: the `version` file is missing (required).
    MissingVersion(PathBuf),
    /// Legacy: the `version` file does not contain `<version> <release>`.
    InvalidVersion {
        path: PathBuf,
        line: String,
    },
    /// TOML: parse error or schema violation (message explains which).
    Toml {
        path: PathBuf,
        message: String,
    },
}

impl RecipeError {
    pub fn io(e: std::io::Error) -> Self {
        RecipeError::Io(e)
    }
}

impl std::fmt::Display for RecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecipeError::Io(e) => write!(f, "io error: {e}"),
            RecipeError::NotADirectory(p) => write!(f, "not a directory: {}", p.display()),
            RecipeError::MissingVersion(p) => {
                write!(f, "missing version file: {}", p.display())
            }
            RecipeError::InvalidVersion { path, line } => write!(
                f,
                "invalid version file {} ('{}' is not '<version> <release>')",
                path.display(),
                line
            ),
            RecipeError::Toml { path, message } => {
                write!(f, "invalid recipe.toml {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for RecipeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RecipeError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl Recipe {
    /// Load a recipe directory: prefer the new `recipe.toml` format,
    /// fall back to the legacy 3-file layout.
    pub fn load(dir: &Path) -> Result<Self, RecipeError> {
        if paths::recipe_toml_file(dir).is_file() {
            return toml_recipe::load(dir);
        }
        Self::load_legacy(dir)
    }

    /// Load the legacy (kiss-style) layout explicitly.
    ///
    /// Kept as a public entry point so the M3 migration tool can convert
    /// legacy recipes to TOML without ambiguity.
    pub fn load_legacy(dir: &Path) -> Result<Self, RecipeError> {
        legacy::load(dir)
    }

    /// Find the first recipe directory matching `name` under `bases`
    /// (searched left to right; first hit wins).
    pub fn find(name: &str, bases: &[PathBuf]) -> Option<PathBuf> {
        for base in bases {
            let dir = base.join(name);
            if dir.is_dir() {
                return Some(dir);
            }
        }
        None
    }

    /// Search recipe directories under `bases` whose *directory name*
    /// matches the shell-glob `query` (`*`, `?` supported). Mirrors the
    /// shell `gozpak search`: matches are returned sorted, one per line.
    pub fn search(query: &str, bases: &[PathBuf]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for base in bases {
            let entries = match std::fs::read_dir(base) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if glob::glob_match(&name, query) {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    /// The v1.1 comparison key for the solver/index (`vercmp`).
    pub fn version_key(&self) -> gozpak_core::VersionKey {
        gozpak_core::VersionKey::new(
            self.epoch,
            self.version.version.clone(),
            self.version.release.clone(),
        )
    }
}

/// Serialize a recipe's `[package]`-ish header (used by M2/M3 index and
/// `.gpkg` manifest tooling; M1 exposes it for completeness).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageHeader {
    pub name: String,
    pub epoch: u64,
    pub version: String,
    pub release: String,
    pub arch: String,
    pub desc: String,
}
