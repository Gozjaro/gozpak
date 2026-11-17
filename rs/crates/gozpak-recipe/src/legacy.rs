//! Legacy (kiss-style) recipe layout — the "3-file" format.
//!
//! A recipe directory contains:
//!
//! | file        | required | content                          |
//! |-------------|----------|----------------------------------|
//! | `version`   | yes      | single line `<version> <release>`|
//! | `build`     | no*      | shell script (DESTDIR = `$1`)    |
//! | `depends`   | no       | one dep per line; trailing ` make` = build-only; `#` comments |
//! | `sources`   | no       | one source per line; optional second column = cache subdir; `git+`; `#` comments |
//! | `meta`      | no       | `key=value` lines                |
//! | `checksums` | no       | one SHA-256 per source line, or `SKIP` |
//!
//! \* The shell version dies on a missing/non-executable `build` at build
//! time (M2); M1 is read-only, so `build` is optional here and merely
//! captured into [`Recipe::build_steps`] when present.
//!
//! This module exists for parity with the shell version and to power the
//! M3 migration tool; the new primary format is `recipe.toml`
//! (see `toml_recipe.rs`).
//!
//! Legacy recipes have no epoch (0), no provides/conflicts/replaces, no
//! homepage/backup — all v1.1 fields default.

use std::collections::BTreeMap;
use std::path::Path;

use gozpak_core::{paths, DepReq, VersionRel};

use crate::{Recipe, RecipeError, RecipeKind, Source, SourceFile};

fn read_optional(dir: &Path, file: &str) -> Result<Option<String>, RecipeError> {
    let p = dir.join(file);
    if !p.is_file() {
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(&p).map_err(RecipeError::Io)?))
}

fn read_required(dir: &Path, file: &str) -> Result<String, RecipeError> {
    match std::fs::read_to_string(dir.join(file)) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(RecipeError::MissingVersion(dir.join(file)))
        }
        Err(e) => Err(RecipeError::Io(e)),
    }
}

/// Parse one legacy `sources` line into a normalized [`Source`].
///
/// The raw line carries `VERSION`-style markers verbatim; substitution
/// happens at use time (`subst::Subst::apply_shell`).
pub fn source_from_line(raw: &str, dest: Option<&str>) -> Option<Source> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // Git source: `git+https://host/repo[#ref]` (legacy may have no ref).
    if let Some(rest) = raw.strip_prefix("git+") {
        let (url, pin) = match rest.split_once(['#', '@']) {
            Some((u, r)) => (u.to_string(), Some(r.to_string())),
            None => (rest.to_string(), None),
        };
        return Some(Source::Git { repo: url, pin });
    }

    // Remote URL (anything with a scheme): a one-file `[source].urls`
    // entry without a checksum.
    if raw.contains("://") {
        return Some(Source::Files {
            files: vec![SourceFile {
                url: raw.to_string(),
                sha256: None,
                dest: dest.map(str::to_string),
                strip_topdir: None,
            }],
        });
    }

    // Local file/directory (relative to the recipe dir, or absolute).
    Some(Source::Local {
        path: std::path::PathBuf::from(raw),
    })
}

/// Load the legacy layout from `dir`.
pub fn load(dir: &Path) -> Result<Recipe, RecipeError> {
    if !dir.is_dir() {
        return Err(RecipeError::NotADirectory(dir.to_path_buf()));
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| RecipeError::NotADirectory(dir.to_path_buf()))?;

    // version (required): single line "<version> <release>".
    let version_text = match read_required(dir, "version") {
        Ok(t) => t,
        Err(e @ RecipeError::MissingVersion(_)) => return Err(e),
        Err(e) => return Err(e),
    };
    let version_line = version_text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let version = VersionRel::parse(version_line).ok_or(RecipeError::InvalidVersion {
        path: paths::recipe_version_file(dir),
        line: version_line.to_string(),
    })?;

    // build (optional in M1; captured verbatim, never executed).
    let build_steps = match read_optional(dir, "build")? {
        Some(script) => vec![script],
        None => Vec::new(),
    };

    // depends (optional): "<name>" or "<name> make"; # comments.
    let mut depends = Vec::new();
    let mut build_depends = Vec::new();
    if let Some(text) = read_optional(dir, "depends")? {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(dep) = DepReq::parse_legacy_line(line) {
                if dep.build_only {
                    build_depends.push(dep);
                } else {
                    depends.push(dep);
                }
            }
        }
    }

    // sources (optional): "raw [dest]"; # comments.
    let mut sources = Vec::new();
    if let Some(text) = read_optional(dir, "sources")? {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            let raw = it.next().unwrap_or_default();
            let dest = it.next();
            if let Some(src) = source_from_line(raw, dest) {
                sources.push(src);
            }
        }
    }

    // meta (optional): key=value lines.
    let mut meta: BTreeMap<String, String> = BTreeMap::new();
    if let Some(text) = read_optional(dir, "meta")? {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                meta.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }

    // checksums (optional): one line per source, or SKIP.
    let checksums = read_optional(dir, "checksums")?
        .map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default();

    let desc = meta.get("description").cloned();
    let license = meta.get("license").cloned();

    Ok(Recipe {
        kind: RecipeKind::Legacy,
        name,
        epoch: 0,
        version,
        dir: dir.to_path_buf(),
        arch: None,
        desc,
        license,
        homepage: None,
        depends,
        build_depends,
        provides: Vec::new(),
        conflicts: Vec::new(),
        replaces: Vec::new(),
        sources,
        checksums,
        meta,
        build_system: None,
        build_steps,
        meson_args: Vec::new(),
        cmake_args: Vec::new(),
        configure_args: Vec::new(),
        make_target: None,
        install_post: Vec::new(),
        install_files: String::from("auto"),
        install_backup: Vec::new(),
        warnings: Vec::new(),
    })
}
