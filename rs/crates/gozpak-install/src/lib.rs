//! `gozpak-install` (M2b): the install side of gozpak.
//!
//! The flow one `gozpak get`/`install` runs for a set of packages
//! (one *transaction*):
//!
//! 1. [`unpack_gpkg`] — decode the `.gpkg` (tar.zst) into a staging
//!    dir: `manifest.toml` + `rootfs/`.
//! 2. [`verify_staged`] — recompute the deterministic tree hash of
//!    `rootfs/` (the exact [`gozpak_build::pack::rootfs_sha256`]
//!    algorithm, not a re-implementation) and compare it with
//!    `[manifest].rootfs_sha256`; check the file set.
//! 3. [`check_deps`] — every `[package].depends` of the wanted
//!    packages must be satisfied by the installed DB, the rest of the
//!    transaction, or a `provides` entry; `[package].conflicts` must
//!    not hit the installed DB.
//! 4. [`install_staged`] — copy the `rootfs/` tree into the target
//!    root, preserving symlinks and permissions; config files listed
//!    in `[install].backup` that exist with different content are
//!    saved as `<file>.pkgnew` first.
//! 5. [`run_hooks`] — the registry hooks from every installed
//!    package's `[install].post`, **deduplicated per transaction**
//!    (the same hook+match declared by two packages runs once).
//! 6. [`record_install`] — update the installed DB
//!    (`<db>/<pkg>/version` + `manifest`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gozpak_build::manifest::Manifest;
use gozpak_build::pack::{list_files, rootfs_sha256};
use gozpak_core::{InstalledDb, VersionRel};
use gozpak_recipe::glob::glob_match;
use gozpak_repo::BinaryEntry;

/// One install-side error.
#[derive(Debug)]
pub enum InstallError {
    /// The `.gpkg` container or its manifest is unreadable.
    Unpack { file: String, message: String },
    /// The extracted tree does not match the manifest (the pack →
    /// extract invariant is broken).
    Verification { pkg: String, detail: String },
    /// A dependency is unsatisfiable.
    MissingDep { pkg: String, dep: String },
    /// A conflict with the installed system.
    Conflict { pkg: String, against: String },
    /// IO during the file copy.
    Io(std::io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Unpack { file, message } => {
                write!(f, "cannot unpack {file}: {message}")
            }
            InstallError::Verification { pkg, detail } => {
                write!(f, "verification failed for {pkg}: {detail}")
            }
            InstallError::MissingDep { pkg, dep } => {
                write!(f, "{pkg}: dependency '{dep}' is not satisfied")
            }
            InstallError::Conflict { pkg, against } => {
                write!(f, "{pkg} conflicts with installed '{against}'")
            }
            InstallError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        InstallError::Io(e)
    }
}

/// A staged (unpacked + verified) package ready for install.
#[derive(Debug)]
pub struct Staged {
    /// The staging dir (`manifest.toml` + `rootfs/`).
    pub dir: PathBuf,
    /// The parsed manifest.
    pub manifest: Manifest,
}

impl Staged {
    /// The package name (from the manifest — never the filename).
    pub fn name(&self) -> &str {
        &self.manifest.package.name
    }
}

/// Unpack a `.gpkg` (tar.zst) into `tmp/<name>-staging`:
/// zstd-decode, then the system `tar` (the same two tools the M2a
/// packer used — the container is plain tar.zst, no private format).
pub fn unpack_gpkg(gpkg: &Path, tmp: &Path) -> Result<Staged, InstallError> {
    let name = gpkg
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("gpkg");
    let dir = tmp.join(format!("{name}-staging"));
    let err = |m: String| InstallError::Unpack {
        file: name.to_string(),
        message: m,
    };
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(InstallError::Io)?;

    let raw = std::fs::read(gpkg).map_err(|e| err(e.to_string()))?;
    let decoded =
        zstd::decode_all(&raw[..]).map_err(|e| err(format!("zstd: {e}")))?;
    let tarball = tmp.join(format!("{name}.tar"));
    std::fs::write(&tarball, &decoded).map_err(InstallError::Io)?;
    let status = std::process::Command::new("tar")
        .args(["-xf", tarball.to_str().unwrap(), "-C", dir.to_str().unwrap()])
        .status()
        .map_err(|e| err(e.to_string()))?;
    if !status.success() {
        return Err(err(format!("tar exit: {status}")));
    }
    let _ = std::fs::remove_file(&tarball);

    let manifest_text = std::fs::read_to_string(dir.join("manifest.toml"))
        .map_err(|e| err(format!("no manifest.toml: {e}")))?;
    let manifest: Manifest =
        toml::from_str(&manifest_text).map_err(|e| err(format!("manifest.toml: {e}")))?;
    Ok(Staged { dir, manifest })
}

/// Verify the staged tree against its manifest: the deterministic tree
/// hash of `rootfs/` must equal `[manifest].rootfs_sha256`, and the
/// file set must equal `[manifest].files`.
pub fn verify_staged(staged: &Staged) -> Result<(), InstallError> {
    let rootfs = staged.dir.join("rootfs");
    let got = rootfs_sha256(&rootfs).map_err(|e| InstallError::Verification {
        pkg: staged.name().to_string(),
        detail: e.to_string(),
    })?;
    let want = &staged.manifest.manifest.rootfs_sha256;
    if got != *want {
        return Err(InstallError::Verification {
            pkg: staged.name().to_string(),
            detail: format!("rootfs_sha256 mismatch: manifest {want}, got {got}"),
        });
    }
    let files = list_files(&rootfs).map_err(|e| InstallError::Verification {
        pkg: staged.name().to_string(),
        detail: e.to_string(),
    })?;
    let got_set: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let want_set: BTreeSet<&str> = staged
        .manifest
        .manifest
        .files
        .iter()
        .map(String::as_str)
        .collect();
    if got_set != want_set {
        return Err(InstallError::Verification {
            pkg: staged.name().to_string(),
            detail: format!(
                "file set mismatch: manifest has {}, tree has {}",
                want_set.len(),
                got_set.len()
            ),
        });
    }
    Ok(())
}

/// Copy the staged `rootfs/` tree into `root`, preserving symlinks
/// and permissions.
///
/// For every file in `[install].backup` that already exists under
/// `root` with content different from the package's copy, the existing
/// file is first saved as `<file>.pkgnew` (user config is never lost;
/// an existing `.pkgnew` is overwritten — it always holds the last
/// replaced copy). Returns the list of created `.pkgnew` paths.
pub fn install_staged(root: &Path, staged: &Staged) -> Result<Vec<PathBuf>, InstallError> {
    let rootfs = staged.dir.join("rootfs");
    let backup: BTreeSet<&str> =
        staged.manifest.install.backup.iter().map(String::as_str).collect();

    // 1. Snapshot the existing content of every backup file BEFORE the
    //    tree copy overwrites it.
    let mut saved: Vec<(String, Vec<u8>)> = Vec::new();
    for rel in staged.manifest.manifest.files.iter() {
        if !backup.contains(rel.as_str()) {
            continue;
        }
        let target = root.join(rel);
        if let Ok(old) = std::fs::read(&target) {
            let new = std::fs::read(rootfs.join(rel));
            if new.is_ok() && new.unwrap() != old {
                saved.push((rel.clone(), old));
            }
        }
    }

    // 2. Copy the whole tree (the M2a packer's copy semantics, in
    //    reverse): directories, files, symlinks.
    copy_tree_into(&rootfs, root)?;

    // 3. Restore the .pkgnew files (only if the copy succeeded).
    let mut pkgnew = Vec::new();
    for (rel, old) in saved {
        let p = root.join(format!("{rel}.pkgnew"));
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, old)?;
        pkgnew.push(p);
    }
    Ok(pkgnew)
}

fn copy_tree_into(src: &Path, dst: &Path) -> Result<(), InstallError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let p = entry.path();
        let q = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            let target = std::fs::read_link(&p)?;
            let _ = std::fs::remove_file(&q);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &q)?;
            #[cfg(not(unix))]
            {
                if target.is_dir() {
                    copy_tree_into(&target, &q)?;
                } else {
                    std::fs::copy(&p, &q)?;
                }
            }
        } else if ft.is_dir() {
            copy_tree_into(&p, &q)?;
        } else {
            // Preserve the packaged permission bits (a tar extract has
            // already restored them; copy keeps them on most platforms
            // — set them explicitly from the source metadata).
            std::fs::copy(&p, &q)?;
            #[cfg(unix)]
            {
                let perms = std::fs::metadata(&p)?.permissions();
                std::fs::set_permissions(&q, perms)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Post-install hooks (registry hooks, deduplicated per transaction).
// ---------------------------------------------------------------------------

/// One hook declaration: a registry hook name plus an optional
/// file-glob `match` filter (the `[install].post` entry forms, see
/// `gozpak-recipe`'s `HookEntry`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hook {
    pub name: String,
    /// `{ hook = "...", match = "..." }`'s glob; runs only when at
    /// least one file of the declaring package matches.
    pub match_filter: Option<String>,
}

/// Parse `[install].post` (mixed string / table entries) into hooks.
pub fn parse_hooks(post: &[toml::Value]) -> Vec<Hook> {
    post.iter()
        .filter_map(|v| match v {
            toml::Value::String(s) => Some(Hook {
                name: s.clone(),
                match_filter: None,
            }),
            toml::Value::Table(t) => {
                let name = t
                    .get("hook")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)?;
                let mf = t
                    .get("match")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                Some(Hook {
                    name,
                    match_filter: mf,
                })
            }
            _ => None,
        })
        .collect()
}

/// The hooks a transaction must run: the union of every package's
/// `[install].post`, deduplicated by `(name, match)` — the same hook
/// declared by two packages in one `gozpak install a b` runs once.
///
/// Each `(Manifest, &files)` pair supplies the declaring package's file
/// list for `match` filtering; hooks whose filter matches no file are
/// dropped.
///
/// The result is sorted by `(name, match_filter)`: hook execution order
/// is deterministic and independent of the package order on the command
/// line (`install a b` and `install b a` plan the same transaction).
pub fn transaction_hooks(
    packages: &[(Manifest, &Vec<String>)],
) -> Vec<Hook> {
    let mut seen: BTreeSet<(String, Option<String>)> = BTreeSet::new();
    for (m, files) in packages {
        for h in parse_hooks(&m.install.post) {
            let keep = match &h.match_filter {
                Some(pat) => files.iter().any(|f| {
                    let base = f.rsplit('/').next().unwrap_or(f);
                    glob_match(base, pat)
                }),
                None => true,
            };
            if keep {
                let _ = seen.insert((h.name.clone(), h.match_filter.clone()));
            }
        }
    }
    let mut out: Vec<Hook> = seen
        .into_iter()
        .map(|(name, match_filter)| Hook { name, match_filter })
        .collect();
    out.sort();
    out
}

/// The registry hook name → the system command it drives.
pub fn hook_command(name: &str) -> Option<&'static str> {
    Some(match name {
        "ldconfig" => "ldconfig",
        "update-mime-database" => "update-mime-database",
        "gdk-pixbuf-query-loaders" => "gdk-pixbuf-query-loaders",
        "desktop-database" => "desktop-file-update",
        "fontconfig" => "fc-cache",
        "shared-mime-info" => "update-mime-database",
        _ => return None,
    })
}

/// The result of running one hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Ran and exited 0.
    Ran { command: String },
    /// Unknown registry name or missing binary — skipped (a hook
    /// missing on the target system must not break the install).
    Skipped { name: String, reason: String },
    /// Ran but exited non-zero — recorded, non-fatal.
    Failed { command: String, status: String },
}

/// Run a transaction's hooks. `root` is the target root: when it is
/// not `/`, commands run inside it via `chroot` (chrooted install);
/// otherwise directly.
pub fn run_hooks(root: &Path, hooks: &[Hook]) -> Vec<HookOutcome> {
    let chrooted = root != Path::new("/");
    hooks
        .iter()
        .map(|h| {
            let Some(cmd) = hook_command(&h.name) else {
                return HookOutcome::Skipped {
                    name: h.name.clone(),
                    reason: "unknown registry hook".into(),
                };
            };
            let mut argv = vec![cmd];
            if chrooted {
                argv = vec!["chroot", root.to_str().unwrap(), cmd];
            }
            let status = match std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .status()
            {
                Ok(s) => s,
                Err(_) => {
                    return HookOutcome::Skipped {
                        name: h.name.clone(),
                        reason: format!("'{cmd}' not found / not executable"),
                    };
                }
            };
            if status.success() {
                HookOutcome::Ran {
                    command: argv.join(" "),
                }
            } else {
                HookOutcome::Failed {
                    command: argv.join(" "),
                    status: status.to_string(),
                }
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Dependency checks (the index carries the solver's input).
// ---------------------------------------------------------------------------

/// The installed side a check runs against: name → version.
pub type InstalledList = Vec<(String, VersionRel)>;

/// Check that every `depends` of `wanted` is satisfied by the
/// installed system, the rest of the transaction, or a `provides`
/// entry, and that no `conflicts` hits the installed system.
///
/// Version constraints use the M1 solver input (`DepReq` + `vercmp`).
pub fn check_deps(
    wanted: &[BinaryEntry],
    installed: &InstalledList,
) -> Result<(), InstallError> {
    // What this transaction will provide: every wanted package by
    // name, plus its `provides`.
    let will_provide: BTreeSet<String> = wanted
        .iter()
        .flat_map(|w| std::iter::once(w.name.clone()).chain(w.provides.iter().cloned()))
        .collect();

    for w in wanted {
        for dep in &w.depends {
            let req = gozpak_core::DepReq::parse_constraint(dep, false)
                .map_err(|e| InstallError::MissingDep {
                    pkg: w.name.clone(),
                    dep: format!("{dep} ({e})"),
                })?;
            if will_provide.contains(&req.name) {
                continue;
            }
            if installed
                .iter()
                .any(|(n, v)| {
                    n == &req.name
                        && req.satisfied_by(&gozpak_core::VersionKey::new(
                            0,
                            v.version.clone(),
                            v.release.clone(),
                        ))
                })
            {
                continue;
            }
            return Err(InstallError::MissingDep {
                pkg: w.name.clone(),
                dep: req.display(),
            });
        }
        for conf in &w.conflicts {
            let name = conf.split(|c| c == '<' || c == '>' || c == '=').next().unwrap_or(conf);
            if installed.iter().any(|(n, _)| n == name) {
                return Err(InstallError::Conflict {
                    pkg: w.name.clone(),
                    against: name.to_string(),
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Installed-DB writes (the M2 side of the M1 "manifest: M2" note).
// ---------------------------------------------------------------------------

/// Record one installed package: `<db>/<pkg>/version`
/// (`<version> <release>`) plus `<db>/<pkg>/manifest` (the file list,
/// one per line — what the M1 `InstalledDb` doc reserved for M2).
pub fn record_install(
    db: &InstalledDb,
    name: &str,
    version: &str,
    release: &str,
    files: &[String],
) -> Result<PathBuf, InstallError> {
    let dir = db.pkg_dir(name);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("version"),
        format!("{version} {release}\n"),
    )?;
    let manifest = files
        .iter()
        .map(|f| format!("{f}\n"))
        .collect::<String>();
    std::fs::write(dir.join("manifest"), manifest)?;
    Ok(dir)
}

/// Remove one package's DB record (`gozpak remove` is M3; this is the
/// primitive it will call).
pub fn forget_install(db: &InstalledDb, name: &str) -> Result<(), InstallError> {
    let dir = db.pkg_dir(name);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Convenience: parse the installed DB into [`InstalledList`] for
/// [`check_deps`].
pub fn installed_list(db: &InstalledDb) -> InstalledList {
    db.list()
        .into_iter()
        .map(|p| (p.name, p.version))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gozpak_build::pack;
    use gozpak_core::{paths, VersionRel};
    use gozpak_recipe::HookSpec;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Pack a staged tree directly (no recipe round-trip) so the install
    /// tests control `[install].post`/`backup` freely — the container is
    /// the real M2a pipeline output.
    fn pack_tree(
        t: &Path,
        name: &str,
        files: &[(&str, &str)],
        post: &[HookSpec],
        backup: &[&str],
    ) -> PathBuf {
        let dir = t.join(name);
        let dest = t.join(format!("{name}-dest"));
        let _ = std::fs::remove_dir_all(&dest);
        for (rel, content) in files {
            let p = dest.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
            // Simulate a real LFS `make install`: program entries carry
            // the exec bit (the source tree would have it).
            #[cfg(unix)]
            if rel.starts_with("bin/") {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&p).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&p, perms).unwrap();
            }
        }
        std::fs::create_dir_all(&dir).unwrap();
        let recipe = gozpak_recipe::Recipe {
            kind: gozpak_recipe::RecipeKind::Toml,
            name: name.into(),
            epoch: 0,
            version: VersionRel::new("1.0", "1"),
            dir: dir.clone(),
            arch: Some("x86_64".into()),
            desc: None,
            license: None,
            homepage: None,
            depends: vec![],
            build_depends: vec![],
            provides: vec![],
            conflicts: vec![],
            replaces: vec![],
            sources: vec![],
            checksums: vec![],
            meta: std::collections::BTreeMap::new(),
            build_system: None,
            build_steps: vec![],
            meson_args: vec![],
            cmake_args: vec![],
            configure_args: vec![],
            make_target: None,
            install_post: post.to_vec(),
            install_files: "auto".into(),
            install_backup: backup.iter().map(|s| s.to_string()).collect(),
            warnings: vec![],
        };
        let out = dir.join("out");
        pack::pack(&recipe, &dest, &dir, &out, false).unwrap().gpkg
    }

    fn temp(name: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("gozpak-install-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        t
    }

    #[test]
    fn unpack_verify_install_roundtrip() {
        let t = temp("rt");
        let gpkg = pack_tree(
            &t,
            "demo",
            &[
                ("bin/demo", "#!/bin/sh\necho demo\n"),
                ("etc/demo.conf", "mode=default\n"),
            ],
            &[],
            &["etc/demo.conf"],
        );

        let staged = unpack_gpkg(&gpkg, &t).unwrap();
        assert_eq!(staged.name(), "demo");
        verify_staged(&staged).unwrap();

        let root = t.join("root");
        std::fs::create_dir_all(&root).unwrap();
        // Pre-existing user-modified config → must be saved as .pkgnew.
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::write(root.join("etc/demo.conf"), "mode=user-modified\n").unwrap();

        let pkgnew = install_staged(&root, &staged).unwrap();
        assert_eq!(pkgnew, vec![root.join("etc/demo.conf.pkgnew")]);
        assert_eq!(
            std::fs::read_to_string(root.join("etc/demo.conf")).unwrap(),
            "mode=default\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("etc/demo.conf.pkgnew")).unwrap(),
            "mode=user-modified\n"
        );
        // The executable bit survives the tree copy.
        #[cfg(unix)]
        {
            let p = std::fs::metadata(root.join("bin/demo")).unwrap().permissions();
            assert!(p.mode() & 0o111 != 0, "bin/demo lost its exec bit");
        }

        // DB record.
        let db = InstalledDb::new(paths::installed_db_root(&root));
        record_install(&db, "demo", "1.0", "1",
            &vec!["bin/demo".to_string(), "etc/demo.conf".to_string()])
            .unwrap();
        assert_eq!(db.version_of("demo").unwrap().display(), "1.0-1");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn verification_catches_tamper() {
        let t = temp("tamper");
        let gpkg = pack_tree(&t, "t", &[("bin/t", "x")], &[], &[]);
        let staged = unpack_gpkg(&gpkg, &t).unwrap();
        // Tamper with the extracted tree → tree hash no longer matches.
        std::fs::write(staged.dir.join("rootfs/bin/t"), "tampered").unwrap();
        let e = verify_staged(&staged).unwrap_err();
        assert!(matches!(e, InstallError::Verification { .. }));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn unmodified_config_gets_no_pkgnew() {
        let t = temp("same");
        let gpkg = pack_tree(
            &t,
            "d",
            &[("etc/d.conf", "same\n")],
            &[],
            &["etc/d.conf"],
        );
        let staged = unpack_gpkg(&gpkg, &t).unwrap();
        let root = t.join("root");
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::write(root.join("etc/d.conf"), "same\n").unwrap();
        assert!(install_staged(&root, &staged).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&t);
    }

    fn manifest_with_hooks(name: &str, post: Vec<toml::Value>) -> Manifest {
        let mut m: Manifest = toml::from_str(
            r#"
[package]
name = "a"
version = "1.0"
release = "1"
epoch = 0
arch = "x86_64"

[install]
files = "auto"

[manifest]
arch = "x86_64"
rootfs_sha256 = ""
"#,
        )
        .unwrap();
        m.package.name = name.into();
        m.install.post = post;
        m
    }

    #[test]
    fn hooks_dedupe_per_transaction() {
        let m1 = manifest_with_hooks(
            "a",
            vec![
                toml::Value::String("ldconfig".into()),
                toml::Value::String("fontconfig".into()),
            ],
        );
        let m2 = manifest_with_hooks("b", vec![toml::Value::String("ldconfig".into())]);
        let a_files: Vec<String> = vec!["usr/lib/liba.so".into()];
        let b_files: Vec<String> = vec!["usr/lib/libb.so".into()];
        let hooks = transaction_hooks(&[(m1.clone(), &a_files), (m2.clone(), &b_files)]);
        // ldconfig declared twice → planned once; fontconfig once.
        let names: Vec<_> = hooks.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["fontconfig", "ldconfig"]);
    }

    #[test]
    fn match_filter_drops_unmatched_hooks() {
        let mut m = manifest_with_hooks(
            "a",
            {
                let mut t = toml::Table::new();
                t.insert("hook".into(), toml::Value::String("update-mime-database".into()));
                t.insert("match".into(), toml::Value::String("*.desktop".into()));
                vec![toml::Value::Table(t)]
            },
        );
        let files: Vec<String> = vec!["usr/lib/liba.so".into()];
        assert!(transaction_hooks(&[(m.clone(), &files)]).is_empty());
        let files2: Vec<String> = vec!["usr/share/applications/a.desktop".into()];
        let planned = transaction_hooks(&[(m.clone(), &files2)]);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].match_filter.as_deref(), Some("*.desktop"));
        m.install.post = vec![];
    }

    #[test]
    fn deps_check() {
        use gozpak_repo::BinaryEntry;
        fn entry(name: &str, depends: &[&str], conflicts: &[&str]) -> BinaryEntry {
            BinaryEntry {
                name: name.into(),
                version: "1.0".into(),
                release: 1,
                epoch: 0,
                arch: "x86_64".into(),
                sha256: "0".into(),
                size: 0,
                url: format!("{name}.gpkg"),
                depends: depends.iter().map(|s| s.to_string()).collect(),
                provides: vec![],
                conflicts: conflicts.iter().map(|s| s.to_string()).collect(),
                replaces: vec![],
            }
        }
        let installed: InstalledList = vec![
            ("zlib".into(), VersionRel::new("1.3.1", "1")),
            ("openssl".into(), VersionRel::new("3.5", "2")),
        ];
        // Satisfied by the installed system.
        check_deps(&[entry("a", &["zlib>=1.3"], &[])], &installed).unwrap();
        // Unsatisfied → MissingDep.
        let e = check_deps(&[entry("a", &["curl>=8"], &[])], &installed).unwrap_err();
        assert!(matches!(e, InstallError::MissingDep { .. }));
        // A transaction package satisfies its sibling's dep.
        let pair = vec![entry("b", &["a>=1.0"], &[]), entry("a", &[], &[])];
        check_deps(&pair, &installed).unwrap();
        // A conflict with the installed system.
        let e = check_deps(&[entry("a", &[], &["openssl"])], &installed).unwrap_err();
        assert!(matches!(e, InstallError::Conflict { .. }));
    }

    #[test]
    fn record_and_forget_install() {
        let t = temp("db");
        let dbroot = t.join("installed");
        let db = InstalledDb::new(&dbroot);
        record_install(&db, "zlib", "1.3.1", "1", &["usr/lib/libz.so".to_string()]).unwrap();
        assert!(db.is_installed("zlib"));
        forget_install(&db, "zlib").unwrap();
        assert!(!db.is_installed("zlib"));
        let _ = std::fs::remove_dir_all(&t);
    }
}
