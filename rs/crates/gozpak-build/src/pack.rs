//! The packaging stage: DESTDIR tree → `manifest.toml` + `rootfs/` →
//! `.gpkg` (tar.zst).
//!
//! Layout of a `.gpkg` (recipe format v1.2):
//!
//! ```text
//! <name>-<epoch?>:<version>-<release>.gpkg   # tar.zst
//! ├── manifest.toml
//! └── rootfs/...                              # the DESTDIR tree, relative paths
//! ```
//!
//! The container is produced by the system `tar` (the archive itself)
//! compressed with the `zstd` crate (the compression). The filename
//! always comes from [`gozpak_repo::gpkg_filename`] — the builder,
//! never a parser. The manifest records `sha256(rootfs)`: a
//! deterministic tree hash ([`rootfs_sha256`]) that the M2b verifier
//! re-computes from the *extracted* rootfs/ tree, so the invariant
//! survives the pack → extract round trip (a hash of a transient
//! tarball would not).

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::collections::HashMap;

use gozpak_core::sha256;
use gozpak_recipe::Recipe;
use gozpak_repo::gpkg_filename;

use crate::manifest::build_manifest;
use crate::proc;
use crate::BuildError;

/// The packed result.
#[derive(Debug, Clone)]
pub struct Packed {
    /// The `.gpkg` file (tar.zst).
    pub gpkg: PathBuf,
    /// sha256 (lowercase hex) of the `.gpkg` file itself.
    pub gpkg_sha256: String,
    /// The rootfs file list (relative paths, sorted) — what the
    /// manifest and the repo index will carry.
    pub files: Vec<String>,
}

/// Pack the staged tree: write `manifest.toml`, tar `rootfs/`,
/// zstd-compress to `<out_dir>/<gpkg filename>`.
pub fn pack(
    recipe: &Recipe,
    dest: &Path,
    tmp: &Path,
    out_dir: &Path,
    repo_meta: bool,
) -> Result<Packed, BuildError> {
    let pkg = recipe.name.as_str();
    let files = list_files(dest)?;
    // 1. sha256(rootfs): the deterministic tree hash recorded in the
    //    manifest; the M2b verifier recomputes it from the extracted
    //    rootfs/ tree.
    let rootfs_sha256 = rootfs_sha256(dest)?;

    // 2. manifest.toml + rootfs/ staging tree.
    let manifest = build_manifest(recipe, &files, &rootfs_sha256, repo_meta);
    let stage = tmp.join("gpkg-stage");
    std::fs::create_dir_all(&stage)?;
    std::fs::write(
        stage.join("manifest.toml"),
        toml::to_string(&manifest).map_err(|e| BuildError::Io(std::io::Error::other(e)))?,
    )?;
    copy_tree(dest, &stage.join("rootfs"))?;

    // 3. The container: tar the staging tree, zstd-compress it.
    let gpkg_tar = tmp.join("gpkg.tar");
    proc::run(
        pkg,
        tmp,
        "tar",
        &["-C", stage.to_str().unwrap(), "-cf", gpkg_tar.to_str().unwrap(), "."],
        &[],
    )?;
    let raw = std::fs::read(&gpkg_tar)?;

    std::fs::create_dir_all(out_dir)?;
    let filename = gpkg_filename(
        pkg,
        recipe.epoch,
        &recipe.version.version,
        &recipe.version.release,
    );
    let gpkg_path = out_dir.join(&filename);
    // zstd 0.13: encode_all(source: Read, level) -> compressed Vec<u8>.
    let compressed =
        zstd::encode_all(&raw[..], zstd::DEFAULT_COMPRESSION_LEVEL).map_err(|e| {
            BuildError::Io(std::io::Error::other(format!("zstd compression: {e}")))
        })?;
    std::fs::write(&gpkg_path, &compressed)?;
    let gpkg_sha256 = sha256::hash_file(&gpkg_path).map_err(BuildError::Io)?;

    Ok(Packed {
        gpkg: gpkg_path,
        gpkg_sha256,
        files,
    })
}

/// Every regular file in the tree (symlinks count as files;
/// directories are not listed), as relative forward-slash paths,
/// sorted.
pub fn list_files(root: &Path) -> Result<Vec<String>, BuildError> {
    let mut out: Vec<String> = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), BuildError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        let rel = p
            .strip_prefix(root)
            .map(|r| r.display().to_string())
            .ok()
            .ok_or_else(|| BuildError::Io(std::io::Error::other("path outside root")))?;
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            out.push(rel);
        } else if ft.is_dir() {
            walk(root, &p, out)?;
        } else if ft.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

/// Deterministic tree hash for `sha256(rootfs)` — the value the
/// manifest records and the M2b verifier re-computes from the
/// *extracted* rootfs/ tree.
///
/// Canonical form (spec-stable, independent of readdir order, mtime,
/// owner and platform): for every regular file and symlink in the tree
/// (exactly the set [`list_files`] returns), sorted by relative path,
/// emit one record
///
/// ```text
/// <relpath>\0<type>\0<content>\0
/// ```
///
/// where `type` is `f` (content = lowercase-hex sha256 of the file
/// bytes) or `s` (content = the symlink target string). The digest is
/// SHA-256 over the concatenation of all records (an empty tree is
/// SHA-256 of the empty string).
pub fn rootfs_sha256(root: &Path) -> Result<String, BuildError> {
    let files = list_files(root)?;
    let mut records = Vec::new();
    for rel in &files {
        let p = root.join(rel);
        let ft = std::fs::symlink_metadata(&p)?;
        if ft.file_type().is_symlink() {
            let target = std::fs::read_link(&p)?.to_string_lossy().into_owned();
            records.push(format!("{rel}\0s\0{target}\0"));
        } else {
            let hex = sha256::hash_file(&p)?;
            records.push(format!("{rel}\0f\0{hex}\0"));
        }
    }
    let blob = records.concat();
    Ok(sha256::hash_hex(&blob.as_bytes()))
}

/// Copy a tree into the staging area, preserving symlinks AND
/// hardlinks.
///
/// - Symlinks: packaged as symlinks (the .gpkg must carry them, not
///   their targets).
/// - Hardlinks: a tree may contain hardlinked regular files (git's
///   `make install` hardlinks every `git-*` subcommand onto the single
///   dispatcher binary; perl hardlinks its library files). Copying
///   each occurrence's *content* would duplicate the bytes N times —
///   for git that is ~280 × 20 MB, and zstd (8 MB window) cannot
///   dedupe across the copies. Instead, the first path of each
///   `(dev, ino)` inode pair is copied once; every later path of the
///   same inode becomes a real hardlink in the staging tree, so the
///   system `tar` records it as one hardlink entry and the bytes are
///   stored (and compressed) exactly once.
///
/// On non-unix targets the legacy behavior (symlinks resolved, no
/// hardlinks) is kept: hardlink tracking requires unix inodes.
#[cfg(unix)]
fn copy_tree(src: &Path, dst: &Path) -> Result<(), BuildError> {
    // Canonical member of each hardlink group = the sorted-first
    // relative path of that inode (list_files is sorted), so the
    // choice is deterministic and independent of readdir order.
    let files = list_files(src)?;
    let mut canonical: HashMap<(u64, u64), String> = HashMap::new();
    for rel in &files {
        let p = src.join(rel);
        let ft = std::fs::symlink_metadata(&p)?.file_type();
        if ft.is_symlink() {
            continue;
        }
        use std::os::unix::fs::MetadataExt;
        let meta = p.metadata()?;
        canonical
            .entry((meta.dev(), meta.ino()))
            .or_insert_with(|| rel.clone());
    }

    // Phase 1: create the directory skeleton.
    let mut dirs = Vec::new();
    walk_dirs(src, &mut dirs)?;
    for p in &dirs {
        let rel = p
            .strip_prefix(src)
            .map(|r| r.display().to_string())
            .ok()
            .filter(|r| !r.is_empty())
            .unwrap_or_default();
        if !rel.is_empty() {
            std::fs::create_dir_all(dst.join(&rel))?;
        }
    }

    // Phase 2: files in sorted relative-path order. The canonical
    // member of a hardlink group sorts first, so when a later path of
    // the same inode is reached its link target already exists.
    for rel in &files {
        let p = src.join(rel);
        let q = dst.join(rel);
        let ft = std::fs::symlink_metadata(&p)?.file_type();
        if ft.is_symlink() {
            let target = std::fs::read_link(&p)?;
            use std::os::unix::fs::symlink;
            symlink(&target, &q)?;
        } else {
            use std::os::unix::fs::MetadataExt;
            let meta = p.metadata()?;
            let key = (meta.dev(), meta.ino());
            let canon = canonical
                .get(&key)
                .cloned()
                .ok_or_else(|| BuildError::Io(std::io::Error::other("no canonical inode")))?;
            if canon == *rel {
                std::fs::copy(&p, &q)?;
            } else {
                std::fs::hard_link(dst.join(&canon), &q)?;
            }
        }
    }
    Ok(())
}

/// Collect every directory under `src` (excluding `src` itself),
/// depth-first, into `out`.
#[cfg(unix)]
fn walk_dirs(src: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let p = entry.path();
        if entry.file_type()?.is_dir() {
            out.push(p.clone());
            walk_dirs(&p, out)?;
        }
    }
    Ok(())
}

/// Non-unix: the legacy copy (symlinks resolved to their targets when
/// they point inside the tree, otherwise a content copy). Hardlinks
/// are not tracked on this target.
#[cfg(not(unix))]
fn copy_tree(src: &Path, dst: &Path) -> Result<(), BuildError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let p = entry.path();
        let q = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            let target = std::fs::read_link(&p)?;
            if target.is_dir() {
                copy_tree(&target, &q)?;
            } else {
                std::fs::copy(&p, &q)?;
            }
        } else if ft.is_dir() {
            copy_tree(&p, &q)?;
        } else {
            std::fs::copy(&p, &q)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("gozpak-build-{pid}-pack-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn list_files_is_relative_and_sorted() {
        let t = temp("list");
        std::fs::create_dir_all(t.join("usr/lib")).unwrap();
        std::fs::create_dir_all(t.join("bin")).unwrap();
        std::fs::create_dir_all(t.join("etc")).unwrap();
        std::fs::write(t.join("usr/lib/libfoo.so"), b"lib").unwrap();
        std::fs::write(t.join("bin/tool"), b"bin").unwrap();
        std::fs::write(t.join("etc/conf"), b"etc").unwrap();
        let files = list_files(&t).unwrap();
        assert_eq!(
            files,
            vec!["bin/tool", "etc/conf", "usr/lib/libfoo.so"]
        );
    }

    #[test]
    fn list_files_includes_symlinks_not_dirs() {
        let t = temp("links");
        std::fs::create_dir_all(t.join("usr/lib")).unwrap();
        std::fs::write(t.join("usr/lib/libfoo.so.1"), b"lib").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("libfoo.so.1", t.join("usr/lib/libfoo.so")).unwrap();
        }
        let files = list_files(&t).unwrap();
        assert!(files.contains(&"usr/lib/libfoo.so.1".to_string()));
        assert!(
            !files.iter().any(|f| f == "usr/lib" || f.ends_with('/')),
            "directories are not listed: {files:?}"
        );
        #[cfg(unix)]
        assert!(files.contains(&"usr/lib/libfoo.so".to_string()));
    }

    #[test]
    fn pack_round_trips_through_zstd_and_tar() {
        use gozpak_core::VersionRel;
        use gozpak_recipe::{Recipe, RecipeKind};

        let t = temp("roundtrip");
        let dest = t.join("dest");
        std::fs::create_dir_all(dest.join("bin")).unwrap();
        std::fs::write(dest.join("bin/tool"), b"payload").unwrap();

        let recipe = Recipe {
            kind: RecipeKind::Toml,
            name: "tool".into(),
            epoch: 0,
            version: VersionRel::new("1.0", "2"),
            dir: t.clone(),
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
            install_post: vec![],
            install_files: "auto".into(),
            install_backup: vec![],
            warnings: vec![],
        };

        let out_dir = t.join("out");
        let p = pack(&recipe, &dest, &t, &out_dir, false).unwrap();

        // Filename comes from the gpkg builder: epoch 0 omitted.
        assert_eq!(
            p.gpkg.file_name().unwrap(),
            "tool-1.0-2.gpkg"
        );
        assert_eq!(p.files, vec!["bin/tool"]);

        // .gpkg is a zstd stream: decode it, extract with tar.
        let bytes = std::fs::read(&p.gpkg).unwrap();
        let raw = zstd::decode_all(&bytes[..]).unwrap();
        let extracted = t.join("extracted");
        std::fs::create_dir_all(&extracted).unwrap();
        let tarball = t.join("decoded.tar");
        std::fs::write(&tarball, &raw).unwrap();
        let status = std::process::Command::new("tar")
            .args([
                "xf",
                &tarball.display().to_string(),
                "-C",
                &extracted.display().to_string(),
            ])
            .status()
            .unwrap();
        assert!(status.success());

        // manifest.toml parses and matches.
        let manifest_text = std::fs::read_to_string(extracted.join("manifest.toml")).unwrap();
        let m: crate::Manifest = toml::from_str(&manifest_text).unwrap();
        assert_eq!(m.package.name, "tool");
        assert_eq!(m.manifest.files, vec!["bin/tool".to_string()]);
        assert!(m.manifest.repo.is_none());
        // rootfs content is byte-identical.
        assert_eq!(
            std::fs::read(extracted.join("rootfs/bin/tool")).unwrap(),
            b"payload"
        );
        // The .gpkg's own sha256 verifies.
        assert_eq!(
            sha256::hash_file(&p.gpkg).unwrap(),
            p.gpkg_sha256
        );

        // sha256(rootfs) recomputes from the *extracted* tree — the
        // exact flow the M2b verifier will use.
        assert_eq!(
            rootfs_sha256(&extracted.join("rootfs")).unwrap(),
            m.manifest.rootfs_sha256
        );
    }

    #[test]
    fn rootfs_sha256_matches_independent_oracle() {
        // Vectors computed with python3 hashlib over the canonical
        // record form (path \0 type \0 content \0), independent of the
        // Rust implementation.
        let t = temp("tree-hash-a");
        std::fs::write(t.join("a"), b"x\n").unwrap();
        assert_eq!(
            rootfs_sha256(&t).unwrap(),
            "e8509fc7f42d80fed2acbd33ef76b42a590f4acdc4e1ad35cb819e91999d5bac"
        );

        // Empty tree: SHA-256 of the empty string.
        let e = temp("tree-hash-empty");
        assert_eq!(
            rootfs_sha256(&e).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        #[cfg(unix)]
        {
            let t = temp("tree-hash-symlink");
            std::fs::create_dir_all(t.join("b")).unwrap();
            std::fs::write(t.join("b/y.txt"), b"data").unwrap();
            std::os::unix::fs::symlink("b/y.txt", t.join("link")).unwrap();
            assert_eq!(
                rootfs_sha256(&t).unwrap(),
                "4059dba5b727785d7d409ec6c645a077f25fe7c8bb8242e43e74aff690567395"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn pack_preserves_hardlinks_not_duplicated_content() {
        // A hardlink group (two paths, one inode) must be archived
        // as one file + one tar hardlink entry — never as two copies
        // of the content (the git-git-* case: 280 × 20 MB).
        use gozpak_core::VersionRel;
        use gozpak_recipe::{Recipe, RecipeKind};
        use std::os::unix::fs::MetadataExt;

        let t = temp("hardlink");
        let dest = t.join("dest");
        std::fs::create_dir_all(dest.join("usr/libexec")).unwrap();
        let canon = dest.join("usr/libexec/git");
        std::fs::write(&canon, b"payload-payload").unwrap();
        let link = dest.join("usr/libexec/git-am");
        std::fs::hard_link(&canon, &link).unwrap();

        // Confirm the fixture really is one inode.
        assert_eq!(canon.metadata().unwrap().ino(), link.metadata().unwrap().ino());

        let recipe = Recipe {
            kind: RecipeKind::Toml,
            name: "git".into(),
            epoch: 0,
            version: VersionRel::new("1.0", "1"),
            dir: t.clone(),
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
            install_post: vec![],
            install_files: "auto".into(),
            install_backup: vec![],
            warnings: vec![],
        };

        let out_dir = t.join("out");
        let p = pack(&recipe, &dest, &t, &out_dir, false).unwrap();

        // Both paths are listed as files.
        assert_eq!(p.files, vec!["usr/libexec/git", "usr/libexec/git-am"]);

        // The archive must be far smaller than two copies of the
        // content would be: the second entry is a tar hardlink.
        let bytes = std::fs::read(&p.gpkg).unwrap();
        let raw = zstd::decode_all(&bytes[..]).unwrap();
        let extracted = t.join("extracted");
        std::fs::create_dir_all(&extracted).unwrap();
        let tarball = t.join("decoded-hl.tar");
        std::fs::write(&tarball, &raw).unwrap();
        let status = std::process::Command::new("tar")
            .args(["xf", &tarball.display().to_string(), "-C", &extracted.display().to_string()])
            .status()
            .unwrap();
        assert!(status.success());

        let a = extracted.join("rootfs/usr/libexec/git");
        let b = extracted.join("rootfs/usr/libexec/git-am");
        assert_eq!(std::fs::read(&a).unwrap(), b"payload-payload");
        assert_eq!(std::fs::read(&b).unwrap(), b"payload-payload");
        // And the hardlink survived the round trip: same inode.
        assert_eq!(a.metadata().unwrap().ino(), b.metadata().unwrap().ino());
    }

    #[test]
    fn rootfs_sha256_sensitivity() {
        let t = temp("tree-sensitivity");
        std::fs::create_dir_all(t.join("d")).unwrap();
        std::fs::write(t.join("d/f"), b"one").unwrap();
        let h1 = rootfs_sha256(&t).unwrap();
        // Different content, same name.
        std::fs::write(t.join("d/f"), b"two").unwrap();
        let h2 = rootfs_sha256(&t).unwrap();
        assert_ne!(h1, h2);
        // Same content, different path.
        let t2 = temp("tree-sensitivity-2");
        std::fs::create_dir_all(t2.join("e")).unwrap();
        std::fs::write(t2.join("e/f"), b"one").unwrap();
        assert_ne!(h1, rootfs_sha256(&t2).unwrap());
        // Deterministic: identical trees hash identical.
        assert_eq!(h2, {
            let t3 = temp("tree-sensitivity-3");
            std::fs::create_dir_all(t3.join("d")).unwrap();
            std::fs::write(t3.join("d/f"), b"two").unwrap();
            rootfs_sha256(&t3).unwrap()
        });
    }
}
