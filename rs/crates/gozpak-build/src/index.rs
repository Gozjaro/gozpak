//! Build the binary-repo `index.json` from a directory of `.gpkg`
//! files (M2b, packaging host side).
//!
//! Each `.gpkg` (tar.zst: `manifest.toml` + `rootfs/`) is unpacked
//! into a scratch dir, its manifest read, and one
//! [`gozpak_repo::BinaryEntry`] produced per package:
//!
//! - `sha256`/`size` — of the `.gpkg` file itself (what `gozpak get`
//!   verifies after the download),
//! - `url` — `<base-url>/<filename>` when a base URL is given (the
//!   `releases/download/<tag>` asset channel, see
//!   [`gozpak_repo::remote`]), the bare filename when building a
//!   local/relay repo,
//! - everything else straight from the manifest's `[package]` table.
//!
//! Entries are sorted by name (deterministic output); the JSON is
//! written pretty so a `git diff` on `index.json` stays readable.

use std::path::{Path, PathBuf};

use gozpak_repo::BinaryEntry;

use crate::manifest::Manifest;
use crate::proc;

/// One build-side index error.
#[derive(Debug)]
pub enum IndexError {
    /// No `*.gpkg` files under the directory.
    Empty,
    /// A `.gpkg` failed to unpack/parse.
    Package { file: String, message: String },
    /// Writing `index.json` failed.
    Io(std::io::Error),
    /// Serialization failed.
    Serialize(String),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Empty => write!(f, "no .gpkg files found"),
            IndexError::Package { file, message } => {
                write!(f, "cannot index {file}: {message}")
            }
            IndexError::Io(e) => write!(f, "io error: {e}"),
            IndexError::Serialize(m) => write!(f, "cannot serialize index.json: {m}"),
        }
    }
}

impl std::error::Error for IndexError {}

/// One unpacked `.gpkg`'s contribution to the index.
fn entry_from_gpkg(file: &Path, base_url: &str) -> Result<BinaryEntry, IndexError> {
    let tmp = std::env::temp_dir().join(format!(
        "gozpak-index-{}-{}",
        std::process::id(),
        file.file_name().and_then(|n| n.to_str()).unwrap_or("gpkg")
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    let stage = tmp.join("stage");
    std::fs::create_dir_all(&stage).map_err(IndexError::Io)?;
    let err = |m: String| IndexError::Package {
        file: file.display().to_string(),
        message: m,
    };

    // Decode the tar.zst container: zstd crate + system tar (the M2a
    // pipeline's own tools).
    let raw = std::fs::read(file).map_err(|e| err(e.to_string()))?;
    let decoded =
        zstd::decode_all(&raw[..]).map_err(|e| err(format!("zstd decode: {e}")))?;
    let tarball = tmp.join("pkg.tar");
    std::fs::write(&tarball, &decoded).map_err(IndexError::Io)?;
    proc::run(
        "index",
        &tmp,
        "tar",
        &["-xf", tarball.to_str().unwrap(), "-C", stage.to_str().unwrap()],
        &[],
    )
    .map_err(|e| err(e.to_string()))?;

    let manifest_text = std::fs::read_to_string(stage.join("manifest.toml"))
        .map_err(|e| err(format!("missing manifest.toml: {e}")))?;
    let m: Manifest =
        toml::from_str(&manifest_text).map_err(|e| err(format!("manifest.toml: {e}")))?;
    let _ = std::fs::remove_dir_all(&tmp);

    let filename = match file.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_string(),
        None => return Err(err("unreadable filename".into())),
    };
    let release: u32 = m
        .package
        .release
        .parse()
        .map_err(|_| err(format!("release '{}' is not a u32", m.package.release)))?;

    Ok(BinaryEntry {
        name: m.package.name,
        version: m.package.version,
        release,
        epoch: m.package.epoch,
        arch: m.package.arch,
        sha256: gozpak_core::sha256::hash_file(file).map_err(IndexError::Io)?,
        size: std::fs::metadata(file).map_err(IndexError::Io)?.len(),
        url: if base_url.is_empty() {
            filename
        } else {
            format!("{base_url}/{filename}")
        },
        depends: m.package.depends,
        provides: m.package.provides,
        conflicts: m.package.conflicts,
        replaces: m.package.replaces,
    })
}

/// Index every `*.gpkg` in `dir` (non-recursive, sorted by name).
pub fn build_index(dir: &Path, base_url: &str) -> Result<Vec<BinaryEntry>, IndexError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(IndexError::Io)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "gpkg"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(IndexError::Empty);
    }
    let mut entries = files
        .iter()
        .map(|f| entry_from_gpkg(f, base_url))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Write `index.json` (pretty, one entry per package) into `dir`.
pub fn write_index(dir: &Path, entries: &[BinaryEntry]) -> Result<PathBuf, IndexError> {
    let text = serde_json::to_string_pretty(entries).map_err(|e| IndexError::Serialize(e.to_string()))?;
    let path = dir.join("index.json");
    std::fs::write(&path, text + "\n").map_err(IndexError::Io)?;
    Ok(path)
}

/// Sign `dir/index.json` (the exact bytes on disk) with the seed,
/// writing `dir/index.json.sig` (128 hex chars + newline).
pub fn sign_index(
    dir: &Path,
    seed: &[u8],
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let index = dir.join("index.json");
    let bytes = std::fs::read(&index)?;
    let sig = gozpak_repo::signing::sign(seed, &bytes)?;
    let out = dir.join("index.json.sig");
    std::fs::write(&out, gozpak_repo::signing::to_hex(&sig) + "\n")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gozpak_core::VersionRel;
    use gozpak_recipe::{Recipe, RecipeKind};

    /// Pack a one-file fixture `.gpkg` (the same flow M2a's pack test
    /// uses) and return its path.
    fn fixture_gpkg(dir: &Path, name: &str, version: &str, release: &str, payload: &str) -> PathBuf {
        let dest = dir.join(format!("{name}-dest"));
        std::fs::create_dir_all(dest.join("bin")).unwrap();
        std::fs::write(dest.join("bin/tool"), payload).unwrap();
        let recipe = Recipe {
            kind: RecipeKind::Toml,
            name: name.into(),
            epoch: 0,
            version: VersionRel::new(version, release),
            dir: dir.to_path_buf(),
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
        let packed = crate::pack::pack(&recipe, &dest, dir, dir, true).unwrap();
        // Move the .gpkg out of the work dir into `dir` cleanly.
        let out = dir.join(packed.gpkg.file_name().unwrap());
        std::fs::rename(&packed.gpkg, &out).unwrap();
        out
    }

    #[test]
    fn build_index_from_gpkgs() {
        let t = std::env::temp_dir().join(format!(
            "gozpak-build-test-{}-index-build",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();

        let a = fixture_gpkg(&t, "libfoo", "1.0", "1", "foo");
        let b = fixture_gpkg(&t, "tool", "2.0", "3", "tool");
        let entries = build_index(&t, "https://h.example.org/releases/download/stable").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "libfoo"); // sorted by name
        assert_eq!(entries[0].version, "1.0");
        assert_eq!(entries[0].release, 1);
        assert_eq!(
            entries[0].url,
            "https://h.example.org/releases/download/stable/libfoo-1.0-1.gpkg"
        );
        assert_eq!(entries[1].name, "tool");
        assert_eq!(entries[1].release, 3);
        // sha256/size describe the .gpkg file itself.
        assert_eq!(
            entries[0].sha256,
            gozpak_core::sha256::hash_file(&a).unwrap()
        );
        assert!(entries[0].size > 0);
        assert!(b.exists() && a.exists());

        // The written index.json round-trips through the M1 parser.
        let written = write_index(&t, &entries).unwrap();
        let text = std::fs::read_to_string(&written).unwrap();
        let parsed = gozpak_repo::parse_index_json(&text).unwrap();
        assert_eq!(parsed, entries);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn build_index_empty_dir() {
        let t = std::env::temp_dir().join(format!(
            "gozpak-build-test-{}-index-empty",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        assert!(matches!(build_index(&t, ""), Err(IndexError::Empty)));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn sign_index_verifies() {
        let t = std::env::temp_dir().join(format!(
            "gozpak-build-test-{}-index-sign",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        fixture_gpkg(&t, "libfoo", "1.0", "1", "foo");
        let entries = build_index(&t, "").unwrap();
        write_index(&t, &entries).unwrap();

        let (seed, pubkey) = gozpak_repo::signing::keygen();
        let sig_path = sign_index(&t, &seed).unwrap();
        assert_eq!(sig_path, t.join("index.json.sig"));
        let index_bytes = std::fs::read(t.join("index.json")).unwrap();
        let sig = gozpak_repo::signing::hex_sig(
            &std::fs::read_to_string(&sig_path).unwrap(),
        )
        .unwrap();
        assert!(gozpak_repo::signing::verify(&pubkey, &index_bytes, &sig).is_ok());
        // Tamper → fails.
        let _ = std::fs::write(t.join("index.json"), b"[]\n");
        let index_bytes = std::fs::read(t.join("index.json")).unwrap();
        assert!(gozpak_repo::signing::verify(&pubkey, &index_bytes, &sig).is_err());
        let _ = std::fs::remove_dir_all(&t);
    }
}
