//! The `manifest.toml` carried inside every `.gpkg`.
//!
//! Per recipe format v1.2, the manifest is the serialized `[package]`
//! and `[install]` tables plus the pack-time metadata:
//!
//! ```toml
//! [package]
//! name = "hello-raw"
//! version = "1.0.0"
//! release = "1"
//! epoch = 0
//! arch = "x86_64"
//! ...
//!
//! [install]
//! post = []
//! files = "auto"
//! backup = []
//!
//! [manifest]
//! arch = "x86_64"
//! rootfs_sha256 = "<sha256(rootfs): deterministic tree hash>"
//! files = [ "bin/hello-raw", "etc/hello-raw.conf", ... ]
//! repo = "main"        # repo-build only
//! built_by = "local"   # repo-build only: recipe git hash, else "local"
//! ```
//!
//! Dependency lists are plain constraint strings (`zlib>=1.3`) — the
//! solver's input, joined per the index format rules.

use std::path::Path;

use gozpak_core::DepReq;
use gozpak_recipe::{HookSpec, Recipe};
use serde::{Deserialize, Serialize};

/// The full `manifest.toml` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(rename = "package")]
    pub package: PackageTable,
    #[serde(rename = "install")]
    pub install: InstallTable,
    #[serde(rename = "manifest")]
    pub manifest: ManifestTable,
}

/// The serialized `[package]` table (v1.2 solver input).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageTable {
    pub name: String,
    pub version: String,
    pub release: String,
    /// Always written (the filename omits the `epoch:` token when it is
    /// 0; the manifest does not — the manifest is the source of truth).
    pub epoch: u64,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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

/// The serialized `[install]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstallTable {
    /// Registry hook names and/or `{ hook = "...", match = "..." }`
    /// tables, exactly as declared in the recipe.
    #[serde(default)]
    pub post: Vec<toml::Value>,
    #[serde(default)]
    pub files: String,
    #[serde(default)]
    pub backup: Vec<String>,
}

/// Pack-time metadata (the `[manifest]` table).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestTable {
    /// The package's target architecture (redundant with `[package]`;
    /// the spec lists it as its own manifest field).
    pub arch: String,
    /// `sha256(rootfs)`: the deterministic tree hash of the rootfs
    /// tree (lowercase hex). Re-computed by the M2b verifier from the
    /// extracted tree — see [`crate::pack::rootfs_sha256`].
    pub rootfs_sha256: String,
    /// Every regular file in the rootfs tree, relative paths, sorted.
    #[serde(default)]
    pub files: Vec<String>,
    /// `repo-build` only: which repo this package belongs to
    /// (`[meta].repo`, default `"main"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// `repo-build` only: the recipe's git commit hash when the recipe
    /// dir lives in a git repository, else the string `"local"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_by: Option<String>,
}

/// Build the manifest for a staged tree. `repo_meta = false` (the
/// plain `build` command) omits `repo`/`built_by` entirely.
pub fn build_manifest(
    recipe: &Recipe,
    files: &[String],
    rootfs_sha256: &str,
    repo_meta: bool,
) -> Manifest {
    let dep = |d: &DepReq| {
        // The raw constraint string, minus the legacy " make" marker
        // (a TOML build_depends entry's raw has no marker; legacy does).
        d.raw
            .strip_suffix(" make")
            .unwrap_or(&d.raw)
            .to_string()
    };

    let post: Vec<toml::Value> = recipe
        .install_post
        .iter()
        .map(hook_value)
        .collect();

    let arch = recipe
        .arch
        .clone()
        .unwrap_or_else(|| "x86_64".into());

    let (repo, built_by) = if repo_meta {
        (
            Some(recipe.meta.get("repo").cloned().unwrap_or_else(|| "main".into())),
            Some(recipe_git_hash(&recipe.dir).unwrap_or_else(|| "local".into())),
        )
    } else {
        (None, None)
    };

    Manifest {
        package: PackageTable {
            name: recipe.name.clone(),
            version: recipe.version.version.clone(),
            release: recipe.version.release.clone(),
            epoch: recipe.epoch,
            arch: arch.clone(),
            desc: recipe.desc.clone(),
            license: recipe.license.clone(),
            url: recipe.homepage.clone(),
            depends: recipe.depends.iter().map(dep).collect(),
            build_depends: recipe.build_depends.iter().map(dep).collect(),
            provides: recipe.provides.iter().map(dep).collect(),
            conflicts: recipe.conflicts.iter().map(dep).collect(),
            replaces: recipe.replaces.iter().map(dep).collect(),
        },
        install: InstallTable {
            post,
            files: recipe.install_files.clone(),
            backup: recipe.install_backup.clone(),
        },
        manifest: ManifestTable {
            arch,
            rootfs_sha256: rootfs_sha256.to_string(),
            files: files.to_vec(),
            repo,
            built_by,
        },
    }
}

/// One `[install].post` entry as the recipe wrote it: a plain registry
/// hook name, or `{ hook, match }` when a path filter is attached.
fn hook_value(h: &HookSpec) -> toml::Value {
    match &h.match_filter {
        None => toml::Value::String(h.hook.clone()),
        Some(m) => toml::Value::Table({
            let mut t = toml::map::Map::new();
            t.insert("hook".into(), toml::Value::String(h.hook.clone()));
            t.insert("match".into(), toml::Value::String(m.clone()));
            t
        }),
    }
}

/// The recipe's git commit hash when its directory is inside a git
/// repository (M2a: `built-by` provenance); `None` → "local".
fn recipe_git_hash(dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let hash = s.trim();
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gozpak_core::DepReq;

    fn recipe() -> Recipe {
        // A minimal recipe built in memory (no disk).
        use gozpak_core::VersionRel;
        use gozpak_recipe::{HookSpec, Recipe, RecipeKind};
        Recipe {
            kind: RecipeKind::Toml,
            name: "demo".into(),
            epoch: 1,
            version: VersionRel::new("2.0", "3"),
            dir: std::path::PathBuf::from("/tmp/demo"),
            arch: Some("x86_64".into()),
            desc: Some("demo package".into()),
            license: Some("MIT".into()),
            homepage: Some("https://example.com".into()),
            depends: vec![DepReq::parse_constraint("zlib>=1.3", false).unwrap()],
            build_depends: vec![DepReq::parse_constraint("cmake>=3.16", true).unwrap()],
            provides: vec![],
            conflicts: vec![],
            replaces: vec![],
            sources: vec![],
            checksums: vec![],
            meta: std::collections::BTreeMap::from([("repo".to_string(), "main".to_string())]),
            build_system: Some("raw".into()),
            build_steps: vec!["echo hi".into()],
            meson_args: vec![],
            cmake_args: vec![],
            configure_args: vec![],
            make_target: None,
            install_post: vec![
                HookSpec {
                    hook: "ldconfig".into(),
                    match_filter: None,
                },
                HookSpec {
                    hook: "update-mime-database".into(),
                    match_filter: Some("/usr/share/mime/**".into()),
                },
            ],
            install_files: "auto".into(),
            install_backup: vec!["etc/sysctl.conf".into()],
            warnings: vec![],
        }
    }

    #[test]
    fn manifest_round_trips_through_toml() {
        let m = build_manifest(&recipe(), &["bin/demo".into()], "ab12", true);
        let text = toml::to_string(&m).unwrap();
        let parsed: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(parsed, m);
        assert_eq!(parsed.package.epoch, 1);
        assert_eq!(parsed.package.depends, vec!["zlib>=1.3"]);
        assert_eq!(parsed.package.build_depends, vec!["cmake>=3.16"]);
        assert_eq!(parsed.manifest.repo.as_deref(), Some("main"));
        assert!(parsed.manifest.built_by.is_some());
        assert_eq!(parsed.install.files, "auto");
        assert_eq!(parsed.install.backup, vec!["etc/sysctl.conf"]);
        // Mixed post entries: string + {hook, match} table.
        assert_eq!(parsed.install.post.len(), 2);
        assert!(matches!(
            parsed.install.post[0],
            toml::Value::String(ref s) if s == "ldconfig"
        ));
        assert!(parsed.install.post[1].is_table());
    }

    #[test]
    fn no_repo_meta_omits_repo_and_built_by() {
        let m = build_manifest(&recipe(), &[], "ab12", false);
        let text = toml::to_string(&m).unwrap();
        assert!(!text.contains("repo ="));
        assert!(!text.contains("built_by"));
        let parsed: Manifest = toml::from_str(&text).unwrap();
        assert!(parsed.manifest.repo.is_none());
        assert!(parsed.manifest.built_by.is_none());
    }
}
