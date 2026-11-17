//! The new primary recipe format: a single `recipe.toml` (TOML 1.0),
//! recipe format v1.1 (docs/recipe-format.md).
//!
//! ```toml
//! [package]
//! name     = "openssl"
//! version  = "3.5.1"
//! release  = 1                    # required
//! epoch    = 0                    # optional, default 0
//! arch     = "x86_64"             # default; "any" only explicit + warned
//! desc     = "..."
//! license  = "Apache-2.0"
//! url      = "https://www.openssl.org/"
//! depends        = ["zlib>=1.3", "pcre2"]   # constraint strings
//! build_depends  = ["cmake>=3.16", "make"]
//! provides       = []            # virtual packages, same syntax
//! conflicts      = []
//! replaces       = []
//!
//! [source]
//! urls = [ { url = "https://host/openssl-$VERSION.tar.xz", sha256 = "..." } ]
//! # OR: git = "git+https://github.com/org/repo#<pinned ref>"
//! patches = ["patches/fix-build.patch"]
//!
//! [build]
//! system = "raw"                 # raw | autotools | meson | cmake | make
//! steps = ["./configure", "make DESTDIR=$DESTDIR install"]
//! meson_args = []  cmake_args = []  configure_args = []  make_target = ""
//!
//! [install]
//! post = ["ldconfig", { hook = "update-mime-database", match = "/usr/share/mime/**" }]
//! files = "auto"
//! backup = ["etc/sysctl.conf"]
//!
//! [meta]
//! maintainer = "..."
//! ```
//!
//! M1 is read-only: `steps` are captured (single-shell semantics are
//! documented, not executed); `backup` is carried, not applied.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gozpak_core::{paths, DepReq, VersionRel};
use serde::Deserialize;

use crate::{
    HOOK_REGISTRY, HookEntry, HookSpec, Recipe, RecipeError, RecipeKind, Source, SourceFile,
};

const VALID_ARCH: [&str; 2] = ["x86_64", "any"];
const DEFAULT_ARCH: &str = "x86_64";
const VALID_BUILD_SYSTEMS: [&str; 5] = ["raw", "autotools", "meson", "cmake", "make"];

#[derive(Debug, Default, Deserialize)]
pub struct TomlRecipe {
    #[serde(default)]
    pub package: PackageSection,
    #[serde(default)]
    pub source: Option<SourceSection>,
    #[serde(default)]
    pub build: Option<BuildSection>,
    #[serde(default)]
    pub install: Option<InstallSection>,
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PackageSection {
    pub name: Option<String>,
    pub version: Option<String>,
    /// v1.1: integer release, required.
    pub release: Option<u32>,
    /// Optional; default 0. Raises precedence when upstream changes
    /// versioning schemes (e.g. 1.x → 0.9).
    #[serde(default)]
    pub epoch: u64,
    /// Default `"x86_64"`; `"any"` must be written explicitly (warned).
    pub arch: Option<String>,
    pub desc: Option<String>,
    pub license: Option<String>,
    /// Package homepage.
    pub url: Option<String>,
    /// Constraint strings: "name", "name>=1.3", ops { =, <, >, <=, >= }.
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub build_depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SourceSection {
    /// Multi-file sources; sha256 REQUIRED for non-git sources (M1: warn).
    #[serde(default)]
    pub urls: Vec<UrlEntry>,
    /// A git source: `git+` prefix optional; a PINNED ref after `#` is
    /// mandatory (commit or tag — branch names are rejected).
    pub git: Option<String>,
    #[serde(default)]
    pub patches: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UrlEntry {
    pub url: String,
    pub sha256: Option<String>,
    /// Opt out of `--strip-components=1` for this archive (M2a strips
    /// the top-level dir by default). Set `false` when the recipe's
    /// build steps reference the archive's top-level dir by name
    /// (e.g. a source tree + a boot JDK shipped as two tarballs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strip_topdir: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct BuildSection {
    pub system: Option<String>,
    /// For `system = "raw"`: ONE shell script (single `sh -eu` process,
    /// working dir = unpacked source tree; cd/export persist across lines).
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub meson_args: Vec<String>,
    #[serde(default)]
    pub cmake_args: Vec<String>,
    #[serde(default)]
    pub configure_args: Vec<String>,
    #[serde(default)]
    pub make_target: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct InstallSection {
    /// Registry hook names and/or `{ hook = "...", match = "..." }`
    /// tables. No free-form shell.
    #[serde(default)]
    pub post: Vec<HookEntry>,
    pub files: Option<String>,
    /// User-modified config paths (relative to /); M1 carries only.
    #[serde(default)]
    pub backup: Vec<String>,
}

fn toml_err(dir: &Path, message: String) -> RecipeError {
    RecipeError::Toml {
        path: paths::recipe_toml_file(dir),
        message,
    }
}

/// Parse and validate `recipe.toml` in `dir`, producing a unified
/// [`Recipe`] of kind [`RecipeKind::Toml`].
pub fn load(dir: &Path) -> Result<Recipe, RecipeError> {
    if !dir.is_dir() {
        return Err(RecipeError::NotADirectory(dir.to_path_buf()));
    }
    let text =
        std::fs::read_to_string(paths::recipe_toml_file(dir)).map_err(RecipeError::Io)?;
    let tr: TomlRecipe =
        toml::from_str(&text).map_err(|e| toml_err(dir, format!("TOML parse error: {e}")))?;
    tr.into_recipe(dir)
}

/// A "pinned" git ref: a commit hash (hex, always has digits) or a
/// versioned tag. Purely-alphabetic names are treated as branch names.
fn is_pinned_ref(pin: &str) -> bool {
    pin.bytes().any(|b| b.is_ascii_digit())
}

fn parse_constraint_list(
    dir: &Path,
    section: &str,
    field: &str,
    entries: &[String],
    build_only: bool,
) -> Result<Vec<DepReq>, RecipeError> {
    entries
        .iter()
        .map(|e| {
            DepReq::parse_constraint(e, build_only).map_err(|msg| {
                toml_err(dir, format!("[package].{field}: {msg} (in {section})"))
            })
        })
        .collect()
}

impl TomlRecipe {
    pub fn into_recipe(self, dir: &Path) -> Result<Recipe, RecipeError> {
        let dir_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let name = self.package.name.clone().unwrap_or(dir_name.clone());
        if name.is_empty() {
            return Err(toml_err(dir, "[package].name is empty".into()));
        }

        let version = match self.package.version {
            Some(v) if !v.trim().is_empty() => v,
            _ => return Err(toml_err(dir, "[package].version is required".into())),
        };
        let release = match self.package.release {
            Some(r) => r,
            None => {
                return Err(toml_err(
                    dir,
                    "[package].release is required (v1.1: integer, bump when the recipe \
                     itself changes)"
                        .into(),
                ))
            }
        };
        let epoch = self.package.epoch;

        // arch: default x86_64; "any" must be explicit (and is warned).
        let mut warnings: Vec<String> = Vec::new();
        let arch = self
            .package
            .arch
            .clone()
            .unwrap_or_else(|| DEFAULT_ARCH.to_string());
        if !VALID_ARCH.contains(&arch.as_str()) {
            return Err(toml_err(
                dir,
                format!("[package].arch must be one of {VALID_ARCH:?} (got '{arch}')"),
            ));
        }
        if arch == "any" {
            warnings.push(
                "arch is \"any\" — reserve for arch-independent packages (data, docs); \
                 x86_64 is the default"
                    .into(),
            );
        }

        let (build_system, build_steps, meson_args, cmake_args, configure_args, make_target) =
            match self.build.as_ref() {
                Some(b) => {
                    let system = b.system.clone().unwrap_or_default();
                    if !system.is_empty() && !VALID_BUILD_SYSTEMS.contains(&system.as_str()) {
                        return Err(toml_err(
                            dir,
                            format!(
                                "[build].system must be one of {VALID_BUILD_SYSTEMS:?} (got '{system}')"
                            ),
                        ));
                    }
                    if system == "raw" && b.steps.is_empty() {
                        return Err(toml_err(
                            dir,
                            "build system 'raw' requires a non-empty steps[] list".into(),
                        ));
                    }
                    (
                        Some(system),
                        b.steps.clone(),
                        b.meson_args.clone(),
                        b.cmake_args.clone(),
                        b.configure_args.clone(),
                        b.make_target.clone(),
                    )
                }
                None => (None, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None),
            };

        // Sources: either files (urls[]) or a git repo — never both.
        let mut sources: Vec<Source> = Vec::new();
        if let Some(src) = &self.source {
            let has_files = src.urls.iter().any(|u| !u.url.trim().is_empty());
            if has_files && src.git.is_some() {
                return Err(toml_err(
                    dir,
                    "[source] is either urls[] or git, not both".into(),
                ));
            }
            if let Some(git) = src.git.as_deref().filter(|g| !g.trim().is_empty()) {
                let repo = git.trim();
                let body = repo.strip_prefix("git+").unwrap_or(repo);
                let (repo_url, pin) = match body.split_once('#') {
                    Some((r, p)) if !p.is_empty() => (r.to_string(), p.to_string()),
                    _ => {
                        return Err(toml_err(
                            dir,
                            format!(
                                "[source].git '{repo}' must contain a pinned ref after '#' \
                                 (a commit or tag; bare branch names are not allowed)"
                            ),
                        ))
                    }
                };
                if !is_pinned_ref(&pin) {
                    return Err(toml_err(
                        dir,
                        format!(
                            "[source].git ref '{pin}' looks like a branch name; \
                             pin a commit hash or a tag instead"
                        ),
                    ));
                }
                sources.push(Source::Git {
                    repo: repo_url,
                    pin: Some(pin),
                });
            }
            if has_files {
                let files: Vec<SourceFile> = src
                    .urls
                    .iter()
                    .filter(|u| !u.url.trim().is_empty())
                    .map(|u| {
                        if u.sha256.is_none() {
                            warnings.push(format!(
                                "source '{url}' has no sha256; non-git sources require a checksum",
                                url = u.url
                            ));
                        }
                        SourceFile {
                            url: u.url.clone(),
                            sha256: u.sha256.clone(),
                            dest: None,
                            strip_topdir: u.strip_topdir,
                        }
                    })
                    .collect();
                if !files.is_empty() {
                    sources.push(Source::Files { files });
                }
            }
            for p in &src.patches {
                if !p.trim().is_empty() {
                    sources.push(Source::Local {
                        path: PathBuf::from(p),
                    });
                }
            }
        }

        let meta: BTreeMap<String, String> = self.meta;
        let desc = self.package.desc.clone().or_else(|| meta.get("description").cloned());
        let license = self
            .package
            .license
            .clone()
            .or_else(|| meta.get("license").cloned());
        let homepage = self.package.url.clone();

        // Dependency-solver fields: constraint strings.
        let depends =
            parse_constraint_list(dir, "package", "depends", &self.package.depends, false)?;
        let build_depends =
            parse_constraint_list(dir, "package", "build_depends", &self.package.build_depends, true)?;
        let provides =
            parse_constraint_list(dir, "package", "provides", &self.package.provides, false)?;
        let conflicts =
            parse_constraint_list(dir, "package", "conflicts", &self.package.conflicts, false)?;
        let replaces =
            parse_constraint_list(dir, "package", "replaces", &self.package.replaces, false)?;

        // Install: registry hooks only (no free-form shell).
        let (mut install_post, mut install_backup) = (Vec::new(), Vec::new());
        if let Some(install) = self.install.as_ref() {
            for entry in &install.post {
                match entry {
                    HookEntry::Simple(s) => {
                        let s = s.trim();
                        if s.contains(';') || s.contains('$') {
                            return Err(toml_err(
                                dir,
                                format!(
                                    "[install].post entry '{s}' looks like shell; \
                                     use a registry hook name or {{ hook = \"...\", match = \"...\" }}"
                                ),
                            ));
                        }
                        let mut words = s.split_whitespace();
                        let hook = match words.next() {
                            Some(h) => h.to_string(),
                            None => {
                                return Err(toml_err(
                                    dir,
                                    "[install].post entry is empty".into(),
                                ))
                            }
                        };
                        if !HOOK_REGISTRY.contains(&hook.as_str()) {
                            return Err(toml_err(
                                dir,
                                format!(
                                    "[install].post hook '{hook}' is not in the registry \
                                     ({})",
                                    HOOK_REGISTRY.join(", ")
                                ),
                            ));
                        }
                        let arg = words.next();
                        if words.next().is_some() {
                            return Err(toml_err(
                                dir,
                                format!(
                                    "[install].post entry '{s}' has too many arguments \
                                     (at most one path argument)"
                                ),
                            ));
                        }
                        install_post.push(HookSpec {
                            hook,
                            match_filter: arg.map(str::to_string),
                        });
                    }
                    HookEntry::Named {
                        hook,
                        match_filter,
                    } => {
                        let hook = hook.trim();
                        if !HOOK_REGISTRY.contains(&hook) {
                            return Err(toml_err(
                                dir,
                                format!(
                                    "[install].post hook '{hook}' is not in the registry \
                                     ({})",
                                    HOOK_REGISTRY.join(", ")
                                ),
                            ));
                        }
                        install_post.push(HookSpec {
                            hook: hook.to_string(),
                            match_filter: match_filter.clone(),
                        });
                    }
                }
            }
            install_backup = install.backup.clone();
        }
        let install_files = self
            .install
            .as_ref()
            .and_then(|i| i.files.clone())
            .unwrap_or_else(|| "auto".into());

        Ok(Recipe {
            kind: RecipeKind::Toml,
            name,
            epoch,
            version: VersionRel::new(version, release.to_string()),
            dir: dir.to_path_buf(),
            arch: Some(arch),
            desc,
            license,
            homepage,
            depends,
            build_depends,
            provides,
            conflicts,
            replaces,
            sources,
            checksums: Vec::new(),
            meta,
            build_system,
            build_steps,
            meson_args,
            cmake_args,
            configure_args,
            make_target,
            install_post,
            install_files,
            install_backup,
            warnings,
        })
    }
}
